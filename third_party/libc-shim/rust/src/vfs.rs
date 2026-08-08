//! VFS hooks for the C shim's fopen/open family.
//!
//! TinyCC reads sources and writes ELF output through the libc shim. The C
//! side calls these symbols; the kernel installs real implementations that
//! talk to `crate::fs`. Function pointers are stored as `AtomicUsize` the
//! same way the clock and serial writer are — this crate has no `spin`
//! dependency on purpose.

use core::ffi::{c_char, c_int, c_uchar};
use core::ptr;
use core::slice;
use core::sync::atomic::{AtomicUsize, Ordering};

type ReadFn = fn(&str) -> Result<alloc::vec::Vec<u8>, &'static str>;
type WriteFn = fn(&str, &[u8]) -> Result<(), &'static str>;
type RemoveFn = fn(&str) -> Result<(), &'static str>;
type ExistsFn = fn(&str) -> bool;

static READ: AtomicUsize = AtomicUsize::new(0);
static WRITE: AtomicUsize = AtomicUsize::new(0);
static REMOVE: AtomicUsize = AtomicUsize::new(0);
static EXISTS: AtomicUsize = AtomicUsize::new(0);

/// Point the shim's file I/O at the kernel VFS.
pub fn install(read: ReadFn, write: WriteFn, remove: RemoveFn, exists: ExistsFn) {
    READ.store(read as usize, Ordering::Release);
    WRITE.store(write as usize, Ordering::Release);
    REMOVE.store(remove as usize, Ordering::Release);
    EXISTS.store(exists as usize, Ordering::Release);
}

fn cstr<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    unsafe {
        let mut len = 0usize;
        while *p.add(len) != 0 {
            len += 1;
            if len > 4096 {
                return None;
            }
        }
        core::str::from_utf8(slice::from_raw_parts(p as *const u8, len)).ok()
    }
}

#[no_mangle]
pub unsafe extern "C" fn os101_vfs_read_file(
    path: *const c_char,
    out_data: *mut *mut c_uchar,
    out_len: *mut usize,
) -> c_int {
    let Some(path) = cstr(path) else {
        return -1;
    };
    let raw = READ.load(Ordering::Acquire);
    if raw == 0 {
        return -1;
    }
    let read: ReadFn = unsafe { core::mem::transmute(raw) };
    let Ok(bytes) = read(path) else {
        return -1;
    };
    let len = bytes.len();
    let buf = unsafe {
        extern "C" {
            fn malloc(size: usize) -> *mut core::ffi::c_void;
        }
        let p = malloc(if len == 0 { 1 } else { len }) as *mut u8;
        if p.is_null() {
            return -1;
        }
        if len > 0 {
            ptr::copy_nonoverlapping(bytes.as_ptr(), p, len);
        }
        p
    };
    unsafe {
        *out_data = buf;
        *out_len = len;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn os101_vfs_write_file(
    path: *const c_char,
    data: *const c_uchar,
    len: usize,
) -> c_int {
    let Some(path) = cstr(path) else {
        return -1;
    };
    let raw = WRITE.load(Ordering::Acquire);
    if raw == 0 {
        return -1;
    }
    let write: WriteFn = unsafe { core::mem::transmute(raw) };
    let bytes = if len == 0 || data.is_null() {
        &[][..]
    } else {
        unsafe { slice::from_raw_parts(data, len) }
    };
    match write(path, bytes) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn os101_vfs_remove_file(path: *const c_char) -> c_int {
    let Some(path) = cstr(path) else {
        return -1;
    };
    let raw = REMOVE.load(Ordering::Acquire);
    if raw == 0 {
        return -1;
    }
    let remove: RemoveFn = unsafe { core::mem::transmute(raw) };
    match remove(path) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn os101_vfs_file_exists(path: *const c_char) -> c_int {
    let Some(path) = cstr(path) else {
        return -1;
    };
    let raw = EXISTS.load(Ordering::Acquire);
    if raw == 0 {
        return -1;
    }
    let exists: ExistsFn = unsafe { core::mem::transmute(raw) };
    if exists(path) {
        0
    } else {
        -1
    }
}
