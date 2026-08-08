//! Minimal UHCI host controller driver for HID boot devices.
//!
//! Supports Intel PIIX3 UHCI (and any PCI class 0x0C/0x03 prog-IF 0x00) with
//! I/O BAR register access. Control transfers run synchronously; interrupt
//! IN endpoints are re-armed each [`Uhci::poll`].

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::memory::{self, DmaRegion};
use crate::pci::{self, Bar, Device};

const VENDOR_INTEL: u16 = 0x8086;
const UHCI_PIIX3: u16 = 0x7020;
const UHCI_PIIX4: u16 = 0x7112;
const UHCI_ICH: u16 = 0x2412;

// UHCI I/O register offsets
const USBCMD: u16 = 0x00;
const USBSTS: u16 = 0x02;
const USBINTR: u16 = 0x04;
const FRNUM: u16 = 0x06;
const FLBASEADD: u16 = 0x08;
const SOFMOD: u16 = 0x0C;
const PORTSC: u16 = 0x10; // port N at + N*2

const USBCMD_RS: u16 = 1 << 0;
const USBCMD_HCRESET: u16 = 1 << 1;
const USBCMD_GRESET: u16 = 1 << 2;
const USBCMD_MAXP: u16 = 1 << 7;

const PORT_CCS: u16 = 1 << 0;
const PORT_CSC: u16 = 1 << 1;
const PORT_PED: u16 = 1 << 2;
const PORT_PR: u16 = 1 << 9;

// TD / QH link flags
const LINK_TERM: u32 = 1;
const LINK_QH: u32 = 1 << 1;
const LINK_DEPTH: u32 = 1 << 2;

// TD control/status
const TD_ACTIVE: u32 = 1 << 23;
const TD_IOC: u32 = 1 << 24;
const TD_SPD: u32 = 1 << 29;

const PID_SETUP: u8 = 0x2D;
const PID_IN: u8 = 0x69;
const PID_OUT: u8 = 0xE1;

const MAX_PORTS: usize = 2;
const MAX_DEVICES: usize = 4;

#[repr(C, align(16))]
struct Td {
    link: u32,
    status: u32,
    token: u32,
    buffer: u32,
}

#[repr(C, align(16))]
struct Qh {
    head_link: u32,
    element_link: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HidKind {
    Keyboard,
    Mouse,
}

struct HidDevice {
    addr: u8,
    endpoint: u8,
    max_packet: u16,
    kind: HidKind,
    /// Interrupt TD lives in the shared DMA pool; index into td slots.
    td_slot: usize,
    buf_off: usize,
    prev_kbd: [u8; 8],
    report_len: usize,
}

/// A USB Mass Storage (Bulk-Only Transport) device — bulk endpoint addresses
/// and per-endpoint DATA0/1 toggle state. The SCSI/BOT protocol itself lives
/// in `usb::msc`; this struct is just enough for [`Uhci::msc_bot_transfer`]
/// to move bytes over the two bulk pipes.
struct MscDevice {
    addr: u8,
    ep_in: u8,
    ep_out: u8,
    mps_in: u16,
    mps_out: u16,
    in_toggle: bool,
    out_toggle: bool,
}

pub struct Uhci {
    io_base: u16,
    /// 4 KiB frame list + scratch (TDs, QHs, buffers).
    dma: DmaRegion,
    frame_list_off: usize,
    /// Scratch QH for one-shot control transfers.
    ctrl_qh_off: usize,
    /// Scratch TDs for control (SETUP + DATA + STATUS).
    ctrl_td_off: usize,
    /// Interrupt TD slots (one per HID device).
    int_td_base: usize,
    /// Report buffers.
    buf_base: usize,
    next_addr: u8,
    devices: Vec<HidDevice>,
    /// At most one mass-storage device — one USB drive is what this OS needs.
    msc: Option<MscDevice>,
    /// Set for one `poll()` after a mass-storage device is newly attached,
    /// so `fs::usb_tick` knows to try a mount without polling constantly.
    msc_just_attached: bool,
    ports_seen: [bool; MAX_PORTS],
}

impl Uhci {
    pub fn probe() -> Option<Self> {
        let dev = pci::find(VENDOR_INTEL, &[UHCI_PIIX3, UHCI_PIIX4, UHCI_ICH])
            .or_else(find_uhci_by_class)?;
        let io = match dev.bar(4).or_else(|| dev.bar(0))? {
            Bar::Io(a) => a as u16,
            Bar::Memory(_) => {
                crate::warn_line("USB: UHCI BAR is MMIO — expected I/O space");
                return None;
            }
        };
        // UHCI needs I/O space + bus master for DMA to the frame list.
        dev.enable(pci::command::BUS_MASTER | pci::command::IO_SPACE);

        let dma = memory::alloc_dma(16 * 1024)?;
        // Layout inside DMA:
        //   [0, 4096)           frame list
        //   [4096, 4112)        control QH
        //   [4112, 4112+16*16)  up to 16 control TDs (SETUP + DATA*N + STATUS)
        //   [4368, 4368+4*16)   interrupt TDs
        //   [4432, ...)         buffers
        let ctrl_qh_off = 4096;
        let ctrl_td_off = 4112;
        let int_td_base = ctrl_td_off + 16 * 16;
        let buf_base = int_td_base + 4 * 16;
        let mut uhci = Uhci {
            io_base: io,
            dma,
            frame_list_off: 0,
            ctrl_qh_off,
            ctrl_td_off,
            int_td_base,
            buf_base,
            next_addr: 1,
            devices: Vec::new(),
            msc: None,
            msc_just_attached: false,
            ports_seen: [false; MAX_PORTS],
        };
        uhci.reset_and_start();
        Some(uhci)
    }

