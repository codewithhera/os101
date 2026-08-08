//! PCI configuration space access and bus enumeration.
//!
//! Uses the legacy I/O port mechanism (`0xCF8`/`0xCFC`) rather than memory
//! mapped ECAM. It is limited to the first 256 bytes of each device's config
//! space, which is all a conventional PCI device needs, and it works without
//! having to parse ACPI tables first.
//!
//! Only what the network card needs is implemented: a brute-force scan of
//! every bus/slot/function, BAR decoding, and the handful of command-register
//! bits required to switch a device on.

use alloc::vec::Vec;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

/// Command register bits (offset 0x04).
pub mod command {
    pub const IO_SPACE: u16 = 1 << 0;
    pub const MEMORY_SPACE: u16 = 1 << 1;
    /// Required before the device may act as a DMA initiator.
    pub const BUS_MASTER: u16 = 1 << 2;
}

unsafe fn outl(port: u16, value: u32) {
    core::arch::asm!("out dx, eax", in("dx") port, in("eax") value,
                     options(nomem, nostack, preserves_flags));
}

unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    core::arch::asm!("in eax, dx", in("dx") port, out("eax") value,
                     options(nomem, nostack, preserves_flags));
    value
}

/// Build a config-space address. Bit 31 enables the access; the offset's low
/// two bits are always zero because reads are dword-wide.
fn address(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((slot as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC)
}

pub fn read32(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    unsafe {
        outl(CONFIG_ADDRESS, address(bus, slot, func, offset));
        inl(CONFIG_DATA)
    }
}

pub fn write32(bus: u8, slot: u8, func: u8, offset: u8, value: u32) {
    unsafe {
        outl(CONFIG_ADDRESS, address(bus, slot, func, offset));
        outl(CONFIG_DATA, value);
    }
}

pub fn read16(bus: u8, slot: u8, func: u8, offset: u8) -> u16 {
    let dword = read32(bus, slot, func, offset & 0xFC);
    ((dword >> ((offset as u32 & 2) * 8)) & 0xFFFF) as u16
}

pub fn write16(bus: u8, slot: u8, func: u8, offset: u8, value: u16) {
    let shift = (offset as u32 & 2) * 8;
    let mut dword = read32(bus, slot, func, offset & 0xFC);
    dword &= !(0xFFFF << shift);
    dword |= (value as u32) << shift;
    write32(bus, slot, func, offset & 0xFC, dword);
}

/// One PCI function found during enumeration.
#[derive(Debug, Clone, Copy)]
pub struct Device {
    pub bus: u8,
    pub slot: u8,
    pub func: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
}

impl Device {
    /// Read base address register `index` (0..=5).
    ///
    /// Returns the decoded base address and whether it is memory-mapped.
    /// 64-bit memory BARs consume two slots; the upper half is folded in
    /// here so callers see one address.
    pub fn bar(&self, index: u8) -> Option<Bar> {
        if index > 5 {
            return None;
        }
        let offset = 0x10 + index * 4;
        let raw = read32(self.bus, self.slot, self.func, offset);
        if raw == 0 {
            return None;
        }

        if raw & 1 == 1 {
            return Some(Bar::Io((raw & !0x3) as u64));
        }

        let is_64 = (raw >> 1) & 0x3 == 0x2;
        let mut base = (raw & !0xF) as u64;
        if is_64 {
            let high = read32(self.bus, self.slot, self.func, offset + 4);
            base |= (high as u64) << 32;
        }
        Some(Bar::Memory(base))
    }

    pub fn command(&self) -> u16 {
        read16(self.bus, self.slot, self.func, 0x04)
    }

    /// Set bits in the command register, leaving the rest untouched.
    pub fn enable(&self, bits: u16) {
        let current = self.command();
        write16(self.bus, self.slot, self.func, 0x04, current | bits);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bar {
    Memory(u64),
    Io(u64),
}

/// Walk every bus, slot and function, returning what is present.
///
/// A brute-force scan of all 256 buses is fine here: config reads are cheap
/// and this runs once at boot. It avoids having to follow PCI-to-PCI bridges
/// to discover which buses actually exist.
pub fn scan() -> Vec<Device> {
    let mut found = Vec::new();
    for bus in 0..=255u8 {
        for slot in 0..32u8 {
            // A missing function 0 means the whole slot is empty.
            if read16(bus, slot, 0, 0x00) == 0xFFFF {
                continue;
            }
            // Bit 7 of the header type says whether the device is multi-function.
            let header_type = (read32(bus, slot, 0, 0x0C) >> 16) as u8;
            let funcs = if header_type & 0x80 != 0 { 8 } else { 1 };

            for func in 0..funcs {
                let ids = read32(bus, slot, func, 0x00);
                let vendor_id = (ids & 0xFFFF) as u16;
                if vendor_id == 0xFFFF {
                    continue;
                }
                let classes = read32(bus, slot, func, 0x08);
                found.push(Device {
                    bus,
                    slot,
                    func,
                    vendor_id,
                    device_id: (ids >> 16) as u16,
                    class: (classes >> 24) as u8,
                    subclass: (classes >> 16) as u8,
                });
            }
        }
    }
    found
}

/// Find the first device matching a vendor/device ID pair.
pub fn find(vendor_id: u16, device_ids: &[u16]) -> Option<Device> {
    scan()
        .into_iter()
        .find(|d| d.vendor_id == vendor_id && device_ids.contains(&d.device_id))
}

/// Human-readable class name, for the `pci` shell command.
pub fn class_name(class: u8, subclass: u8) -> &'static str {
    match (class, subclass) {
        (0x01, 0x01) => "IDE controller",
        (0x01, 0x06) => "SATA controller",
        (0x01, _) => "Storage controller",
        (0x02, _) => "Network controller",
        (0x03, _) => "Display controller",
        (0x04, _) => "Multimedia controller",
        (0x06, 0x00) => "Host bridge",
        (0x06, 0x01) => "ISA bridge",
        (0x06, 0x04) => "PCI-to-PCI bridge",
        (0x06, _) => "Bridge",
        (0x0C, 0x03) => "USB controller",
        (0x0C, _) => "Serial bus controller",
        _ => "Unknown device",
    }
}
