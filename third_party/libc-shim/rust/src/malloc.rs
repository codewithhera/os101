//! malloc, realloc, free and malloc_usable_size on top of the kernel's heap.
//!
//! C's `free` carries no size and Rust's deallocator demands one, so every block
//! gets a header holding the size that was asked for. That header has to be
//! sixteen bytes rather than eight: the x86_64 ABI promises malloc'd memory is
//! aligned to `max_align_t`, code generators rely on it when they emit aligned
//! SSE stores, and QuickJS hands these blocks straight to the interpreter as
//! arrays of sixteen-byte JSValues.
//!
//! Storing the requested size rather than the rounded-up capacity is what lets
//! `malloc_usable_size` be exact, which in turn is what makes
//! `JS_SetMemoryLimit` mean something — QuickJS adds the usable size on every
//! allocation and subtracts it on every free, so an estimate would drift until
//! the limit fired at the wrong time.

use alloc::alloc::{alloc, dealloc, realloc as rust_realloc, Layout};
use core::ptr;

const HEADER: usize = 16;
const ALIGN: usize = 16;

/// Written into the second header word so that a free of a pointer this shim
/// never returned is caught here rather than corrupting the heap's free list.
const MAGIC: u64 = 0x4f53_3130_314a_5300; // "OS101JS\0"

/// The largest block we will even try to allocate. The kernel heap is 32 MiB, so
/// a request past that is a bug or an attacker-supplied length rather than
/// something worth asking the allocator about; returning null lets QuickJS raise
/// a JavaScript OOM error instead.
const MAX_ALLOC: usize = 64 * 1024 * 1024;

/// # Safety
/// `user` must be a pointer this module returned and has not yet freed.
unsafe fn header_of(user: *mut u8) -> *mut u64 {
    unsafe { user.sub(HEADER) as *mut u64 }
}

/// # Safety
/// `user` must be a pointer this module returned and has not yet freed.
unsafe fn stored_size(user: *mut u8) -> usize {
    let head = unsafe { header_of(user) };
    let magic = unsafe { ptr::read(head.add(1)) };
    if magic != MAGIC {
        panic!("libc shim: free/realloc of a pointer it did not allocate");
    }
    unsafe { ptr::read(head) as usize }
}

fn layout_for(size: usize) -> Layout {
    // from_size_align only fails on an overflowing size, which MAX_ALLOC has
    // already excluded, so the unwrap here cannot fire.
    Layout::from_size_align(HEADER + size, ALIGN).expect("libc shim: bad layout")
}

fn allocate(size: usize) -> *mut u8 {
    // A zero-byte request is undefined for Rust's allocator, and QuickJS
    // asserts against making one, but a distinct non-null pointer is what C
    // callers expect so the request is rounded up rather than refused.
    let want = if size == 0 { 1 } else { size };
    if want > MAX_ALLOC {
        return ptr::null_mut();
    }
    let base = unsafe { alloc(layout_for(want)) };
    if base.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        ptr::write(base as *mut u64, want as u64);
        ptr::write((base as *mut u64).add(1), MAGIC);
        base.add(HEADER)
    }
}

/// # Safety
/// `user` must be a live pointer from [`allocate`].
unsafe fn release(user: *mut u8) {
    let size = unsafe { stored_size(user) };
    let base = unsafe { user.sub(HEADER) };
    unsafe { dealloc(base, layout_for(size)) };
}

/// # Safety
/// `user` must be a live pointer from [`allocate`].
unsafe fn resize(user: *mut u8, size: usize) -> *mut u8 {
    let old = unsafe { stored_size(user) };
    let want = if size == 0 { 1 } else { size };
    if want > MAX_ALLOC {
        return ptr::null_mut();
    }
    let base = unsafe { user.sub(HEADER) };
    let grown = unsafe { rust_realloc(base, layout_for(old), HEADER + want) };
    if grown.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        ptr::write(grown as *mut u64, want as u64);
        grown.add(HEADER)
    }
}

// The C-visible names are hidden from the test build on purpose. `cargo test`
// links the standard library, whose allocator is itself `malloc`; exporting our
// own would make Rust's allocator call this module, which calls Rust's
// allocator, forever. The tests exercise the functions above instead, which is
// the whole of the logic — the wrappers below only adapt the calling convention.
#[cfg(not(test))]
mod exports {
    use super::*;

    #[no_mangle]
    pub extern "C" fn malloc(size: usize) -> *mut u8 {
        allocate(size)
    }

    /// # Safety
    /// `ptr` is either null or a pointer previously returned by `malloc` or
    /// `realloc` in this module.
    #[no_mangle]
    pub unsafe extern "C" fn free(ptr: *mut u8) {
        if !ptr.is_null() {
            unsafe { release(ptr) };
        }
    }

    /// # Safety
    /// `ptr` is either null or a pointer previously returned by `malloc` or
    /// `realloc` in this module.
    #[no_mangle]
    pub unsafe extern "C" fn realloc(ptr: *mut u8, size: usize) -> *mut u8 {
        if ptr.is_null() {
            return allocate(size);
        }
        // C's realloc(p, 0) may free and return null; QuickJS's own wrapper
        // handles the zero case before it gets here, so the block is merely
        // shrunk to its minimum and stays live.
        unsafe { resize(ptr, size) }
    }

    /// # Safety
    /// `ptr` is either null or a live pointer from this module.
    #[no_mangle]
    pub unsafe extern "C" fn malloc_usable_size(ptr: *const u8) -> usize {
        if ptr.is_null() {
            return 0;
        }
        unsafe { stored_size(ptr as *mut u8) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usable_size_is_exactly_what_was_asked_for() {
        for size in [1usize, 7, 8, 16, 17, 1000, 65536] {
            let p = allocate(size);
            assert!(!p.is_null());
            assert_eq!(unsafe { stored_size(p) }, size);
            unsafe { release(p) };
        }
    }

    #[test]
    fn blocks_are_sixteen_byte_aligned() {
        let mut live = alloc::vec::Vec::new();
        for size in 1..64usize {
            let p = allocate(size);
            assert_eq!(p as usize % ALIGN, 0, "size {size} came back misaligned");
            live.push(p);
        }
        for p in live {
            unsafe { release(p) };
        }
    }

    #[test]
    fn contents_survive_a_grow_and_a_shrink() {
        let p = allocate(4);
        unsafe { ptr::copy_nonoverlapping(b"abcd".as_ptr(), p, 4) };

        let p = unsafe { resize(p, 4096) };
        assert_eq!(unsafe { core::slice::from_raw_parts(p, 4) }, b"abcd");
        assert_eq!(unsafe { stored_size(p) }, 4096);

        let p = unsafe { resize(p, 2) };
        assert_eq!(unsafe { core::slice::from_raw_parts(p, 2) }, b"ab");
        assert_eq!(unsafe { stored_size(p) }, 2);
        unsafe { release(p) };
    }

    #[test]
    fn an_absurd_request_is_refused_rather_than_attempted() {
        assert!(allocate(MAX_ALLOC + 1).is_null());
        assert!(allocate(usize::MAX).is_null());
    }

    #[test]
    fn a_zero_byte_request_still_yields_a_distinct_block() {
        let a = allocate(0);
        let b = allocate(0);
        assert!(!a.is_null() && !b.is_null());
        assert_ne!(a, b);
        unsafe { release(a) };
        unsafe { release(b) };
    }

    #[test]
    #[should_panic(expected = "did not allocate")]
    fn freeing_a_foreign_pointer_is_caught() {
        let mut junk = [0u64; 8];
        unsafe { release((junk.as_mut_ptr() as *mut u8).add(HEADER)) };
    }
}
