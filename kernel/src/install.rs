//! In-OS permanent installer.
//!
//! Copies a bootable OS101 disk image onto a chosen target drive so the
//! machine can boot without the install USB. The live system itself is the
//! install medium: by default the source is the disk we booted from (ATA
//! master), and the target is any other ATA drive or a USB mass-storage stick
//! large enough to hold the image.
//!
//! Supported targets are whatever this kernel can already talk to — primary
//! ATA (IDE) master/slave and UHCI USB MSC. AHCI/NVMe are out of scope; the
//! wizard will simply not list them.
//!
//! Destructive by design: the target's existing contents are overwritten from
//! LBA 0. The UI requires typing `ERASE` before anything is written.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ata::{self, Drive};
use crate::diskfs::{RamDisk, Sectors};

/// How many sectors to move per iteration (64 KiB). Matches the ATA driver's
/// per-transfer ceiling so we never split a request the hardware cannot take.
const CHUNK_SECTORS: u32 = 128;
const SECTOR_SIZE: usize = 512;

/// Where an install image can be read from / written to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskId {
    AtaMaster,
    AtaSlave,
    Usb,
}

impl DiskId {
    pub fn label(self) -> &'static str {
        match self {
            DiskId::AtaMaster => "ATA Master (primary IDE)",
            DiskId::AtaSlave => "ATA Slave (secondary IDE)",
            DiskId::Usb => "USB flash drive",
        }
    }
}

/// One disk the wizard can offer as a source or target.
#[derive(Clone, Debug)]
pub struct DiskInfo {
    pub id: DiskId,
    pub model: String,
    pub sectors: u32,
}

impl DiskInfo {
    pub fn size_mib(&self) -> u32 {
        self.sectors / 2048
    }

    pub fn describe(&self) -> String {
        format!(
            "{} — {} ({} MiB)",
            self.id.label(),
            self.model,
            self.size_mib()
        )
    }
}

/// Every disk the kernel can see right now.
pub fn list_disks() -> Vec<DiskInfo> {
    let mut out = Vec::new();
    if let Some(info) = ata::identify(Drive::Master) {
        out.push(DiskInfo {
            id: DiskId::AtaMaster,
            model: info.model,
            sectors: info.sectors,
        });
    }
    if let Some(info) = ata::identify(Drive::Slave) {
        out.push(DiskInfo {
            id: DiskId::AtaSlave,
            model: info.model,
            sectors: info.sectors,
        });
    }
    if let Some(usb) = crate::usb::UsbDisk::open() {
        out.push(DiskInfo {
            id: DiskId::Usb,
            model: String::from("USB Mass Storage"),
            sectors: usb.sector_count(),
        });
    }
    out
}

/// Default install source: the disk we almost always boot from in QEMU and
/// when the install USB is attached as the primary IDE device.
pub fn default_source(disks: &[DiskInfo]) -> Option<DiskId> {
    disks
        .iter()
        .find(|d| d.id == DiskId::AtaMaster)
        .map(|d| d.id)
        .or_else(|| disks.first().map(|d| d.id))
}

/// Targets that are safe to erase: everything except the chosen source.
pub fn install_targets(disks: &[DiskInfo], source: DiskId) -> Vec<DiskInfo> {
    disks
        .iter()
        .filter(|d| d.id != source)
        .cloned()
        .collect()
}

/// Progress callback: `(sectors_done, sectors_total)`.
pub type ProgressFn = fn(u32, u32);

/// Copy `source` onto `target` from LBA 0 through the end of the source image.
///
/// The target must have at least as many sectors as the source. USB targets
/// cause `/usb` to be unmounted first so the mass-storage device is not held
/// by the VFS during the wipe.
pub fn install(source: DiskId, target: DiskId, progress: Option<ProgressFn>) -> Result<(), &'static str> {
    if source == target {
        return Err("source and target must be different disks");
    }

    let disks = list_disks();
    let src_info = disks
        .iter()
        .find(|d| d.id == source)
        .ok_or("source disk is no longer attached")?;
    let dst_info = disks
        .iter()
        .find(|d| d.id == target)
        .ok_or("target disk is no longer attached")?;

    if dst_info.sectors < src_info.sectors {
        return Err("target disk is smaller than the install image");
    }

    // Drop filesystem mounts on disks we are about to overwrite.
    if source == DiskId::Usb || target == DiskId::Usb {
        crate::fs::unmount_usb();
    }
    if source == DiskId::AtaSlave || target == DiskId::AtaSlave {
        crate::fs::unmount_disk();
    }

    let total = src_info.sectors;
    copy_sectors(source, target, total, progress)?;

    // If the USB stick survived as a non-target (e.g. we installed ATA→ATA),
    // try to bring `/usb` back. Failure is fine — hotplug will retry.
    if source != DiskId::Usb && target != DiskId::Usb {
        crate::fs::mount_usb();
    }

    Ok(())
}

/// Raw sector clone used by the self-test (against RAM disks).
fn copy_sectors_between<S: Sectors, D: Sectors>(
    src: &S,
    dst: &mut D,
    total: u32,
    progress: Option<ProgressFn>,
) -> Result<(), &'static str> {
    if dst.sector_count() < total {
        return Err("target disk is smaller than the install image");
    }
    let mut buf = alloc::vec![0u8; CHUNK_SECTORS as usize * SECTOR_SIZE];
    let mut lba = 0u32;
    while lba < total {
        let n = (total - lba).min(CHUNK_SECTORS);
        let bytes = n as usize * SECTOR_SIZE;
        let chunk = &mut buf[..bytes];
        src.read(lba, chunk)?;
        dst.write(lba, chunk)?;
        lba += n;
        if let Some(cb) = progress {
            cb(lba, total);
        }
    }
    Ok(())
}

