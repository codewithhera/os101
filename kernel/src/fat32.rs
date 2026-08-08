//! A real, writable FAT32 driver — subdirectories, long file name (LFN)
//! reads, and enough of the write path (create, overwrite, delete, mkdir) to
//! use an ordinary FAT32-formatted USB drive as a normal filesystem, without
//! ever silently reformatting it.
//!
//! Deliberately out of scope: FAT12/16, exFAT, and writing LFN entries —
//! files this OS creates get a valid, collision-free 8.3 short name only, so
//! they may look terse (`MYFILE~1.TXT`) from Windows or macOS, but they are
//! always valid FAT32 and never corrupt the volume. Names already on the
//! drive with a long-name entry are read (and displayed) in full.
//!
//! Every walk over untrusted, on-disk structures (cluster chains, directory
//! entries) is bounded, the same discipline the read-only demo FAT32 in
//! `fs.rs` and the O1FS driver in `diskfs.rs` already use: a corrupt or
//! hostile volume must come back as an error, never a hang or a panic.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::diskfs::Sectors;

const SECTOR_SIZE: usize = 512;
const MAX_CLUSTER_CHAIN: usize = 262_144;
const MAX_DIR_ENTRIES: usize = 65_536;
const MAX_FILE_SIZE: usize = 512 * 1024 * 1024;
const FAT_FREE: u32 = 0;
const FAT_EOC: u32 = 0x0FFF_FFFF;

/// One resolved directory entry (long name if present, else derived from the
/// 8.3 short name), plus enough about its on-disk position to patch or
/// delete it in place.
#[derive(Clone)]
struct DirEntry {
    name: String,
    attr: u8,
    first_cluster: u32,
    size: u32,
    short_name: [u8; 11],
    /// Byte offset of the short-name record within the parent directory's
    /// concatenated cluster buffer.
    buf_offset: usize,
    /// Byte offset of the first LFN record belonging to this entry (equal to
    /// `buf_offset` if there is none). Together with `buf_offset + 32` this
    /// spans every record that must be deleted or relocated with the entry.
    record_start: usize,
}

impl DirEntry {
    fn is_dir(&self) -> bool {
        self.attr & 0x10 != 0
    }
}

pub struct Fat32Fs<D: Sectors> {
    dev: D,
    sectors_per_cluster: u8,
    reserved_sectors: u16,
    fat_count: u8,
    fat_size_sectors: u32,
    root_cluster: u32,
    first_data_sector: u32,
    total_clusters: u32,
    alloc_cursor: u32,
}

impl<D: Sectors> Fat32Fs<D> {
    /// Mount an existing FAT32 volume. Returns an error — never formats —
    /// for anything else, including FAT12/16: this is meant to sit on a
    /// user's real USB drive, and a filesystem driver that reformats what it
    /// cannot parse is a data-loss bug waiting to happen.
    pub fn mount(dev: D) -> Result<Self, &'static str> {
        let mut b = [0u8; SECTOR_SIZE];
        dev.read(0, &mut b)?;
        if b[510] != 0x55 || b[511] != 0xAA {
            return Err("no FAT boot signature");
        }
        let bytes_per_sector = u16::from_le_bytes([b[11], b[12]]);
        let sectors_per_cluster = b[13];
        let reserved_sectors = u16::from_le_bytes([b[14], b[15]]);
        let fat_count = b[16];
        let fat_size_16 = u16::from_le_bytes([b[22], b[23]]) as u32;
        let fat_size_32 = u32::from_le_bytes([b[36], b[37], b[38], b[39]]);
        let root_cluster = u32::from_le_bytes([b[44], b[45], b[46], b[47]]);
        let total_sectors_16 = u16::from_le_bytes([b[19], b[20]]) as u32;
        let total_sectors_32 = u32::from_le_bytes([b[32], b[33], b[34], b[35]]);
        let total_sectors = if total_sectors_16 != 0 {
            total_sectors_16
        } else {
            total_sectors_32
        };

        if bytes_per_sector as usize != SECTOR_SIZE {
            return Err("unsupported FAT sector size");
        }
        if fat_size_16 != 0 {
            // FAT32 always routes the FAT size through the 32-bit field and
            // zeroes this legacy one; a non-zero value means FAT12/16.
            return Err("FAT12/16 volumes are not supported, only FAT32");
        }
        if sectors_per_cluster == 0 || fat_count == 0 || fat_size_32 == 0 || root_cluster < 2 {
            return Err("unsupported FAT32 geometry");
        }
        let first_data_sector = reserved_sectors as u32 + fat_count as u32 * fat_size_32;
        if first_data_sector >= total_sectors {
            return Err("unsupported FAT32 geometry");
        }
        let data_sectors = total_sectors.saturating_sub(first_data_sector);
        let total_clusters = data_sectors / sectors_per_cluster as u32;
        if total_clusters < 1 {
            return Err("FAT32 volume has no data clusters");
        }

