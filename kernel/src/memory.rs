use bootloader_api::info::{MemoryRegions, MemoryRegionKind};
use alloc::vec::Vec;
use spin::Mutex;

/// Mask selecting the physical frame address out of a page-table entry.
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Standard x86_64 page size.
pub const PAGE_SIZE: u64 = 4096;

/// A physical frame allocator that uses the bootloader's memory map.
pub trait FrameAllocator {
    fn allocate_frame(&mut self) -> Option<u64>;
}

/// A Page Table (Level 1-4) in x86_64 long mode.
#[repr(C, align(4096))]
pub struct PageTable {
    pub entries: [u64; 512],
}

impl PageTable {
    pub const fn empty() -> Self {
        Self { entries: [0; 512] }
    }

    pub fn clear(&mut self) {
        for entry in self.entries.iter_mut() {
            *entry = 0;
        }
    }
}

/// Page Table entry flags.
pub mod flags {
    pub const PRESENT: u64 = 1 << 0;
    pub const WRITABLE: u64 = 1 << 1;
    pub const USER_ACCESSIBLE: u64 = 1 << 2;
    pub const NO_EXECUTE: u64 = 1 << 63;
}

pub struct BootInfoFrameAllocator {
    memory_regions: &'static MemoryRegions,
    /// Cursor into `memory_regions` for frames never handed out yet.
    region_idx: usize,
    /// Byte offset into the current region.
    cursor: u64,
    /// Frames returned by `free_frame`, reused before carving new ones.
    /// A plain `Vec` is fine here: it stays empty (and therefore never
    /// allocates) until the first process exits, which is long after the
    /// heap is up.
    free_list: Vec<u64>,
}

impl BootInfoFrameAllocator {
    pub unsafe fn init(memory_regions: &'static MemoryRegions) -> Self {
        Self {
            memory_regions,
            region_idx: 0,
            cursor: 0,
            free_list: Vec::new(),
        }
    }

    /// Carve the next never-used frame.
    ///
    /// The previous implementation rebuilt the region iterator and called
    /// `.nth(next)` on every single allocation, making start-up O(n²) in the
    /// number of frames. This walks a cursor instead.
    fn carve_frame(&mut self) -> Option<u64> {
        loop {
            let region = self.memory_regions.get(self.region_idx)?;
            if region.kind != MemoryRegionKind::Usable {
                self.region_idx += 1;
                self.cursor = 0;
                continue;
            }
            // Round the region base up to a page boundary before slicing it.
            let base = (region.start + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
            let frame = base + self.cursor;
            if frame + PAGE_SIZE > region.end {
                self.region_idx += 1;
                self.cursor = 0;
                continue;
            }
            self.cursor += PAGE_SIZE;
            return Some(frame);
        }
    }

    /// Carve `count` physically consecutive frames.
    ///
    /// Device DMA needs memory that is contiguous in *physical* space, which
    /// the ordinary allocator cannot promise — it hands back freed frames in
    /// whatever order they were returned. This takes the frames straight off
    /// the region cursor, skipping to the next region if the current one
    /// cannot satisfy the whole run, so the addresses are consecutive by
    /// construction.
    ///
    /// There is no matching free: NIC rings live for the lifetime of the
    /// system, and a hole-free allocator is not worth the complexity for a
    /// handful of pages.
    fn carve_contiguous(&mut self, count: usize) -> Option<u64> {
        if count == 0 {
            return None;
        }
        let span = PAGE_SIZE * count as u64;
        loop {
            let region = self.memory_regions.get(self.region_idx)?;
            if region.kind != MemoryRegionKind::Usable {
                self.region_idx += 1;
                self.cursor = 0;
                continue;
            }
            let base = (region.start + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
            let start = base + self.cursor;
            if start + span > region.end {
                self.region_idx += 1;
                self.cursor = 0;
                continue;
            }
            self.cursor += span;
            return Some(start);
        }
    }

    /// Return a frame to the pool. Called when a process is torn down.
    pub fn free_frame(&mut self, frame: u64) {
        self.free_list.push(frame & !(PAGE_SIZE - 1));
    }

    /// Frames currently parked on the free list.
    pub fn free_count(&self) -> usize {
        self.free_list.len()
    }
}

impl FrameAllocator for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<u64> {
        if let Some(frame) = self.free_list.pop() {
            return Some(frame);
        }
        self.carve_frame()
    }
}

struct RuntimeMapper {
    l4_table: *mut PageTable,
    physical_memory_offset: u64,
    frame_allocator: BootInfoFrameAllocator,
}

// SAFETY: RuntimeMapper is only accessed behind RUNTIME_MAPPER's mutex.
unsafe impl Send for RuntimeMapper {}

static RUNTIME_MAPPER: Mutex<Option<RuntimeMapper>> = Mutex::new(None);

/// Returns a reference to the active Level 4 page table.
pub unsafe fn active_l4_table(physical_memory_offset: u64) -> &'static mut PageTable {
    let cr3: u64;
    core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
    let l4_table_phys = cr3 & !0xFFF;
    let l4_table_virt = physical_memory_offset + l4_table_phys;
    &mut *(l4_table_virt as *mut PageTable)
}

