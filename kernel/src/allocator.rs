use core::alloc::Layout;
use linked_list_allocator::LockedHeap;
use crate::memory::{self, FrameAllocator};

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub const HEAP_START: usize = 0x_4444_4444_0000;
// 192 MiB. The framebuffer back buffer (~4.5 MB at the 1348x840 virtual
// canvas — see `framebuffer::FramebufferWriter`) and the cached wallpaper
// (another ~4.5 MB) scale with the screen; the rest is the network stack's
// buffers, the compositor's per-window surfaces and app allocations.
pub const HEAP_SIZE: usize = 192 * 1024 * 1024;

pub fn init_heap(
    l4_table: &mut memory::PageTable,
    frame_allocator: &mut impl FrameAllocator,
    physical_memory_offset: u64,
) -> Result<(), &'static str> {
    let heap_start = HEAP_START as u64;
    let heap_end = heap_start + HEAP_SIZE as u64;

    for page_addr in (heap_start..heap_end).step_by(4096) {
        let frame = frame_allocator.allocate_frame().ok_or("out of frames for heap")?;
        unsafe {
            memory::map_page(
                page_addr,
                frame,
                memory::flags::WRITABLE,
                l4_table,
                physical_memory_offset,
                frame_allocator,
            );
            // We usually don't need flush_tlb for new mappings if we're not 
            // replacing existing ones, but it's safer.
            memory::flush_tlb(page_addr);
        }
    }

    unsafe {
        ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE);
    }

    Ok(())
}

/// Returns the current amount of used memory in bytes.
pub fn used() -> usize {
    ALLOCATOR.lock().used()
}

/// Returns the total size of the heap in bytes.
pub fn size() -> usize {
    ALLOCATOR.lock().size()
}

/// Allocate one block and report whether it came back on the boundary the
/// layout asked for, then give it straight back.
fn honours_align(align: usize) -> bool {
    let Ok(layout) = Layout::from_size_align(96, align) else {
        return false;
    };
    // SAFETY: non-zero size, and the block is freed with the layout it was
    // allocated with before this returns.
    unsafe {
        let ptr = alloc::alloc::alloc(layout);
        if ptr.is_null() {
            return false;
        }
        let aligned = (ptr as usize) % align == 0;
        alloc::alloc::dealloc(ptr, layout);
        aligned
    }
}

/// Boot-time checks.
///
/// SSE is why this is worth a self-test. `movaps` and the rest of the aligned
/// moves raise #GP on an address that is not a multiple of 16, and the
/// compiler emits them against any allocation whose `Layout` promises that
/// much — a `Vec` of a 16-byte-aligned type, a boxed SIMD-shaped struct. A
/// first-fit allocator that ignored `align` would still work for every
/// pointer-aligned request in the kernel and then fail on the first of these,
/// under load, as a #GP with no obvious cause.
pub fn selftest() -> crate::selftest::Report {
    let mut report = crate::selftest::Report::new();

    report.check("heap base is 16-byte aligned", HEAP_START % 16 == 0);
    report.check("16-byte alignment honoured", honours_align(16));
    report.check("32-byte alignment honoured", honours_align(32));
    report.check("64-byte alignment honoured", honours_align(64));

    // With the free list already on a 16-byte boundary every request looks
    // aligned whether the allocator honours `align` or not. Skew it with a
    // one-byte block first so the answer means something.
    let skew = Layout::from_size_align(1, 1).expect("1/1 is a valid layout");
    // SAFETY: as `honours_align` — non-zero size, freed with the same layout.
    let skewed = unsafe {
        let ptr = alloc::alloc::alloc(skew);
        let ok = !ptr.is_null() && honours_align(16);
        if !ptr.is_null() {
            alloc::alloc::dealloc(ptr, skew);
        }
        ok
    };
    report.check("16-byte alignment honoured after an odd block", skewed);

    report
}