        Ok(Self {
            dev,
            sectors_per_cluster,
            reserved_sectors,
            fat_count,
            fat_size_sectors: fat_size_32,
            root_cluster,
            first_data_sector,
            total_clusters,
            alloc_cursor: 2,
        })
    }

    pub fn sector_count(&self) -> u32 {
        self.dev.sector_count()
    }

    fn cluster_size(&self) -> usize {
        self.sectors_per_cluster as usize * SECTOR_SIZE
    }

    fn cluster_to_lba(&self, cluster: u32) -> u32 {
        self.first_data_sector + (cluster - 2) * self.sectors_per_cluster as u32
    }

    fn read_cluster(&self, cluster: u32, out: &mut [u8]) -> Result<(), &'static str> {
        if cluster < 2 {
            return Err("invalid cluster number");
        }
        self.dev.read(self.cluster_to_lba(cluster), out)
    }

    fn write_cluster(&mut self, cluster: u32, data: &[u8]) -> Result<(), &'static str> {
        if cluster < 2 {
            return Err("invalid cluster number");
        }
        self.dev.write(self.cluster_to_lba(cluster), data)
    }

    fn read_fat_entry(&self, cluster: u32) -> Result<u32, &'static str> {
        let fat_offset = cluster as usize * 4;
        let fat_sector = self.reserved_sectors as u32 + (fat_offset / SECTOR_SIZE) as u32;
        let ent_off = fat_offset % SECTOR_SIZE;
        let mut sec = [0u8; SECTOR_SIZE];
        self.dev.read(fat_sector, &mut sec)?;
        let val = u32::from_le_bytes([
            sec[ent_off],
            sec[ent_off + 1],
            sec[ent_off + 2],
            sec[ent_off + 3],
        ]);
        Ok(val & 0x0FFF_FFFF)
    }

    /// Write one FAT entry to every FAT copy the volume has, preserving the
    /// top four reserved bits.
    fn write_fat_entry(&mut self, cluster: u32, val: u32) -> Result<(), &'static str> {
        let fat_offset = cluster as usize * 4;
        let sector_in_fat = (fat_offset / SECTOR_SIZE) as u32;
        let ent_off = fat_offset % SECTOR_SIZE;
        for fat_idx in 0..self.fat_count as u32 {
            let fat_sector =
                self.reserved_sectors as u32 + fat_idx * self.fat_size_sectors + sector_in_fat;
            let mut sec = [0u8; SECTOR_SIZE];
            self.dev.read(fat_sector, &mut sec)?;
            let old = u32::from_le_bytes([
                sec[ent_off],
                sec[ent_off + 1],
                sec[ent_off + 2],
                sec[ent_off + 3],
            ]);
            let new = (old & 0xF000_0000) | (val & 0x0FFF_FFFF);
            sec[ent_off..ent_off + 4].copy_from_slice(&new.to_le_bytes());
            self.dev.write(fat_sector, &sec)?;
        }
        Ok(())
    }

    fn next_cluster(&self, cluster: u32) -> Result<Option<u32>, &'static str> {
        let next = self.read_fat_entry(cluster)?;
        if next < 2 || next >= 0x0FFF_FFF7 {
            return Ok(None);
        }
        Ok(Some(next))
    }

    /// First free cluster at or after the allocation cursor, wrapping once.
    /// A linear FAT scan, same as the O1FS bitmap scan — fine for the sizes
    /// of volume this OS deals with, and bounded by the volume's own cluster
    /// count rather than an arbitrary constant.
    ///
    /// Caches the current FAT sector across iterations instead of calling
    /// [`Self::read_fat_entry`] (one device read per cluster) — on a real USB
    /// drive a worst-case scan (nearly-full volume, or the cursor wrapping
    /// around) would otherwise turn one file creation into one Bulk-Only
    /// Transport round trip per cluster on the volume.
    fn alloc_cluster(&mut self) -> Result<u32, &'static str> {
        let max_cluster = self.total_clusters.saturating_add(1);
        if max_cluster < 2 {
            return Err("USB drive has no data clusters");
        }
        let span = max_cluster - 1; // clusters 2..=max_cluster
        let entries_per_sector = (SECTOR_SIZE / 4) as u32;
        let mut sec = [0u8; SECTOR_SIZE];
        let mut cached_sector: Option<u32> = None;
        let mut c = self.alloc_cursor.clamp(2, max_cluster);
        for _ in 0..span {
            let fat_sector = self.reserved_sectors as u32 + c / entries_per_sector;
            if cached_sector != Some(fat_sector) {
                self.dev.read(fat_sector, &mut sec)?;
                cached_sector = Some(fat_sector);
            }
            let off = (c % entries_per_sector) as usize * 4;
            let val = u32::from_le_bytes([sec[off], sec[off + 1], sec[off + 2], sec[off + 3]])
                & 0x0FFF_FFFF;
            if val == FAT_FREE {
                self.alloc_cursor = if c == max_cluster { 2 } else { c + 1 };
                return Ok(c);
            }
            c = if c == max_cluster { 2 } else { c + 1 };
        }
        Err("USB drive is full")
    }

    fn free_chain(&mut self, start: u32) -> Result<(), &'static str> {
        let mut cluster = start;
        let mut steps = 0usize;
        while cluster >= 2 {
            if steps >= MAX_CLUSTER_CHAIN {
                return Err("cluster chain too long (corrupt or cyclic)");
            }
            steps += 1;
            let next = self.next_cluster(cluster)?;
            self.write_fat_entry(cluster, FAT_FREE)?;
            match next {
                Some(n) => cluster = n,
                None => break,
            }
        }
        Ok(())
    }

    /// Every cluster of a directory (or file) chain, concatenated, plus the
    /// cluster number backing each `cluster_size()`-byte window — so a later
    /// patch can be written back to exactly the right place.
    fn read_dir_clusters(&self, first_cluster: u32) -> Result<(Vec<u8>, Vec<u32>), &'static str> {
        if first_cluster < 2 {
            return Err("invalid directory cluster");
        }
        let cs = self.cluster_size();
        let mut buf = Vec::new();
        let mut clusters = Vec::new();
        let mut cluster = first_cluster;
        let mut steps = 0usize;
        loop {
            if steps >= MAX_CLUSTER_CHAIN {
                return Err("directory cluster chain too long (corrupt or cyclic)");
            }
            steps += 1;
            if buf.len() + cs > MAX_DIR_ENTRIES * 32 {
                return Err("directory has too many entries");
            }
            let mut c = vec![0u8; cs];
            self.read_cluster(cluster, &mut c)?;
            buf.extend_from_slice(&c);
            clusters.push(cluster);
            match self.next_cluster(cluster)? {
                Some(n) => cluster = n,
                None => break,
            }
        }
        Ok((buf, clusters))
    }

    /// Write back the one cluster covering `byte_off`.
    fn flush_dir_range(
        &mut self,
        clusters: &[u32],
        buf: &[u8],
        byte_off: usize,
    ) -> Result<(), &'static str> {
        let cs = self.cluster_size();
        let idx = byte_off / cs;
        let cluster = *clusters.get(idx).ok_or("directory index out of range")?;
        let chunk = buf
            .get(idx * cs..idx * cs + cs)
            .ok_or("directory buffer short")?;
        self.write_cluster(cluster, chunk)
    }

    /// Allocate and zero a new cluster, link it onto the end of `clusters`,
    /// and append its zeroed bytes to `buf` — used both to grow a directory
    /// that has run out of room and to give a brand new one its first
    /// cluster.
    fn append_dir_cluster(
        &mut self,
        clusters: &mut Vec<u32>,
        buf: &mut Vec<u8>,
    ) -> Result<u32, &'static str> {
        let new_cluster = self.alloc_cluster()?;
        if let Some(&last) = clusters.last() {
            self.write_fat_entry(last, new_cluster)?;
        }
        self.write_fat_entry(new_cluster, FAT_EOC)?;
        let zeros = vec![0u8; self.cluster_size()];
        self.write_cluster(new_cluster, &zeros)?;
        clusters.push(new_cluster);
        buf.extend_from_slice(&zeros);
        Ok(new_cluster)
    }

    /// A 32-byte slot ready to hold a new entry: a deleted (`0xE5`) slot, the
    /// end-of-directory marker (rewritten in place, with the following slot
    /// re-zeroed so the directory still ends cleanly), or — if neither
    /// exists — a fresh cluster appended to the chain.
    fn find_or_make_free_slot(
        &mut self,
        buf: &mut Vec<u8>,
        clusters: &mut Vec<u32>,
    ) -> Result<usize, &'static str> {
        let mut i = 0usize;
        while i + 32 <= buf.len() {
            if buf[i] == 0xE5 {
                return Ok(i);
            }
            if buf[i] == 0x00 {
                if i + 64 <= buf.len() {
                    for b in &mut buf[i + 32..i + 64] {
                        *b = 0;
                    }
                    self.flush_dir_range(clusters, buf, i + 32)?;
                }
                return Ok(i);
            }
            i += 32;
        }
        let cs = self.cluster_size();
        self.append_dir_cluster(clusters, buf)?;
        Ok(buf.len() - cs)
    }

    /// Grow, shrink or freshly allocate a cluster chain so it holds exactly
    /// `needed_bytes`, returning its (possibly new) first cluster — `0` if
    /// `needed_bytes` is zero.
    fn set_chain_length(
        &mut self,
        first_cluster: u32,
        needed_bytes: usize,
    ) -> Result<u32, &'static str> {
        let cs = self.cluster_size();
        let needed_clusters = (needed_bytes + cs - 1) / cs;

        if needed_clusters == 0 {
            if first_cluster >= 2 {
                self.free_chain(first_cluster)?;
            }
            return Ok(0);
        }

        if first_cluster < 2 {
            let mut prev = 0u32;
            let mut head = 0u32;
            for _ in 0..needed_clusters {
                let c = self.alloc_cluster()?;
                if prev == 0 {
                    head = c;
                } else {
                    self.write_fat_entry(prev, c)?;
                }
                self.write_fat_entry(c, FAT_EOC)?;
                prev = c;
            }
            return Ok(head);
        }

        let mut last = first_cluster;
        let mut have = 1usize;
        let mut steps = 0usize;
        while have < needed_clusters {
            if steps >= MAX_CLUSTER_CHAIN {
                return Err("cluster chain too long (corrupt or cyclic)");
            }
            steps += 1;
            match self.next_cluster(last)? {
                Some(n) => {
                    last = n;
                    have += 1;
                }
                None => break,
            }
        }
        if have < needed_clusters {
            for _ in have..needed_clusters {
                let c = self.alloc_cluster()?;
                self.write_fat_entry(last, c)?;
                self.write_fat_entry(c, FAT_EOC)?;
                last = c;
            }
        } else if steps < MAX_CLUSTER_CHAIN {
            // The existing chain reached exactly `needed_clusters`; anything
            // still linked past `last` is now unused.
            if let Some(tail) = self.next_cluster(last)? {
                self.write_fat_entry(last, FAT_EOC)?;
                self.free_chain(tail)?;
            }
        }
        Ok(first_cluster)
    }

    fn write_file_chain(&mut self, first_cluster: u32, data: &[u8]) -> Result<(), &'static str> {
        if data.is_empty() {
            return Ok(());
        }
        let cs = self.cluster_size();
        let mut cluster = first_cluster;
        let mut off = 0usize;
        let mut steps = 0usize;
        loop {
            if cluster < 2 {
                return Err("cluster chain shorter than data (internal error)");
            }
            if steps >= MAX_CLUSTER_CHAIN {
                return Err("cluster chain too long (corrupt or cyclic)");
            }
            steps += 1;
            let chunk_len = (data.len() - off).min(cs);
            if chunk_len == cs {
                self.write_cluster(cluster, &data[off..off + cs])?;
            } else {
                let mut tmp = vec![0u8; cs];
                tmp[..chunk_len].copy_from_slice(&data[off..off + chunk_len]);
                self.write_cluster(cluster, &tmp)?;
            }
            off += chunk_len;
            if off >= data.len() {
                break;
            }
            cluster = self
                .next_cluster(cluster)?
                .ok_or("cluster chain shorter than data (internal error)")?;
        }
        Ok(())
    }

    fn read_file_chain(&self, first_cluster: u32, size: usize) -> Result<Vec<u8>, &'static str> {
        if size == 0 || first_cluster < 2 {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(size.min(MAX_FILE_SIZE));
        let mut cluster = first_cluster;
        let mut buf = vec![0u8; self.cluster_size()];
        let mut steps = 0usize;
        while out.len() < size {
            if steps >= MAX_CLUSTER_CHAIN {
                return Err("file cluster chain too long (corrupt or cyclic)");
            }
            steps += 1;
            self.read_cluster(cluster, &mut buf)?;
            let remain = size - out.len();
            out.extend_from_slice(&buf[..remain.min(buf.len())]);
            match self.next_cluster(cluster)? {
                Some(n) => cluster = n,
                None => break,
            }
        }
        out.truncate(size);
        Ok(out)
    }

    /// The cluster backing a directory named by a (possibly empty, meaning
    /// root) relative path.
    fn dir_cluster_for(&self, dir_path: &str) -> Result<u32, &'static str> {
        let mut cluster = self.root_cluster;
        for part in dir_path.split('/').filter(|s| !s.is_empty()) {
            let (buf, _clusters) = self.read_dir_clusters(cluster)?;
            let entries = parse_dir_entries(&buf);
            let e = entries
                .iter()
                .find(|e| e.is_dir() && e.name.eq_ignore_ascii_case(part))
                .ok_or("directory not found")?;
            if e.first_cluster < 2 {
                return Err("corrupt directory entry");
            }
            cluster = e.first_cluster;
        }
        Ok(cluster)
    }

    pub fn list(&self, dir_path: &str) -> Result<Vec<(String, bool, u32)>, &'static str> {
        let cluster = self.dir_cluster_for(dir_path)?;
        let (buf, _clusters) = self.read_dir_clusters(cluster)?;
        Ok(parse_dir_entries(&buf)
            .into_iter()
            .map(|e| {
                let is_dir = e.is_dir();
                (e.name, is_dir, e.size)
            })
            .collect())
    }

    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, &'static str> {
        let (dir, name) = split_parent(path)?;
        let cluster = self.dir_cluster_for(dir)?;
        let (buf, _clusters) = self.read_dir_clusters(cluster)?;
        let e = parse_dir_entries(&buf)
            .into_iter()
            .find(|e| !e.is_dir() && e.name.eq_ignore_ascii_case(name))
            .ok_or("file not found")?;
        if e.size as usize > MAX_FILE_SIZE {
            return Err("file is too large");
        }
        self.read_file_chain(e.first_cluster, e.size as usize)
    }

    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), &'static str> {
        if data.len() > MAX_FILE_SIZE {
            return Err("file is too large");
        }
        let (dir, name) = split_parent(path)?;
        let dir_cluster = self.dir_cluster_for(dir)?;
        let (mut buf, mut clusters) = self.read_dir_clusters(dir_cluster)?;
        let entries = parse_dir_entries(&buf);
        let existing = entries
            .iter()
            .find(|e| !e.is_dir() && e.name.eq_ignore_ascii_case(name))
            .cloned();

        let old_first_cluster = existing.as_ref().map(|e| e.first_cluster).unwrap_or(0);
        let new_first_cluster = self.set_chain_length(old_first_cluster, data.len())?;
        self.write_file_chain(new_first_cluster, data)?;

        if let Some(e) = existing {
            let off = e.buf_offset;
            write_dirent_fields(&mut buf[off..off + 32], new_first_cluster, data.len() as u32);
            self.flush_dir_range(&clusters, &buf, off)?;
        } else {
            let slot_off = self.find_or_make_free_slot(&mut buf, &mut clusters)?;
            let name83 = make_short_name(name, &entries);
            let mut rec = [0u8; 32];
            rec[0..11].copy_from_slice(&name83);
            rec[11] = 0x20; // ATTR_ARCHIVE
            write_dirent_fields(&mut rec, new_first_cluster, data.len() as u32);
            buf[slot_off..slot_off + 32].copy_from_slice(&rec);
            self.flush_dir_range(&clusters, &buf, slot_off)?;
        }
        Ok(())
    }

    pub fn mkdir(&mut self, path: &str) -> Result<(), &'static str> {
        let (parent, name) = split_parent(path)?;
        let parent_cluster = self.dir_cluster_for(parent)?;
        let (mut buf, mut clusters) = self.read_dir_clusters(parent_cluster)?;
        let entries = parse_dir_entries(&buf);
        if entries.iter().any(|e| e.name.eq_ignore_ascii_case(name)) {
            // mkdir -p semantics: already there is not an error.
            return Ok(());
        }

        let new_cluster = self.alloc_cluster()?;
        self.write_fat_entry(new_cluster, FAT_EOC)?;
        let mut initial = vec![0u8; self.cluster_size()];
        let dotdot_target = if parent_cluster == self.root_cluster {
            0
        } else {
            parent_cluster
        };
        write_dot_entries(&mut initial, new_cluster, dotdot_target);
        self.write_cluster(new_cluster, &initial)?;

        let slot_off = self.find_or_make_free_slot(&mut buf, &mut clusters)?;
        let name83 = make_short_name(name, &entries);
        let mut rec = [0u8; 32];
        rec[0..11].copy_from_slice(&name83);
        rec[11] = 0x10; // ATTR_DIRECTORY
        write_dirent_fields(&mut rec, new_cluster, 0);
        buf[slot_off..slot_off + 32].copy_from_slice(&rec);
        self.flush_dir_range(&clusters, &buf, slot_off)
    }

    pub fn remove(&mut self, path: &str) -> Result<(), &'static str> {
        let (parent, name) = split_parent(path)?;
        let parent_cluster = self.dir_cluster_for(parent)?;
        let (mut buf, clusters) = self.read_dir_clusters(parent_cluster)?;
        let e = parse_dir_entries(&buf)
            .into_iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
            .ok_or("not found")?;

        if e.is_dir() {
            if e.first_cluster < 2 {
                return Err("corrupt directory entry");
            }
            let (child_buf, _c) = self.read_dir_clusters(e.first_cluster)?;
            if !parse_dir_entries(&child_buf).is_empty() {
                return Err("directory not empty");
            }
            self.free_chain(e.first_cluster)?;
        } else if e.first_cluster >= 2 {
            self.free_chain(e.first_cluster)?;
        }

        let mut off = e.record_start;
        while off <= e.buf_offset {
            buf[off] = 0xE5;
            off += 32;
        }
        self.flush_dir_range(&clusters, &buf, e.record_start)?;
        let cs = self.cluster_size();
        if e.record_start / cs != e.buf_offset / cs {
            self.flush_dir_range(&clusters, &buf, e.buf_offset)?;
        }
        Ok(())
    }

    /// (bytes used, bytes total) — a full FAT scan, same cost as O1FS's
    /// bitmap scan; fine to call for a "My Computer" status line, not fine
    /// to call every frame.
    ///
    /// Walks the FAT one sector at a time (128 entries per 512-byte sector)
    /// rather than calling [`Self::read_fat_entry`] per cluster — on a real
    /// USB drive each `read_fat_entry` call is a full Bulk-Only-Transport
    /// round trip, so doing that once per cluster instead of once per sector
    /// turns a 512-byte-cluster volume's status line into tens of thousands
    /// of USB transactions and a multi-minute "hang".
    pub fn usage(&self) -> (usize, usize) {
        let cs = self.cluster_size();
        let entries_per_sector = SECTOR_SIZE / 4;
        let last_cluster = self.total_clusters.saturating_add(1);
        let mut used = 0u64;
        let mut sec = [0u8; SECTOR_SIZE];
        let mut c = 2u32;
        while c <= last_cluster {
            let fat_sector = self.reserved_sectors as u32 + c / entries_per_sector as u32;
            if self.dev.read(fat_sector, &mut sec).is_err() {
                break;
            }
            let start_in_sector = c as usize % entries_per_sector;
            for i in start_in_sector..entries_per_sector {
                if c > last_cluster {
                    break;
                }
                let off = i * 4;
                let val = u32::from_le_bytes([sec[off], sec[off + 1], sec[off + 2], sec[off + 3]])
                    & 0x0FFF_FFFF;
                if val != FAT_FREE {
                    used += 1;
                }
                c += 1;
            }
        }
        (
            (used as usize).saturating_mul(cs),
            (self.total_clusters as usize).saturating_mul(cs),
        )
    }
}

