//! ATA (IDE) driver for the primary bus, polled programmed I/O.
//!
//! This is the least clever disk driver that can still be trusted: no DMA, no
//! interrupts, 28-bit LBA only. The reason is the kernel's IRQ path — handlers
//! run with the heap lock potentially held by the main loop, so an IRQ14 that
//! wanted to allocate would deadlock us. Polling keeps all the disk work on
//! the caller's stack, where it is allowed to allocate and to fail. The price
//! is throughput: every 512-byte sector crosses the bus as 256 `in ax, dx`
//! instructions, so callers should batch (see [`MAX_SECTORS_PER_TRANSFER`])
//! and should not put disk writes on a redraw path.
//!
//! Nothing here ever blocks indefinitely. Every wait is a bounded poll that
//! reports a timeout instead, because the common failure mode for a hobby OS
//! is not a corrupt read but a boot that hangs forever on a bus with no drive
//! attached to it.
//!
//! Nothing in the kernel drives this module until the caller mounts a disk, so
//! the whole module opts out of the dead-code lint rather than sprinkling
//! attributes over each entry point.
#![allow(dead_code)]

use alloc::string::String;
use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

const SECTOR_SIZE: usize = 512;

/// The 28-bit LBA ceiling. Beyond this a request needs the 48-bit command set,
/// which we deliberately do not implement: 128 GiB is more disk than this OS
/// has any use for.
const LBA28_LIMIT: u32 = 1 << 28;

/// The sector count register is a single byte where 0 means 256. We never send
/// 0, so we cap a transfer at 128 sectors (64 KiB) and let the count register
/// speak for itself.
pub const MAX_SECTORS_PER_TRANSFER: usize = 128;

/// How many status reads a wait is allowed before it gives up.
///
/// QEMU answers within a handful of reads and real hardware within a few
/// thousand; the number is this large only so that a drive genuinely spinning
/// up is not mistaken for a dead one. The trade-off is the cost of the
/// pathological case: a wedged controller stalls the caller for a couple of
/// seconds per request rather than forever, which is the behaviour we want at
/// boot.
const POLL_LIMIT: u32 = 2_000_000;

const PORT_DATA: u16 = 0x1F0;
const PORT_ERROR: u16 = 0x1F1;
const PORT_FEATURES: u16 = 0x1F1;
const PORT_SECTOR_COUNT: u16 = 0x1F2;
const PORT_LBA_LOW: u16 = 0x1F3;
const PORT_LBA_MID: u16 = 0x1F4;
const PORT_LBA_HIGH: u16 = 0x1F5;
const PORT_DRIVE: u16 = 0x1F6;
const PORT_STATUS: u16 = 0x1F7;
const PORT_COMMAND: u16 = 0x1F7;
const PORT_ALT_STATUS: u16 = 0x3F6;
const PORT_DEV_CONTROL: u16 = 0x3F6;

const STATUS_ERR: u8 = 0x01;
const STATUS_DRQ: u8 = 0x08;
const STATUS_DF: u8 = 0x20;
const STATUS_BSY: u8 = 0x80;

const CMD_READ_SECTORS: u8 = 0x20;
const CMD_WRITE_SECTORS: u8 = 0x30;
const CMD_CACHE_FLUSH: u8 = 0xE7;
const CMD_IDENTIFY: u8 = 0xEC;

/// Device control bit 1: stop the drive asserting IRQ14.
const DEV_CTRL_NIEN: u8 = 0x02;

/// Drive select base, LBA addressing enabled.
const SELECT_LBA: u8 = 0xE0;
/// Drive select base for IDENTIFY, which predates LBA and wants CHS bits.
const SELECT_CHS: u8 = 0xA0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Drive {
    Master,
    Slave,
}

impl Drive {
    /// Bit 4 of the drive select register, which is the whole difference
    /// between the two devices on a channel.
    fn slave_bit(self) -> u8 {
        match self {
            Drive::Master => 0x00,
            Drive::Slave => 0x10,
        }
    }
}

/// What IDENTIFY reported, if a drive answered at all.
pub struct DriveInfo {
    pub sectors: u32,
    pub model: String,
}