/// Map a virtual page to a physical frame.
pub unsafe fn map_page(
    virt_page: u64,
    phys_frame: u64,
    flags: u64,
    l4_table: &mut PageTable,
    physical_memory_offset: u64,
    frame_allocator: &mut impl FrameAllocator,
) {
    let indexes = [
        ((virt_page >> 39) & 0x1FF) as usize, // L4
        ((virt_page >> 30) & 0x1FF) as usize, // L3
        ((virt_page >> 21) & 0x1FF) as usize, // L2
        ((virt_page >> 12) & 0x1FF) as usize, // L1
    ];

    let mut table = l4_table;

    for (i, &idx) in indexes.iter().enumerate() {
        let entry = table.entries[idx];
        
        if i == 3 {
            // Level 1: set the final mapping
            table.entries[idx] = phys_frame | flags | flags::PRESENT;
            return;
        }

        if entry & flags::PRESENT == 0 {
            // Table missing: allocate a new one.
            let new_frame = frame_allocator.allocate_frame().expect("out of memory for page tables");
            let new_table_virt = physical_memory_offset + new_frame;
            let new_table = &mut *(new_table_virt as *mut PageTable);
            new_table.clear();

            let mut table_flags = flags::PRESENT | flags::WRITABLE;
            if flags & flags::USER_ACCESSIBLE != 0 {
                table_flags |= flags::USER_ACCESSIBLE;
            }
            table.entries[idx] = new_frame | table_flags;
            table = new_table;
        } else {
            // Existing parent entries must also allow user traversal/execution
            // when mapping user pages. Any missing permission bit on an upper
            // level causes faults even if the leaf PTE looks correct.
            let mut updated = entry;
            if flags & flags::USER_ACCESSIBLE != 0 {
                updated |= flags::USER_ACCESSIBLE;
            }
            if flags & flags::WRITABLE != 0 {
                updated |= flags::WRITABLE;
            }
            if flags & flags::NO_EXECUTE == 0 {
                updated &= !flags::NO_EXECUTE;
            }
            if updated != entry {
                table.entries[idx] = updated;
            }

            // Table exists: follow it.
            let phys = updated & 0x000F_FFFF_FFFF_F000;
            let virt = physical_memory_offset + phys;
            table = &mut *(virt as *mut PageTable);
        }
    }
}

/// Flush the TLB for a specific virtual address.
pub unsafe fn flush_tlb(virt_addr: u64) {
    core::arch::asm!("invlpg [{}]", in(reg) virt_addr, options(nostack, preserves_flags));
}

/// Install runtime paging context so later subsystems (ELF loader/userspace)
/// can allocate and map pages after early boot.
pub fn install_runtime(
    l4_table: &'static mut PageTable,
    frame_allocator: BootInfoFrameAllocator,
    physical_memory_offset: u64,
) {
    *RUNTIME_MAPPER.lock() = Some(RuntimeMapper {
        l4_table: l4_table as *mut PageTable,
        physical_memory_offset,
        frame_allocator,
    });
}

