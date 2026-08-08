//! OS101FS — a deliberately small filesystem for the second, dedicated data
//! disk.
//!
//! The point of this layer is that a file the user saves survives a reboot.
//! Everything else was traded away to keep the format small enough to read in
//! one sitting and to make every operation provably bounded.
//!
//! # On-disk layout
//!
//! Sectors are 512 bytes; blocks are 4096 bytes (8 sectors). Every field is
//! little-endian.
//!
//! ```text
//! block 0                     superblock
//! block 1 .. 1+B              free-space bitmap   (B = bitmap_blocks)
//! block 1+B .. 1+B+D          directory table     (D = directory_blocks)
//! block 1+B+D .. total_blocks data
//! ```
//!
//! ## Superblock (block 0)
//!
//! | offset | size | field            |
//! |--------|------|------------------|
//! | 0      | 8    | magic `OS101FS1` |
//! | 8      | 4    | version (1)      |
//! | 12     | 4    | block_size       |
//! | 16     | 4    | total_blocks     |
//! | 20     | 4    | directory_blocks |
//! | 24     | 4    | bitmap_blocks    |
//!
//! The rest of the block is zero.
//!
//! ## Bitmap
//!
//! One bit per block, least significant bit first, set meaning in use. The
//! metadata blocks are marked in use by `format`, as are the bits past
//! `total_blocks`, so the allocator cannot hand out something that is not
//! there.
//!
//! ## Directory (flat, 128-byte entries, 32 per block)
//!
//! | offset | size | field                                      |
//! |--------|------|--------------------------------------------|
//! | 0      | 96   | path, UTF-8, NUL-padded, no leading slash  |
//! | 96     | 1    | flags — bit 0 = entry in use                |
//! | 100    | 4    | first block                                |
//! | 104    | 4    | length in bytes                            |
//! | 108    | 8    | monotonic sequence number                   |
//!
//! Everything else in an entry is reserved and written as zero.
//!
//! # Two deliberate simplifications
//!
//! **Files are contiguous.** A file occupies one run of consecutive blocks, so
//! an entry needs a start and a length rather than a block list or an extent
//! tree, and reading a file is one sequential sweep — which matters a great
//! deal when the transport is polled PIO. This is only safe because the API
//! has no `append` and no partial write: a file is always written whole, so
//! its final size is known before a single block is allocated. The cost is
//! fragmentation. A disk churned with mixed sizes can refuse a write it has
//! room for, because the room is not in one piece; there is no compaction
//! pass, and the honest answer for now is that reformatting is the defragmenter.
//!
//! **Directories are implicit.** There is no directory entry type. `downloads`
//! exists because some path begins `downloads/`, and listing a folder derives
//! the sub-folder names from the prefixes of the paths stored in the flat
//! table. That keeps the format to one table while still letting a file
//! manager walk a tree. Two consequences fall out of it: an empty folder
//! cannot exist, so listing one is indistinguishable from listing a folder
//! that was never created (both give an empty list), and nothing stops a file
//! and a folder sharing a name.
//!
//! # Crash behaviour
//!
//! The in-memory bitmap and directory are the working copy; each mutation
//! flushes only the sectors it actually changed. Ordering is chosen so that an
//! interruption can lose the file being written but never damage a different
//! one:
//!
//! * `write_file` puts the data blocks down first, then the bitmap, then the
//!   directory entry that points at them. A crash before the last step leaves
//!   blocks nobody claims — invisible, and reclaimed by the next format.
//! * `remove` clears the directory entry first and frees the blocks after, for
//!   the same reason in reverse: blocks that leak are harmless, blocks freed
//!   while an entry still points at them get handed to the next file.
//!
//! There is no journal, so a crash *during* `write_file` still leaves that one
//! file with a mixture of old and new contents. Fixing that properly needs
//! either a write-ahead log or copy-on-write allocation, neither of which is
//! worth it while the only writer is a single-threaded kernel.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::selftest::Report;

const SECTOR_SIZE: usize = 512;
const BLOCK_SIZE: usize = 4096;
const SECTORS_PER_BLOCK: u32 = (BLOCK_SIZE / SECTOR_SIZE) as u32;

const MAGIC: [u8; 8] = *b"OS101FS1";
const VERSION: u32 = 1;

const DIR_ENTRY_SIZE: usize = 128;
const ENTRIES_PER_SECTOR: usize = SECTOR_SIZE / DIR_ENTRY_SIZE;
const ENTRIES_PER_BLOCK: usize = BLOCK_SIZE / DIR_ENTRY_SIZE;
const MAX_PATH: usize = 96;
const FLAG_IN_USE: u8 = 0x01;

const SUPERBLOCK_BLOCK: u32 = 0;
const BITMAP_START_BLOCK: u32 = 1;

const BITS_PER_BITMAP_BLOCK: u32 = (BLOCK_SIZE * 8) as u32;

/// Superblock, one bitmap block, one directory block and some data. Below this
/// the format has nowhere to put anything.
const MIN_BLOCKS: u32 = 8;

/// One directory block (32 files) per 64 blocks of disk, so the table grows
/// with the volume instead of being a guess. The clamp keeps a tiny disk
/// usable and stops a large one spending megabytes on a table nothing will
/// fill — 64 blocks is 2048 files, comfortably more than this OS will produce.
const DIR_BLOCKS_PER_64: u32 = 64;
const MAX_DIRECTORY_BLOCKS: u32 = 64;

/// Ceiling on the metadata we will read into the heap when mounting. The
/// geometry comes off an untrusted disk, and a superblock claiming a
/// gigabyte-sized directory should be an error rather than an out-of-memory
/// abort.
const MAX_METADATA_BLOCKS: u32 = 1024;

/// A block device addressed in whole 512-byte sectors.
///
/// The filesystem is written against this rather than against the ATA driver
/// so that the self-test can exercise every path — including the ones that
/// only happen on a full or corrupt disk — with no hardware present.
pub trait Sectors {
    fn sector_count(&self) -> u32;
    fn read(&self, lba: u32, buf: &mut [u8]) -> Result<(), &'static str>;
    fn write(&mut self, lba: u32, buf: &[u8]) -> Result<(), &'static str>;
}

// ── Devices ─────────────────────────────────────────────────────────────

/// A disk that is just a `Vec<u8>`, for the self-test.
pub struct RamDisk {
    image: Vec<u8>,
}

impl RamDisk {
    pub fn new(sectors: u32) -> Self {
        Self {
            image: vec![0u8; (sectors as usize).saturating_mul(SECTOR_SIZE)],
        }
    }

    /// Byte range for a transfer, applying the same contract the ATA driver
    /// enforces so the self-test cannot pass on a request real hardware would
    /// reject.
    fn window(&self, lba: u32, len: usize) -> Result<(usize, usize), &'static str> {
        if len == 0 || len % SECTOR_SIZE != 0 {
            return Err("transfer is not a whole number of sectors");
        }
        if len / SECTOR_SIZE > crate::ata::MAX_SECTORS_PER_TRANSFER {
            return Err("transfer is longer than 128 sectors");
        }
        let start = (lba as usize)
            .checked_mul(SECTOR_SIZE)
            .ok_or("LBA overflows the address space")?;
        let end = start.checked_add(len).ok_or("transfer overflows the disk")?;
        if end > self.image.len() {
            return Err("transfer runs past the end of the disk");
        }
        Ok((start, end))
    }
}

impl Sectors for RamDisk {
    fn sector_count(&self) -> u32 {
        (self.image.len() / SECTOR_SIZE) as u32
    }

