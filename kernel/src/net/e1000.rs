//! Intel 82540EM (e1000) gigabit Ethernet driver.
//!
//! This is the card QEMU emulates by default on the `pc` machine, so it is
//! the shortest path to a working link. The driver is deliberately minimal:
//! descriptor rings, MAC address read-out, transmit, and receive.
//!
//! Receive is **polled** from the main loop rather than driven by an
//! interrupt. The kernel's IRQ handlers run with the heap lock potentially
//! held by the interrupted code, so anything that allocates — which parsing
//! a packet does — cannot safely run in interrupt context. Polling costs a
//! few register reads per frame and avoids that whole class of deadlock.

use alloc::vec::Vec;

use crate::memory::{self, DmaRegion};
use crate::pci;

pub const VENDOR_INTEL: u16 = 0x8086;
/// Device IDs this driver understands. 0x100E is the 82540EM QEMU emulates;
/// the others are close relatives with the same register layout.
pub const DEVICE_IDS: [u16; 3] = [0x100E, 0x153A, 0x10D3];

// Register offsets, in bytes from the start of BAR0.
mod reg {
    pub const CTRL: usize = 0x0000;
    pub const STATUS: usize = 0x0008;
    pub const EERD: usize = 0x0014;
    pub const ICR: usize = 0x00C0;
    pub const IMC: usize = 0x00D8;
    pub const RCTL: usize = 0x0100;
    pub const TCTL: usize = 0x0400;
    pub const RDBAL: usize = 0x2800;
    pub const RDBAH: usize = 0x2804;
    pub const RDLEN: usize = 0x2808;
    pub const RDH: usize = 0x2810;
    pub const RDT: usize = 0x2818;
    pub const TDBAL: usize = 0x3800;
    pub const TDBAH: usize = 0x3804;
    pub const TDLEN: usize = 0x3808;
    pub const TDH: usize = 0x3810;
    pub const TDT: usize = 0x3818;
    pub const RAL0: usize = 0x5400;
    pub const RAH0: usize = 0x5404;
}

// CTRL bits.
const CTRL_RST: u32 = 1 << 26;
const CTRL_ASDE: u32 = 1 << 5;
const CTRL_SLU: u32 = 1 << 6;

// RCTL bits.
const RCTL_EN: u32 = 1 << 1;
const RCTL_SBP: u32 = 1 << 2;
const RCTL_UPE: u32 = 1 << 3;
const RCTL_MPE: u32 = 1 << 4;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_SECRC: u32 = 1 << 26;
/// Receive buffer size 2048 bytes (BSIZE=00 with BSEX=0).
const RCTL_BSIZE_2048: u32 = 0;

// TCTL bits.
const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;

// Transmit descriptor command bits.
const TXD_CMD_EOP: u8 = 1 << 0;
const TXD_CMD_IFCS: u8 = 1 << 1;
const TXD_CMD_RS: u8 = 1 << 3;
const TXD_STA_DD: u8 = 1 << 0;

// Receive descriptor status bits.
const RXD_STAT_DD: u8 = 1 << 0;
const RXD_STAT_EOP: u8 = 1 << 1;

const NUM_RX_DESC: usize = 32;
const NUM_TX_DESC: usize = 16;
const RX_BUFFER_SIZE: usize = 2048;
const TX_BUFFER_SIZE: usize = 2048;

/// Hardware receive descriptor. The layout is fixed by the card.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RxDesc {
    addr: u64,
    length: u16,
    checksum: u16,
    status: u8,
    errors: u8,
    special: u16,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct TxDesc {
    addr: u64,
    length: u16,
    cso: u8,
    cmd: u8,
    status: u8,
    css: u8,
    special: u16,
}

pub struct E1000 {
    mmio: u64,
    mac: [u8; 6],

    // Kept alive for the lifetime of the driver; the card DMAs into them.
    _rx_ring: DmaRegion,
    _tx_ring: DmaRegion,
    _rx_buffers: DmaRegion,
    _tx_buffers: DmaRegion,

    rx_desc: *mut RxDesc,
    tx_desc: *mut TxDesc,
    rx_buf_virt: u64,
    tx_buf_virt: u64,

    rx_cur: usize,
    tx_cur: usize,

    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_dropped: u64,
}

// The card is owned by a single global behind a Mutex; the raw pointers are
// into DMA memory that lives forever.
unsafe impl Send for E1000 {}

impl E1000 {
    fn write_reg(&self, offset: usize, value: u32) {
        unsafe {
            core::ptr::write_volatile((self.mmio + offset as u64) as *mut u32, value);
        }
    }

    fn read_reg(&self, offset: usize) -> u32 {
        unsafe { core::ptr::read_volatile((self.mmio + offset as u64) as *const u32) }
    }