// ── Port helpers ────────────────────────────────────────────────────────
//
// Same shape as the PS/2 helpers in `mouse.rs`, plus the word-wide pair the
// data register needs.

unsafe fn outb(port: u16, val: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") val,
                     options(nomem, nostack, preserves_flags));
}

unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    core::arch::asm!("in al, dx", in("dx") port, out("al") val,
                     options(nomem, nostack, preserves_flags));
    val
}

unsafe fn outw(port: u16, val: u16) {
    core::arch::asm!("out dx, ax", in("dx") port, in("ax") val,
                     options(nomem, nostack, preserves_flags));
}

unsafe fn inw(port: u16) -> u16 {
    let val: u16;
    core::arch::asm!("in ax, dx", in("dx") port, out("ax") val,
                     options(nomem, nostack, preserves_flags));
    val
}

// ── Waiting ─────────────────────────────────────────────────────────────

/// The mandatory settle after touching the drive select register.
///
/// Four reads of the *alternate* status port is the canonical way to spend
/// 400ns without a timer: it is the one register whose read has no side
/// effects, so it cannot acknowledge an interrupt or disturb a command in
/// flight.
fn settle() {
    for _ in 0..4 {
        let _ = unsafe { inb(PORT_ALT_STATUS) };
    }
}

fn status() -> u8 {
    unsafe { inb(PORT_STATUS) }
}

/// Turn the error register into something a caller can print. The bits are far
/// more specific than "I/O error" and the distinction matters when debugging
/// whether the filesystem or the geometry is at fault.
fn error_reason() -> &'static str {
    let e = unsafe { inb(PORT_ERROR) };
    if e & 0x40 != 0 {
        "ATA uncorrectable data error"
    } else if e & 0x10 != 0 {
        "ATA sector not found"
    } else if e & 0x04 != 0 {
        "ATA command aborted"
    } else if e & 0x02 != 0 {
        "ATA track 0 not found"
    } else {
        "ATA drive reported an error"
    }
}

fn wait_not_busy() -> Result<(), &'static str> {
    for _ in 0..POLL_LIMIT {
        if status() & STATUS_BSY == 0 {
            return Ok(());
        }
    }
    Err("ATA timed out waiting for the drive to go idle")
}

/// Wait until the drive has a sector to hand over (or is ready to take one).
///
/// ERR and DF are checked before DRQ deliberately: a failed command can raise
/// both, and reading the data register in that state returns rubbish that
/// looks like a successful short read.
fn wait_data_ready() -> Result<(), &'static str> {
    for _ in 0..POLL_LIMIT {
        let s = status();
        if s & STATUS_BSY != 0 {
            continue;
        }
        if s & (STATUS_ERR | STATUS_DF) != 0 {
            return Err(error_reason());
        }
        if s & STATUS_DRQ != 0 {
            return Ok(());
        }
    }
    Err("ATA timed out waiting for a data transfer")
}

// ── Drive selection ─────────────────────────────────────────────────────

const NOTHING_SELECTED: u8 = 0xFF;

/// Which drive the controller is currently pointed at.
///
/// The status register belongs to whichever drive is selected, so a caller
/// alternating between master and slave can otherwise read the *other* drive's
/// idle status and conclude a busy drive is ready. Tracking the selection lets
/// us tell a genuine switch from a repeat access and wait properly for the one
/// that just came online.
static SELECTED: AtomicU8 = AtomicU8::new(NOTHING_SELECTED);

/// The drive select register's contents. Split out from [`select`] because the
/// masking is the one piece of this that can be wrong without any symptom
/// except a transfer landing 16 MiB away from where it was asked to.
fn select_byte(base: u8, drive: Drive, lba_top_nibble: u8) -> u8 {
    base | drive.slave_bit() | (lba_top_nibble & 0x0F)
}

fn select(drive: Drive, base: u8, lba_top_nibble: u8) -> bool {
    let changed = SELECTED.swap(drive.slave_bit(), Ordering::Relaxed) != drive.slave_bit();
    unsafe {
        // We poll, so keep IRQ14 permanently quiet: the kernel's interrupt
        // path must not be dragged into disk work.
        outb(PORT_DEV_CONTROL, DEV_CTRL_NIEN);
        outb(PORT_DRIVE, select_byte(base, drive, lba_top_nibble));
    }
    settle();
    changed
}