    fn read(&self, lba: u32, buf: &mut [u8]) -> Result<(), &'static str> {
        let (start, end) = self.window(lba, buf.len())?;
        let Some(src) = self.image.get(start..end) else {
            return Err("read past the end of the disk");
        };
        buf.copy_from_slice(src);
        Ok(())
    }

    fn write(&mut self, lba: u32, buf: &[u8]) -> Result<(), &'static str> {
        let (start, end) = self.window(lba, buf.len())?;
        let Some(dst) = self.image.get_mut(start..end) else {
            return Err("write past the end of the disk");
        };
        dst.copy_from_slice(buf);
        Ok(())
    }
}

/// The real thing: one of the two drives on the primary ATA channel.
///
/// Unused until the VFS mounts a disk, which happens outside this module — so
/// it and its two impls opt out of the dead-code lint. Everything else here is
/// reachable from them or from [`selftest`].
#[allow(dead_code)]
pub struct AtaDisk {
    drive: crate::ata::Drive,
    sectors: u32,
}

#[allow(dead_code)]
impl AtaDisk {
    /// `None` if no drive answered IDENTIFY on that slot, which is the normal
    /// case when the run script has not attached a second disk.
    pub fn open(drive: crate::ata::Drive) -> Option<Self> {
        let info = crate::ata::identify(drive)?;
        crate::serial_println!(
            "ATA: {:?} is \"{}\", {} sectors ({} MiB)",
            drive,
            info.model,
            info.sectors,
            info.sectors / 2048
        );
        if info.sectors == 0 {
            return None;
        }
        Some(Self {
            drive,
            sectors: info.sectors,
        })
    }
}

#[allow(dead_code)]
impl Sectors for AtaDisk {
    fn sector_count(&self) -> u32 {
        self.sectors
    }

    fn read(&self, lba: u32, buf: &mut [u8]) -> Result<(), &'static str> {
        crate::ata::read(self.drive, lba, buf)
    }

    fn write(&mut self, lba: u32, buf: &[u8]) -> Result<(), &'static str> {
        crate::ata::write(self.drive, lba, buf)
    }
}

// ── Byte helpers ────────────────────────────────────────────────────────
//
// Everything here reads from a buffer whose contents came off a disk, so the
// accessors return Option rather than indexing: a truncated or hostile
// structure has to become an error, never a panic.

fn le_u32(buf: &[u8], off: usize) -> Option<u32> {
    match buf.get(off..)?.get(..4)? {
        [a, b, c, d] => Some(u32::from_le_bytes([*a, *b, *c, *d])),
        _ => None,
    }
}

fn le_u64(buf: &[u8], off: usize) -> Option<u64> {
    match buf.get(off..)?.get(..8)? {
        [a, b, c, d, e, f, g, h] => Some(u64::from_le_bytes([*a, *b, *c, *d, *e, *f, *g, *h])),
        _ => None,
    }
}

fn put_u32(buf: &mut [u8], off: usize, val: u32) {
    if let Some(slot) = buf.get_mut(off..).and_then(|s| s.get_mut(..4)) {
        slot.copy_from_slice(&val.to_le_bytes());
    }
}

fn put_u64(buf: &mut [u8], off: usize, val: u64) {
    if let Some(slot) = buf.get_mut(off..).and_then(|s| s.get_mut(..8)) {
        slot.copy_from_slice(&val.to_le_bytes());
    }
}

fn ceil_div(a: u32, b: u32) -> u32 {
    if b == 0 {
        return 0;
    }
    a / b + if a % b != 0 { 1 } else { 0 }
}

fn blocks_for(len: u32) -> u32 {
    ceil_div(len, BLOCK_SIZE as u32)
}

fn block_lba(block: u32) -> u32 {
    block.saturating_mul(SECTORS_PER_BLOCK)
}

/// Inclusive range of sectors covering the byte range `[low, high)`.
///
/// Split out so it can be checked directly: reaching the second sector of a
/// bitmap for real needs a volume of more than 16 MiB, which is more heap than
/// a boot-time test should be allocating, and getting this wrong would lose
/// every allocation above block 4096 on exactly the large disk we cannot
/// afford to simulate.
fn sector_span(low: usize, high: usize) -> (usize, usize) {
    (low / SECTOR_SIZE, high.saturating_sub(1) / SECTOR_SIZE)
}

// ── Bitmap ──────────────────────────────────────────────────────────────

/// A block outside the bitmap reads as in use, so a short or corrupt bitmap
/// makes the allocator conservative rather than dangerous.
fn bit_get(bitmap: &[u8], block: u32) -> bool {
    let byte = block as usize / 8;
    let mask = 1u8 << (block % 8);
    bitmap.get(byte).map(|b| b & mask != 0).unwrap_or(true)
}

fn bit_put(bitmap: &mut [u8], block: u32, used: bool) {
    let byte = block as usize / 8;
    let mask = 1u8 << (block % 8);
    if let Some(b) = bitmap.get_mut(byte) {
        if used {
            *b |= mask;
        } else {
            *b &= !mask;
        }
    }
}

fn mark_run(bitmap: &mut [u8], first: u32, blocks: u32, used: bool) {
    for i in 0..blocks {
        bit_put(bitmap, first.saturating_add(i), used);
    }
}

/// First-fit run of `blocks` consecutive free blocks in `[from, to)`.
///
/// First fit rather than best fit because the allocator has no size hints to
/// work with and a linear scan of a bitmap this small is not worth optimising.
fn find_run(bitmap: &[u8], from: u32, to: u32, blocks: u32) -> Option<u32> {
    if blocks == 0 {
        return None;
    }
    let mut run_start = from;
    let mut run = 0u32;
    let mut block = from;
    while block < to {
        if bit_get(bitmap, block) {
            run = 0;
        } else {
            if run == 0 {
                run_start = block;
            }
            run += 1;
            if run == blocks {
                return Some(run_start);
            }
        }
        block += 1;
    }
    None
}

// ── Paths ───────────────────────────────────────────────────────────────

/// The one place path syntax is decided, used by every entry point and again
/// when decoding an entry off the disk.
///
/// The rules exist because the flat table gives a path no structure of its own:
/// `a//b` and `a/b` would be two entries naming one file, and `..` would let a
/// caller describe a location the listing code cannot represent.
fn validate_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("empty path");
    }
    if path.len() > MAX_PATH {
        return Err("path is longer than 96 bytes");
    }
    if path.starts_with('/') {
        return Err("path must be relative");
    }
    if path.ends_with('/') {
        return Err("path names a directory");
    }
    if path.contains('\0') {
        return Err("path contains a NUL");
    }
    for part in path.split('/') {
        if part.is_empty() {
            return Err("path has an empty component");
        }
        if part == "." || part == ".." {
            return Err("path contains . or ..");
        }
    }
    Ok(())
}

// ── Superblock ──────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Superblock {
    total_blocks: u32,
    directory_blocks: u32,
    bitmap_blocks: u32,
}

impl Superblock {
    fn dir_start(&self) -> u32 {
        BITMAP_START_BLOCK.saturating_add(self.bitmap_blocks)
    }

    fn data_start(&self) -> u32 {
        self.dir_start().saturating_add(self.directory_blocks)
    }

    fn entry_count(&self) -> usize {
        (self.directory_blocks as usize).saturating_mul(ENTRIES_PER_BLOCK)
    }

