//! Phase 10 — Block device + FAT32 (read-only) + VFS + embedded initramfs.
//! Writable, nested tree under `/data` (RAM-backed) — all mutations take the
//! global `VFS` mutex (same lock as `cmd_ls` / `cmd_cat`) so they are
//! serialised with other FS work on the same kernel thread / event loop.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

pub const SECTOR_SIZE: usize = 512;

/// Ceilings applied to every FAT walk. The on-disk structures are untrusted:
/// a cyclic cluster chain would otherwise hang the kernel, and a bogus file
/// size would abort it with an out-of-memory panic.
const MAX_CLUSTER_CHAIN: usize = 65_536;
const MAX_DIR_ENTRIES: usize = 4_096;
const MAX_FILE_SIZE: usize = 4 * 1024 * 1024;

pub trait BlockDevice {
    fn read_sector(&self, lba: u64, out: &mut [u8; SECTOR_SIZE]) -> Result<(), &'static str>;
}

pub struct MemBlockDevice {
    image: Vec<u8>,
}

impl MemBlockDevice {
    pub fn new(image: Vec<u8>) -> Self {
        Self { image }
    }
}

impl BlockDevice for MemBlockDevice {
    fn read_sector(&self, lba: u64, out: &mut [u8; SECTOR_SIZE]) -> Result<(), &'static str> {
        let off = lba as usize * SECTOR_SIZE;
        let end = off + SECTOR_SIZE;
        let src = self.image.get(off..end).ok_or("sector out of range")?;
        out.copy_from_slice(src);
        Ok(())
    }
}

#[derive(Clone)]
struct InitFile {
    path: &'static str,
    data: &'static [u8],
}

struct InitRamFs {
    files: Vec<InitFile>,
}

impl InitRamFs {
    fn new() -> Self {
        Self {
            files: Vec::from([
                InitFile {
                    path: "/init/welcome.txt",
                    data: b"OS101 initramfs online.\n",
                },
                InitFile {
                    path: "/init/motd.txt",
                    data: b"Phase 10 VFS mounted.\n",
                },
            ]),
        }
    }

    fn list_root(&self) -> Vec<String> {
        let mut out = Vec::new();
        for f in &self.files {
            out.push(f.path.to_string());
        }
        out
    }

    fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        for f in &self.files {
            if f.path == path {
                return Some(f.data.to_vec());
            }
        }
        None
    }
}

#[derive(Clone, Copy)]
struct DirEntry {
    attr: u8,
    first_cluster: u32,
    size: u32,
    name: [u8; 11],
}

impl DirEntry {
    fn is_end(&self) -> bool { self.name[0] == 0 }
    fn is_lfn(&self) -> bool { self.attr == 0x0F }
    fn is_dir(&self) -> bool { self.attr & 0x10 != 0 }
    fn is_file(&self) -> bool { self.attr & 0x20 != 0 }
}

struct Fat32<D: BlockDevice> {
    dev: D,
    bytes_per_sector: u16,
    sectors_per_cluster: u8,
    reserved_sectors: u16,
    fat_count: u8,
    fat_size_sectors: u32,
    root_cluster: u32,
    first_data_sector: u32,
}