fn split_parent(path: &str) -> Result<(&str, &str), &'static str> {
    let p = path.trim_matches('/');
    if p.is_empty() {
        return Err("empty path");
    }
    match p.rfind('/') {
        Some(i) => Ok((&p[..i], &p[i + 1..])),
        None => Ok(("", p)),
    }
}

fn write_dirent_fields(rec: &mut [u8], first_cluster: u32, size: u32) {
    let hi = ((first_cluster >> 16) as u16).to_le_bytes();
    let lo = (first_cluster as u16).to_le_bytes();
    rec[20] = hi[0];
    rec[21] = hi[1];
    rec[26] = lo[0];
    rec[27] = lo[1];
    rec[28..32].copy_from_slice(&size.to_le_bytes());
}

fn write_dot_entries(buf: &mut [u8], self_cluster: u32, parent_cluster: u32) {
    let mut dot = [0u8; 32];
    dot[0..11].copy_from_slice(b".          ");
    dot[11] = 0x10;
    write_dirent_fields(&mut dot, self_cluster, 0);
    buf[0..32].copy_from_slice(&dot);

    let mut dotdot = [0u8; 32];
    dotdot[0..11].copy_from_slice(b"..         ");
    dotdot[11] = 0x10;
    write_dirent_fields(&mut dotdot, parent_cluster, 0);
    buf[32..64].copy_from_slice(&dotdot);
}