    /// Geometry for a fresh volume on a device of `device_sectors` sectors.
    fn fresh(device_sectors: u32) -> Result<Self, &'static str> {
        let total_blocks = device_sectors / SECTORS_PER_BLOCK;
        if total_blocks < MIN_BLOCKS {
            return Err("disk is too small for OS101FS");
        }
        let bitmap_blocks = ceil_div(total_blocks, BITS_PER_BITMAP_BLOCK).max(1);
        let directory_blocks = (total_blocks / DIR_BLOCKS_PER_64).clamp(1, MAX_DIRECTORY_BLOCKS);
        let sb = Superblock {
            total_blocks,
            directory_blocks,
            bitmap_blocks,
        };
        sb.validate(device_sectors)?;
        Ok(sb)
    }

    fn parse(sector: &[u8], device_sectors: u32) -> Result<Self, &'static str> {
        if sector.get(0..8) != Some(&MAGIC[..]) {
            return Err("not an OS101FS volume");
        }
        let version = le_u32(sector, 8).ok_or("truncated superblock")?;
        let block_size = le_u32(sector, 12).ok_or("truncated superblock")?;
        let total_blocks = le_u32(sector, 16).ok_or("truncated superblock")?;
        let directory_blocks = le_u32(sector, 20).ok_or("truncated superblock")?;
        let bitmap_blocks = le_u32(sector, 24).ok_or("truncated superblock")?;
        if version != VERSION {
            return Err("unsupported OS101FS version");
        }
        if block_size as usize != BLOCK_SIZE {
            return Err("unsupported OS101FS block size");
        }
        let sb = Superblock {
            total_blocks,
            directory_blocks,
            bitmap_blocks,
        };
        sb.validate(device_sectors)?;
        Ok(sb)
    }

    /// Reject anything that would make a later calculation wrap, over-allocate
    /// or address a block the device does not have.
    fn validate(&self, device_sectors: u32) -> Result<(), &'static str> {
        if self.total_blocks < MIN_BLOCKS {
            return Err("OS101FS volume is impossibly small");
        }
        if self.total_blocks > device_sectors / SECTORS_PER_BLOCK {
            return Err("OS101FS volume is larger than its disk");
        }
        if self.bitmap_blocks == 0 || self.directory_blocks == 0 {
            return Err("OS101FS volume has no bitmap or directory");
        }
        let metadata = self
            .bitmap_blocks
            .checked_add(self.directory_blocks)
            .and_then(|m| m.checked_add(1))
            .ok_or("OS101FS metadata size overflows")?;
        if metadata > MAX_METADATA_BLOCKS {
            return Err("OS101FS metadata is implausibly large");
        }
        if metadata >= self.total_blocks {
            return Err("OS101FS metadata leaves no room for data");
        }
        let addressable = self
            .bitmap_blocks
            .checked_mul(BITS_PER_BITMAP_BLOCK)
            .ok_or("OS101FS bitmap size overflows")?;
        if addressable < self.total_blocks {
            return Err("OS101FS bitmap cannot cover the volume");
        }
        Ok(())
    }

    fn encode(&self) -> [u8; BLOCK_SIZE] {
        let mut block = [0u8; BLOCK_SIZE];
        if let Some(slot) = block.get_mut(0..8) {
            slot.copy_from_slice(&MAGIC);
        }
        put_u32(&mut block, 8, VERSION);
        put_u32(&mut block, 12, BLOCK_SIZE as u32);
        put_u32(&mut block, 16, self.total_blocks);
        put_u32(&mut block, 20, self.directory_blocks);
        put_u32(&mut block, 24, self.bitmap_blocks);
        block
    }
}

// ── Directory entries ───────────────────────────────────────────────────

#[derive(Clone)]
struct DirEnt {
    used: bool,
    path: String,
    first_block: u32,
    length: u32,
    seq: u64,
}

impl DirEnt {
    fn free() -> Self {
        DirEnt {
            used: false,
            path: String::new(),
            first_block: 0,
            length: 0,
            seq: 0,
        }
    }

    /// Decode one 128-byte slot.
    ///
    /// Anything we would have refused to write — bad UTF-8, a path with `..`
    /// in it, a truncated slot — comes back as a free entry. Treating a
    /// corrupt slot as free rather than as an error means one bad byte cannot
    /// make the whole volume unmountable, and a free slot cannot shadow a real
    /// file or be read from.
    fn decode(raw: &[u8]) -> Self {
        let Some(flags) = raw.get(96).copied() else {
            return Self::free();
        };
        if flags & FLAG_IN_USE == 0 {
            return Self::free();
        }
        let Some(name) = raw.get(0..MAX_PATH) else {
            return Self::free();
        };
        let end = name.iter().position(|&b| b == 0).unwrap_or(MAX_PATH);
        let Some(bytes) = name.get(..end) else {
            return Self::free();
        };
        let Ok(path) = core::str::from_utf8(bytes) else {
            return Self::free();
        };
        if validate_path(path).is_err() {
            return Self::free();
        }
        let (Some(first_block), Some(length), Some(seq)) =
            (le_u32(raw, 100), le_u32(raw, 104), le_u64(raw, 108))
        else {
            return Self::free();
        };
        DirEnt {
            used: true,
            path: String::from(path),
            first_block,
            length,
            seq,
        }
    }

    fn encode(&self, raw: &mut [u8]) {
        for b in raw.iter_mut() {
            *b = 0;
        }
        if !self.used {
            return;
        }
        let bytes = self.path.as_bytes();
        if let Some(slot) = raw.get_mut(..bytes.len()) {
            slot.copy_from_slice(bytes);
        }
        if let Some(flags) = raw.get_mut(96) {
            *flags = FLAG_IN_USE;
        }
        put_u32(raw, 100, self.first_block);
        put_u32(raw, 104, self.length);
        put_u64(raw, 108, self.seq);
    }
}

// ── The filesystem ──────────────────────────────────────────────────────

pub struct DiskFs<D: Sectors> {
    dev: D,
    sb: Superblock,
    bitmap: Vec<u8>,
    dir: Vec<DirEnt>,
    next_seq: u64,
}

fn read_region<D: Sectors>(dev: &D, first_block: u32, blocks: u32) -> Result<Vec<u8>, &'static str> {
    let bytes = (blocks as usize)
        .checked_mul(BLOCK_SIZE)
        .ok_or("OS101FS metadata region overflows")?;
    let mut out = vec![0u8; bytes];
    for (i, block) in out.chunks_mut(BLOCK_SIZE).enumerate() {
        dev.read(block_lba(first_block.saturating_add(i as u32)), block)?;
    }
    Ok(out)
}

impl<D: Sectors> DiskFs<D> {
    /// Write a fresh superblock, bitmap and empty directory.
    ///
    /// The superblock goes down last on purpose: until it lands the volume is
    /// unrecognised, so a format interrupted half way looks blank to
    /// [`Self::mount_or_format`] rather than looking like a filesystem whose
    /// bitmap is garbage.
    pub fn format(mut dev: D) -> Result<Self, &'static str> {
        let sb = Superblock::fresh(dev.sector_count())?;

        let mut bitmap = vec![0u8; (sb.bitmap_blocks as usize).saturating_mul(BLOCK_SIZE)];
        mark_run(&mut bitmap, 0, sb.data_start(), true);
        // The tail of the last bitmap block addresses blocks the disk does not
        // have. Marking them used keeps the allocator honest without a special
        // case in the scan.
        for block in sb.total_blocks..sb.bitmap_blocks.saturating_mul(BITS_PER_BITMAP_BLOCK) {
            bit_put(&mut bitmap, block, true);
        }

        for (i, block) in bitmap.chunks(BLOCK_SIZE).enumerate() {
            dev.write(
                block_lba(BITMAP_START_BLOCK.saturating_add(i as u32)),
                block,
            )?;
        }

        let empty = [0u8; BLOCK_SIZE];
        for i in 0..sb.directory_blocks {
            dev.write(block_lba(sb.dir_start().saturating_add(i)), &empty)?;
        }

        dev.write(block_lba(SUPERBLOCK_BLOCK), &sb.encode())?;

