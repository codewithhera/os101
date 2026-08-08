//! What happens when QuickJS decides it cannot continue.
//!
//! Both entry points here end in a Rust panic, which the kernel's panic handler
//! turns into a message and a halt. That is the right answer for a kernel: an
//! assertion inside the engine has caught an invariant break in a single address
//! space with no process boundary to contain it, so continuing would corrupt
//! something else instead.
//!
//! The vendored engine's roughly 160 assertions are left enabled, matching
//! Bellard's own release build. Defining NDEBUG in the build recipe removes
//! them, but measurement says that buys under 8 KiB of code out of 900 — so
//! there is no size argument for turning them off, only a speed one.

use core::ffi::c_char;

/// Read a NUL-terminated C string for a panic message, tolerating both a null
/// pointer and non-UTF-8 bytes, because the panic path is the last place that
/// should introduce a second failure.
///
/// # Safety
/// `ptr` is either null or points at a NUL-terminated string.
unsafe fn c_str(ptr: *const c_char) -> &'static str {
    if ptr.is_null() {
        return "<null>";
    }
    let mut len = 0usize;
    // Bounded so that a missing terminator cannot walk the whole address space.
    while len < 256 && unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
    match core::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => "<not utf-8>",
    }
}

/// The target of the `assert` macro in the shim's <assert.h>.
///
/// # Safety
/// `expr` and `file` are either null or NUL-terminated strings.
#[no_mangle]
pub unsafe extern "C" fn os101_shim_assert_fail(
    expr: *const c_char,
    file: *const c_char,
    line: i32,
) -> ! {
    panic!(
        "quickjs assertion failed: {} at {}:{}",
        unsafe { c_str(expr) },
        unsafe { c_str(file) },
        line
    );
}

// Hidden from the test build: `cargo test` links the standard library, whose own
// abort path goes through the C `abort`, and shadowing it with something that
// panics would turn a double panic into an infinite one.
#[cfg(not(test))]
#[no_mangle]
pub extern "C" fn abort() -> ! {
    panic!("quickjs called abort()");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_string_reads_as_a_placeholder() {
        assert_eq!(unsafe { c_str(core::ptr::null()) }, "<null>");
    }

    #[test]
    fn an_ordinary_string_reads_back() {
        assert_eq!(unsafe { c_str(c"x == y".as_ptr()) }, "x == y");
    }

    #[test]
    fn a_string_with_no_terminator_stops_at_the_bound() {
        let junk = [b'a' as c_char; 512];
        assert_eq!(unsafe { c_str(junk.as_ptr()) }.len(), 256);
    }
}