/// Map one 4 KiB user page and return its mapped virtual page base.
pub fn map_user_page(virt_page: u64, writable: bool, executable: bool) -> Result<u64, &'static str> {
    let mut runtime = RUNTIME_MAPPER.lock();
    let rt = runtime.as_mut().ok_or("runtime mapper not initialized")?;
    let frame = rt.frame_allocator.allocate_frame().ok_or("out of frames")?;
    let mut map_flags = flags::USER_ACCESSIBLE;
    if writable {
        map_flags |= flags::WRITABLE;
    }
    if !executable {
        map_flags |= flags::NO_EXECUTE;
    }
    unsafe {
        map_page(
            virt_page,
            frame,
            map_flags,
            &mut *rt.l4_table,
            rt.physical_memory_offset,
            &mut rt.frame_allocator,
        );
        flush_tlb(virt_page);
    }
    Ok(virt_page)
}

/// Frames sitting on the reclaim list, i.e. memory handed back by exited
/// processes and ready for reuse.
pub fn free_frame_count() -> usize {
    RUNTIME_MAPPER
        .lock()
        .as_ref()
        .map(|rt| rt.frame_allocator.free_count())
        .unwrap_or(0)
}

// ── Per-process address spaces ──────────────────────────────────────────────
//
// The boot dump (`vm` in the shell) shows the kernel occupying level-4 slots
// 0, 2–7 and 136, leaving slot 1 entirely unused. Userspace lives there, and
// nothing else does — which is what makes isolation cheap.
//
// A process's whole address space is one level-3 table. Switching to a
// process writes that table into slot 1 of the single kernel level-4 table
// and flushes the TLB; every kernel mapping is shared automatically because
// no other slot is touched. When a process is not scheduled, its pages are
// not reachable from the active page tables at all, so one process cannot
// name — let alone read — another's memory.
//
// The alternative, a full level-4 table per process, would mean copying and
// resynchronising every kernel mapping on each spawn. This gets the same
// isolation for one entry write.

/// The level-4 slot that holds userspace. Must stay in sync with
/// `process::USER_BASE`.
pub const USER_L4_SLOT: usize = 1;

/// A process's private user address space: the physical frame of its
/// level-3 table.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct AddressSpace(u64);

impl AddressSpace {
    /// Sentinel for "no address space", used before one is created.
    pub const NONE: Self = Self(0);

    pub fn is_none(self) -> bool {
        self.0 == 0
    }
}

/// Reload CR3, flushing every non-global TLB entry.
///
/// Changing a level-4 entry invalidates a 512 GiB range, which `invlpg`
/// cannot express, so the whole TLB goes.
unsafe fn reload_cr3() {
    let cr3: u64;
    core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
    core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
}

/// Allocate an empty private address space.
pub fn create_address_space() -> Result<AddressSpace, &'static str> {
    let mut runtime = RUNTIME_MAPPER.lock();
    let rt = runtime.as_mut().ok_or("runtime mapper not initialized")?;
    let frame = rt
        .frame_allocator
        .allocate_frame()
        .ok_or("out of frames for address space")?;
    unsafe {
        let table = (rt.physical_memory_offset + frame) as *mut PageTable;
        (*table).clear();
    }
    Ok(AddressSpace(frame))
}

/// Install `space` as the current userspace mapping.
///
/// Passing [`AddressSpace::NONE`] unmaps userspace entirely, which is what
/// the kernel runs with when no process is scheduled.
pub fn activate_address_space(space: AddressSpace) {
    let runtime = RUNTIME_MAPPER.lock();
    let Some(rt) = runtime.as_ref() else {
        return;
    };
    unsafe {
        let l4 = &mut *rt.l4_table;
        l4.entries[USER_L4_SLOT] = if space.is_none() {
            0
        } else {
            space.0 | flags::PRESENT | flags::WRITABLE | flags::USER_ACCESSIBLE
        };
        reload_cr3();
    }
}