    fn reset_and_start(&mut self) {
        // Global reset briefly, then host controller reset.
        self.outw(USBCMD, USBCMD_GRESET);
        busy_wait(10);
        self.outw(USBCMD, 0);
        busy_wait(1);
        self.outw(USBCMD, USBCMD_HCRESET);
        for _ in 0..10_000 {
            if self.inw(USBCMD) & USBCMD_HCRESET == 0 {
                break;
            }
            busy_wait(1);
        }

        // Empty frame list: every slot terminates.
        let fl = self.frame_list_mut();
        for e in fl.iter_mut() {
            *e = LINK_TERM;
        }
        let fl_phys = self.dma.phys + self.frame_list_off as u64;
        self.outl(FLBASEADD, fl_phys as u32);
        self.outw(FRNUM, 0);
        self.outw(USBINTR, 0); // poll mode — no IRQ yet
        self.outw(SOFMOD, 64);
        self.outw(USBSTS, 0xFFFF); // clear status
        // Run + 64-byte max packet.
        self.outw(USBCMD, USBCMD_RS | USBCMD_MAXP);
    }

    /// Reset connected ports and attach HID boot devices. Returns device count.
    pub fn enumerate_ports(&mut self) -> usize {
        for port in 0..MAX_PORTS {
            let sc = self.inw(PORTSC + (port as u16) * 2);
            if sc & PORT_CCS == 0 {
                continue;
            }
            if !self.reset_port(port) {
                continue;
            }
            self.ports_seen[port] = true;
            self.attach_from_address0();
            if self.devices.len() >= MAX_DEVICES {
                break;
            }
        }
        self.devices.len()
    }

    /// Whatever sits at address 0 after a port reset — HID device or hub.
    fn attach_from_address0(&mut self) {
        let mut desc = [0u8; 18];
        if self
            .control_in(0, 0x80, 6, 0x0100, 0, &mut desc[..8], 8)
            .is_err()
        {
            return;
        }
        let max_packet = if desc[7] == 0 { 8 } else { desc[7] as u16 };
        let addr = self.next_addr;
        if self.set_address(0, addr).is_err() {
            return;
        }
        self.next_addr = self.next_addr.saturating_add(1);
        let _ = self.control_in(addr, 0x80, 6, 0x0100, 0, &mut desc, max_packet);

        // Hub at the device class level (common for QEMU's intermediate hub).
        if desc[4] == 9 {
            crate::serial_println!("USB: hub at addr {}", addr);
            self.enumerate_hub(addr, max_packet);
            return;
        }

        if let Some(dev) = self.finish_hid_attach(addr, max_packet) {
            self.devices.push(dev);
            return;
        }
        if self.msc.is_none() {
            if let Some(dev) = self.finish_msc_attach(addr, max_packet) {
                self.msc = Some(dev);
                self.msc_just_attached = true;
            }
        }
    }

    /// Power and reset each hub port, then attach whatever appears at addr 0.
    fn enumerate_hub(&mut self, hub_addr: u8, mps: u16) {
        let mut cfg_hdr = [0u8; 9];
        if self
            .control_in(hub_addr, 0x80, 6, 0x0200, 0, &mut cfg_hdr, mps)
            .is_err()
        {
            return;
        }
        let cfg_value = cfg_hdr[5];
        if self
            .control_out(hub_addr, 0x00, 9, cfg_value as u16, 0, &[], mps)
            .is_err()
        {
            return;
        }

        // Hub descriptor (class GET_DESCRIPTOR, type 0x29).
        let mut hub_desc = [0u8; 16];
        if self
            .control_in(hub_addr, 0xA0, 6, 0x2900, 0, &mut hub_desc[..9], mps)
            .is_err()
        {
            return;
        }
        let nports = hub_desc[2] as usize;
        let nports = nports.clamp(1, 8);
        crate::serial_println!("USB: hub {} has {} ports", hub_addr, nports);

        for port in 1..=nports {
            // PORT_POWER = 8
            let _ = self.control_out(hub_addr, 0x23, 3, 8, port as u16, &[], mps);
        }
        busy_wait(100);

        for port in 1..=nports {
            if self.devices.len() >= MAX_DEVICES {
                break;
            }
            let mut status = [0u8; 4];
            if self
                .control_in(hub_addr, 0xA3, 0, 0, port as u16, &mut status, mps)
                .is_err()
            {
                continue;
            }
            let st = u16::from_le_bytes([status[0], status[1]]);
            if st & 1 == 0 {
                continue; // not connected
            }
            // PORT_RESET = 4
            let _ = self.control_out(hub_addr, 0x23, 3, 4, port as u16, &[], mps);
            busy_wait(50);
            // Wait for reset complete (C_PORT_RESET in change bits).
            for _ in 0..20 {
                busy_wait(10);
                if self
                    .control_in(hub_addr, 0xA3, 0, 0, port as u16, &mut status, mps)
                    .is_err()
                {
                    break;
                }
                let change = u16::from_le_bytes([status[2], status[3]]);
                if change & (1 << 4) != 0 {
                    break;
                }
            }
            // CLEAR_FEATURE C_PORT_RESET = 20
            let _ = self.control_out(hub_addr, 0x23, 1, 20, port as u16, &[], mps);
            // CLEAR_FEATURE C_PORT_CONNECTION = 16
            let _ = self.control_out(hub_addr, 0x23, 1, 16, port as u16, &[], mps);
            busy_wait(10);

            // New device answers at address 0.
            self.attach_from_address0();
        }
    }

