//! USB Mass Storage Class — Bulk-Only Transport (BOT) plus the handful of
//! SCSI block commands a flash drive actually needs: INQUIRY, TEST UNIT
//! READY, REQUEST SENSE, READ CAPACITY(10), READ(10) and WRITE(10).
//!
//! Sits on top of [`super::uhci::Uhci::msc_bot_transfer`], which owns the raw
//! bulk IN/OUT pipes; this module only knows CBW/CSW framing and SCSI
//! command bytes. [`UsbDisk`] wraps it as a [`crate::diskfs::Sectors`] so the
//! rest of the kernel can treat a USB drive exactly like the ATA data disk.

use core::sync::atomic::{AtomicU32, Ordering};

const CBW_SIG: u32 = 0x4342_5355; // "USBC"
const CSW_SIG: u32 = 0x5342_5355; // "USBS"

static NEXT_TAG: AtomicU32 = AtomicU32::new(1);

fn next_tag() -> u32 {
    NEXT_TAG.fetch_add(1, Ordering::Relaxed)
}

fn build_cbw(tag: u32, transfer_len: u32, data_in: bool, cdb: &[u8]) -> [u8; 31] {
    let mut cbw = [0u8; 31];
    cbw[0..4].copy_from_slice(&CBW_SIG.to_le_bytes());
    cbw[4..8].copy_from_slice(&tag.to_le_bytes());
    cbw[8..12].copy_from_slice(&transfer_len.to_le_bytes());
    cbw[12] = if data_in { 0x80 } else { 0x00 };
    cbw[13] = 0; // LUN 0 — one drive is all this OS looks for.
    let n = cdb.len().min(16);
    cbw[14] = n as u8;
    cbw[15..15 + n].copy_from_slice(&cdb[..n]);
    cbw
}

/// Send one SCSI command block through Bulk-Only Transport and wait for its
/// status. `data` is the (optional) data-phase buffer; `data_in` says which
/// direction it moves.
fn command(cdb: &[u8], data: Option<&mut [u8]>, data_in: bool) -> Result<(), &'static str> {
    let tag = next_tag();
    let len = data.as_ref().map(|d| d.len()).unwrap_or(0) as u32;
    let cbw = build_cbw(tag, len, data_in, cdb);

    let mut ctrl = super::CONTROLLER.lock();
    let uhci = ctrl.as_mut().ok_or("no USB controller")?;
    let csw = uhci
        .msc_bot_transfer(&cbw, data, data_in)
        .map_err(|_| "USB mass-storage transfer failed")?;
    drop(ctrl);

    if u32::from_le_bytes([csw[0], csw[1], csw[2], csw[3]]) != CSW_SIG {
        return Err("mass storage: bad CSW signature");
    }
    if u32::from_le_bytes([csw[4], csw[5], csw[6], csw[7]]) != tag {
        return Err("mass storage: CSW tag mismatch");
    }
    match csw[12] {
        0 => Ok(()),
        _ => Err("mass storage: SCSI command failed"),
    }
}

pub fn test_unit_ready() -> Result<(), &'static str> {
    command(&[0x00, 0, 0, 0, 0, 0], None, false)
}

/// Clears whatever "unit attention" condition a drive raises right after it
/// is powered on — harmless to call and ignore the result of.
pub fn request_sense() -> Result<[u8; 18], &'static str> {
    let mut buf = [0u8; 18];
    command(&[0x03, 0, 0, 0, 18, 0], Some(&mut buf), true)?;
    Ok(buf)
}

pub fn inquiry() -> Result<[u8; 36], &'static str> {
    let mut buf = [0u8; 36];
    command(&[0x12, 0, 0, 0, 36, 0], Some(&mut buf), true)?;
    Ok(buf)
}

/// (sector count, bytes per sector).
pub fn read_capacity10() -> Result<(u32, u32), &'static str> {
    let mut buf = [0u8; 8];
    command(&[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], Some(&mut buf), true)?;
    let last_lba = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let block_size = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    Ok((last_lba.saturating_add(1), block_size))
}

pub fn read10(lba: u32, buf: &mut [u8]) -> Result<(), &'static str> {
    if buf.is_empty() || buf.len() % 512 != 0 {
        return Err("USB read is not a whole number of sectors");
    }
    let count = (buf.len() / 512) as u32;
    if count > 0xFFFF {
        return Err("USB read is longer than 65535 sectors");
    }
    let cdb = [
        0x28,
        0,
        (lba >> 24) as u8,
        (lba >> 16) as u8,
        (lba >> 8) as u8,
        lba as u8,
        0,
        (count >> 8) as u8,
        count as u8,
        0,
    ];
    command(&cdb, Some(buf), true)
}

pub fn write10(lba: u32, buf: &[u8]) -> Result<(), &'static str> {
    if buf.is_empty() || buf.len() % 512 != 0 {
        return Err("USB write is not a whole number of sectors");
    }
    let count = (buf.len() / 512) as u32;
    if count > 0xFFFF {
        return Err("USB write is longer than 65535 sectors");
    }
    let cdb = [
        0x2A,
        0,
        (lba >> 24) as u8,
        (lba >> 16) as u8,
        (lba >> 8) as u8,
        lba as u8,
        0,
        (count >> 8) as u8,
        count as u8,
        0,
    ];
    // The bulk layer's signature is direction-agnostic (`&mut [u8]` either
    // way); for an OUT transfer it only ever reads from this copy.
    let mut tmp = buf.to_vec();
    command(&cdb, Some(&mut tmp), false)
}

/// A USB flash drive's block storage, exposed the same way the ATA data disk
/// is: whole 512-byte sectors, addressed by LBA.
pub struct UsbDisk {
    sectors: u32,
}

impl UsbDisk {
    /// `None` if no mass-storage device is attached, or it never answers.
    /// Pokes TEST UNIT READY a few times first: real drives (and QEMU's
    /// `usb-storage`) commonly raise "unit attention" right after
    /// enumeration and need one REQUEST SENSE before they settle down.
    pub fn open() -> Option<Self> {
        if !super::has_msc() {
            return None;
        }
        for _ in 0..5 {
            if test_unit_ready().is_ok() {
                break;
            }
            let _ = request_sense();
            busy_wait_ticks(4);
        }
        let (sectors, block_size) = read_capacity10().ok()?;
        if sectors == 0 {
            crate::warn_line("USB: drive answered but reports zero sectors");
            return None;
        }
        if block_size != 512 {
            crate::warn_line(&alloc::format!(
                "USB: drive uses {}-byte sectors, only 512 is supported",
                block_size
            ));
            return None;
        }
        crate::serial_println!(
            "USB: mass storage ready — {} sectors ({} MiB)",
            sectors,
            (sectors as u64) / 2048
        );
        Some(Self { sectors })
    }
}

impl crate::diskfs::Sectors for UsbDisk {
    fn sector_count(&self) -> u32 {
        self.sectors
    }

    fn read(&self, lba: u32, buf: &mut [u8]) -> Result<(), &'static str> {
        read10(lba, buf)
    }

    fn write(&mut self, lba: u32, buf: &[u8]) -> Result<(), &'static str> {
        write10(lba, buf)
    }
}

fn busy_wait_ticks(ticks: u64) {
    let start = crate::clock::ticks();
    while crate::clock::ticks().wrapping_sub(start) < ticks {
        core::hint::spin_loop();
    }
}