        Ok(Self {
            dev,
            sb,
            bitmap,
            dir: vec![DirEnt::free(); sb.entry_count()],
            next_seq: 0,
        })
    }

    /// Read an existing filesystem.
    pub fn mount(dev: D) -> Result<Self, &'static str> {
        let mut sector = [0u8; SECTOR_SIZE];
        dev.read(block_lba(SUPERBLOCK_BLOCK), &mut sector)?;
        let sb = Superblock::parse(&sector, dev.sector_count())?;

        let bitmap = read_region(&dev, BITMAP_START_BLOCK, sb.bitmap_blocks)?;
        let raw_dir = read_region(&dev, sb.dir_start(), sb.directory_blocks)?;

        let mut dir = Vec::new();
        for slot in raw_dir.chunks(DIR_ENTRY_SIZE) {
            dir.push(DirEnt::decode(slot));
        }
        dir.resize(sb.entry_count(), DirEnt::free());

        // Carry on numbering where the previous boot left off, so creation
        // order still means something across a reboot.
        let next_seq = dir
            .iter()
            .filter(|e| e.used)
            .map(|e| e.seq)
            .max()
            .map(|m| m.saturating_add(1))
            .unwrap_or(0);

        Ok(Self {
            dev,
            sb,
            bitmap,
            dir,
            next_seq,
        })
    }

    /// Mount, falling back to formatting a disk we do not recognise.
    ///
    /// The superblock is inspected before committing to either path, because
    /// `mount` consumes the device and we must not reformat a volume that
    /// *is* OS101FS but failed to mount for some other reason — that would
    /// turn a transient read error into data loss.
    pub fn mount_or_format(dev: D) -> Result<Self, &'static str> {
        let mut sector = [0u8; SECTOR_SIZE];
        let recognised = dev.read(block_lba(SUPERBLOCK_BLOCK), &mut sector).is_ok()
            && Superblock::parse(&sector, dev.sector_count()).is_ok();
        if recognised {
            Self::mount(dev)
        } else {
            Self::format(dev)
        }
    }

    /// Hand the device back, dropping the filesystem. Used by the self-test to
    /// simulate a reboot; a caller that wants to unmount can throw it away.
    pub fn into_device(self) -> D {
        self.dev
    }

    fn find(&self, path: &str) -> Option<usize> {
        self.dir.iter().position(|e| e.used && e.path == path)
    }

    fn free_slot(&self) -> Option<usize> {
        self.dir.iter().position(|e| !e.used)
    }

    pub fn exists(&self, path: &str) -> bool {
        self.find(path).is_some()
    }

    /// Length in bytes, or `None` if there is no such file.
    pub fn size(&self, path: &str) -> Option<usize> {
        self.dir
            .get(self.find(path)?)
            .map(|e| e.length as usize)
    }

    /// (bytes used, bytes total), counting only the data area — a status line
    /// wants to say how much the user can still store, not how much of the
    /// disk the bookkeeping took.
    pub fn usage(&self) -> (usize, usize) {
        let start = self.sb.data_start();
        let total = self.sb.total_blocks;
        let mut used = 0usize;
        let mut block = start;
        while block < total {
            if bit_get(&self.bitmap, block) {
                used += 1;
            }
            block += 1;
        }
        let capacity = total.saturating_sub(start) as usize;
        (
            used.saturating_mul(BLOCK_SIZE),
            capacity.saturating_mul(BLOCK_SIZE),
        )
    }

    /// Entries directly inside `dir` (`""` for the root).
    ///
    /// Sub-folders come back with a trailing slash and sorted; files come back
    /// as their own name in creation order, which is what the sequence number
    /// is for. Folders first because that is what a file manager wants to
    /// draw, and alphabetical because a derived folder has no creation time of
    /// its own to sort by.
    pub fn list(&self, dir: &str) -> Result<Vec<String>, &'static str> {
        // Checked before the trailing slash is taken off, so `"/"` cannot slip
        // through as a spelling of the root.
        if dir.starts_with('/') {
            return Err("path must be relative");
        }
        let dir = dir.strip_suffix('/').unwrap_or(dir);
        if !dir.is_empty() {
            validate_path(dir)?;
        }
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            alloc::format!("{}/", dir)
        };

        let mut folders: Vec<&str> = Vec::new();
        let mut files: Vec<(u64, &str)> = Vec::new();
        for entry in self.dir.iter().filter(|e| e.used) {
            let Some(rest) = entry.path.strip_prefix(prefix.as_str()) else {
                continue;
            };
            match rest.find('/') {
                Some(i) => {
                    if let Some(head) = rest.get(..i) {
                        if !folders.contains(&head) {
                            folders.push(head);
                        }
                    }
                }
                None => files.push((entry.seq, rest)),
            }
        }
        folders.sort_unstable();
        files.sort_unstable_by_key(|(seq, _)| *seq);

        let mut out = Vec::new();
        for folder in folders {
            out.push(alloc::format!("{}/", folder));
        }
        for (_, name) in files {
            out.push(String::from(name));
        }
        Ok(out)
    }

    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, &'static str> {
        validate_path(path)?;
        let index = self.find(path).ok_or("file not found")?;
        let entry = self.dir.get(index).ok_or("file not found")?;
        let length = entry.length as usize;
        let blocks = blocks_for(entry.length);

        if blocks > 0 {
            let end = entry
                .first_block
                .checked_add(blocks)
                .ok_or("corrupt directory entry")?;
            if entry.first_block < self.sb.data_start() || end > self.sb.total_blocks {
                return Err("corrupt directory entry");
            }
        }

        let mut out: Vec<u8> = Vec::new();
        let mut block = [0u8; BLOCK_SIZE];
        for i in 0..blocks {
            self.dev
                .read(block_lba(entry.first_block.saturating_add(i)), &mut block)?;
            let remaining = length.saturating_sub(out.len());
            if let Some(slice) = block.get(..remaining.min(BLOCK_SIZE)) {
                out.extend_from_slice(slice);
            }
        }
        out.truncate(length);
        Ok(out)
    }

    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), &'static str> {
        validate_path(path)?;
        if data.len() > u32::MAX as usize {
            return Err("file is too large");
        }
        let needed = blocks_for(data.len() as u32);
        let existing = self.find(path);

        // Plan the allocation on a scratch copy. The old run has to go back to
        // the free pool before we look for space — otherwise rewriting a file
        // would need room for two copies of it, and repeated rewrites would
        // leak the disk away — but a plan that turns out not to fit must leave
        // no trace, hence the copy rather than an in-place free and undo.
        let mut plan = self.bitmap.clone();
        if let Some(i) = existing {
            if let Some(entry) = self.dir.get(i) {
                mark_run(
                    &mut plan,
                    entry.first_block,
                    blocks_for(entry.length),
                    false,
                );
            }
        }

        let first_block = if needed == 0 {
            0
        } else {
            find_run(&plan, self.sb.data_start(), self.sb.total_blocks, needed)
                .ok_or("disk full")?
        };
        let slot = match existing {
            Some(i) => i,
            None => self.free_slot().ok_or("directory full")?,
        };
        mark_run(&mut plan, first_block, needed, true);

        // Data first: a crash from here to the end of the function leaves
        // blocks that no entry claims, which loses this file and nothing else.
        let mut block = [0u8; BLOCK_SIZE];
        for (i, chunk) in data.chunks(BLOCK_SIZE).enumerate() {
            if chunk.len() < BLOCK_SIZE {
                // Zero the tail so the last block of a short file cannot carry
                // whatever the previous occupant left there.
                block = [0u8; BLOCK_SIZE];
            }
            if let Some(slot) = block.get_mut(..chunk.len()) {
                slot.copy_from_slice(chunk);
            }
            self.dev
                .write(block_lba(first_block.saturating_add(i as u32)), &block)?;
        }

        // Only the bytes that actually moved get written back, so a one-block
        // file costs one sector of bitmap rather than the whole thing.
        let mut low = usize::MAX;
        let mut high = 0usize;
        for (i, (before, after)) in self.bitmap.iter().zip(plan.iter()).enumerate() {
            if before != after {
                if i < low {
                    low = i;
                }
                high = i + 1;
            }
        }
        self.bitmap = plan;
        if low < high {
            self.flush_bitmap(low, high)?;
        }

        // An overwrite keeps its original sequence number: a rewrite is not a
        // creation, and a file manager should not see a saved file jump to the
        // end of the list.
        let seq = match existing.and_then(|i| self.dir.get(i)).map(|e| e.seq) {
            Some(seq) => seq,
            None => {
                let seq = self.next_seq;
                self.next_seq = self.next_seq.saturating_add(1);
                seq
            }
        };
        if let Some(entry) = self.dir.get_mut(slot) {
            *entry = DirEnt {
                used: true,
                path: String::from(path),
                first_block,
                length: data.len() as u32,
                seq,
            };
        }
        self.flush_dir_entry(slot)
    }

    pub fn remove(&mut self, path: &str) -> Result<(), &'static str> {
        validate_path(path)?;
        let slot = self.find(path).ok_or("file not found")?;
        let (first_block, blocks) = match self.dir.get(slot) {
            Some(entry) => (entry.first_block, blocks_for(entry.length)),
            None => return Err("file not found"),
        };

        // Entry first, blocks second — the opposite order to writing, and for
        // the same reason: blocks that leak are invisible, blocks freed under
        // a live entry get handed to the next file.
        if let Some(entry) = self.dir.get_mut(slot) {
            *entry = DirEnt::free();
        }
        self.flush_dir_entry(slot)?;

        if blocks == 0 {
            return Ok(());
        }
        mark_run(&mut self.bitmap, first_block, blocks, false);
        let low = first_block as usize / 8;
        let high = (first_block.saturating_add(blocks) as usize / 8).saturating_add(1);
        self.flush_bitmap(low, high)
    }

    /// Write back every bitmap sector touching the byte range `[low, high)`.
    fn flush_bitmap(&mut self, low: usize, high: usize) -> Result<(), &'static str> {
        let (first, last) = sector_span(low, high);
        let base = block_lba(BITMAP_START_BLOCK);
        for sector in first..=last {
            let offset = sector.saturating_mul(SECTOR_SIZE);
            let Some(chunk) = self
                .bitmap
                .get(offset..)
                .and_then(|rest| rest.get(..SECTOR_SIZE))
            else {
                break;
            };
            self.dev.write(base.saturating_add(sector as u32), chunk)?;
        }
        Ok(())
    }

    /// Write back the one directory sector holding `index`, rebuilt from the
    /// four entries that share it.
    fn flush_dir_entry(&mut self, index: usize) -> Result<(), &'static str> {
        let sector = index / ENTRIES_PER_SECTOR;
        let base = sector.saturating_mul(ENTRIES_PER_SECTOR);
        let mut buf = [0u8; SECTOR_SIZE];
        for slot in 0..ENTRIES_PER_SECTOR {
            let offset = slot.saturating_mul(DIR_ENTRY_SIZE);
            let Some(dst) = buf
                .get_mut(offset..)
                .and_then(|rest| rest.get_mut(..DIR_ENTRY_SIZE))
            else {
                continue;
            };
            match self.dir.get(base.saturating_add(slot)) {
                Some(entry) => entry.encode(dst),
                None => DirEnt::free().encode(dst),
            }
        }
        let lba = block_lba(self.sb.dir_start()).saturating_add(sector as u32);
        self.dev.write(lba, &buf)
    }
}