impl<D: BlockDevice> Fat32<D> {
    fn new(dev: D) -> Result<Self, &'static str> {
        let mut b = [0u8; SECTOR_SIZE];
        dev.read_sector(0, &mut b)?;
        if b[510] != 0x55 || b[511] != 0xAA {
            return Err("invalid boot signature");
        }
        let bytes_per_sector = u16::from_le_bytes([b[11], b[12]]);
        let sectors_per_cluster = b[13];
        let reserved_sectors = u16::from_le_bytes([b[14], b[15]]);
        let fat_count = b[16];
        let fat_size_16 = u16::from_le_bytes([b[22], b[23]]) as u32;
        let fat_size_32 = u32::from_le_bytes([b[36], b[37], b[38], b[39]]);
        let fat_size_sectors = if fat_size_16 != 0 { fat_size_16 } else { fat_size_32 };
        let root_cluster = u32::from_le_bytes([b[44], b[45], b[46], b[47]]);
        if bytes_per_sector as usize != SECTOR_SIZE || sectors_per_cluster == 0 {
            return Err("unsupported BPB geometry");
        }
        let first_data_sector = reserved_sectors as u32 + fat_count as u32 * fat_size_sectors;
        Ok(Self {
            dev,
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sectors,
            fat_count,
            fat_size_sectors,
            root_cluster,
            first_data_sector,
        })
    }

    fn cluster_to_lba(&self, cluster: u32) -> u64 {
        let cluster_index = cluster.saturating_sub(2);
        (self.first_data_sector + cluster_index * self.sectors_per_cluster as u32) as u64
    }

    fn cluster_size(&self) -> usize {
        self.sectors_per_cluster as usize * SECTOR_SIZE
    }

    /// Read every sector of `cluster` into `out`, replacing its contents.
    ///
    /// The previous version read only the first sector regardless of
    /// `sectors_per_cluster`, silently truncating file and directory data on
    /// any image whose clusters span more than one sector.
    fn read_cluster(&self, cluster: u32, out: &mut Vec<u8>) -> Result<(), &'static str> {
        if cluster < 2 {
            return Err("invalid cluster number");
        }
        out.clear();
        let mut sec = [0u8; SECTOR_SIZE];
        let base = self.cluster_to_lba(cluster);
        for i in 0..self.sectors_per_cluster as u64 {
            self.dev.read_sector(base + i, &mut sec)?;
            out.extend_from_slice(&sec);
        }
        Ok(())
    }

    /// Follow the FAT to the next cluster.
    ///
    /// Returns `Ok(None)` at end-of-chain. A corrupt or hostile FAT can point
    /// a cluster back at itself, so every caller pairs this with a step
    /// budget — without one the walk spins forever with the GUI frozen.
    fn next_cluster(&self, cluster: u32) -> Result<Option<u32>, &'static str> {
        let next = self.read_fat_entry(cluster)?;
        // Reserved (0/1), bad (0x0FFFFFF7) and EOC (>= 0x0FFFFFF8) all end it.
        if next < 2 || next >= 0x0FFF_FFF7 {
            return Ok(None);
        }
        Ok(Some(next))
    }

    fn read_fat_entry(&self, cluster: u32) -> Result<u32, &'static str> {
        let fat_offset = cluster as usize * 4;
        let fat_sector = self.reserved_sectors as u64 + (fat_offset / SECTOR_SIZE) as u64;
        let ent_off = fat_offset % SECTOR_SIZE;
        let mut sec = [0u8; SECTOR_SIZE];
        self.dev.read_sector(fat_sector, &mut sec)?;
        let val = u32::from_le_bytes([
            sec[ent_off],
            sec[ent_off + 1],
            sec[ent_off + 2],
            sec[ent_off + 3],
        ]);
        Ok(val & 0x0FFF_FFFF)
    }

    fn read_root_entries(&self) -> Result<Vec<DirEntry>, &'static str> {
        let mut entries = Vec::new();
        let mut cluster = self.root_cluster;
        let mut buf: Vec<u8> = Vec::with_capacity(self.cluster_size());
        let mut steps = 0usize;
        loop {
            if steps >= MAX_CLUSTER_CHAIN {
                return Err("FAT directory chain too long (corrupt or cyclic)");
            }
            steps += 1;
            if entries.len() >= MAX_DIR_ENTRIES {
                return Err("FAT directory has too many entries");
            }
            self.read_cluster(cluster, &mut buf)?;
            let mut i = 0usize;
            while i + 32 <= buf.len() {
                let mut name = [0u8; 11];
                name.copy_from_slice(&buf[i..i + 11]);
                let attr = buf[i + 11];
                let first_hi = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as u32;
                let first_lo = u16::from_le_bytes([buf[i + 26], buf[i + 27]]) as u32;
                let size = u32::from_le_bytes([buf[i + 28], buf[i + 29], buf[i + 30], buf[i + 31]]);
                let ent = DirEntry {
                    attr,
                    first_cluster: (first_hi << 16) | first_lo,
                    size,
                    name,
                };
                if ent.is_end() {
                    return Ok(entries);
                }
                if !ent.is_lfn() && ent.name[0] != 0xE5 {
                    if entries.len() >= MAX_DIR_ENTRIES {
                        return Err("FAT directory has too many entries");
                    }
                    entries.push(ent);
                }
                i += 32;
            }
            match self.next_cluster(cluster)? {
                Some(next) => cluster = next,
                None => break,
            }
        }
        Ok(entries)
    }

    fn name_83_to_string(name: &[u8; 11]) -> String {
        let base = &name[0..8];
        let ext = &name[8..11];
        let mut b = String::new();
        for &c in base {
            if c != b' ' { b.push((c as char).to_ascii_lowercase()); }
        }
        let mut e = String::new();
        for &c in ext {
            if c != b' ' { e.push((c as char).to_ascii_lowercase()); }
        }
        if e.is_empty() { b } else { alloc::format!("{}.{}", b, e) }
    }

    fn read_file_by_name(&self, wanted: &str) -> Result<Vec<u8>, &'static str> {
        let entries = self.read_root_entries()?;
        let wanted = wanted.trim_start_matches('/').trim_start_matches("fat/").to_ascii_lowercase();
        let mut target: Option<DirEntry> = None;
        for e in entries {
            if !e.is_file() { continue; }
            let n = Self::name_83_to_string(&e.name);
            if n == wanted {
                target = Some(e);
                break;
            }
        }
        let e = target.ok_or("file not found")?;
        let size = e.size as usize;
        if size > MAX_FILE_SIZE {
            return Err("FAT file too large");
        }
        // Grow on demand rather than trusting the directory entry up front:
        // `size` is attacker-controlled, and reserving it outright is a free
        // out-of-memory abort.
        let mut out: Vec<u8> = Vec::new();
        let mut cluster = e.first_cluster;
        let mut buf: Vec<u8> = Vec::with_capacity(self.cluster_size());
        let mut steps = 0usize;
        while cluster >= 2 && out.len() < size {
            if steps >= MAX_CLUSTER_CHAIN {
                return Err("FAT file chain too long (corrupt or cyclic)");
            }
            steps += 1;
            self.read_cluster(cluster, &mut buf)?;
            let remain = size - out.len();
            let n = remain.min(buf.len());
            out.extend_from_slice(&buf[..n]);
            match self.next_cluster(cluster)? {
                Some(next) => cluster = next,
                None => break,
            }
        }
        out.truncate(size);
        Ok(out)
    }

    fn list_root_files(&self) -> Result<Vec<String>, &'static str> {
        let mut out = Vec::new();
        for e in self.read_root_entries()? {
            if e.is_file() {
                out.push(alloc::format!("/fat/{}", Self::name_83_to_string(&e.name)));
            } else if e.is_dir() {
                out.push(alloc::format!("/fat/{}/", Self::name_83_to_string(&e.name)));
            }
        }
        Ok(out)
    }

    fn _debug_geometry(&self) -> (u16, u8, u16, u8, u32) {
        (
            self.bytes_per_sector,
            self.sectors_per_cluster,
            self.reserved_sectors,
            self.fat_count,
            self.fat_size_sectors,
        )
    }
}