/// Point the controller at `drive` for an LBA command.
fn select_for_lba(drive: Drive, lba: u32) -> Result<(), &'static str> {
    if select(drive, SELECT_LBA, lba_registers(lba).3) {
        // The freshly selected drive may still be finishing something of its
        // own, and until it is idle its status tells us nothing useful.
        wait_not_busy()?;
    }
    Ok(())
}

// ── Capacity cache ──────────────────────────────────────────────────────

static MASTER_SECTORS: AtomicU32 = AtomicU32::new(0);
static SLAVE_SECTORS: AtomicU32 = AtomicU32::new(0);

fn capacity_cache(drive: Drive) -> &'static AtomicU32 {
    match drive {
        Drive::Master => &MASTER_SECTORS,
        Drive::Slave => &SLAVE_SECTORS,
    }
}

/// The drive's reported size, probing it if the caller never did.
///
/// Probing here rather than demanding the caller call [`identify`] first means
/// a read aimed at an empty bus fails immediately with "no drive" instead of
/// burning a timeout per request.
fn capacity(drive: Drive) -> Option<u32> {
    let cached = capacity_cache(drive).load(Ordering::Relaxed);
    if cached != 0 {
        return Some(cached);
    }
    identify(drive).map(|info| info.sectors)
}

// ── IDENTIFY ────────────────────────────────────────────────────────────

/// Probe a drive. `None` if nothing is attached.
pub fn identify(drive: Drive) -> Option<DriveInfo> {
    select(drive, SELECT_CHS, 0);

    // A channel with no drive on it floats high, so 0xFF means "nobody home".
    // Checking before the command is what stops an empty bus from costing a
    // full timeout.
    if status() == 0xFF {
        return None;
    }

    unsafe {
        outb(PORT_SECTOR_COUNT, 0);
        outb(PORT_LBA_LOW, 0);
        outb(PORT_LBA_MID, 0);
        outb(PORT_LBA_HIGH, 0);
        outb(PORT_COMMAND, CMD_IDENTIFY);
    }
    settle();

    // An absent drive leaves the status register at zero.
    if status() == 0 {
        return None;
    }
    wait_not_busy().ok()?;

    // A packet device or a SATA bridge refuses IDENTIFY by writing a signature
    // into the LBA registers. We have no use for either, and treating them as
    // an ordinary disk would hand back nonsense geometry.
    let signature_mid = unsafe { inb(PORT_LBA_MID) };
    let signature_high = unsafe { inb(PORT_LBA_HIGH) };
    if signature_mid != 0 || signature_high != 0 {
        return None;
    }

    let mut ready = false;
    for _ in 0..POLL_LIMIT {
        let s = status();
        if s & (STATUS_ERR | STATUS_DF) != 0 {
            return None;
        }
        if s & STATUS_DRQ != 0 {
            ready = true;
            break;
        }
    }
    if !ready {
        return None;
    }

    let mut words = [0u16; 256];
    for w in words.iter_mut() {
        *w = unsafe { inw(PORT_DATA) };
    }

    // Words 60/61: total addressable sectors in 28-bit mode.
    let low = words.get(60).copied().unwrap_or(0) as u32;
    let high = words.get(61).copied().unwrap_or(0) as u32;
    let sectors = (low | (high << 16)).min(LBA28_LIMIT);

    // Words 27..46 hold the model as big-endian character pairs.
    let mut model = String::new();
    if let Some(name) = words.get(27..47) {
        for &w in name {
            for c in [(w >> 8) as u8, w as u8] {
                if c == b' ' || c.is_ascii_graphic() {
                    model.push(c as char);
                }
            }
        }
    }
    while model.ends_with(' ') {
        model.pop();
    }

    capacity_cache(drive).store(sectors, Ordering::Relaxed);
    Some(DriveInfo { sectors, model })
}

// ── Transfers ───────────────────────────────────────────────────────────