// ── Self-test ───────────────────────────────────────────────────────────

/// 1 MiB: 256 blocks, of which 250 are data and the directory holds 128
/// entries. Big enough for the nesting and many-files cases, small enough that
/// the heap does not notice.
const TEST_SECTORS: u32 = 2048;
/// 64 KiB: 16 blocks, 13 of them data and 32 directory entries. Small enough
/// to fill by hand.
const SMALL_SECTORS: u32 = 128;

fn expect<T>(r: &mut Report, name: &'static str, res: Result<T, &'static str>) -> Option<T> {
    r.check(name, res.is_ok());
    res.ok()
}

fn fresh(r: &mut Report, name: &'static str, sectors: u32) -> Option<DiskFs<RamDisk>> {
    expect(r, name, DiskFs::format(RamDisk::new(sectors)))
}

/// Boot-time checks for the whole filesystem, run against a RAM disk.
///
/// Everything a real disk adds — timeouts, cache flushes, a slow bus — is in
/// `ata`, and none of it changes the answers here. What this suite is for is
/// the part that would silently eat a user's files: allocation, the free-space
/// accounting, path handling, and above all that bytes written before a reboot
/// come back after one.
///
/// Unused here on purpose: the boot sequence that calls it lives outside this
/// module.
#[allow(dead_code)]
pub fn selftest() -> Report {
    let mut r = Report::new();

    geometry(&mut r);
    round_trip(&mut r);
    overwriting(&mut r);
    deletion(&mut r);
    many_files(&mut r);
    tree(&mut r);
    bad_paths(&mut r);
    exhaustion(&mut r);
    corruption(&mut r);
    persistence(&mut r);

    r
}

fn geometry(r: &mut Report) {
    r.check(
        "a disk with no sectors cannot be formatted",
        DiskFs::format(RamDisk::new(0)).is_err(),
    );
    r.check(
        "a disk below the minimum cannot be formatted",
        DiskFs::format(RamDisk::new(8)).is_err(),
    );
    r.check(
        "a blank disk does not mount",
        DiskFs::mount(RamDisk::new(TEST_SECTORS)).is_err(),
    );
    r.check(
        "a blank disk is formatted instead",
        DiskFs::mount_or_format(RamDisk::new(TEST_SECTORS)).is_ok(),
    );

    r.check("a change inside one sector flushes one sector", sector_span(0, 32) == (0, 0));
    r.check("a change at the very end of a sector stays in it", sector_span(500, 512) == (0, 0));
    r.check("a change straddling the boundary flushes both", sector_span(511, 513) == (0, 1));
    r.check("a change in the second sector flushes only it", sector_span(512, 600) == (1, 1));
    r.check("a change spanning three sectors flushes three", sector_span(500, 1100) == (0, 2));
    r.check("an empty range collapses to the first sector", sector_span(0, 0) == (0, 0));

    let Some(fs) = fresh(r, "format succeeds", TEST_SECTORS) else {
        return;
    };
    let (used, total) = fs.usage();
    r.check("a fresh volume uses nothing", used == 0);
    r.check("a fresh volume reports its data area", total == 250 * BLOCK_SIZE);
    r.check(
        "a fresh volume lists nothing",
        fs.list("").map(|v| v.is_empty()) == Ok(true),
    );
    r.check("a fresh volume has no files", !fs.exists("anything"));
    r.check("size of a missing file is None", fs.size("anything").is_none());
    r.check(
        "reading a missing file fails",
        fs.read_file("anything").is_err(),
    );

    let disk = fs.into_device();
    r.check("a formatted volume mounts", DiskFs::mount(disk).is_ok());

    // Corrupt superblocks must be errors, not panics.
    let mut disk = RamDisk::new(TEST_SECTORS);
    let Some(fs) = expect(r, "format for corruption checks", DiskFs::format(disk)) else {
        return;
    };
    disk = fs.into_device();

    let mut sector = [0u8; SECTOR_SIZE];
    r.check(
        "superblock reads back",
        disk.read(0, &mut sector).is_ok(),
    );

    let mut bad = sector;
    bad[0] = b'X';
    r.check("bad magic is rejected", overwrite_and_mount(&mut disk, &bad).is_err());

    let mut bad = sector;
    put_u32(&mut bad, 8, 99);
    r.check("wrong version is rejected", overwrite_and_mount(&mut disk, &bad).is_err());

    let mut bad = sector;
    put_u32(&mut bad, 12, 1024);
    r.check("wrong block size is rejected", overwrite_and_mount(&mut disk, &bad).is_err());

    let mut bad = sector;
    put_u32(&mut bad, 16, 1_000_000);
    r.check("a volume bigger than its disk is rejected", overwrite_and_mount(&mut disk, &bad).is_err());

    let mut bad = sector;
    put_u32(&mut bad, 20, 0);
    r.check("a volume with no directory is rejected", overwrite_and_mount(&mut disk, &bad).is_err());

    let mut bad = sector;
    put_u32(&mut bad, 24, 0);
    r.check("a volume with no bitmap is rejected", overwrite_and_mount(&mut disk, &bad).is_err());

    let mut bad = sector;
    put_u32(&mut bad, 20, 255);
    r.check("a directory that leaves no data blocks is rejected", overwrite_and_mount(&mut disk, &bad).is_err());

    let mut bad = sector;
    put_u32(&mut bad, 20, 2000);
    r.check("an implausibly large directory is rejected", overwrite_and_mount(&mut disk, &bad).is_err());

    let mut bad = sector;
    bad[3] = b'Z';
    r.check(
        "an unrecognised volume is reformatted",
        overwrite(&mut disk, &bad).is_ok() && DiskFs::mount_or_format(disk).is_ok(),
    );
}