/// In-memory R/W files and directories.
///
/// Mounted at the roots in [`WRITABLE_ROOTS`]: `/data` for user files and
/// `/apps` for installed packages.
#[derive(Clone)]
struct RamLayer {
    dirs: BTreeSet<String>,
    files: BTreeMap<String, Vec<u8>>,
}

/// Paths the RAM layer owns. Everything else is read-only.
const WRITABLE_ROOTS: [&str; 2] = ["/data", "/apps"];

/// Ceiling on the RAM layer, so writing files cannot consume the whole heap.
const MAX_RAM_BYTES: usize = 6 * 1024 * 1024;

impl RamLayer {
    fn new() -> Self {
        let mut dirs = BTreeSet::new();
        for root in WRITABLE_ROOTS {
            dirs.insert(String::from(root));
        }
        Self {
            dirs,
            files: BTreeMap::new(),
        }
    }

    /// The writable root containing `path`, if any.
    fn root_of(path: &str) -> Option<&'static str> {
        WRITABLE_ROOTS
            .into_iter()
            .find(|root| path == *root || path.starts_with(&alloc::format!("{}/", root)))
    }

    fn total_bytes(&self) -> usize {
        self.files.values().map(|v| v.len()).sum()
    }

    fn parent_of(path: &str) -> String {
        let p = path.trim_end_matches('/');
        if p.is_empty() {
            return String::from("/");
        }
        if let Some(i) = p.rfind('/') {
            if i == 0 {
                String::from("/")
            } else {
                p[..i].to_string()
            }
        } else {
            String::from("/")
        }
    }

    fn is_writable(path: &str) -> bool {
        Self::root_of(path).is_some()
    }

    fn mkdir_p(&mut self, path: &str) -> Result<(), &'static str> {
        let Some(root) = Self::root_of(path) else {
            return Err("path is read-only");
        };
        let p = path.trim_end_matches('/').to_string();
        if p.is_empty() || p == root {
            self.dirs.insert(String::from(root));
            return Ok(());
        }
        let prefix = alloc::format!("{}/", root);
        if !p.starts_with(&prefix) {
            return Err("invalid path");
        }
        self.dirs.insert(String::from(root));
        let mut acc = String::from(root);
        for part in p.trim_start_matches(&prefix).split('/').filter(|x| !x.is_empty()) {
            acc.push('/');
            acc.push_str(part);
            self.dirs.insert(acc.clone());
        }
        Ok(())
    }

    /// List direct children; names are VFS-style full paths (dirs end with `/`).
    fn ls(&self, parent: &str) -> Result<Vec<String>, &'static str> {
        let pnorm = parent.trim_end_matches('/').to_string();
        if pnorm.is_empty() {
            return Err("empty path");
        }
        if !self.dirs.contains(&pnorm) {
            return Err("not a directory");
        }
        let mut out: Vec<String> = Vec::new();
        for d in &self.dirs {
            if WRITABLE_ROOTS.contains(&d.as_str()) {
                continue;
            }
            if Self::parent_of(d) == pnorm {
                out.push(alloc::format!("{}/", d));
            }
        }
        for f in self.files.keys() {
            if Self::parent_of(f) == pnorm {
                out.push(f.clone());
            }
        }
        out.sort();
        Ok(out)
    }

    fn create_file(&mut self, path: &str) -> Result<(), &'static str> {
        if !Self::is_writable(path) {
            return Err("path is read-only");
        }
        if path.ends_with('/') {
            return Err("not a file path");
        }
        let parent = Self::parent_of(path);
        if !self.dirs.contains(&parent) {
            self.mkdir_p(&parent)?;
        }
        let pkey = path.trim_end_matches('/').to_string();
        if self.dirs.contains(&pkey) {
            return Err("is a directory");
        }
        self.files.insert(path.to_string(), Vec::new());
        Ok(())
    }

    fn write_file(&mut self, path: &str, data: Vec<u8>) -> Result<(), &'static str> {
        if !Self::is_writable(path) {
            return Err("path is read-only");
        }
        if path.ends_with('/') {
            return Err("not a file path");
        }
        // Charge the write against the RAM budget, discounting whatever the
        // file already occupies (an overwrite is not a new allocation).
        let existing = self.files.get(path).map(|v| v.len()).unwrap_or(0);
        let after = self
            .total_bytes()
            .saturating_sub(existing)
            .saturating_add(data.len());
        if after > MAX_RAM_BYTES {
            return Err("filesystem full");
        }
        if !self.files.contains_key(path) {
            self.create_file(path)?;
        }
        *self
            .files
            .get_mut(path)
            .ok_or("write failed")? = data;
        Ok(())
    }

    fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        self.files.get(path).cloned()
    }

    fn remove(&mut self, path: &str) -> Result<(), &'static str> {
        if !Self::is_writable(path) {
            return Err("path is read-only");
        }
        let p = path.trim_end_matches('/').to_string();
        if WRITABLE_ROOTS.contains(&p.as_str()) {
            return Err("cannot remove a mount root");
        }
        if self.files.remove(path).is_some() {
            return Ok(());
        }
        if !self.dirs.contains(&p) {
            return Err("not found");
        }
        for d in &self.dirs {
            if d != &p && d.starts_with(&alloc::format!("{}/", p)) {
                return Err("directory not empty");
            }
        }
        for f in self.files.keys() {
            if Self::parent_of(f) == p {
                return Err("directory not empty");
            }
        }
        self.dirs.remove(&p);
        Ok(())
    }
}