fn copy_sectors(
    source: DiskId,
    target: DiskId,
    total: u32,
    progress: Option<ProgressFn>,
) -> Result<(), &'static str> {
    let mut buf = alloc::vec![0u8; CHUNK_SECTORS as usize * SECTOR_SIZE];
    let mut lba = 0u32;
    while lba < total {
        let n = (total - lba).min(CHUNK_SECTORS);
        let bytes = n as usize * SECTOR_SIZE;
        let chunk = &mut buf[..bytes];
        read_sectors(source, lba, chunk)?;
        write_sectors(target, lba, chunk)?;
        lba += n;
        if let Some(cb) = progress {
            cb(lba, total);
        }
    }
    Ok(())
}

fn read_sectors(id: DiskId, lba: u32, buf: &mut [u8]) -> Result<(), &'static str> {
    match id {
        DiskId::AtaMaster => ata::read(Drive::Master, lba, buf),
        DiskId::AtaSlave => ata::read(Drive::Slave, lba, buf),
        DiskId::Usb => {
            let disk = crate::usb::UsbDisk::open().ok_or("USB drive disappeared")?;
            disk.read(lba, buf)
        }
    }
}

fn write_sectors(id: DiskId, lba: u32, buf: &[u8]) -> Result<(), &'static str> {
    match id {
        DiskId::AtaMaster => ata::write(Drive::Master, lba, buf),
        DiskId::AtaSlave => ata::write(Drive::Slave, lba, buf),
        DiskId::Usb => {
            let mut disk = crate::usb::UsbDisk::open().ok_or("USB drive disappeared")?;
            disk.write(lba, buf)
        }
    }
}

/// If `/usb/autoinst.txt` exists, run an unattended install to the target
/// named in that file (`master`, `slave`, or `usb`). Used for automated
/// testing and for a one-file "install without the wizard" flow.
pub fn try_autoinstall() {
    let Ok(bytes) = crate::fs::cmd_cat("/usb/autoinst.txt") else {
        return;
    };
    let text = core::str::from_utf8(&bytes).unwrap_or("").trim();
    // Ignore AppleDouble / empty leftovers.
    let target = match text.lines().next().unwrap_or("").trim() {
        "master" => DiskId::AtaMaster,
        "slave" => DiskId::AtaSlave,
        "usb" => DiskId::Usb,
        other if other.is_empty() => return,
        other => {
            crate::warn_line(&format!(
                "install: /usb/autoinst.txt has unknown target '{}'",
                other
            ));
            return;
        }
    };
    let disks = list_disks();
    let Some(source) = default_source(&disks) else {
        crate::warn_line("install: autoinst skipped — no source disk");
        return;
    };
    crate::ok_line(&format!(
        "install: autoinst {} -> {}",
        source.label(),
        target.label()
    ));
    match install(source, target, None) {
        Ok(()) => crate::ok_line("install: autoinst finished — reboot from the target disk"),
        Err(e) => crate::warn_line(&format!("install: autoinst failed: {}", e)),
    }
}

/// Boot-time sanity checks for the installer (no real disks required for the
/// copy logic — that is exercised on two RAM disks).
pub fn selftest() -> crate::selftest::Report {
    let mut r = crate::selftest::Report::new();
    r.check("DiskId labels are non-empty", !DiskId::AtaMaster.label().is_empty());
    r.check(
        "install refuses identical source and target",
        install(DiskId::AtaMaster, DiskId::AtaMaster, None).is_err(),
    );

    // A tiny "boot image" with a recognisable signature, cloned onto a larger
    // blank disk — the same path the wizard uses on real hardware.
    let mut src = RamDisk::new(16);
    let mut marker = [0u8; SECTOR_SIZE];
    marker[0..8].copy_from_slice(b"OS101IMG");
    marker[8..12].copy_from_slice(&0xA5A5_A5A5u32.to_le_bytes());
    r.check("self-test source sector writes", src.write(0, &marker).is_ok());
    let mut last = [0u8; SECTOR_SIZE];
    last[0..4].copy_from_slice(b"END!");
    r.check("self-test source tail writes", src.write(15, &last).is_ok());

    let mut dst = RamDisk::new(32);
    r.check(
        "RAM-disk clone succeeds",
        copy_sectors_between(&src, &mut dst, 16, None).is_ok(),
    );
    let mut got = [0u8; SECTOR_SIZE];
    r.check("cloned boot signature reads back", dst.read(0, &mut got).is_ok());
    r.check("cloned boot signature matches", got[0..8] == marker[0..8]);
    r.check(
        "cloned tail sector matches",
        dst.read(15, &mut got).is_ok() && got[0..4] == last[0..4],
    );
    r.check(
        "clone refuses an undersized target",
        copy_sectors_between(&src, &mut RamDisk::new(8), 16, None).is_err(),
    );

    let disks = list_disks();
    if let Some(src_id) = default_source(&disks) {
        let targets = install_targets(&disks, src_id);
        r.check(
            "default source is excluded from targets",
            targets.iter().all(|t| t.id != src_id),
        );
    } else {
        r.check("no disks present is a valid empty list", disks.is_empty());
    }
    r
}