fn overwrite(disk: &mut RamDisk, sector: &[u8; SECTOR_SIZE]) -> Result<(), &'static str> {
    disk.write(0, sector)
}

/// Stamp a doctored superblock over block 0 and try to mount it. The disk is
/// borrowed rather than consumed so one volume can be corrupted many ways.
fn overwrite_and_mount(
    disk: &mut RamDisk,
    sector: &[u8; SECTOR_SIZE],
) -> Result<(), &'static str> {
    overwrite(disk, sector)?;
    let mut probe = [0u8; SECTOR_SIZE];
    disk.read(0, &mut probe)?;
    Superblock::parse(&probe, disk.sector_count()).map(|_| ())
}

fn round_trip(r: &mut Report) {
    let Some(mut fs) = fresh(r, "format for round trips", TEST_SECTORS) else {
        return;
    };

    let small = b"the quick brown fox".to_vec();
    expect(r, "write a small file", fs.write_file("notes.txt", &small));
    r.check("the small file exists", fs.exists("notes.txt"));
    r.check("its size is its length", fs.size("notes.txt") == Some(small.len()));
    r.check(
        "it reads back byte for byte",
        fs.read_file("notes.txt").as_deref() == Ok(small.as_slice()),
    );
    r.check("it occupies one block", fs.usage().0 == BLOCK_SIZE);

    // Several blocks, with a pattern that would show a block written out of
    // order or an off-by-one at a boundary.
    let big: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    expect(r, "write a multi-block file", fs.write_file("big.bin", &big));
    r.check("the multi-block size is right", fs.size("big.bin") == Some(big.len()));
    r.check(
        "the multi-block file reads back",
        fs.read_file("big.bin").as_deref() == Ok(big.as_slice()),
    );
    r.check("the multi-block file adds three blocks", fs.usage().0 == 4 * BLOCK_SIZE);

    // An exact multiple of the block size is where a ceiling division goes
    // wrong by a whole block in either direction.
    let exact: Vec<u8> = (0..2 * BLOCK_SIZE).map(|i| (i % 7) as u8).collect();
    expect(r, "write an exact multiple of a block", fs.write_file("exact.bin", &exact));
    r.check("the exact size is right", fs.size("exact.bin") == Some(exact.len()));
    r.check(
        "the exact file reads back",
        fs.read_file("exact.bin").as_deref() == Ok(exact.as_slice()),
    );
    r.check("an exact multiple adds exactly two blocks", fs.usage().0 == 6 * BLOCK_SIZE);

    expect(r, "write an empty file", fs.write_file("empty.dat", &[]));
    r.check("the empty file exists", fs.exists("empty.dat"));
    r.check("its size is zero", fs.size("empty.dat") == Some(0));
    r.check(
        "it reads back empty",
        fs.read_file("empty.dat").map(|v| v.is_empty()) == Ok(true),
    );
    r.check("an empty file costs no blocks", fs.usage().0 == 6 * BLOCK_SIZE);

    let (used, total) = fs.usage();
    r.check("usage never exceeds capacity", used <= total);
}

fn overwriting(r: &mut Report) {
    let Some(mut fs) = fresh(r, "format for overwrites", TEST_SECTORS) else {
        return;
    };

    expect(r, "write the original", fs.write_file("file", &[1u8; 100]));
    r.check("the original costs one block", fs.usage().0 == BLOCK_SIZE);

    let longer = vec![2u8; 3 * BLOCK_SIZE];
    expect(r, "overwrite with something longer", fs.write_file("file", &longer));
    r.check(
        "the longer contents read back",
        fs.read_file("file").as_deref() == Ok(longer.as_slice()),
    );
    r.check("the longer file costs three blocks", fs.usage().0 == 3 * BLOCK_SIZE);

    let shorter = vec![3u8; 10];
    expect(r, "overwrite with something shorter", fs.write_file("file", &shorter));
    r.check(
        "the shorter contents read back",
        fs.read_file("file").as_deref() == Ok(shorter.as_slice()),
    );
    r.check("the shorter file gives the blocks back", fs.usage().0 == BLOCK_SIZE);
    r.check("the shorter length is recorded", fs.size("file") == Some(10));

    // The leak this guards against is subtle: free the old run *after*
    // allocating the new one and every rewrite strands a block.
    let mut leaked = false;
    for i in 0..40u32 {
        let body = vec![(i % 255) as u8; 2 * BLOCK_SIZE + 1];
        if fs.write_file("file", &body).is_err() {
            leaked = true;
            break;
        }
        if fs.usage().0 != 3 * BLOCK_SIZE {
            leaked = true;
            break;
        }
    }
    r.check("rewriting forty times leaks nothing", !leaked);
    r.check("the file survived the churn", fs.size("file") == Some(2 * BLOCK_SIZE + 1));
}

fn deletion(r: &mut Report) {
    let Some(mut fs) = fresh(r, "format for deletions", TEST_SECTORS) else {
        return;
    };

    let body = vec![9u8; 5 * BLOCK_SIZE];
    expect(r, "write before deleting", fs.write_file("gone.bin", &body));
    r.check("it is there first", fs.exists("gone.bin"));
    expect(r, "delete it", fs.remove("gone.bin"));
    r.check("it is gone", !fs.exists("gone.bin"));
    r.check("reading it now fails", fs.read_file("gone.bin").is_err());
    r.check("its size is gone too", fs.size("gone.bin").is_none());
    r.check("the space came back", fs.usage().0 == 0);
    r.check("deleting it twice fails", fs.remove("gone.bin").is_err());
    r.check("deleting a missing file fails", fs.remove("never").is_err());
    r.check("deleting an invalid path fails", fs.remove("../escape").is_err());

    // The freed run must be genuinely reusable, not merely uncounted.
    expect(r, "reuse the freed space", fs.write_file("again.bin", &body));
    r.check(
        "the reused space holds the same bytes",
        fs.read_file("again.bin").as_deref() == Ok(body.as_slice()),
    );
    r.check("accounting matches after reuse", fs.usage().0 == 5 * BLOCK_SIZE);
}