/// Where the persistent disk is mounted. Everything under here is on the
/// second drive and survives a reboot; everything else is rebuilt at boot.
pub const DISK_ROOT: &str = "/disk";

/// Strip the mount point off a VFS path to get a path the disk understands.
/// `/disk/downloads/cat.jpg` → `downloads/cat.jpg`, `/disk` → ``.
fn disk_relative(path: &str) -> Option<&str> {
    let rest = path.strip_prefix(DISK_ROOT)?;
    match rest.chars().next() {
        None => Some(""),
        Some('/') => Some(rest.trim_start_matches('/').trim_end_matches('/')),
        // `/diskette` is not on the disk.
        Some(_) => None,
    }
}

/// Where a USB flash drive is mounted, if one is attached and its
/// filesystem is recognised.
pub const USB_ROOT: &str = "/usb";

/// Strip the mount point off a VFS path to get a path the USB FAT32 driver
/// understands. `/usb/photos/cat.jpg` → `photos/cat.jpg`, `/usb` → ``.
fn usb_relative(path: &str) -> Option<&str> {
    let rest = path.strip_prefix(USB_ROOT)?;
    match rest.chars().next() {
        None => Some(""),
        Some('/') => Some(rest.trim_start_matches('/').trim_end_matches('/')),
        Some(_) => None,
    }
}

struct Vfs {
    initramfs: InitRamFs,
    fat: Fat32<MemBlockDevice>,
    data: RamLayer,
    /// The persistent disk, if one is attached and readable.
    disk: Option<crate::diskfs::DiskFs<crate::diskfs::AtaDisk>>,
    /// A USB flash drive, if one is attached and carries a FAT32 volume this
    /// driver recognises. Never auto-formatted — an unrecognised or missing
    /// drive just means this stays `None`, unlike `disk` above.
    usb: Option<crate::fat32::Fat32Fs<crate::usb::UsbDisk>>,
}

