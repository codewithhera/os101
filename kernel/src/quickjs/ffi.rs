//! The declarations for `third_party/libc-shim/src/quickjs_glue.c`.
//!
//! Nothing here talks to `<quickjs.h>` directly. Half the API in that header is
//! `static inline` and so has no symbol to bind to, and the other half passes
//! `JSValue` — a sixteen-byte struct — by value. The glue file exports real
//! functions that take and return values through pointers instead, so this
//! module never has to be right about a struct-passing ABI, only about a
//! pointer.
//!
//! [`JsValue`] is still declared, because the arguments a native function
//! receives arrive as an array of them. It is never passed or returned by value
//! across the boundary, and [`super::selftest`] checks its size, its alignment
//! and its field offsets against the C compiler's own answers at boot.

use core::ffi::{c_char, c_int};

/// `JS_EVAL_TYPE_GLOBAL` — ordinary script, not a module.
pub const EVAL_TYPE_GLOBAL: c_int = 0;

/// Opaque handles. QuickJS never exposes the contents of either.
#[repr(C)]
pub struct JSRuntime {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct JSContext {
    _opaque: [u8; 0],
}

/// The engine's tagged value: `struct JSValue { JSValueUnion u; int64_t tag; }`,
/// where the union is `{ uint64_t; double; void *; int64_t }`. Sixteen bytes,
/// eight-byte aligned, with the payload first.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct JsValue {
    pub payload: u64,
    pub tag: i64,
}

const _: () = assert!(core::mem::size_of::<JsValue>() == 16);
const _: () = assert!(core::mem::align_of::<JsValue>() == 8);

extern "C" {
    // ── runtime and context ───────────────────────────────────────────────
    /// Returns null for a zero `stack_size`, which the glue refuses because
    /// zero means "no stack check" to QuickJS. `memory_limit` of 0 leaves the
    /// engine's default, which is unlimited.
    pub fn os101_qjs_new_runtime(stack_size: usize, memory_limit: usize) -> *mut JSRuntime;
    pub fn os101_qjs_free_runtime(rt: *mut JSRuntime);
    pub fn os101_qjs_new_context(rt: *mut JSRuntime) -> *mut JSContext;
    pub fn os101_qjs_free_context(ctx: *mut JSContext);
    pub fn os101_qjs_run_gc(rt: *mut JSRuntime);
    pub fn os101_qjs_memory_usage(rt: *mut JSRuntime, size: *mut i64, count: *mut i64);
    pub fn os101_qjs_execute_pending_job(rt: *mut JSRuntime) -> c_int;
    pub fn os101_qjs_is_job_pending(rt: *mut JSRuntime) -> c_int;
    /// Point `JS_SetInterruptHandler` at the glue's handler, which asks
    /// [`super::interrupt_poll`] whether the deadline has passed.
    pub fn os101_qjs_install_interrupt_handler(rt: *mut JSRuntime);

    // ── layout proof ──────────────────────────────────────────────────────
    pub fn os101_qjs_value_size() -> usize;
    pub fn os101_qjs_value_align() -> usize;
    pub fn os101_qjs_value_roundtrip(payload: u64, tag: i64, out: *mut JsValue);
    pub fn os101_qjs_value_check(value: *const JsValue, payload: u64, tag: i64) -> c_int;

    // ── values ────────────────────────────────────────────────────────────
    pub fn os101_qjs_free_value(ctx: *mut JSContext, value: *const JsValue);
    pub fn os101_qjs_is_exception(value: *const JsValue) -> c_int;
    pub fn os101_qjs_is_nullish(value: *const JsValue) -> c_int;
    pub fn os101_qjs_undefined(out: *mut JsValue);
    pub fn os101_qjs_null(out: *mut JsValue);
    pub fn os101_qjs_new_int32(ctx: *mut JSContext, out: *mut JsValue, value: i32);
    pub fn os101_qjs_new_float64(ctx: *mut JSContext, out: *mut JsValue, value: f64);
    pub fn os101_qjs_new_bool(out: *mut JsValue, value: c_int);
    pub fn os101_qjs_new_string(
        ctx: *mut JSContext,
        bytes: *const c_char,
        len: usize,
        out: *mut JsValue,
    );
    /// The UTF-8 form of a value, allocated by the engine. Null if converting
    /// it threw. Release it with [`os101_qjs_free_cstring`].
    pub fn os101_qjs_to_cstring(
        ctx: *mut JSContext,
        value: *const JsValue,
        len: *mut usize,
    ) -> *const c_char;
    pub fn os101_qjs_free_cstring(ctx: *mut JSContext, text: *const c_char);
    pub fn os101_qjs_to_float64(
        ctx: *mut JSContext,
        value: *const JsValue,
        out: *mut f64,
    ) -> c_int;

    // ── eval ──────────────────────────────────────────────────────────────
    /// `input` must be NUL-terminated at `input[len]`; QuickJS requires it.
    pub fn os101_qjs_eval(
        ctx: *mut JSContext,
        input: *const c_char,
        len: usize,
        filename: *const c_char,
        flags: c_int,
        out: *mut JsValue,
    );
    pub fn os101_qjs_get_exception(ctx: *mut JSContext, out: *mut JsValue);
    /// Call a function found on the global object. `argv` stays owned by the
    /// caller; `out` becomes theirs.
    pub fn os101_qjs_call_global(
        ctx: *mut JSContext,
        name: *const c_char,
        argc: c_int,
        argv: *const JsValue,
        out: *mut JsValue,
    );

    // ── native functions ──────────────────────────────────────────────────
    pub fn os101_qjs_define_global_function(
        ctx: *mut JSContext,
        name: *const c_char,
        argc: c_int,
        slot: c_int,
    ) -> c_int;
}