    fn finish_hid_attach(&mut self, addr: u8, max_packet: u16) -> Option<HidDevice> {
        let mut cfg_hdr = [0u8; 9];
        if self
            .control_in(addr, 0x80, 6, 0x0200, 0, &mut cfg_hdr, max_packet)
            .is_err()
        {
            return None;
        }
        let total = u16::from_le_bytes([cfg_hdr[2], cfg_hdr[3]]) as usize;
        if total < 9 || total > 256 {
            return None;
        }
        let mut cfg = [0u8; 256];
        cfg[..9].copy_from_slice(&cfg_hdr);
        if total > 9 {
            if self
                .control_in(addr, 0x80, 6, 0x0200, 0, &mut cfg[..total], max_packet)
                .is_err()
            {
                return None;
            }
        }

        // Nested hub (rare on UHCI root) — recurse.
        if cfg.len() > 5 {
            // Check interface class in config for hub-as-interface.
            if let Some(9) = iface_class(&cfg[..total]) {
                crate::serial_println!("USB: interface-hub at addr {}", addr);
                let cfg_value = cfg[5];
                let _ = self.control_out(addr, 0x00, 9, cfg_value as u16, 0, &[], max_packet);
                self.enumerate_hub(addr, max_packet);
                return None;
            }
        }

        let cfg_value = cfg[5];
        let Some((iface, ep, maxp, kind)) = parse_hid_boot(&cfg[..total]) else {
            crate::serial_println!("USB: addr {} not a HID boot keyboard/mouse", addr);
            return None;
        };

        if self
            .control_out(addr, 0x00, 9, cfg_value as u16, 0, &[], max_packet)
            .is_err()
        {
            return None;
        }
        let _ = self.control_out(addr, 0x21, 0x0B, 0, iface as u16, &[], max_packet);
        let _ = self.control_out(addr, 0x21, 0x0A, 0, iface as u16, &[], max_packet);

        let slot = self.devices.len();
        let report_len = match kind {
            HidKind::Keyboard => 8,
            HidKind::Mouse => 4,
        };
        let buf_off = self.buf_base + slot * 16;
        self.arm_interrupt_td(slot, addr, ep, maxp, buf_off, report_len);

        let name = match kind {
            HidKind::Keyboard => "keyboard",
            HidKind::Mouse => "mouse",
        };
        crate::serial_println!(
            "USB: HID boot {} at addr {} ep {} (maxpkt {})",
            name,
            addr,
            ep,
            maxp
        );

        Some(HidDevice {
            addr,
            endpoint: ep,
            max_packet: maxp,
            kind,
            td_slot: slot,
            buf_off,
            prev_kbd: [0; 8],
            report_len,
        })
    }

    /// Whatever sits at address 0 is a mass-storage (Bulk-Only Transport)
    /// device — mirrors `finish_hid_attach`, but pulls out the two bulk
    /// endpoints instead of arming an interrupt TD.
    fn finish_msc_attach(&mut self, addr: u8, max_packet: u16) -> Option<MscDevice> {
        let mut cfg_hdr = [0u8; 9];
        if self
            .control_in(addr, 0x80, 6, 0x0200, 0, &mut cfg_hdr, max_packet)
            .is_err()
        {
            return None;
        }
        let total = u16::from_le_bytes([cfg_hdr[2], cfg_hdr[3]]) as usize;
        if total < 9 || total > 256 {
            return None;
        }
        let mut cfg = [0u8; 256];
        cfg[..9].copy_from_slice(&cfg_hdr);
        if total > 9 {
            if self
                .control_in(addr, 0x80, 6, 0x0200, 0, &mut cfg[..total], max_packet)
                .is_err()
            {
                return None;
            }
        }

        let cfg_value = cfg[5];
        let Some((_iface, ep_in, ep_out, mps_in, mps_out)) = parse_msc(&cfg[..total]) else {
            crate::serial_println!("USB: addr {} not a mass-storage bulk-only device", addr);
            return None;
        };
        if self
            .control_out(addr, 0x00, 9, cfg_value as u16, 0, &[], max_packet)
            .is_err()
        {
            return None;
        }

        crate::serial_println!(
            "USB: mass storage device at addr {} (in ep {} mps {}, out ep {} mps {})",
            addr,
            ep_in,
            mps_in,
            ep_out,
            mps_out
        );
        Some(MscDevice {
            addr,
            ep_in,
            ep_out,
            mps_in: mps_in.clamp(1, 64),
            mps_out: mps_out.clamp(1, 64),
            in_toggle: false,
            out_toggle: false,
        })
    }