fn many_files(r: &mut Report) {
    let Some(mut fs) = fresh(r, "format for many files", TEST_SECTORS) else {
        return;
    };

    let count = 40usize;
    let mut all_written = true;
    for i in 0..count {
        let name = alloc::format!("f{}.bin", i);
        let body = vec![i as u8; 100 + i];
        if fs.write_file(&name, &body).is_err() {
            all_written = false;
            break;
        }
    }
    r.check("forty files write", all_written);

    let mut all_read = true;
    for i in 0..count {
        let name = alloc::format!("f{}.bin", i);
        let expected = vec![i as u8; 100 + i];
        if fs.read_file(&name).as_deref() != Ok(expected.as_slice()) {
            all_read = false;
            break;
        }
    }
    r.check("forty files read back", all_read);
    r.check(
        "the root lists all forty",
        fs.list("").map(|v| v.len()) == Ok(count),
    );
    r.check("forty one-block files cost forty blocks", fs.usage().0 == count * BLOCK_SIZE);
    r.check(
        "the listing is in creation order",
        fs.list("").ok().and_then(|v| v.first().cloned()) == Some(String::from("f0.bin")),
    );

    // Forty entries span ten directory sectors, so this is the case that would
    // catch a flush that only ever wrote the first one.
    let disk = fs.into_device();
    let Some(fs) = expect(r, "a full directory remounts", DiskFs::mount(disk)) else {
        return;
    };
    let mut all_survived = true;
    for i in 0..count {
        let expected = vec![i as u8; 100 + i];
        if fs.read_file(&alloc::format!("f{}.bin", i)).as_deref() != Ok(expected.as_slice()) {
            all_survived = false;
            break;
        }
    }
    r.check("entries beyond the first directory sector survive", all_survived);
    r.check(
        "and the whole listing survives",
        fs.list("").map(|v| v.len()) == Ok(count),
    );
    r.check("with its accounting", fs.usage().0 == count * BLOCK_SIZE);
}

fn tree(r: &mut Report) {
    let Some(mut fs) = fresh(r, "format for the tree", TEST_SECTORS) else {
        return;
    };

    // Written in this order so the listing checks below also pin down creation
    // ordering.
    for path in [
        "readme.txt",
        "downloads/cat.png",
        "downloads/dog.png",
        "downloads/deep/inner.bin",
        "docs/a/b/c.txt",
        "alpha.txt",
    ] {
        if fs.write_file(path, path.as_bytes()).is_err() {
            r.check("building the tree", false);
            return;
        }
    }
    r.check("building the tree", true);

    let root = fs.list("").unwrap_or_default();
    r.check(
        "the root lists folders then files",
        root == ["docs/", "downloads/", "readme.txt", "alpha.txt"],
    );
    r.check(
        "a folder appears once, not once per file inside it",
        root.iter().filter(|n| *n == "downloads/").count() == 1,
    );

    r.check(
        "a folder lists its own children",
        fs.list("downloads").unwrap_or_default() == ["deep/", "cat.png", "dog.png"],
    );
    r.check(
        "a trailing slash is accepted",
        fs.list("downloads/") == fs.list("downloads"),
    );
    r.check(
        "a nested folder lists its children",
        fs.list("downloads/deep").unwrap_or_default() == ["inner.bin"],
    );
    r.check(
        "each level of a deep path shows the next",
        fs.list("docs").unwrap_or_default() == ["a/"],
    );
    r.check(
        "and the level below that",
        fs.list("docs/a").unwrap_or_default() == ["b/"],
    );
    r.check(
        "down to the file",
        fs.list("docs/a/b").unwrap_or_default() == ["c.txt"],
    );
    r.check(
        "a folder that does not exist lists as empty",
        fs.list("nowhere").map(|v| v.is_empty()) == Ok(true),
    );
    r.check(
        "a partial name is not a folder",
        fs.list("down").map(|v| v.is_empty()) == Ok(true),
    );
    r.check(
        "nested files read back",
        fs.read_file("downloads/deep/inner.bin").as_deref() == Ok(b"downloads/deep/inner.bin".as_slice()),
    );
    r.check(
        "a nested name is not also a root file",
        !fs.exists("cat.png"),
    );

    // Emptying a folder must make it vanish from its parent, since the folder
    // only ever existed as a prefix of its contents.
    expect(r, "remove the only file in a folder", fs.remove("downloads/deep/inner.bin"));
    r.check(
        "an emptied folder lists as empty",
        fs.list("downloads/deep").map(|v| v.is_empty()) == Ok(true),
    );
    r.check(
        "an emptied folder leaves its parent",
        fs.list("downloads").unwrap_or_default() == ["cat.png", "dog.png"],
    );
    r.check("listing an invalid folder fails", fs.list("/docs").is_err());
    r.check("a bare slash is not the root", fs.list("/").is_err());
    r.check("nor is a doubled one", fs.list("//").is_err());
    r.check("nor a doubled trailing one", fs.list("downloads//").is_err());
    r.check("a traversal cannot be listed", fs.list("..").is_err());
}

fn bad_paths(r: &mut Report) {
    let Some(mut fs) = fresh(r, "format for path checks", TEST_SECTORS) else {
        return;
    };

    for (name, path) in [
        ("empty path", ""),
        ("leading slash", "/etc/passwd"),
        ("trailing slash", "downloads/"),
        ("double slash", "a//b"),
        ("bare dot", "."),
        ("bare dot dot", ".."),
        ("dot component", "a/./b"),
        ("dot dot component", "a/../b"),
        ("trailing dot dot", "a/.."),
        ("leading dot dot", "../a"),
        ("embedded NUL", "a\0b"),
        ("only a slash", "/"),
    ] {
        r.check(name, fs.write_file(path, b"x").is_err());
    }

    let long = "x".repeat(MAX_PATH + 1);
    r.check("a 97-byte path is rejected", fs.write_file(&long, b"x").is_err());
    let limit = "y".repeat(MAX_PATH);
    r.check("a 96-byte path is accepted", fs.write_file(&limit, b"x").is_ok());
    r.check(
        "the 96-byte path reads back",
        fs.read_file(&limit).as_deref() == Ok(b"x".as_slice()),
    );
    r.check(
        "the 96-byte path survives a round trip through an entry",
        fs.list("").unwrap_or_default() == [limit.clone()],
    );
    r.check("rejected paths wrote nothing", fs.usage().0 == BLOCK_SIZE);
    r.check("reading an invalid path fails", fs.read_file("../x").is_err());
    r.check("a valid path with dots in a name is fine", fs.write_file("a.b.c", b"x").is_ok());
}

