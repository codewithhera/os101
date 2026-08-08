//! FFI declarations for the vendored TinyCC (`libtcc.h`).

#![allow(non_camel_case_types)]

use core::ffi::{c_char, c_int, c_void};

pub type TCCState = c_void;

pub const TCC_OUTPUT_MEMORY: c_int = 1;
pub const TCC_OUTPUT_EXE: c_int = 2;
pub const TCC_OUTPUT_DLL: c_int = 3;
pub const TCC_OUTPUT_OBJ: c_int = 4;
pub const TCC_RELOCATE_AUTO: *mut c_void = 1 as *mut c_void;

type ErrorFunc = Option<unsafe extern "C" fn(*mut c_void, *const c_char)>;

extern "C" {
    pub fn tcc_new() -> *mut TCCState;
    pub fn tcc_delete(s: *mut TCCState);
    pub fn tcc_set_lib_path(s: *mut TCCState, path: *const c_char);
    pub fn tcc_set_error_func(s: *mut TCCState, opaque: *mut c_void, f: ErrorFunc);
    pub fn tcc_set_options(s: *mut TCCState, str: *const c_char) -> c_int;
    pub fn tcc_add_include_path(s: *mut TCCState, pathname: *const c_char) -> c_int;
    pub fn tcc_add_sysinclude_path(s: *mut TCCState, pathname: *const c_char) -> c_int;
    pub fn tcc_define_symbol(s: *mut TCCState, sym: *const c_char, value: *const c_char);
    pub fn tcc_add_file(s: *mut TCCState, filename: *const c_char) -> c_int;
    pub fn tcc_compile_string(s: *mut TCCState, buf: *const c_char) -> c_int;
    pub fn tcc_set_output_type(s: *mut TCCState, output_type: c_int) -> c_int;
    pub fn tcc_add_library_path(s: *mut TCCState, pathname: *const c_char) -> c_int;
    pub fn tcc_add_library(s: *mut TCCState, libraryname: *const c_char) -> c_int;
    pub fn tcc_add_symbol(s: *mut TCCState, name: *const c_char, val: *const c_void) -> c_int;
    pub fn tcc_output_file(s: *mut TCCState, filename: *const c_char) -> c_int;
    pub fn tcc_relocate(s: *mut TCCState, ptr: *mut c_void) -> c_int;
    pub fn tcc_get_symbol(s: *mut TCCState, name: *const c_char) -> *mut c_void;
}