    fn reset_port(&mut self, port: usize) -> bool {
        let reg = PORTSC + (port as u16) * 2;
        // Reset 50ms, then enable.
        let mut sc = self.inw(reg);
        sc |= PORT_PR;
        self.outw(reg, sc);
        busy_wait(50);
        sc = self.inw(reg);
        sc &= !PORT_PR;
        self.outw(reg, sc);
        busy_wait(10);
        // Clear connect status change, enable port.
        sc = self.inw(reg);
        sc |= PORT_PED | PORT_CSC;
        self.outw(reg, sc);
        busy_wait(10);
        self.inw(reg) & PORT_PED != 0
    }

    fn arm_interrupt_td(
        &mut self,
        slot: usize,
        addr: u8,
        ep: u8,
        _maxp: u16,
        buf_off: usize,
        len: usize,
    ) {
        let td_off = self.int_td_base + slot * core::mem::size_of::<Td>();
        let td_phys = (self.dma.phys + td_off as u64) as u32;
        self.reactivate_interrupt_td(slot, buf_off, len, addr, ep);
        // Stagger devices across frames so multiple HID endpoints coexist.
        let fl = self.frame_list_mut();
        for i in (slot..1024).step_by(8) {
            fl[i] = (td_phys & !0xF) | LINK_DEPTH;
        }
    }

    /// Reactivate an existing interrupt TD without rewriting the frame list.
    fn reactivate_interrupt_td(
        &mut self,
        slot: usize,
        buf_off: usize,
        len: usize,
        addr: u8,
        ep: u8,
    ) {
        let td_off = self.int_td_base + slot * core::mem::size_of::<Td>();
        let buf_phys = (self.dma.phys + buf_off as u64) as u32;
        unsafe {
            core::ptr::write_bytes((self.dma.virt + buf_off as u64) as *mut u8, 0, 16);
            let td = (self.dma.virt + td_off as u64) as *mut Td;
            let maxlen = (len.saturating_sub(1) as u32) & 0x7FF;
            (*td).link = LINK_TERM;
            (*td).status = TD_ACTIVE | TD_IOC | (3 << 27);
            (*td).token = (maxlen << 21)
                | ((0u32) << 19)
                | ((ep as u32) << 15)
                | ((addr as u32) << 8)
                | (PID_IN as u32);
            (*td).buffer = buf_phys;
        }
    }