fn name_83_to_string(name: &[u8; 11], case_info: u8) -> String {
    let base = &name[0..8];
    let ext = &name[8..11];
    let base_end = base.iter().rposition(|&c| c != b' ').map(|p| p + 1).unwrap_or(0);
    let ext_end = ext.iter().rposition(|&c| c != b' ').map(|p| p + 1).unwrap_or(0);
    let lower_base = case_info & 0x08 != 0;
    let lower_ext = case_info & 0x10 != 0;
    let mut s = String::new();
    for &c in &base[..base_end] {
        let c = if c == 0x05 { 0xE5 } else { c };
        s.push(if lower_base {
            (c as char).to_ascii_lowercase()
        } else {
            c as char
        });
    }
    if ext_end > 0 {
        s.push('.');
        for &c in &ext[..ext_end] {
            s.push(if lower_ext {
                (c as char).to_ascii_lowercase()
            } else {
                c as char
            });
        }
    }
    s
}

/// Parse one directory's worth of 32-byte records, joining long-name (`0x0F`)
/// runs into the file name they spell out and skipping volume labels, `.`
/// and `..`. Stops at the first end-of-directory marker.
fn parse_dir_entries(buf: &[u8]) -> Vec<DirEntry> {
    let mut out = Vec::new();
    let mut lfn_fragments: Vec<[u16; 13]> = Vec::new();
    let mut record_start: Option<usize> = None;
    let mut i = 0usize;
    while i + 32 <= buf.len() {
        let rec = &buf[i..i + 32];
        if rec[0] == 0x00 {
            break;
        }
        if rec[0] == 0xE5 {
            lfn_fragments.clear();
            record_start = None;
            i += 32;
            continue;
        }
        let attr = rec[11];
        if attr == 0x0F {
            if record_start.is_none() {
                record_start = Some(i);
            }
            let mut chars = [0u16; 13];
            for k in 0..5 {
                chars[k] = u16::from_le_bytes([rec[1 + 2 * k], rec[2 + 2 * k]]);
            }
            for k in 0..6 {
                chars[5 + k] = u16::from_le_bytes([rec[14 + 2 * k], rec[15 + 2 * k]]);
            }
            for k in 0..2 {
                chars[11 + k] = u16::from_le_bytes([rec[28 + 2 * k], rec[29 + 2 * k]]);
            }
            lfn_fragments.push(chars);
            i += 32;
            continue;
        }

        let start = record_start.take().unwrap_or(i);
        let mut short_name = [0u8; 11];
        short_name.copy_from_slice(&rec[0..11]);
        let first_hi = u16::from_le_bytes([rec[20], rec[21]]) as u32;
        let first_lo = u16::from_le_bytes([rec[26], rec[27]]) as u32;
        let size = u32::from_le_bytes([rec[28], rec[29], rec[30], rec[31]]);
        let first_cluster = (first_hi << 16) | first_lo;

        // Dot entries and volume labels are not real children of anything.
        let is_dot = short_name[0] == b'.';
        let is_volume_label = attr & 0x08 != 0;
        if is_dot || is_volume_label {
            lfn_fragments.clear();
            i += 32;
            continue;
        }

        let name = if !lfn_fragments.is_empty() {
            lfn_fragments.reverse();
            let mut units: Vec<u16> = Vec::new();
            'outer: for frag in &lfn_fragments {
                for &u in frag {
                    if u == 0x0000 {
                        break 'outer;
                    }
                    if u == 0xFFFF {
                        continue;
                    }
                    units.push(u);
                }
            }
            String::from_utf16_lossy(&units)
        } else {
            name_83_to_string(&short_name, rec[12])
        };
        lfn_fragments.clear();

        if out.len() >= MAX_DIR_ENTRIES {
            break;
        }
        out.push(DirEntry {
            name,
            attr,
            first_cluster,
            size,
            short_name,
            buf_offset: i,
            record_start: start,
        });
        i += 32;
    }
    out
}