/// Whole sectors in `len`, rejecting anything the command registers cannot
/// express.
fn transfer_sectors(len: usize) -> Result<u8, &'static str> {
    if len == 0 || len % SECTOR_SIZE != 0 {
        return Err("ATA transfer is not a whole number of sectors");
    }
    let sectors = len / SECTOR_SIZE;
    if sectors > MAX_SECTORS_PER_TRANSFER {
        return Err("ATA transfer is longer than 128 sectors");
    }
    Ok(sectors as u8)
}

/// A 28-bit LBA split the way the registers want it: low, mid, high, and the
/// top nibble that rides along in the drive select byte.
fn lba_registers(lba: u32) -> (u8, u8, u8, u8) {
    (
        lba as u8,
        (lba >> 8) as u8,
        (lba >> 16) as u8,
        (lba >> 24) as u8 & 0x0F,
    )
}

/// Whether a transfer fits inside a drive of `total` sectors.
fn range_ok(lba: u32, sectors: u8, total: u32) -> Result<(), &'static str> {
    let end = lba
        .checked_add(sectors as u32)
        .ok_or("ATA request overflows the LBA space")?;
    if end > total || end > LBA28_LIMIT {
        return Err("ATA request runs past the end of the drive");
    }
    Ok(())
}

fn check_range(drive: Drive, lba: u32, sectors: u8) -> Result<(), &'static str> {
    let total = capacity(drive).ok_or("no ATA drive answered")?;
    range_ok(lba, sectors, total)
}

fn issue(command: u8, lba: u32, sectors: u8) {
    let (low, mid, high, _) = lba_registers(lba);
    unsafe {
        outb(PORT_FEATURES, 0);
        outb(PORT_SECTOR_COUNT, sectors);
        outb(PORT_LBA_LOW, low);
        outb(PORT_LBA_MID, mid);
        outb(PORT_LBA_HIGH, high);
        outb(PORT_COMMAND, command);
    }
}

/// Little-endian word from a two-byte slice.
///
/// The callers feed it `chunks_exact(2)`, so the fallback is unreachable; it
/// exists so the transfer loops contain no indexing that could panic.
fn le_word(pair: &[u8]) -> u16 {
    match pair {
        [lo, hi] => u16::from_le_bytes([*lo, *hi]),
        _ => 0,
    }
}

/// Read whole sectors. `buf.len()` must be a multiple of 512 and at most
/// 128 sectors' worth.
pub fn read(drive: Drive, lba: u32, buf: &mut [u8]) -> Result<(), &'static str> {
    let sectors = transfer_sectors(buf.len())?;
    check_range(drive, lba, sectors)?;
    select_for_lba(drive, lba)?;
    wait_not_busy()?;
    issue(CMD_READ_SECTORS, lba, sectors);

    for sector in buf.chunks_exact_mut(SECTOR_SIZE) {
        wait_data_ready()?;
        for pair in sector.chunks_exact_mut(2) {
            let bytes = unsafe { inw(PORT_DATA) }.to_le_bytes();
            if let [lo, hi] = pair {
                *lo = bytes[0];
                *hi = bytes[1];
            }
        }
    }
    Ok(())
}

/// Write whole sectors, then flush the drive's write cache so the data is on
/// the platter (or at least out of QEMU's hands) before we return.
pub fn write(drive: Drive, lba: u32, buf: &[u8]) -> Result<(), &'static str> {
    let sectors = transfer_sectors(buf.len())?;
    check_range(drive, lba, sectors)?;
    select_for_lba(drive, lba)?;
    wait_not_busy()?;
    issue(CMD_WRITE_SECTORS, lba, sectors);

    for sector in buf.chunks_exact(SECTOR_SIZE) {
        wait_data_ready()?;
        for pair in sector.chunks_exact(2) {
            unsafe { outw(PORT_DATA, le_word(pair)) };
        }
        // The drive raises BSY as it latches the sector. Reading status too
        // soon sees the previous sector's DRQ still set and we would push the
        // next sector into a drive that is not listening.
        settle();
    }

    flush_cache()
}