    pub fn poll(&mut self) {
        // Hot-plug: check unused ports for new connect.
        for port in 0..MAX_PORTS {
            if self.ports_seen[port] || self.devices.len() >= MAX_DEVICES {
                continue;
            }
            let sc = self.inw(PORTSC + (port as u16) * 2);
            if sc & PORT_CCS != 0 {
                if self.reset_port(port) {
                    self.ports_seen[port] = true;
                    self.attach_from_address0();
                }
            }
        }

        for i in 0..self.devices.len() {
            let td_off = self.int_td_base + self.devices[i].td_slot * core::mem::size_of::<Td>();
            let status = unsafe {
                let td = (self.dma.virt + td_off as u64) as *const Td;
                (*td).status
            };
            if status & TD_ACTIVE != 0 {
                continue;
            }
            // Transfer finished (success or error). Read buffer on success.
            let actual = (status & 0x7FF) as usize;
            let ok = status & (1 << 22 | 1 << 21 | 1 << 20 | 1 << 19 | 1 << 18) == 0;
            if ok && actual != 0x7FF {
                let len = actual.wrapping_add(1).min(self.devices[i].report_len);
                let mut report = [0u8; 8];
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        (self.dma.virt + self.devices[i].buf_off as u64) as *const u8,
                        report.as_mut_ptr(),
                        len.min(8),
                    );
                }
                match self.devices[i].kind {
                    HidKind::Keyboard => {
                        let prev = self.devices[i].prev_kbd;
                        super::hid::handle_keyboard_report(&prev, &report);
                        self.devices[i].prev_kbd = report;
                    }
                    HidKind::Mouse => {
                        super::hid::handle_mouse_report(&report[..len.max(3).min(8)]);
                    }
                }
            }
            // Re-arm TD in place (frame list already points here).
            let d = &self.devices[i];
            let (slot, buf, len, addr, ep) =
                (d.td_slot, d.buf_off, d.report_len, d.addr, d.endpoint);
            self.reactivate_interrupt_td(slot, buf, len, addr, ep);
        }
    }

    pub fn status_line(&self) -> String {
        let mut parts = Vec::new();
        for d in &self.devices {
            parts.push(format!(
                "{}@{}",
                match d.kind {
                    HidKind::Keyboard => "kbd",
                    HidKind::Mouse => "mouse",
                },
                d.addr
            ));
        }
        if let Some(m) = &self.msc {
            parts.push(format!("mass-storage@{}", m.addr));
        }
        if parts.is_empty() {
            return format!(
                "USB: UHCI @ I/O {:#x}, no devices (try -device usb-kbd -device usb-mouse -device usb-storage)",
                self.io_base
            );
        }
        format!(
            "USB: UHCI @ I/O {:#x}, {}",
            self.io_base,
            parts.join(", ")
        )
    }

    // ── Control transfers ────────────────────────────────────────────────

    fn set_address(&mut self, current: u8, new_addr: u8) -> Result<(), ()> {
        self.control_out(current, 0x00, 5, new_addr as u16, 0, &[], 8)
    }

    fn control_in(
        &mut self,
        addr: u8,
        bm: u8,
        req: u8,
        value: u16,
        index: u16,
        data: &mut [u8],
        max_packet: u16,
    ) -> Result<(), ()> {
        let mut setup = [0u8; 8];
        setup[0] = bm;
        setup[1] = req;
        setup[2] = (value & 0xFF) as u8;
        setup[3] = (value >> 8) as u8;
        setup[4] = (index & 0xFF) as u8;
        setup[5] = (index >> 8) as u8;
        setup[6] = (data.len() & 0xFF) as u8;
        setup[7] = ((data.len() >> 8) & 0xFF) as u8;
        self.run_control(addr, &setup, Some(data), true, max_packet.max(8))
    }

    fn control_out(
        &mut self,
        addr: u8,
        bm: u8,
        req: u8,
        value: u16,
        index: u16,
        data: &[u8],
        max_packet: u16,
    ) -> Result<(), ()> {
        let mut setup = [0u8; 8];
        setup[0] = bm;
        setup[1] = req;
        setup[2] = (value & 0xFF) as u8;
        setup[3] = (value >> 8) as u8;
        setup[4] = (index & 0xFF) as u8;
        setup[5] = (index >> 8) as u8;
        setup[6] = (data.len() & 0xFF) as u8;
        setup[7] = ((data.len() >> 8) & 0xFF) as u8;
        if data.is_empty() {
            self.run_control(addr, &setup, None, false, max_packet.max(8))
        } else {
            let mut buf = [0u8; 64];
            if data.len() > buf.len() {
                return Err(());
            }
            buf[..data.len()].copy_from_slice(data);
            self.run_control(
                addr,
                &setup,
                Some(&mut buf[..data.len()]),
                false,
                max_packet.max(8),
            )
        }
    }

    /// SETUP → DATA (possibly several max-packet TDs) → STATUS.
    fn run_control(
        &mut self,
        addr: u8,
        setup: &[u8; 8],
        data: Option<&mut [u8]>,
        data_in: bool,
        max_packet: u16,
    ) -> Result<(), ()> {
        let setup_buf = self.buf_base + 512;
        let data_buf = self.buf_base + 528;
        unsafe {
            core::ptr::copy_nonoverlapping(
                setup.as_ptr(),
                (self.dma.virt + setup_buf as u64) as *mut u8,
                8,
            );
        }
        let data_len = data.as_ref().map(|d| d.len()).unwrap_or(0);
        if let Some(d) = data.as_ref() {
            if !data_in && !d.is_empty() {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        d.as_ptr(),
                        (self.dma.virt + data_buf as u64) as *mut u8,
                        d.len(),
                    );
                }
            }
        }

        let mps = max_packet as usize;
        let n_data = if data_len == 0 {
            0
        } else {
            (data_len + mps - 1) / mps
        };
        if n_data + 2 > 16 {
            return Err(());
        }

        let qh = self.ctrl_qh_off;
        let td_setup = self.ctrl_td_off;
        let td_status = self.ctrl_td_off + (1 + n_data) * 16;
        let qh_phys = (self.dma.phys + qh as u64) as u32;
        let td_setup_phys = (self.dma.phys + td_setup as u64) as u32;
        let td_status_phys = (self.dma.phys + td_status as u64) as u32;

        let first_after_setup = if n_data > 0 {
            (self.dma.phys + (td_setup + 16) as u64) as u32
        } else {
            td_status_phys
        };

        write_td(
            self.dma.virt,
            td_setup,
            (first_after_setup & !0xF) | LINK_DEPTH,
            TD_ACTIVE | (3 << 27),
            token(7, 0, 0, addr, PID_SETUP),
            (self.dma.phys + setup_buf as u64) as u32,
        );

        let pid = if data_in { PID_IN } else { PID_OUT };
        for i in 0..n_data {
            let off = i * mps;
            let chunk = (data_len - off).min(mps);
            let td_off = td_setup + 16 + i * 16;
            let next = if i + 1 < n_data {
                (self.dma.phys + (td_off + 16) as u64) as u32
            } else {
                td_status_phys
            };
            let toggle = ((i + 1) & 1) as u32; // DATA1, DATA0, DATA1, ...
            write_td(
                self.dma.virt,
                td_off,
                (next & !0xF) | LINK_DEPTH,
                TD_ACTIVE | TD_SPD | (3 << 27),
                token((chunk.saturating_sub(1) as u32) & 0x7FF, toggle, 0, addr, pid),
                (self.dma.phys + (data_buf + off) as u64) as u32,
            );
        }

        let status_pid = if data_len == 0 {
            PID_IN
        } else if data_in {
            PID_OUT
        } else {
            PID_IN
        };
        write_td(
            self.dma.virt,
            td_status,
            LINK_TERM,
            TD_ACTIVE | TD_IOC | (3 << 27),
            token(0x7FF, 1, 0, addr, status_pid),
            0,
        );

        unsafe {
            let q = (self.dma.virt + qh as u64) as *mut Qh;
            (*q).head_link = LINK_TERM;
            (*q).element_link = (td_setup_phys & !0xF) | LINK_DEPTH;
        }

        let saved: Vec<u32> = {
            let fl = self.frame_list_mut();
            let old: Vec<u32> = fl.iter().copied().collect();
            for e in fl.iter_mut() {
                *e = (qh_phys & !0xF) | LINK_QH;
            }
            old
        };

        let start = crate::clock::ticks();
        let ok = loop {
            let st = unsafe {
                let td = (self.dma.virt + td_status as u64) as *const Td;
                (*td).status
            };
            if st & TD_ACTIVE == 0 {
                let err = st & ((1 << 22) | (1 << 21) | (1 << 20) | (1 << 19) | (1 << 18));
                break err == 0;
            }
            if crate::clock::ticks().wrapping_sub(start) > 40 {
                break false;
            }
            busy_wait(1);
        };

        {
            let fl = self.frame_list_mut();
            for (e, v) in fl.iter_mut().zip(saved.iter()) {
                *e = *v;
            }
        }

        if !ok {
            return Err(());
        }
        if data_in {
            if let Some(d) = data {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        (self.dma.virt + data_buf as u64) as *const u8,
                        d.as_mut_ptr(),
                        d.len(),
                    );
                }
            }
        }
        Ok(())
    }

    // ── Mass storage (bulk transfers) ───────────────────────────────────

    pub fn has_msc(&self) -> bool {
        self.msc.is_some()
    }

    /// Consume the "a mass-storage device just showed up" flag.
    pub fn take_msc_just_attached(&mut self) -> bool {
        core::mem::replace(&mut self.msc_just_attached, false)
    }

    /// One Bulk-Only Transport transaction: CBW out, an optional data phase,
    /// then CSW in. `data` longer than one bulk scratch window is chunked
    /// into several [`Uhci::bulk_transfer`] calls automatically.
    pub fn msc_bot_transfer(
        &mut self,
        cbw: &[u8; 31],
        data: Option<&mut [u8]>,
        data_dir_in: bool,
    ) -> Result<[u8; 13], ()> {
        let (addr, ep_in, ep_out, mps_in, mps_out, mut in_t, mut out_t) = {
            let d = self.msc.as_ref().ok_or(())?;
            (
                d.addr, d.ep_in, d.ep_out, d.mps_in, d.mps_out, d.in_toggle, d.out_toggle,
            )
        };

        let mut cbw_buf = *cbw;
        self.bulk_transfer(addr, ep_out, PID_OUT, &mut cbw_buf, mps_out, &mut out_t)?;

        // A multiple of every legal full-speed bulk max-packet size (8/16/32/64),
        // so only the very last chunk of the whole transfer can be a short
        // packet — an earlier short packet would look like end-of-transfer to
        // the device and desync the Bulk-Only protocol.
        const CHUNK: usize = 1024;
        if let Some(buf) = data {
            if data_dir_in {
                for chunk in buf.chunks_mut(CHUNK) {
                    self.bulk_transfer(addr, ep_in, PID_IN, chunk, mps_in, &mut in_t)?;
                }
            } else {
                for chunk in buf.chunks_mut(CHUNK) {
                    self.bulk_transfer(addr, ep_out, PID_OUT, chunk, mps_out, &mut out_t)?;
                }
            }
        }

        let mut csw = [0u8; 13];
        self.bulk_transfer(addr, ep_in, PID_IN, &mut csw, mps_in, &mut in_t)?;

        if let Some(d) = self.msc.as_mut() {
            d.in_toggle = in_t;
            d.out_toggle = out_t;
        }
        Ok(csw)
    }

    /// Move up to 1024 bytes over a bulk endpoint, chunked into max-packet
    /// TDs. Reuses the control-transfer scratch QH/TD/buffer area: bulk and
    /// control transfers are both synchronous and never run concurrently, so
    /// there is nothing to share unsafely.
    fn bulk_transfer(
        &mut self,
        addr: u8,
        ep: u8,
        pid: u8,
        buf: &mut [u8],
        mps: u16,
        toggle: &mut bool,
    ) -> Result<(), ()> {
        let mps = (mps.max(1) as usize).min(64);
        let len = buf.len();
        if len == 0 {
            return Ok(());
        }
        let n = (len + mps - 1) / mps;
        if n > 16 {
            return Err(());
        }

        let data_buf = self.buf_base + 528;
        if pid == PID_OUT {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    buf.as_ptr(),
                    (self.dma.virt + data_buf as u64) as *mut u8,
                    len,
                );
            }
        }

        let td0 = self.ctrl_td_off;
        let mut tgl = *toggle;
        for i in 0..n {
            let off = i * mps;
            let chunk = (len - off).min(mps);
            let td_off = td0 + i * 16;
            let last = i + 1 == n;
            let link = if last {
                LINK_TERM
            } else {
                ((self.dma.phys + (td_off + 16) as u64) as u32 & !0xF) | LINK_DEPTH
            };
            let status = if last {
                TD_ACTIVE | TD_SPD | TD_IOC | (3 << 27)
            } else {
                TD_ACTIVE | TD_SPD | (3 << 27)
            };
            write_td(
                self.dma.virt,
                td_off,
                link,
                status,
                token((chunk.saturating_sub(1) as u32) & 0x7FF, tgl as u32, ep, addr, pid),
                (self.dma.phys + (data_buf + off) as u64) as u32,
            );
            tgl = !tgl;
        }

        let qh = self.ctrl_qh_off;
        let qh_phys = (self.dma.phys + qh as u64) as u32;
        let td0_phys = (self.dma.phys + td0 as u64) as u32;
        unsafe {
            let q = (self.dma.virt + qh as u64) as *mut Qh;
            (*q).head_link = LINK_TERM;
            (*q).element_link = (td0_phys & !0xF) | LINK_DEPTH;
        }

        let last_td_off = td0 + (n - 1) * 16;
        let saved: Vec<u32> = {
            let fl = self.frame_list_mut();
            let old: Vec<u32> = fl.iter().copied().collect();
            for e in fl.iter_mut() {
                *e = (qh_phys & !0xF) | LINK_QH;
            }
            old
        };

        let start = crate::clock::ticks();
        let ok = loop {
            let st = unsafe {
                let td = (self.dma.virt + last_td_off as u64) as *const Td;
                (*td).status
            };
            if st & TD_ACTIVE == 0 {
                let err = st & ((1 << 22) | (1 << 21) | (1 << 20) | (1 << 19) | (1 << 18));
                break err == 0;
            }
            if crate::clock::ticks().wrapping_sub(start) > 40 {
                break false;
            }
            busy_wait(1);
        };

        {
            let fl = self.frame_list_mut();
            for (e, v) in fl.iter_mut().zip(saved.iter()) {
                *e = *v;
            }
        }

        if !ok {
            return Err(());
        }
        *toggle = tgl;
        if pid == PID_IN {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (self.dma.virt + data_buf as u64) as *const u8,
                    buf.as_mut_ptr(),
                    len,
                );
            }
        }
        Ok(())
    }

    fn frame_list_mut(&mut self) -> &mut [u32] {
        unsafe {
            core::slice::from_raw_parts_mut(
                (self.dma.virt + self.frame_list_off as u64) as *mut u32,
                1024,
            )
        }
    }

    fn inw(&self, off: u16) -> u16 {
        let port = self.io_base + off;
        let val: u16;
        unsafe {
            core::arch::asm!("in ax, dx", in("dx") port, out("ax") val,
                             options(nomem, nostack, preserves_flags));
        }
        val
    }

    fn outw(&self, off: u16, val: u16) {
        let port = self.io_base + off;
        unsafe {
            core::arch::asm!("out dx, ax", in("dx") port, in("ax") val,
                             options(nomem, nostack, preserves_flags));
        }
    }

    fn outl(&self, off: u16, val: u32) {
        let port = self.io_base + off;
        unsafe {
            core::arch::asm!("out dx, eax", in("dx") port, in("eax") val,
                             options(nomem, nostack, preserves_flags));
        }
    }
}