/// Free every frame reachable from `space`, then the space itself.
///
/// Walks the three levels below the level-3 table and hands each leaf frame
/// and each intermediate table back to the allocator, so an exiting process
/// leaks nothing. Assumes 4 KiB leaves throughout: user mappings are only
/// ever created by [`map_user_page`], which never sets the huge-page bit.
pub fn destroy_address_space(space: AddressSpace) {
    if space.is_none() {
        return;
    }
    let mut runtime = RUNTIME_MAPPER.lock();
    let Some(rt) = runtime.as_mut() else {
        return;
    };
    let pmo = rt.physical_memory_offset;

    // Collect first, free second: the allocator borrow and the page-table
    // walk would otherwise overlap.
    let mut frames: Vec<u64> = Vec::new();
    unsafe {
        let l3 = &*((pmo + space.0) as *const PageTable);
        for &l3_entry in l3.entries.iter() {
            if l3_entry & flags::PRESENT == 0 || l3_entry & (1 << 7) != 0 {
                continue;
            }
            let l2_frame = l3_entry & ADDR_MASK;
            let l2 = &*((pmo + l2_frame) as *const PageTable);
            for &l2_entry in l2.entries.iter() {
                if l2_entry & flags::PRESENT == 0 || l2_entry & (1 << 7) != 0 {
                    continue;
                }
                let l1_frame = l2_entry & ADDR_MASK;
                let l1 = &*((pmo + l1_frame) as *const PageTable);
                for &l1_entry in l1.entries.iter() {
                    if l1_entry & flags::PRESENT != 0 {
                        frames.push(l1_entry & ADDR_MASK);
                    }
                }
                frames.push(l1_frame);
            }
            frames.push(l2_frame);
        }
    }
    frames.push(space.0);

    for frame in frames {
        rt.frame_allocator.free_frame(frame);
    }
}

/// One occupied top-level slot: `(index, first virtual address it covers,
/// raw entry)`.
pub struct L4Slot {
    pub index: usize,
    pub base: u64,
    pub entry: u64,
}

/// Present entries in the active level-4 table.
///
/// Each L4 slot covers 512 GiB, so this is the coarsest possible view of what
/// the address space contains — which is exactly what you want when deciding
/// where userspace can live without colliding with the kernel.
pub fn l4_occupancy() -> Vec<L4Slot> {
    let runtime = RUNTIME_MAPPER.lock();
    let Some(rt) = runtime.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let table = unsafe { &*rt.l4_table };
    for (index, &entry) in table.entries.iter().enumerate() {
        if entry & flags::PRESENT == 0 {
            continue;
        }
        // Sign-extend bit 47 so the higher half prints as a canonical address.
        let mut base = (index as u64) << 39;
        if index >= 256 {
            base |= 0xFFFF_0000_0000_0000;
        }
        out.push(L4Slot { index, base, entry });
    }
    out
}

/// Offset at which all physical memory is mapped into the kernel's address
/// space.
pub fn physical_memory_offset() -> u64 {
    RUNTIME_MAPPER
        .lock()
        .as_ref()
        .map(|rt| rt.physical_memory_offset)
        .unwrap_or(0)
}

/// A physically contiguous, zeroed buffer that a device can DMA into.
///
/// `virt` is a pointer the kernel can dereference; `phys` is the address to
/// program into the device. The bootloader maps all physical memory at a
/// fixed offset, so no extra page mapping is needed to reach it.
pub struct DmaRegion {
    pub virt: u64,
    pub phys: u64,
    pub len: usize,
}

impl DmaRegion {
    pub fn as_mut_ptr<T>(&self) -> *mut T {
        self.virt as *mut T
    }

    pub fn as_mut_slice(&self) -> &'static mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.virt as *mut u8, self.len) }
    }
}

/// Allocate `bytes` (rounded up to whole pages) of contiguous physical memory
/// for device DMA, and zero it.
pub fn alloc_dma(bytes: usize) -> Option<DmaRegion> {
    let pages = (bytes + PAGE_SIZE as usize - 1) / PAGE_SIZE as usize;
    let mut guard = RUNTIME_MAPPER.lock();
    let rt = guard.as_mut()?;
    let phys = rt.frame_allocator.carve_contiguous(pages)?;
    let virt = rt.physical_memory_offset + phys;
    let len = pages * PAGE_SIZE as usize;
    unsafe {
        core::ptr::write_bytes(virt as *mut u8, 0, len);
    }
    Some(DmaRegion { virt, phys, len })
}

/// Translate an address inside the physical-memory window back to its
/// physical address. Only valid for pointers derived from [`alloc_dma`].
pub fn dma_virt_to_phys(virt: u64) -> u64 {
    virt.wrapping_sub(physical_memory_offset())
}