/// A valid, collision-free 8.3 short name for `name`, uppercased and stripped
/// of anything FAT does not allow. Falls back to a numeric `~N` tail exactly
/// the way real FAT drivers do when the sanitised name is too long or
/// already taken in `existing`.
fn make_short_name(name: &str, existing: &[DirEntry]) -> [u8; 11] {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    };
    let clean = |s: &str, max: usize| -> Vec<u8> {
        let mut out = Vec::new();
        for c in s.chars() {
            if out.len() >= max {
                break;
            }
            let u = c.to_ascii_uppercase();
            if u.is_ascii_alphanumeric() || "!#$%&'()-@^_`{}~".contains(u) {
                out.push(u as u8);
            }
        }
        out
    };
    let base_full = clean(stem, 255);
    let ext_clean = clean(ext, 3);

    let build = |suffix: &str| -> [u8; 11] {
        let mut n = [b' '; 11];
        let avail = 8usize.saturating_sub(suffix.len());
        let base_trunc: Vec<u8> = base_full.iter().take(avail).copied().collect();
        for (i, &b) in base_trunc.iter().enumerate() {
            n[i] = b;
        }
        for (i, &b) in suffix.as_bytes().iter().enumerate() {
            if base_trunc.len() + i < 8 {
                n[base_trunc.len() + i] = b;
            }
        }
        for (i, &b) in ext_clean.iter().take(3).enumerate() {
            n[8 + i] = b;
        }
        n
    };

    let taken = |candidate: &[u8; 11]| existing.iter().any(|e| &e.short_name == candidate);

    let needs_tail = base_full.len() > 8 || base_full.is_empty();
    if !needs_tail {
        let candidate = build("");
        if !taken(&candidate) {
            return candidate;
        }
    }
    for n in 1..=9999u32 {
        let suffix = alloc::format!("~{}", n);
        if suffix.len() > 7 {
            break;
        }
        let candidate = build(&suffix);
        if !taken(&candidate) {
            return candidate;
        }
    }
    let mut n = [b' '; 11];
    n[0..8].copy_from_slice(b"FILE~999");
    n
}