fn token(maxlen: u32, toggle: u32, ep: u8, addr: u8, pid: u8) -> u32 {
    ((maxlen & 0x7FF) << 21)
        | ((toggle & 1) << 19)
        | ((ep as u32) << 15)
        | ((addr as u32) << 8)
        | (pid as u32)
}

fn write_td(dma_virt: u64, off: usize, link: u32, status: u32, token: u32, buffer: u32) {
    unsafe {
        let td = (dma_virt + off as u64) as *mut Td;
        (*td).link = link;
        (*td).status = status;
        (*td).token = token;
        (*td).buffer = buffer;
    }
}

fn find_uhci_by_class() -> Option<Device> {
    for d in pci::scan() {
        if d.class != 0x0C || d.subclass != 0x03 {
            continue;
        }
        let reg = pci::read32(d.bus, d.slot, d.func, 0x08);
        let prog_if = ((reg >> 8) & 0xFF) as u8;
        if prog_if == 0x00 {
            return Some(d);
        }
    }
    None
}

/// First interface class in a configuration descriptor, if any.
fn iface_class(cfg: &[u8]) -> Option<u8> {
    let mut i = 0usize;
    while i + 1 < cfg.len() {
        let len = cfg[i] as usize;
        let dtype = cfg[i + 1];
        if len < 2 || i + len > cfg.len() {
            break;
        }
        if dtype == 0x04 && len >= 9 {
            return Some(cfg[i + 5]);
        }
        i += len;
    }
    None
}

