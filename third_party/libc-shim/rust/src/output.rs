//! Where the C shim's printf output goes.
//!
//! QuickJS only writes to a stream from its diagnostic paths — the memory-usage
//! dump, and the bytecode dumps that are compiled out by default — so the
//! default of discarding the bytes is safe. The kernel installs a writer that
//! forwards to the 16550 serial port, which puts anything the engine says into
//! the QEMU log next to the rest of the kernel's output.

use core::sync::atomic::{AtomicUsize, Ordering};

/// A sink for diagnostic bytes. `stream` is 1 for stdout and 2 for stderr,
/// matching the file descriptor numbers, because that is the only distinction
/// the C side makes.
pub type WriteFn = fn(stream: i32, bytes: &[u8]);

static WRITER: AtomicUsize = AtomicUsize::new(0);

/// Hand the shim somewhere to write. Called once, from kernel start-up.
pub fn set_writer(writer: WriteFn) {
    WRITER.store(writer as usize, Ordering::Release);
}

/// # Safety
/// `buf` must point at `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn os101_shim_write_bytes(
    stream: i32,
    buf: *const u8,
    len: usize,
) {
    let raw = WRITER.load(Ordering::Acquire);
    if raw == 0 || buf.is_null() || len == 0 {
        return;
    }
    let writer: WriteFn = unsafe { core::mem::transmute::<usize, WriteFn>(raw) };
    writer(stream, unsafe { core::slice::from_raw_parts(buf, len) });
}