// ── Self-test ───────────────────────────────────────────────────────────

/// Hand-built minimal FAT32 geometry: 1 sector per cluster (same as the
/// macOS-formatted USB stick this driver is meant to read), 2 FATs, 300 data
/// clusters — enough to span several FAT sectors (128 entries each), which is
/// exactly the case that broke `usage()`/`alloc_cluster()` before they were
/// changed to read the FAT sector-at-a-time instead of once per cluster.
const TEST_SPC: u8 = 1;
const TEST_RESERVED: u16 = 32;
const TEST_FAT_COUNT: u8 = 2;
const TEST_FAT_SECTORS: u32 = 3;
const TEST_DATA_CLUSTERS: u32 = 300;

/// Write a bare BPB plus an all-free FAT and an empty root directory —
/// everything [`Fat32Fs::mount`] actually reads — directly into a
/// [`crate::diskfs::RamDisk`], with no real formatter involved.
fn build_test_image() -> crate::diskfs::RamDisk {
    let first_data_sector =
        TEST_RESERVED as u32 + TEST_FAT_COUNT as u32 * TEST_FAT_SECTORS;
    let total_sectors = first_data_sector + TEST_DATA_CLUSTERS * TEST_SPC as u32;

    let mut disk = crate::diskfs::RamDisk::new(total_sectors);
    let mut boot = [0u8; SECTOR_SIZE];
    boot[11..13].copy_from_slice(&(SECTOR_SIZE as u16).to_le_bytes());
    boot[13] = TEST_SPC;
    boot[14..16].copy_from_slice(&TEST_RESERVED.to_le_bytes());
    boot[16] = TEST_FAT_COUNT;
    // total_sectors_16 stays 0 — real FAT32 always routes size through the
    // 32-bit field, and `mount` falls back to it only when this is zero.
    boot[22..24].copy_from_slice(&0u16.to_le_bytes()); // fat_size_16 = 0 (FAT32 marker)
    boot[32..36].copy_from_slice(&total_sectors.to_le_bytes());
    boot[36..40].copy_from_slice(&TEST_FAT_SECTORS.to_le_bytes());
    boot[44..48].copy_from_slice(&2u32.to_le_bytes()); // root_cluster
    boot[510] = 0x55;
    boot[511] = 0xAA;
    disk.write(0, &boot).expect("boot sector fits");

    // Root directory's own cluster (2) is allocated; mark it end-of-chain in
    // both FAT copies so a corrupt or truncated FAT is never mistaken for a
    // freshly-formatted one.
    for fat_idx in 0..TEST_FAT_COUNT as u32 {
        let mut fat0 = [0u8; SECTOR_SIZE];
        fat0[8..12].copy_from_slice(&FAT_EOC.to_le_bytes()); // cluster 2
        disk.write(TEST_RESERVED as u32 + fat_idx * TEST_FAT_SECTORS, &fat0)
            .expect("fat sector fits");
    }

    // The root directory cluster starts zeroed by `RamDisk::new`, i.e.
    // already a valid (empty) directory — nothing else to write.
    disk
}