/// Walk a config descriptor blob for a HID boot keyboard or mouse.
fn parse_hid_boot(cfg: &[u8]) -> Option<(u8, u8, u16, HidKind)> {
    let mut i = 0usize;
    let mut iface = 0u8;
    let mut class = 0u8;
    let mut sub = 0u8;
    let mut proto = 0u8;
    let mut kind: Option<HidKind> = None;
    while i + 1 < cfg.len() {
        let len = cfg[i] as usize;
        let dtype = cfg[i + 1];
        if len < 2 || i + len > cfg.len() {
            break;
        }
        match dtype {
            0x04 if len >= 9 => {
                // Interface
                iface = cfg[i + 2];
                class = cfg[i + 5];
                sub = cfg[i + 6];
                proto = cfg[i + 7];
                // Boot keyboard=1, boot mouse=2. Some emulated mice report
                // protocol 0 until SET_PROTOCOL; still treat HID/Boot subclass
                // as a candidate and confirm via the interrupt endpoint.
                kind = if class == 3 && sub == 1 && proto == 1 {
                    Some(HidKind::Keyboard)
                } else if class == 3 && sub == 1 && (proto == 2 || proto == 0) {
                    Some(HidKind::Mouse)
                } else if class == 3 && sub == 0 {
                    // Generic HID — guess mouse if we find a small interrupt IN.
                    Some(HidKind::Mouse)
                } else {
                    None
                };
            }
            0x05 if len >= 7 && kind.is_some() => {
                let addr = cfg[i + 2];
                let attr = cfg[i + 3];
                let maxp = u16::from_le_bytes([cfg[i + 4], cfg[i + 5]]);
                if addr & 0x80 != 0 && attr & 0x03 == 0x03 {
                    let mut k = kind.unwrap();
                    // Generic HID with a tiny interrupt endpoint is almost
                    // always a mouse in our QEMU setups.
                    if class == 3 && sub == 0 && maxp <= 8 {
                        k = HidKind::Mouse;
                    }
                    if class == 3 && sub == 1 && proto == 1 {
                        k = HidKind::Keyboard;
                    }
                    return Some((iface, addr & 0x0F, maxp.max(1), k));
                }
            }
            _ => {}
        }
        i += len;
    }
    crate::serial_println!(
        "USB: no HID boot iface in config (last class={}/{}/{})",
        class,
        sub,
        proto
    );
    None
}