/// CACHE FLUSH (0xE7). Without it a write can sit in the drive's buffer, which
/// on a hobby OS with no clean shutdown path means "survives a reboot" is a
/// coin toss.
fn flush_cache() -> Result<(), &'static str> {
    unsafe { outb(PORT_COMMAND, CMD_CACHE_FLUSH) };
    settle();
    wait_not_busy()?;
    if status() & (STATUS_ERR | STATUS_DF) != 0 {
        return Err(error_reason());
    }
    Ok(())
}

/// Address arithmetic and request validation, checked at boot.
///
/// Everything else in this module is port I/O that only a real controller can
/// answer, but these four functions decide *where* a transfer lands and whether
/// it is allowed at all. A mistake in them does not fail loudly — it reads or
/// writes the wrong sector, which on a disk holding the user's files is the
/// worst kind of bug to find by hand.
pub fn selftest() -> crate::selftest::Report {
    let mut r = crate::selftest::Report::new();

    r.check("a zero-length transfer is rejected", transfer_sectors(0).is_err());
    r.check("a partial sector is rejected", transfer_sectors(511).is_err());
    r.check("one sector is one sector", transfer_sectors(512) == Ok(1));
    r.check("a sector and a bit is rejected", transfer_sectors(513).is_err());
    r.check("two sectors is two sectors", transfer_sectors(1024) == Ok(2));
    r.check(
        "the maximum transfer is allowed",
        transfer_sectors(MAX_SECTORS_PER_TRANSFER * 512) == Ok(128),
    );
    r.check(
        "one sector past the maximum is rejected",
        transfer_sectors((MAX_SECTORS_PER_TRANSFER + 1) * 512).is_err(),
    );

    r.check("master selects with the LBA bit", select_byte(SELECT_LBA, Drive::Master, 0) == 0xE0);
    r.check("slave sets bit 4", select_byte(SELECT_LBA, Drive::Slave, 0) == 0xF0);
    r.check("the LBA top nibble rides along", select_byte(SELECT_LBA, Drive::Master, 0x0F) == 0xEF);
    r.check(
        "anything above the nibble is masked off",
        select_byte(SELECT_LBA, Drive::Master, 0xFF) == 0xEF,
    );
    r.check("IDENTIFY selects without the LBA bit", select_byte(SELECT_CHS, Drive::Master, 0) == 0xA0);
    r.check("and the slave likewise", select_byte(SELECT_CHS, Drive::Slave, 0) == 0xB0);

    r.check("LBA zero is all zeroes", lba_registers(0) == (0, 0, 0, 0));
    r.check(
        "each byte goes to its own register",
        lba_registers(0x0FED_CBA9) == (0xA9, 0xCB, 0xED, 0x0F),
    );
    r.check("a 24-bit LBA has an empty nibble", lba_registers(0x00FF_FFFF) == (0xFF, 0xFF, 0xFF, 0x00));
    r.check(
        "bits above the 28th cannot leak into the select byte",
        lba_registers(0xFFFF_FFFF) == (0xFF, 0xFF, 0xFF, 0x0F),
    );

    r.check("the first sector is in range", range_ok(0, 1, 10).is_ok());
    r.check("the last sector is in range", range_ok(9, 1, 10).is_ok());
    r.check("a whole drive is in range", range_ok(0, 10, 10).is_ok());
    r.check("one sector past the end is not", range_ok(10, 1, 10).is_err());
    r.check("a transfer straddling the end is not", range_ok(9, 2, 10).is_err());
    r.check("an empty drive accepts nothing", range_ok(0, 1, 0).is_err());
    r.check(
        "an LBA that would wrap is rejected",
        range_ok(u32::MAX, 1, u32::MAX).is_err(),
    );
    r.check(
        "an LBA past the 28-bit limit is rejected",
        range_ok(LBA28_LIMIT, 1, u32::MAX).is_err(),
    );

    r.check("a word is assembled little-endian", le_word(&[0x34, 0x12]) == 0x1234);
    r.check("a short slice yields zero rather than panicking", le_word(&[0x01]) == 0);
    r.check("so does an empty one", le_word(&[]) == 0);

    r
}