fn exhaustion(r: &mut Report) {
    let Some(mut fs) = fresh(r, "format a small disk", SMALL_SECTORS) else {
        return;
    };
    let (_, capacity) = fs.usage();
    r.check("the small disk has thirteen data blocks", capacity == 13 * BLOCK_SIZE);

    let full = vec![7u8; capacity];
    expect(r, "fill the disk exactly", fs.write_file("full.bin", &full));
    r.check("the disk is full", fs.usage().0 == capacity);

    r.check(
        "one more byte is refused",
        fs.write_file("more.bin", b"x") == Err("disk full"),
    );
    r.check("the refusal changed nothing", fs.usage().0 == capacity);
    r.check("the refused file was not created", !fs.exists("more.bin"));
    r.check(
        "the filesystem still works after a refusal",
        fs.read_file("full.bin").as_deref() == Ok(full.as_slice()),
    );

    expect(r, "empty the disk again", fs.remove("full.bin"));
    r.check("the disk is empty again", fs.usage().0 == 0);
    r.check(
        "a file one block too big is refused",
        fs.write_file("toobig.bin", &vec![0u8; capacity + 1]) == Err("disk full"),
    );
    r.check("the disk is still empty", fs.usage().0 == 0);
    expect(r, "and it can still be filled", fs.write_file("full.bin", &full));

    // Fragmentation is real and the format does not hide it: two runs that add
    // up are not one run that fits.
    let Some(mut fs) = fresh(r, "format for fragmentation", SMALL_SECTORS) else {
        return;
    };
    let one = vec![1u8; BLOCK_SIZE];
    let mut ok = true;
    for i in 0..13 {
        if fs.write_file(&alloc::format!("b{}", i), &one).is_err() {
            ok = false;
        }
    }
    r.check("thirteen one-block files fit", ok);
    expect(r, "free a block in the middle", fs.remove("b5"));
    expect(r, "free another, not adjacent", fs.remove("b9"));
    r.check(
        "two scattered blocks cannot hold a two-block file",
        fs.write_file("two.bin", &vec![0u8; BLOCK_SIZE + 1]) == Err("disk full"),
    );
    r.check(
        "but either hole takes a one-block file",
        fs.write_file("one.bin", &one).is_ok(),
    );

    // Empty files cost no data blocks, so they are the way to reach the other
    // limit: the directory table.
    let Some(mut fs) = fresh(r, "format for the directory limit", SMALL_SECTORS) else {
        return;
    };
    let mut filled = true;
    for i in 0..32 {
        if fs.write_file(&alloc::format!("e{}", i), &[]).is_err() {
            filled = false;
        }
    }
    r.check("thirty-two entries fit the directory", filled);
    r.check(
        "the thirty-third is refused",
        fs.write_file("e32", &[]) == Err("directory full"),
    );
    r.check(
        "an overwrite still works with a full directory",
        fs.write_file("e0", b"still writable").is_ok(),
    );
}

/// A directory full of nonsense must produce errors and empty listings, never a
/// panic — a kernel panic on a bad disk is a machine that will not boot.
fn corruption(r: &mut Report) {
    let Some(mut fs) = fresh(r, "format for directory corruption", TEST_SECTORS) else {
        return;
    };
    expect(r, "write a file to corrupt", fs.write_file("keep.txt", b"keep me"));
    let dir_lba = block_lba(fs.sb.dir_start());
    let mut disk = fs.into_device();

    let junk = [0xFFu8; SECTOR_SIZE];
    expect(r, "stamp junk over the directory", disk.write(dir_lba, &junk));
    let Some(fs) = expect(r, "a junk directory still mounts", DiskFs::mount(disk)) else {
        return;
    };
    r.check("a junk listing is empty, not an error", fs.list("") == Ok(Vec::new()));
    r.check("the junked file is simply gone", !fs.exists("keep.txt"));
    r.check("and reading it fails cleanly", fs.read_file("keep.txt").is_err());
    let mut disk = fs.into_device();

    // A slot that decodes cleanly but points outside the data area is the
    // dangerous case: the path is usable, so only the read can catch it.
    let mut sector = [0u8; SECTOR_SIZE];
    let entries = [
        DirEnt {
            used: true,
            path: String::from("far.bin"),
            first_block: 0xFFFF_FF00,
            length: BLOCK_SIZE as u32,
            seq: 0,
        },
        DirEnt {
            used: true,
            path: String::from("meta.bin"),
            first_block: 0,
            length: BLOCK_SIZE as u32,
            seq: 1,
        },
        DirEnt {
            used: true,
            path: String::from("../../etc/passwd"),
            first_block: 6,
            length: 1,
            seq: 2,
        },
        DirEnt {
            used: true,
            path: String::from("huge.bin"),
            first_block: 6,
            length: u32::MAX,
            seq: 3,
        },
    ];
    for (i, entry) in entries.iter().enumerate() {
        if let Some(slot) = sector
            .get_mut(i * DIR_ENTRY_SIZE..)
            .and_then(|rest| rest.get_mut(..DIR_ENTRY_SIZE))
        {
            entry.encode(slot);
        }
    }
    expect(r, "stamp doctored entries over the directory", disk.write(dir_lba, &sector));
    let Some(fs) = expect(r, "doctored entries still mount", DiskFs::mount(disk)) else {
        return;
    };
    r.check(
        "an entry pointing past the disk is a read error",
        fs.read_file("far.bin") == Err("corrupt directory entry"),
    );
    r.check(
        "an entry pointing at the metadata is a read error",
        fs.read_file("meta.bin") == Err("corrupt directory entry"),
    );
    r.check(
        "an entry claiming a length the disk cannot hold is a read error",
        fs.read_file("huge.bin") == Err("corrupt directory entry"),
    );
    r.check(
        "an entry with a traversal path is discarded on load",
        !fs.exists("../../etc/passwd"),
    );
    r.check(
        "the discarded entry is not in the listing either",
        fs.list("").map(|v| v.len()) == Ok(3),
    );
    r.check("the surviving names are still usable", fs.exists("far.bin"));
}

fn persistence(r: &mut Report) {
    // The whole point of the exercise: bytes written, the filesystem dropped,
    // the same disk mounted again — which is exactly what a reboot does to it.
    let Some(mut fs) = fresh(r, "format before the reboot", TEST_SECTORS) else {
        return;
    };

    let payload: Vec<u8> = (0..9_000u32).map(|i| (i % 199) as u8).collect();
    let mut written = true;
    for path in ["readme.txt", "downloads/cat.png", "docs/a/b/deep.txt"] {
        if fs.write_file(path, path.as_bytes()).is_err() {
            written = false;
        }
    }
    if fs.write_file("downloads/big.bin", &payload).is_err() {
        written = false;
    }
    r.check("write everything before the reboot", written);
    let before = fs.usage();
    let root_before = fs.list("").unwrap_or_default();

    let disk = fs.into_device();
    let Some(fs) = expect(r, "the volume mounts after the reboot", DiskFs::mount(disk)) else {
        return;
    };

    r.check("a small file survived", fs.read_file("readme.txt").as_deref() == Ok(b"readme.txt".as_slice()));
    r.check(
        "a nested file survived",
        fs.read_file("downloads/cat.png").as_deref() == Ok(b"downloads/cat.png".as_slice()),
    );
    r.check(
        "a deeply nested file survived",
        fs.read_file("docs/a/b/deep.txt").as_deref() == Ok(b"docs/a/b/deep.txt".as_slice()),
    );
    r.check(
        "a multi-block file survived intact",
        fs.read_file("downloads/big.bin").as_deref() == Ok(payload.as_slice()),
    );
    r.check("sizes survived", fs.size("downloads/big.bin") == Some(payload.len()));
    r.check("the free-space accounting survived", fs.usage() == before);
    r.check("the root listing survived", fs.list("") == Ok(root_before));
    r.check(
        "the tree survived, still in creation order",
        fs.list("downloads").unwrap_or_default() == ["cat.png", "big.bin"],
    );
    r.check("nothing extra appeared", fs.list("docs/a/b").unwrap_or_default() == ["deep.txt"]);

    // A second boot that writes more must not disturb the first boot's files,
    // and sequence numbers must carry on rather than restart.
    let disk = fs.into_device();
    let Some(mut fs) = expect(r, "mount_or_format keeps an existing volume", DiskFs::mount_or_format(disk)) else {
        return;
    };
    r.check("mount_or_format did not reformat", fs.exists("readme.txt"));
    expect(r, "write again after remounting", fs.write_file("later.txt", b"later"));

    let disk = fs.into_device();
    let Some(fs) = expect(r, "the volume mounts a third time", DiskFs::mount(disk)) else {
        return;
    };
    r.check("the new file survived", fs.read_file("later.txt").as_deref() == Ok(b"later".as_slice()));
    r.check("the old files are still there", fs.exists("readme.txt"));
    r.check(
        "the new file sorts after the old ones",
        fs.list("").ok().and_then(|v| v.last().cloned()) == Some(String::from("later.txt")),
    );
}