impl Vfs {
    fn new() -> Result<Self, &'static str> {
        let initramfs = InitRamFs::new();
        let fat_img = build_demo_fat32_image();
        let fat = Fat32::new(MemBlockDevice::new(fat_img))?;
        Ok(Self {
            initramfs,
            fat,
            data: RamLayer::new(),
            disk: None,
            usb: None,
        })
    }

    fn ls(&self, path: &str) -> Result<Vec<String>, &'static str> {
        if let Some(relative) = disk_relative(path) {
            let disk = self.disk.as_ref().ok_or("no data disk attached")?;
            // The disk names entries relative to their folder; the rest of the
            // VFS deals in absolute paths, so put the mount point back on.
            return Ok(disk
                .list(relative)?
                .into_iter()
                .map(|name| {
                    if relative.is_empty() {
                        alloc::format!("{}/{}", DISK_ROOT, name)
                    } else {
                        alloc::format!("{}/{}/{}", DISK_ROOT, relative, name)
                    }
                })
                .collect());
        }
        if let Some(relative) = usb_relative(path) {
            let usb = self.usb.as_ref().ok_or("no USB drive attached")?;
            return Ok(usb
                .list(relative)?
                .into_iter()
                .map(|(name, is_dir, _size)| {
                    let name = if is_dir { alloc::format!("{}/", name) } else { name };
                    if relative.is_empty() {
                        alloc::format!("{}/{}", USB_ROOT, name)
                    } else {
                        alloc::format!("{}/{}/{}", USB_ROOT, relative, name)
                    }
                })
                .collect());
        }
        match path {
            "/" | "" => {
                let mut out = vec![
                    "/fat/".to_string(),
                    "/data/".to_string(),
                    "/apps/".to_string(),
                ];
                if self.disk.is_some() {
                    out.push(alloc::format!("{}/", DISK_ROOT));
                }
                if self.usb.is_some() {
                    out.push(alloc::format!("{}/", USB_ROOT));
                }
                out.extend(self.initramfs.list_root());
                Ok(out)
            }
            "/fat" | "/fat/" => self.fat.list_root_files(),
            "/init" | "/init/" => {
                let mut out = Vec::new();
                for p in self.initramfs.list_root() {
                    if p.starts_with("/init/") {
                        out.push(p);
                    }
                }
                Ok(out)
            }
            p => {
                let n = p.trim_end_matches('/');
                if RamLayer::root_of(n).is_some() {
                    self.data.ls(n)
                } else {
                    Err("unsupported path")
                }
            }
        }
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, &'static str> {
        if let Some(relative) = disk_relative(path) {
            let disk = self.disk.as_ref().ok_or("no data disk attached")?;
            return disk.read_file(relative);
        }
        if let Some(relative) = usb_relative(path) {
            let usb = self.usb.as_ref().ok_or("no USB drive attached")?;
            return usb.read_file(relative);
        }
        if let Some(v) = self.data.read_file(path) {
            return Ok(v);
        }
        if let Some(v) = self.initramfs.read_file(path) {
            return Ok(v);
        }
        if path.starts_with("/fat/") {
            return self.fat.read_file_by_name(path);
        }
        if !path.starts_with('/') {
            return self.fat.read_file_by_name(path);
        }
        Err("file not found")
    }

    fn write_file(&mut self, path: &str, data: Vec<u8>) -> Result<(), &'static str> {
        if let Some(relative) = disk_relative(path) {
            let disk = self.disk.as_mut().ok_or("no data disk attached")?;
            return disk.write_file(relative, &data);
        }
        if let Some(relative) = usb_relative(path) {
            let usb = self.usb.as_mut().ok_or("no USB drive attached")?;
            return usb.write_file(relative, &data);
        }
        self.data.write_file(path, data)
    }

    fn remove(&mut self, path: &str) -> Result<(), &'static str> {
        if let Some(relative) = disk_relative(path) {
            let disk = self.disk.as_mut().ok_or("no data disk attached")?;
            return disk.remove(relative);
        }
        if let Some(relative) = usb_relative(path) {
            let usb = self.usb.as_mut().ok_or("no USB drive attached")?;
            return usb.remove(relative);
        }
        self.data.remove(path)
    }
}

static VFS: Mutex<Option<Vfs>> = Mutex::new(None);

pub fn init() {
    match Vfs::new() {
        Ok(vfs) => {
            {
                let mut lock = VFS.lock();
                *lock = Some(vfs);
            }
            crate::ok_line("VFS + FAT32 (read-only) online");
            self_test();
        }
        Err(e) => {
            crate::warn_line(&alloc::format!("VFS init failed: {}", e));
        }
    }
}

pub fn self_test() {
    let mut ok = true;
    match cmd_ls(Some("/fat")) {
        Ok(v) => {
            let has_readme = v.iter().any(|p| p.ends_with("/readme.txt"));
            let has_elf = v.iter().any(|p| p.ends_with("/hello.elf"));
            if !has_readme || !has_elf {
                ok = false;
            }
        }
        Err(_) => ok = false,
    }
    match cmd_cat("/fat/readme.txt") {
        Ok(v) => {
            if !v.starts_with(b"OS101 FAT32 demo image.") {
                ok = false;
            }
        }
        Err(_) => ok = false,
    }
    match cmd_cat("/fat/hello.elf") {
        Ok(v) => {
            if v.len() < 4 || &v[0..4] != b"\x7FELF" {
                ok = false;
            }
        }
        Err(_) => ok = false,
    }
    if ok {
        crate::ok_line("FS self-test passed (ls/cat/run assets)");
    } else {
        crate::warn_line("FS self-test failed");
    }
}