    /// Probe the PCI bus for a supported card and bring it up.
    pub fn probe() -> Result<Self, &'static str> {
        let dev = pci::find(VENDOR_INTEL, &DEVICE_IDS).ok_or("no e1000 NIC found on the PCI bus")?;

        let mmio = match dev.bar(0) {
            Some(pci::Bar::Memory(base)) => base,
            _ => return Err("e1000 BAR0 is not memory-mapped"),
        };

        // The card cannot touch memory until it is allowed to be a bus
        // master, and its registers are unreachable without memory space.
        dev.enable(pci::command::MEMORY_SPACE | pci::command::BUS_MASTER);

        // All physical memory is already mapped at a fixed offset, so the
        // BAR is reachable without setting up a new mapping.
        let mmio_virt = memory::physical_memory_offset() + mmio;

        let rx_ring = memory::alloc_dma(NUM_RX_DESC * core::mem::size_of::<RxDesc>())
            .ok_or("out of DMA memory for the RX ring")?;
        let tx_ring = memory::alloc_dma(NUM_TX_DESC * core::mem::size_of::<TxDesc>())
            .ok_or("out of DMA memory for the TX ring")?;
        let rx_buffers = memory::alloc_dma(NUM_RX_DESC * RX_BUFFER_SIZE)
            .ok_or("out of DMA memory for RX buffers")?;
        let tx_buffers = memory::alloc_dma(NUM_TX_DESC * TX_BUFFER_SIZE)
            .ok_or("out of DMA memory for TX buffers")?;

        let mut nic = Self {
            mmio: mmio_virt,
            mac: [0; 6],
            rx_desc: rx_ring.as_mut_ptr::<RxDesc>(),
            tx_desc: tx_ring.as_mut_ptr::<TxDesc>(),
            rx_buf_virt: rx_buffers.virt,
            tx_buf_virt: tx_buffers.virt,
            _rx_ring: rx_ring,
            _tx_ring: tx_ring,
            _rx_buffers: rx_buffers,
            _tx_buffers: tx_buffers,
            rx_cur: 0,
            tx_cur: 0,
            rx_packets: 0,
            tx_packets: 0,
            rx_dropped: 0,
        };

        nic.reset();
        nic.read_mac();
        nic.init_rx();
        nic.init_tx();

        // Link up, and let the card negotiate speed for itself.
        let ctrl = nic.read_reg(reg::CTRL);
        nic.write_reg(reg::CTRL, ctrl | CTRL_SLU | CTRL_ASDE);

        // Mask every interrupt source: this driver polls.
        nic.write_reg(reg::IMC, 0xFFFF_FFFF);
        let _ = nic.read_reg(reg::ICR);