fn expect<T>(r: &mut crate::selftest::Report, name: &'static str, res: Result<T, &'static str>) -> Option<T> {
    r.check(name, res.is_ok());
    res.ok()
}

/// Boot-time checks for the writable USB FAT32 driver, run against a RAM
/// disk built to look like a real (if tiny) FAT32 volume — never against
/// hardware, so this runs on every boot with nothing plugged in.
pub fn selftest() -> crate::selftest::Report {
    use crate::selftest::Report;
    let mut r = Report::new();

    r.check(
        "a disk with no boot signature does not mount",
        Fat32Fs::mount(crate::diskfs::RamDisk::new(64)).is_err(),
    );

    let Some(mut fs) = expect(&mut r, "a hand-built FAT32 image mounts", Fat32Fs::mount(build_test_image())) else {
        return r;
    };

    let (used0, total0) = fs.usage();
    r.check(
        "a fresh volume has only its own root cluster in use",
        used0 == SECTOR_SIZE * TEST_SPC as usize,
    );
    r.check(
        "usage reports the volume's real cluster count",
        total0 == (TEST_DATA_CLUSTERS as usize) * SECTOR_SIZE,
    );
    r.check(
        "a fresh volume lists nothing",
        fs.list("").map(|v| v.is_empty()) == Ok(true),
    );

    r.check(
        "writing a file below the root succeeds",
        fs.write_file("/hello.txt", b"hello, usb!").is_ok(),
    );
    r.check(
        "the written file reads back byte-for-byte",
        fs.read_file("/hello.txt").as_deref() == Ok(b"hello, usb!".as_slice()),
    );
    // A short (8.3-only, no LFN) name always reads back upper-case — FAT
    // short names are case-insensitive on disk, so the driver does not
    // preserve case round-trip unless a long-name entry was written for it.
    let listing_after_write = fs.list("");
    r.check(
        "the written file shows up in a listing",
        listing_after_write
            .as_ref()
            .map(|v| v.iter().any(|e| e.0.eq_ignore_ascii_case("hello.txt") && !e.1))
            .unwrap_or(false),
    );

    r.check("mkdir creates a subdirectory", fs.mkdir("/sub").is_ok());
    r.check(
        "a file can be created inside that subdirectory",
        fs.write_file("/sub/nested.bin", &[1u8, 2, 3, 4, 5]).is_ok(),
    );
    r.check(
        "the nested file reads back from inside the subdirectory",
        fs.read_file("/sub/nested.bin").as_deref() == Ok([1u8, 2, 3, 4, 5].as_slice()),
    );

    let big = alloc::vec![0xABu8; SECTOR_SIZE * 5 + 37];
    r.check(
        "a multi-cluster file (spanning several sectors) writes",
        fs.write_file("/big.bin", &big).is_ok(),
    );
    r.check(
        "a multi-cluster file reads back exactly, including its odd tail",
        fs.read_file("/big.bin").as_deref() == Ok(big.as_slice()),
    );

    r.check(
        "overwriting a file with shorter content truncates it",
        fs.write_file("/hello.txt", b"hi").is_ok(),
    );
    r.check(
        "the truncated file reads back only the new, shorter content",
        fs.read_file("/hello.txt").as_deref() == Ok(b"hi".as_slice()),
    );

    let (used_before_delete, _) = fs.usage();
    r.check(
        "a populated volume shows clusters in use",
        used_before_delete > 0,
    );

    r.check("removing a file succeeds", fs.remove("/big.bin").is_ok());
    r.check(
        "a removed file no longer reads back",
        fs.read_file("/big.bin").is_err(),
    );
    r.check(
        "removing a file frees its clusters",
        fs.usage().0 < used_before_delete,
    );

    r.check(
        "reading a path that was never created fails",
        fs.read_file("/does/not/exist.txt").is_err(),
    );
    r.check(
        "writing outside the root without an existing parent fails",
        fs.write_file("/nope/nested.txt", b"x").is_err(),
    );

    r
}