pub fn cmd_ls(path: Option<&str>) -> Result<Vec<String>, &'static str> {
    let p = path.unwrap_or("/");
    let p = if p.is_empty() { "/" } else { p };
    let v = VFS.lock();
    let vfs = v.as_ref().ok_or("vfs not initialized")?;
    vfs.ls(p)
}

/// Parent path segment for navigation (`/data/a` → `/data`).
pub fn path_parent(path: &str) -> String {
    RamLayer::parent_of(path)
}

/// Create a directory tree under `/data/...`, `/disk/...` or `/usb/...`.
pub fn cmd_mkdir(path: &str) -> Result<(), &'static str> {
    let mut v = VFS.lock();
    let vfs = v.as_mut().ok_or("vfs not initialized")?;
    if let Some(relative) = disk_relative(path) {
        // The disk has no directory entries: a folder exists exactly as long
        // as something inside it does. An empty one therefore needs a marker
        // to hold it open, which is what `.keep` files have always been for.
        let disk = vfs.disk.as_mut().ok_or("no data disk attached")?;
        if relative.is_empty() {
            return Ok(());
        }
        return disk.write_file(&alloc::format!("{}/.keep", relative), b"");
    }
    if let Some(relative) = usb_relative(path) {
        // FAT32 has real directory entries, so an empty folder needs no
        // marker file the way the disk's O1FS does.
        let usb = vfs.usb.as_mut().ok_or("no USB drive attached")?;
        if relative.is_empty() {
            return Ok(());
        }
        return usb.mkdir(relative);
    }
    vfs.data.mkdir_p(path)
}

/// Create an empty file under a writable root (`/data`, `/apps`, `/disk`).
pub fn cmd_create_file(path: &str) -> Result<(), &'static str> {
    let mut v = VFS.lock();
    let vfs = v.as_mut().ok_or("vfs not initialized")?;
    if disk_relative(path).is_some() {
        return vfs.write_file(path, Vec::new());
    }
    vfs.data.create_file(path)
}

/// Write (creating or replacing) a file under a writable root.
pub fn cmd_write_file(path: &str, data: Vec<u8>) -> Result<(), &'static str> {
    let mut v = VFS.lock();
    let vfs = v.as_mut().ok_or("vfs not initialized")?;
    vfs.write_file(path, data)
}

/// Remove a file or empty directory under `/data/...` or `/disk/...`.
pub fn cmd_remove(path: &str) -> Result<(), &'static str> {
    let mut v = VFS.lock();
    let vfs = v.as_mut().ok_or("vfs not initialized")?;
    vfs.remove(path)
}

/// Can this path be written to?
pub fn is_writable(path: &str) -> bool {
    if disk_relative(path).is_some() {
        return has_disk();
    }
    if usb_relative(path).is_some() {
        return has_usb();
    }
    RamLayer::is_writable(path)
}

/// Is the persistent disk mounted?
pub fn has_disk() -> bool {
    VFS.lock().as_ref().is_some_and(|v| v.disk.is_some())
}

/// Bytes used and bytes available on the persistent disk.
pub fn disk_usage() -> Option<(usize, usize)> {
    VFS.lock()
        .as_ref()
        .and_then(|v| v.disk.as_ref())
        .map(|d| d.usage())
}

/// Is a USB drive mounted with a filesystem this OS understands?
pub fn has_usb() -> bool {
    VFS.lock().as_ref().is_some_and(|v| v.usb.is_some())
}

/// Bytes used and bytes available on the USB drive.
pub fn usb_usage() -> Option<(usize, usize)> {
    VFS.lock()
        .as_ref()
        .and_then(|v| v.usb.as_ref())
        .map(|u| u.usage())
}

/// Attach the second drive, formatting it if it has never been used.
///
/// Called after the VFS is up, because a missing or blank disk is not a boot
/// failure — the rest of the system works, it just cannot remember anything.
pub fn mount_disk() {
    let Some(device) = crate::diskfs::AtaDisk::open(crate::ata::Drive::Slave) else {
        crate::warn_line("No data disk attached — nothing will be kept across a reboot");
        return;
    };
    match crate::diskfs::DiskFs::mount_or_format(device) {
        Ok(disk) => {
            let (used, total) = disk.usage();
            if let Some(vfs) = VFS.lock().as_mut() {
                vfs.disk = Some(disk);
            }
            crate::ok_line(&alloc::format!(
                "Data disk mounted at {} — {} KiB used of {} KiB",
                DISK_ROOT,
                used / 1024,
                total / 1024,
            ));
        }
        Err(e) => crate::warn_line(&alloc::format!("Data disk unusable: {}", e)),
    }
}