        Ok(nic)
    }

    fn reset(&mut self) {
        self.write_reg(reg::IMC, 0xFFFF_FFFF);
        self.write_reg(reg::CTRL, self.read_reg(reg::CTRL) | CTRL_RST);
        // The datasheet asks for a short delay; a few register reads are
        // more than the required microseconds and need no timer.
        for _ in 0..1000 {
            let _ = self.read_reg(reg::STATUS);
        }
        self.write_reg(reg::IMC, 0xFFFF_FFFF);
    }

    /// Read the burned-in MAC, preferring the receive address registers and
    /// falling back to the EEPROM.
    fn read_mac(&mut self) {
        let low = self.read_reg(reg::RAL0);
        let high = self.read_reg(reg::RAH0);

        if low != 0 || (high & 0xFFFF) != 0 {
            self.mac = [
                low as u8,
                (low >> 8) as u8,
                (low >> 16) as u8,
                (low >> 24) as u8,
                high as u8,
                (high >> 8) as u8,
            ];
            return;
        }

        for word in 0..3u32 {
            if let Some(value) = self.eeprom_read(word) {
                self.mac[word as usize * 2] = value as u8;
                self.mac[word as usize * 2 + 1] = (value >> 8) as u8;
            }
        }
    }

    fn eeprom_read(&self, word: u32) -> Option<u16> {
        // Start a read, then spin for the done bit.
        self.write_reg(reg::EERD, (word << 8) | 1);
        for _ in 0..10_000 {
            let value = self.read_reg(reg::EERD);
            if value & (1 << 4) != 0 {
                return Some((value >> 16) as u16);
            }
        }
        None
    }

    fn init_rx(&mut self) {
        for i in 0..NUM_RX_DESC {
            let buf_phys = memory::dma_virt_to_phys(self.rx_buf_virt) + (i * RX_BUFFER_SIZE) as u64;
            unsafe {
                core::ptr::write_volatile(
                    self.rx_desc.add(i),
                    RxDesc { addr: buf_phys, length: 0, checksum: 0,
                             status: 0, errors: 0, special: 0 },
                );
            }
        }

        let ring_phys = memory::dma_virt_to_phys(self.rx_desc as u64);
        self.write_reg(reg::RDBAL, ring_phys as u32);
        self.write_reg(reg::RDBAH, (ring_phys >> 32) as u32);
        self.write_reg(reg::RDLEN, (NUM_RX_DESC * core::mem::size_of::<RxDesc>()) as u32);
        self.write_reg(reg::RDH, 0);
        // Tail points one past the last descriptor the card may fill.
        self.write_reg(reg::RDT, (NUM_RX_DESC - 1) as u32);
        self.rx_cur = 0;

        self.write_reg(
            reg::RCTL,
            RCTL_EN | RCTL_SBP | RCTL_UPE | RCTL_MPE | RCTL_BAM | RCTL_SECRC | RCTL_BSIZE_2048,
        );
    }

    fn init_tx(&mut self) {
        for i in 0..NUM_TX_DESC {
            unsafe {
                core::ptr::write_volatile(
                    self.tx_desc.add(i),
                    TxDesc { addr: 0, length: 0, cso: 0, cmd: 0,
                             status: TXD_STA_DD, css: 0, special: 0 },
                );
            }
        }

        let ring_phys = memory::dma_virt_to_phys(self.tx_desc as u64);
        self.write_reg(reg::TDBAL, ring_phys as u32);
        self.write_reg(reg::TDBAH, (ring_phys >> 32) as u32);
        self.write_reg(reg::TDLEN, (NUM_TX_DESC * core::mem::size_of::<TxDesc>()) as u32);
        self.write_reg(reg::TDH, 0);
        self.write_reg(reg::TDT, 0);
        self.tx_cur = 0;

        self.write_reg(reg::TCTL, TCTL_EN | TCTL_PSP);
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// True once the PHY reports the link is up.
    pub fn link_up(&self) -> bool {
        self.read_reg(reg::STATUS) & (1 << 1) != 0
    }

    /// Queue a frame for transmission and wait for the card to consume it.
    pub fn send(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        if frame.len() > TX_BUFFER_SIZE {
            return Err("frame too large");
        }

        let idx = self.tx_cur;
        let buf_virt = self.tx_buf_virt + (idx * TX_BUFFER_SIZE) as u64;
        unsafe {
            core::ptr::copy_nonoverlapping(frame.as_ptr(), buf_virt as *mut u8, frame.len());
            core::ptr::write_volatile(
                self.tx_desc.add(idx),
                TxDesc {
                    addr: memory::dma_virt_to_phys(buf_virt),
                    length: frame.len() as u16,
                    cso: 0,
                    // Report status so we can tell when the card is done, and
                    // let it append the Ethernet CRC for us.
                    cmd: TXD_CMD_EOP | TXD_CMD_IFCS | TXD_CMD_RS,
                    status: 0,
                    css: 0,
                    special: 0,
                },
            );
        }

        self.tx_cur = (idx + 1) % NUM_TX_DESC;
        self.write_reg(reg::TDT, self.tx_cur as u32);

        // Wait for descriptor-done. This bounds how long a wedged card can
        // stall the caller.
        for _ in 0..1_000_000 {
            let status = unsafe { core::ptr::read_volatile(&(*self.tx_desc.add(idx)).status) };
            if status & TXD_STA_DD != 0 {
                self.tx_packets += 1;
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err("transmit timed out")
    }

    /// Take every frame the card has delivered since the last call.
    pub fn receive(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();

        loop {
            let desc = unsafe { core::ptr::read_volatile(self.rx_desc.add(self.rx_cur)) };
            if desc.status & RXD_STAT_DD == 0 {
                break;
            }

            // Anything spanning multiple descriptors is beyond a 2 KiB
            // buffer, so it is not something this stack can use.
            if desc.status & RXD_STAT_EOP != 0 && desc.errors == 0 {
                let len = desc.length as usize;
                let src = self.rx_buf_virt + (self.rx_cur * RX_BUFFER_SIZE) as u64;
                let mut frame = alloc::vec![0u8; len];
                unsafe {
                    core::ptr::copy_nonoverlapping(src as *const u8, frame.as_mut_ptr(), len);
                }
                frames.push(frame);
                self.rx_packets += 1;
            } else {
                self.rx_dropped += 1;
            }

            // Hand the descriptor back to the card and advance the tail.
            unsafe {
                let slot = self.rx_desc.add(self.rx_cur);
                core::ptr::write_volatile(&mut (*slot).status, 0);
            }
            self.write_reg(reg::RDT, self.rx_cur as u32);
            self.rx_cur = (self.rx_cur + 1) % NUM_RX_DESC;
        }

        frames
    }
}
