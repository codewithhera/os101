//! The Rust half of the freestanding C environment that QuickJS runs in.
//!
//! QuickJS expects a hosted C library. This OS has none, so the environment is
//! split in two: `third_party/libc-shim/src/*.c` supplies the parts that are
//! pure computation (string scanning, printf), and this crate supplies the parts
//! that can only come from the kernel — the heap, the clock, the serial port and
//! the panic path. Everything here is exported under a C name and is called only
//! from the vendored engine or from the C shim beside it.
//!
//! Nothing in this crate touches the kernel directly. The two things it cannot
//! invent — what time it is and where diagnostic output goes — are function
//! pointers the kernel installs at boot, so that this crate stays buildable and
//! testable on its own.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]

extern crate alloc;

pub mod clock;
pub mod malloc;
pub mod math;
pub mod output;
pub mod panic;
pub mod vfs;

/// Point the shim at the kernel's clock and serial port.
///
/// Until this runs, `Date.now()` inside QuickJS reports a fixed timestamp and
/// anything the engine prints is discarded. Both are survivable, which is why
/// this is a separate call rather than a hard requirement — a JavaScript
/// evaluation in a unit test does not need either.
pub fn install(clock: clock::ClockFn, writer: output::WriteFn) {
    clock::set_clock(clock);
    output::set_writer(writer);
}

/// Point the shim's `fopen`/`open` family at the kernel VFS.
///
/// Required before TinyCC can read sources or write ELF output. Separate from
/// [`install`] so QuickJS boots without depending on the filesystem.
pub fn install_vfs(
    read: fn(&str) -> Result<alloc::vec::Vec<u8>, &'static str>,
    write: fn(&str, &[u8]) -> Result<(), &'static str>,
    remove: fn(&str) -> Result<(), &'static str>,
    exists: fn(&str) -> bool,
) {
    vfs::install(read, write, remove, exists);
}