/// Try to mount a USB flash drive at `/usb`.
///
/// Unlike [`mount_disk`], an unrecognised volume is never formatted — this
/// is very likely someone's real USB stick, with real files on it, and a
/// filesystem driver has no business erasing that just because it does not
/// speak the format. It is simply left unmounted; `/usb` stays unavailable
/// until a FAT32 drive (or a freshly FAT32-formatted one) is inserted.
pub fn mount_usb() {
    let Some(device) = crate::usb::UsbDisk::open() else {
        crate::warn_line("USB: mass storage device did not answer — not mounting /usb");
        return;
    };
    match crate::fat32::Fat32Fs::mount(device) {
        Ok(usb) => {
            let (used, total) = usb.usage();
            if let Some(vfs) = VFS.lock().as_mut() {
                vfs.usb = Some(usb);
            }
            crate::ok_line(&alloc::format!(
                "USB drive mounted at {} — {} KiB used of {} KiB",
                USB_ROOT,
                used / 1024,
                total / 1024,
            ));
        }
        Err(e) => crate::warn_line(&alloc::format!(
            "USB drive attached but not mounted ({}) — only FAT32 volumes are supported, and it is never reformatted automatically",
            e
        )),
    }
}

/// Drop the `/disk` mount so the ATA slave can be overwritten as a raw
/// install target. Safe when nothing is mounted.
pub fn unmount_disk() {
    if let Some(vfs) = VFS.lock().as_mut() {
        if vfs.disk.take().is_some() {
            crate::warn_line("Data disk: /disk unmounted for raw disk access");
        }
    }
}

/// Drop the `/usb` mount so the stick can be used as a raw block device
/// (the installer writes a whole disk image onto it). Safe to call when
/// nothing is mounted.
pub fn unmount_usb() {
    if let Some(vfs) = VFS.lock().as_mut() {
        if vfs.usb.take().is_some() {
            crate::warn_line("USB: /usb unmounted for raw disk access");
        }
    }
}

/// Called every main-loop tick (same cadence as `usb::poll`). Cheap: it only
/// does real work the moment a mass-storage device newly appears.
pub fn usb_tick() {
    if crate::usb::take_new_msc() {
        mount_usb();
    }
}

/// Save a download, choosing a name that does not collide with an existing
/// file. Returns the full path it was written to.
///
/// `dir` is a VFS path such as `/disk/downloads`; it is created implicitly by
/// writing into it, since the disk derives its folders from the paths stored
/// in it.
pub fn save_download(dir: &str, name: &str, data: &[u8]) -> Result<String, &'static str> {
    let dir = dir.trim_end_matches('/');
    if !is_writable(dir) {
        return Err("that folder cannot be written to");
    }
    let existing = cmd_ls(Some(dir)).unwrap_or_default();
    let taken = |candidate: &str| {
        let full = alloc::format!("{}/{}", dir, candidate);
        existing.iter().any(|e| e.trim_end_matches('/') == full)
    };

    let (stem, extension) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let mut chosen = String::from(name);
    for n in 2..1000u32 {
        if !taken(&chosen) {
            break;
        }
        chosen = alloc::format!("{}-{}{}", stem, n, extension);
    }

    let path = alloc::format!("{}/{}", dir, chosen);
    cmd_write_file(&path, data.to_vec())?;
    Ok(path)
}

/// Where the desktop wallpaper choice is written down.
const WALLPAPER_NOTE: &str = "/disk/system/wallpaper.txt";

/// Remember which picture the user picked, so the next boot can restore it.
pub fn remember_wallpaper(path: &str) -> Result<(), &'static str> {
    cmd_write_file(WALLPAPER_NOTE, path.as_bytes().to_vec())
}

/// Forget it again.
pub fn forget_wallpaper() {
    let _ = cmd_remove(WALLPAPER_NOTE);
}

/// The picture the user last chose, if the note is still there.
pub fn remembered_wallpaper() -> Option<String> {
    let bytes = cmd_cat(WALLPAPER_NOTE).ok()?;
    let path = core::str::from_utf8(&bytes).ok()?.trim();
    if path.is_empty() {
        None
    } else {
        Some(String::from(path))
    }
}

pub fn cmd_cat(path: &str) -> Result<Vec<u8>, &'static str> {
    let v = VFS.lock();
    let vfs = v.as_ref().ok_or("vfs not initialized")?;
    vfs.read_file(path)
}

fn write_dirent(buf: &mut [u8], name83: &[u8; 11], attr: u8, first_cluster: u32, size: u32) {
    buf[0..11].copy_from_slice(name83);
    buf[11] = attr;
    let hi = ((first_cluster >> 16) as u16).to_le_bytes();
    let lo = (first_cluster as u16).to_le_bytes();
    buf[20] = hi[0];
    buf[21] = hi[1];
    buf[26] = lo[0];
    buf[27] = lo[1];
    let sz = size.to_le_bytes();
    buf[28..32].copy_from_slice(&sz);
}