/// Walk a config descriptor blob for a mass-storage (class 8), bulk-only
/// (protocol 0x50) interface, returning its interface number and bulk
/// IN/OUT endpoint addresses (masked to 0..=15, direction already resolved).
///
/// Accepts the common transparent-SCSI and RBC/UFI subclasses — real flash
/// drives and QEMU's `usb-storage` all report subclass 6 (SCSI), but a few
/// odds and ends use 2/4/5, and none of them change how BOT itself works.
fn parse_msc(cfg: &[u8]) -> Option<(u8, u8, u8, u16, u16)> {
    let mut i = 0usize;
    let mut iface = 0u8;
    let mut is_msc = false;
    let mut ep_in: Option<(u8, u16)> = None;
    let mut ep_out: Option<(u8, u16)> = None;
    while i + 1 < cfg.len() {
        let len = cfg[i] as usize;
        let dtype = cfg[i + 1];
        if len < 2 || i + len > cfg.len() {
            break;
        }
        match dtype {
            0x04 if len >= 9 => {
                if is_msc && ep_in.is_some() && ep_out.is_some() {
                    break;
                }
                iface = cfg[i + 2];
                let class = cfg[i + 5];
                let sub = cfg[i + 6];
                let proto = cfg[i + 7];
                is_msc = class == 0x08 && matches!(sub, 2 | 4 | 5 | 6) && proto == 0x50;
                ep_in = None;
                ep_out = None;
            }
            0x05 if len >= 7 && is_msc => {
                let addr = cfg[i + 2];
                let attr = cfg[i + 3];
                let maxp = u16::from_le_bytes([cfg[i + 4], cfg[i + 5]]);
                if attr & 0x03 == 0x02 {
                    if addr & 0x80 != 0 {
                        ep_in = Some((addr & 0x0F, maxp.max(1)));
                    } else {
                        ep_out = Some((addr & 0x0F, maxp.max(1)));
                    }
                }
            }
            _ => {}
        }
        i += len;
    }
    let (ein, mpin) = ep_in?;
    let (eout, mpout) = ep_out?;
    Some((iface, ein, eout, mpin, mpout))
}

fn busy_wait(ms: u32) {
    if ms <= 1 {
        for _ in 0..80_000 {
            core::hint::spin_loop();
        }
        return;
    }
    let start = crate::clock::ticks();
    // PIT ticks ~18.2 Hz ≈ 55 ms each; round up.
    let need = ((ms as u64) + 54) / 55;
    while crate::clock::ticks().wrapping_sub(start) < need {
        core::hint::spin_loop();
    }
}