fn build_demo_fat32_image() -> Vec<u8> {
    let mut hello_elf = crate::process::demo_elf_bytes();
    if hello_elf.len() > SECTOR_SIZE {
        hello_elf.truncate(SECTOR_SIZE);
    }
    let readme = b"OS101 FAT32 demo image.\nUse: ls /fat , cat /fat/readme.txt , run /fat/hello.elf\nInstall the demo app with: pkg install /fat/demo.opk\n";

    // A ready-to-install package, so `pkg install` can be exercised on a
    // fresh boot without any host tooling. Built from the same bundled ELF
    // the launcher already ships.
    let demo_pkg = build_demo_package();

    // Lay the image out dynamically: the demo package is larger than one
    // sector, so file positions can no longer be hard-coded.
    let sectors_needed = |len: usize| (len + SECTOR_SIZE - 1) / SECTOR_SIZE;
    let readme_clusters = sectors_needed(readme.len()).max(1);
    let hello_clusters = sectors_needed(hello_elf.len()).max(1);
    let pkg_clusters = sectors_needed(demo_pkg.len()).max(1);

    // Cluster 2 = root dir, then each file's chain in order.
    let readme_start = 3u32;
    let hello_start = readme_start + readme_clusters as u32;
    let pkg_start = hello_start + hello_clusters as u32;
    let first_free = pkg_start + pkg_clusters as u32;

    let total_sectors = (first_free as usize + 8).max(128);
    let mut img = vec![0u8; total_sectors * SECTOR_SIZE];

    // BPB/Boot sector
    img[0] = 0xEB;
    img[1] = 0x58;
    img[2] = 0x90;
    img[3..11].copy_from_slice(b"OS101FAT");
    img[11..13].copy_from_slice(&(SECTOR_SIZE as u16).to_le_bytes());
    img[13] = 1; // sectors/cluster
    img[14..16].copy_from_slice(&(1u16).to_le_bytes()); // reserved
    img[16] = 1; // num_fats
    img[21] = 0xF8; // media
    img[32..36].copy_from_slice(&(total_sectors as u32).to_le_bytes());
    img[36..40].copy_from_slice(&(1u32).to_le_bytes()); // fat size sectors
    img[44..48].copy_from_slice(&(2u32).to_le_bytes()); // root cluster
    img[510] = 0x55;
    img[511] = 0xAA;

    // FAT at LBA1
    let fat = SECTOR_SIZE;
    let wfat = |entry: usize, val: u32, img: &mut Vec<u8>| {
        let off = fat + entry * 4;
        img[off..off + 4].copy_from_slice(&val.to_le_bytes());
    };
    wfat(0, 0x0FFF_FFF8, &mut img);
    wfat(1, 0xFFFF_FFFF, &mut img);
    wfat(2, 0x0FFF_FFFF, &mut img); // root dir cluster

    // Chain each file's clusters, terminating the last one.
    let chain = |start: u32, count: usize, img: &mut Vec<u8>| {
        for i in 0..count {
            let c = start + i as u32;
            let next = if i + 1 == count { 0x0FFF_FFFF } else { c + 1 };
            wfat(c as usize, next, img);
        }
    };
    chain(readme_start, readme_clusters, &mut img);
    chain(hello_start, hello_clusters, &mut img);
    chain(pkg_start, pkg_clusters, &mut img);

    // Root directory entries (cluster 2 lives at LBA 2 with 1 sector/cluster).
    let root = 2 * SECTOR_SIZE;
    let mut e = [0u8; 32];
    write_dirent(&mut e, b"README  TXT", 0x20, readme_start, readme.len() as u32);
    img[root..root + 32].copy_from_slice(&e);
    let mut e = [0u8; 32];
    write_dirent(&mut e, b"HELLO   ELF", 0x20, hello_start, hello_elf.len() as u32);
    img[root + 32..root + 64].copy_from_slice(&e);
    let mut e = [0u8; 32];
    write_dirent(&mut e, b"DEMO    OPK", 0x20, pkg_start, demo_pkg.len() as u32);
    img[root + 64..root + 96].copy_from_slice(&e);
    img[root + 96] = 0; // end marker

    let place = |start: u32, data: &[u8], img: &mut Vec<u8>| {
        let off = start as usize * SECTOR_SIZE;
        img[off..off + data.len()].copy_from_slice(data);
    };
    place(readme_start, readme, &mut img);
    place(hello_start, &hello_elf, &mut img);
    place(pkg_start, &demo_pkg, &mut img);
    img
}

/// Wrap a bundled ELF in a `.opk` so the demo image ships something
/// installable. Falls back to an empty vector if no ELF app is bundled.
fn build_demo_package() -> Vec<u8> {
    let payload = crate::app_registry::APPS.iter().find_map(|app| match app.kind {
        crate::app_registry::AppKind::Elf(bytes) => Some(bytes),
        _ => None,
    });
    let Some(payload) = payload else {
        return Vec::new();
    };
    let manifest = "\
name = Demo App
version = 1.0.0
description = Installed from /fat/demo.opk
icon = app
";
    crate::package::build(manifest, payload)
}
