//! A safe Rust wrapper around the vendored QuickJS engine.
//!
//! The engine, the freestanding libc it compiles against and the recipe that
//! builds them all live in `third_party/`; that directory's README is the
//! reference for how it works and what it costs. This module is only the
//! embedder: it owns the runtime and context, converts between Rust strings and
//! JavaScript values, and turns exceptions into `Err`.
//!
//! The browser runs page scripts on this. [`crate::browser::script`] is the
//! embedder that binds the document to it; there is no second engine.
//!
//! # The stack
//!
//! `JS_DEFAULT_STACK_SIZE` is one megabyte and so is the kernel stack, which
//! means a QuickJS runtime left at its default may consume all of it — and what
//! follows is a page fault taken inside the interpreter, or a triple fault if
//! the handler cannot get a frame. There is no way to construct an [`Engine`]
//! without a limit: the glue in `libc-shim/src/quickjs_glue.c` calls
//! `JS_SetMaxStackSize` in the same function as `JS_NewRuntime` and refuses a
//! size of zero, and [`MAX_STACK_SIZE`] here caps it at the largest value that
//! has been measured to leave the rest of the kernel enough room.
//!
//! The address the engine measures against is captured inside `JS_NewRuntime`,
//! i.e. wherever [`Engine::new`] was called from. Create an engine in a shallow
//! frame. Calling `JS_UpdateStackTop` from a deep one would re-anchor the budget
//! below the current position and hand out room that is not there, which is why
//! it is not exposed.
//!
//! # Jobs
//!
//! Promises, `async`/`await` and `queueMicrotask` do not run to completion
//! inside `eval`. They queue a job, and the embedder has to drain the queue.
//! [`Engine::pump_jobs`] is that drain, and [`Engine::eval_settled`] is `eval`
//! followed by one. The browser drains after every script, after every event
//! dispatch, and from its idle path — see `browser::script::Session::pump`.
//!
//! # Time
//!
//! An engine has no deadline until one is set. [`Engine::set_time_limit`] is
//! what stops `while (true) {}` from wedging the machine: the interrupt handler
//! QuickJS calls every ten thousand operations compares the clock against it and
//! raises an *uncatchable* `InternalError: interrupted`, so a page cannot swallow
//! its own runaway loop with a `try`. The limit has to be re-armed before each
//! evaluation, because it is an absolute instant and not a stopwatch.

use alloc::ffi::CString;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::{c_char, c_int};
use core::marker::PhantomData;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

pub mod ffi;
pub mod selftest;

/// The stack budget the engine is given, in bytes.
///
/// 256 KiB buys around 250 levels of JavaScript recursion and 220 of expression
/// nesting, which is far more than a real page uses, and leaves three quarters
/// of the kernel stack for the frames above and below the interpreter — the
/// compositor, `window.rs`, the layout pass, and any native callback JavaScript
/// makes, whose own stack use QuickJS does not count until the next re-entry.
pub const DEFAULT_STACK_SIZE: usize = 256 * 1024;

/// The most any engine may ask for. Above this the point of having a limit at
/// all starts to disappear: the whole kernel stack is 1 MiB, and the call path
/// that reaches `eval` is already several frames deep.
pub const MAX_STACK_SIZE: usize = 512 * 1024;

/// The heap budget the engine is given, in bytes. A runtime with every
/// intrinsic costs about 165 KiB of the kernel's 32 MiB, so this is room for a
/// page with a generous amount of script while still leaving the rest of the
/// kernel — framebuffer, wallpaper, network buffers — its share. Exceeding it
/// is a JavaScript `InternalError: out of memory`, not a kernel failure.
pub const DEFAULT_MEMORY_LIMIT: usize = 8 * 1024 * 1024;

/// Point the C shim at the kernel's clock and serial port, and keep the crate
/// in the link.
///
/// The second part is not rhetoric. If nothing in the kernel names a Rust item
/// from `os101-libc-shim`, rustc drops the crate from the graph and every `JS_*`
/// symbol goes undefined even though `libquickjs.a` is bundled inside the rlib.
/// This call is what prevents that, so it stays even though the two things it
/// installs are individually survivable.
pub fn install() {
    os101_libc_shim::install(crate::rtc::unix_millis, |_stream, bytes| {
        crate::serial::write(bytes)
    });
}

// ── native functions ────────────────────────────────────────────────────────

/// The arguments of a call from JavaScript into Rust.
///
/// Values are read out by conversion rather than handed over, because a
/// `JSValue` is only meaningful with its context and only valid for the
/// duration of the call.
pub struct Args<'a> {
    ctx: *mut ffi::JSContext,
    values: &'a [ffi::JsValue],
}

impl Args<'_> {
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Argument `index` as a string, by the same conversion `String(x)` does.
    /// `None` if there is no such argument or the conversion threw.
    pub fn string(&self, index: usize) -> Option<String> {
        let value = self.values.get(index)?;
        read_cstring(self.ctx, value)
    }

    /// Argument `index` as a boolean, by `Boolean(x)` on the number it converts
    /// to — good enough because the only caller is a binding that passes real
    /// booleans and numbers, never the strings where the two rules differ.
    pub fn boolean(&self, index: usize) -> bool {
        matches!(self.number(index), Some(value) if value != 0.0)
    }

    /// Argument `index` as an integer, which is what every node handle is.
    /// Missing, NaN and non-numeric all read as `fallback`.
    pub fn int(&self, index: usize, fallback: i64) -> i64 {
        match self.number(index) {
            Some(value) if value.is_finite() => value as i64,
            _ => fallback,
        }
    }

    /// Argument `index` as a number, by the same conversion `Number(x)` does.
    pub fn number(&self, index: usize) -> Option<f64> {
        let value = self.values.get(index)?;
        let mut out = 0.0f64;
        // SAFETY: `value` points into the argument array the engine gave us,
        // and `out` is a live local.
        let rc = unsafe { ffi::os101_qjs_to_float64(self.ctx, value, &mut out) };
        if rc < 0 {
            return None;
        }
        Some(out)
    }
}

/// What a native function hands back.
///
/// Deliberately only the scalars, which is what keeps every native free of
/// `JSValue` in its signature. A binding that needs to hand back an object or an
/// array returns a string the JavaScript side turns into one — see
/// `browser/domjs.js`, where the wrappers, the node lists and the event objects
/// are all built in JavaScript from ids and separated strings.
pub enum Return {
    Undefined,
    /// JavaScript `null`, which a DOM binding needs distinct from `undefined`:
    /// `getAttribute` on a missing attribute is `null`, a missing *property* is
    /// `undefined`, and pages test for both.
    Null,
    Bool(bool),
    Int(i32),
    Number(f64),
    Text(String),
}

/// An argument passed the other way, from Rust into a JavaScript function.
pub enum Arg<'a> {
    Undefined,
    Bool(bool),
    Int(i32),
    Number(f64),
    Text(&'a str),
}

/// A Rust function that JavaScript can call.
pub type NativeFn = fn(&Args) -> Return;

/// How many native functions may be exposed to JavaScript at once, across every
/// live engine.
///
/// The table has to be global because the only thing the C trampoline carries is
/// an `int` — that is the whole point of it, since the alternative is a Rust
/// function with `JSValue` in its signature. Entries are claimed by
/// [`Engine::register_global`] and released when that engine is dropped, so a
/// browser opening one page after another does not accumulate them.
///
/// The DOM binding is what sets the size: it installs one native per host
/// operation — around sixty — and the number wants headroom above that rather
/// than a ceiling one new method would hit. At sixteen bytes an entry the whole
/// table is a couple of kilobytes.
const MAX_NATIVES: usize = 192;

/// One exposed function, and how many engines are exposing it.
///
/// Two engines registering the *same* Rust function share a slot rather than
/// taking one each, because the slot number is all the trampoline carries and
/// the same number means the same function to both of them. Without that, a
/// table this size would hold three copies of a sixty-function DOM binding and
/// then refuse the fourth — which is exactly what happens when several pages are
/// alive at once.
#[derive(Clone, Copy)]
struct Slot {
    function: NativeFn,
    users: usize,
}

static NATIVES: Mutex<[Option<Slot>; MAX_NATIVES]> = Mutex::new([None; MAX_NATIVES]);

/// How many slots are in use. The self-test reads it to prove that twenty
/// engines in a row do not each keep one.
pub fn registered_natives() -> usize {
    NATIVES.lock().iter().filter(|entry| entry.is_some()).count()
}

fn claim_slot(function: NativeFn) -> Option<usize> {
    let mut table = NATIVES.lock();
    // Function pointers compare by address, so this finds the slot another
    // engine already installed this exact function into.
    if let Some(shared) = table.iter().position(|entry| {
        matches!(entry, Some(slot) if core::ptr::fn_addr_eq(slot.function, function))
    }) {
        if let Some(Some(slot)) = table.get_mut(shared) {
            slot.users += 1;
        }
        return Some(shared);
    }
    let free = table.iter().position(|entry| entry.is_none())?;
    table[free] = Some(Slot { function, users: 1 });
    Some(free)
}

fn release_slot(slot: usize) {
    let mut table = NATIVES.lock();
    let Some(entry) = table.get_mut(slot) else { return };
    match entry {
        Some(held) if held.users > 1 => held.users -= 1,
        _ => *entry = None,
    }
}

/// The Rust end of the trampoline in `quickjs_glue.c`.
///
/// The C side knows only a slot number, which is what keeps every Rust function
/// exposed to JavaScript free of the by-value `JSValue` signature that
/// `JSCFunction` demands.
///
/// # Safety
/// Called by the engine. `argv` points at `argc` values and `out` at one.
#[no_mangle]
pub unsafe extern "C" fn os101_qjs_native_dispatch(
    ctx: *mut ffi::JSContext,
    slot: c_int,
    argc: c_int,
    argv: *const ffi::JsValue,
    out: *mut ffi::JsValue,
) -> c_int {
    let Ok(slot) = usize::try_from(slot) else {
        return -1;
    };
    let Some(function) = NATIVES.lock().get(slot).copied().flatten().map(|held| held.function)
    else {
        return -1;
    };

    let count = argc.max(0) as usize;
    // SAFETY: the engine passes `argc` readable values, and null with a count
    // of zero, which `from_raw_parts` will not accept — so use an empty slice.
    let values: &[ffi::JsValue] = if count == 0 || argv.is_null() {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(argv, count) }
    };

    let result = function(&Args { ctx, values });
    // SAFETY: `out` is a live JSValue the trampoline owns.
    unsafe { write_return(ctx, out, result) };
    0
}

/// # Safety
/// `out` must point at a writable `JsValue` that nothing else owns yet.
unsafe fn write_return(ctx: *mut ffi::JSContext, out: *mut ffi::JsValue, value: Return) {
    match value {
        Return::Undefined => unsafe { ffi::os101_qjs_undefined(out) },
        Return::Null => unsafe { ffi::os101_qjs_null(out) },
        Return::Bool(flag) => unsafe { ffi::os101_qjs_new_bool(out, flag as c_int) },
        Return::Int(n) => unsafe { ffi::os101_qjs_new_int32(ctx, out, n) },
        Return::Number(x) => unsafe { ffi::os101_qjs_new_float64(ctx, out, x) },
        Return::Text(text) => {
            // The engine copies the bytes, so the borrow ends with this call.
            // A NUL inside would truncate a C string, but the length is passed
            // explicitly so the bytes go over exactly as they are.
            unsafe {
                ffi::os101_qjs_new_string(
                    ctx,
                    text.as_ptr() as *const c_char,
                    text.len(),
                    out,
                )
            };
        }
    }
}

// ── values ──────────────────────────────────────────────────────────────────

/// Read a value as UTF-8, releasing the engine's copy before returning.
///
/// Invalid UTF-8 cannot come out of QuickJS for a well-formed string, but a
/// lone surrogate can, and it arrives as CESU-8; taking it lossily is better
/// than losing the whole value.
fn read_cstring(ctx: *mut ffi::JSContext, value: *const ffi::JsValue) -> Option<String> {
    let mut len = 0usize;
    // SAFETY: `value` is a live JSValue belonging to `ctx`.
    let raw = unsafe { ffi::os101_qjs_to_cstring(ctx, value, &mut len) };
    if raw.is_null() {
        return None;
    }
    // SAFETY: the engine promises `len` readable bytes at `raw`.
    let bytes = unsafe { core::slice::from_raw_parts(raw as *const u8, len) };
    let text = String::from_utf8_lossy(bytes).into_owned();
    // SAFETY: `raw` came from the call above and is not used after this.
    unsafe { ffi::os101_qjs_free_cstring(ctx, raw) };
    Some(text)
}

/// A `JSValue` that is freed when it goes out of scope.
///
/// Every value QuickJS returns is owned by the caller, and forgetting one is a
/// leak that only shows up as a runtime that will not shrink. Tying it to a
/// scope is cheaper than remembering.
struct Local<'a> {
    ctx: *mut ffi::JSContext,
    raw: ffi::JsValue,
    _engine: PhantomData<&'a Engine>,
}

impl Local<'_> {
    fn is_exception(&self) -> bool {
        // SAFETY: `raw` is a live value; the predicate only reads its tag.
        unsafe { ffi::os101_qjs_is_exception(&self.raw) != 0 }
    }

    fn is_nullish(&self) -> bool {
        // SAFETY: `raw` is a live value; the predicate only reads its tag.
        unsafe { ffi::os101_qjs_is_nullish(&self.raw) != 0 }
    }

    fn text(&self) -> Option<String> {
        read_cstring(self.ctx, &self.raw)
    }
}

impl Drop for Local<'_> {
    fn drop(&mut self) {
        // SAFETY: the value was produced by this context and is dropped once.
        unsafe { ffi::os101_qjs_free_value(self.ctx, &self.raw) };
    }
}

// ── the engine ──────────────────────────────────────────────────────────────

/// A QuickJS runtime and its context, freed together.
///
/// One per page is the intended granularity: the runtime owns the heap the
/// scripts allocate from, so dropping it is how a page's script memory is
/// reclaimed in one step rather than traced.
pub struct Engine {
    rt: *mut ffi::JSRuntime,
    ctx: *mut ffi::JSContext,
    /// The native-function slots this engine claimed, given back on drop. The
    /// JavaScript function objects that name them die with the context, so
    /// nothing can dispatch to a released slot afterwards.
    slots: Vec<usize>,
}

impl Engine {
    /// A runtime with every intrinsic, the default stack budget and the default
    /// memory budget.
    pub fn new() -> Result<Self, &'static str> {
        Self::with_limits(DEFAULT_STACK_SIZE, DEFAULT_MEMORY_LIMIT)
    }

    /// As [`Engine::new`], with an explicit stack and heap budget.
    ///
    /// `memory_limit` of 0 means no limit, which is QuickJS's own default and
    /// should be a considered choice rather than an omission. `stack_size` has
    /// no such escape: 0 would disable the check this type exists to enforce.
    pub fn with_limits(stack_size: usize, memory_limit: usize) -> Result<Self, &'static str> {
        if stack_size == 0 {
            return Err("a stack size of zero would disable the overflow check");
        }
        if stack_size > MAX_STACK_SIZE {
            return Err("stack size above 512 KiB does not fit the kernel stack");
        }

        // SAFETY: the glue sets the stack limit before returning, and returns
        // null rather than an unlimited runtime.
        let rt = unsafe { ffi::os101_qjs_new_runtime(stack_size, memory_limit) };
        if rt.is_null() {
            return Err("JS_NewRuntime failed");
        }
        // SAFETY: `rt` is a live runtime with no context yet.
        let ctx = unsafe { ffi::os101_qjs_new_context(rt) };
        if ctx.is_null() {
            // SAFETY: no context was created, so the runtime is free to go.
            unsafe { ffi::os101_qjs_free_runtime(rt) };
            return Err("JS_NewContext failed");
        }
        // SAFETY: `rt` is a live runtime. The handler only reads a static and
        // the clock, so it is safe to call from wherever the engine calls it.
        unsafe { ffi::os101_qjs_install_interrupt_handler(rt) };
        clear_time_limit();

        Ok(Engine { rt, ctx, slots: Vec::new() })
    }

    /// Stop the engine if it is still running `micros` microseconds from now.
    ///
    /// Absolute, not a stopwatch: it has to be called again before each
    /// evaluation. The interruption arrives as an uncatchable
    /// `InternalError: interrupted`, so a page cannot keep looping by wrapping
    /// itself in a `try` — and once it has fired, the next evaluation would fire
    /// immediately too if the deadline were left where it was, which is the
    /// other reason for re-arming.
    pub fn set_time_limit(&self, micros: u64) {
        DEADLINE.store(crate::clock::micros().saturating_add(micros), Ordering::Relaxed);
    }

    /// Let it run as long as it likes. What a fresh engine starts with.
    pub fn clear_time_limit(&self) {
        clear_time_limit();
    }

    /// Evaluate `source` and return its value as a string, or the exception as
    /// a string.
    ///
    /// `filename` is what appears in stack traces and syntax errors; it is a
    /// label, not a path, and nothing tries to open it.
    ///
    /// A script that resolves a Promise returns here before the Promise does.
    /// See [`Engine::eval_settled`].
    pub fn eval(&self, source: &str, filename: &str) -> Result<String, String> {
        // QuickJS reads input[len] and requires it to be NUL, so the source has
        // to be copied into a terminated buffer. An interior NUL would make the
        // engine see a shorter program than we passed the length for, so it is
        // refused here rather than silently truncated.
        let Ok(input) = CString::new(source) else {
            return Err("source contains a NUL byte".to_string());
        };
        let Ok(name) = CString::new(filename) else {
            return Err("filename contains a NUL byte".to_string());
        };

        let mut raw = ffi::JsValue { payload: 0, tag: 0 };
        // SAFETY: both buffers are NUL-terminated and outlive the call, and
        // `raw` is a live local the engine writes its result into.
        unsafe {
            ffi::os101_qjs_eval(
                self.ctx,
                input.as_ptr(),
                source.len(),
                name.as_ptr(),
                ffi::EVAL_TYPE_GLOBAL,
                &mut raw,
            )
        };
        let value = Local { ctx: self.ctx, raw, _engine: PhantomData };

        if value.is_exception() {
            return Err(self.take_exception());
        }
        value
            .text()
            .ok_or_else(|| "the result could not be converted to a string".to_string())
    }

    /// Evaluate `source`, then drain the job queue, so that Promises and
    /// `async` functions the script started have run before returning.
    ///
    /// The value is still the one the script itself produced — a Promise
    /// stringifies as `[object Promise]` whether it has settled or not. The
    /// point is the side effects.
    pub fn eval_settled(&self, source: &str, filename: &str) -> Result<String, String> {
        let value = self.eval(source, filename)?;
        self.pump_jobs()?;
        Ok(value)
    }

    /// Run queued jobs until there are none left, and report how many ran.
    ///
    /// A job that throws stops the drain and is reported; the jobs behind it
    /// stay queued, which matches what a browser does with an unhandled
    /// rejection in a microtask.
    pub fn pump_jobs(&self) -> Result<usize, String> {
        let mut ran = 0usize;
        loop {
            // SAFETY: `rt` is live for as long as `self` is.
            let status = unsafe { ffi::os101_qjs_execute_pending_job(self.rt) };
            if status == 0 {
                return Ok(ran);
            }
            if status < 0 {
                return Err(self.take_exception());
            }
            ran += 1;
            // A job that queues another job is ordinary — every `then` does —
            // but a pair that queue each other would spin here forever, which
            // in a kernel is a hang rather than a slow page.
            if ran > MAX_JOBS_PER_PUMP {
                return Err("the job queue did not drain".to_string());
            }
        }
    }

    /// Whether anything is still waiting for [`Engine::pump_jobs`].
    pub fn has_pending_jobs(&self) -> bool {
        // SAFETY: `rt` is live for as long as `self` is.
        unsafe { ffi::os101_qjs_is_job_pending(self.rt) != 0 }
    }

    /// Call a function on the global object and return its value as a string.
    ///
    /// This is how the browser reaches into the page's own script: dispatching
    /// an event, running the timers that are due, formatting a value. Building
    /// a source string and evaluating it would do the same thing, but every
    /// argument would then have to be escaped as a JavaScript literal — and an
    /// escape bug there is a page injecting into its own script through, say, an
    /// element's text.
    pub fn call_global(&self, name: &str, args: &[Arg]) -> Result<String, String> {
        let Ok(name) = CString::new(name) else {
            return Err("the name contains a NUL byte".to_string());
        };

        // Built into locals first so that every one of them is freed on the way
        // out, including when a later conversion fails.
        let mut owned: Vec<Local> = Vec::with_capacity(args.len());
        for arg in args {
            let mut raw = ffi::JsValue { payload: 0, tag: 0 };
            // SAFETY: `raw` is a live local, and each constructor either fills
            // it in or leaves an exception the callee will notice.
            unsafe {
                match arg {
                    Arg::Undefined => ffi::os101_qjs_undefined(&mut raw),
                    Arg::Bool(flag) => ffi::os101_qjs_new_bool(&mut raw, *flag as c_int),
                    Arg::Int(value) => ffi::os101_qjs_new_int32(self.ctx, &mut raw, *value),
                    Arg::Number(value) => ffi::os101_qjs_new_float64(self.ctx, &mut raw, *value),
                    Arg::Text(text) => ffi::os101_qjs_new_string(
                        self.ctx,
                        text.as_ptr() as *const c_char,
                        text.len(),
                        &mut raw,
                    ),
                }
            }
            owned.push(Local { ctx: self.ctx, raw, _engine: PhantomData });
        }

        let values: Vec<ffi::JsValue> = owned.iter().map(|local| local.raw).collect();
        let mut raw = ffi::JsValue { payload: 0, tag: 0 };
        // SAFETY: `values` holds live values this context owns for the duration
        // of the call, and `raw` is a live local.
        unsafe {
            ffi::os101_qjs_call_global(
                self.ctx,
                name.as_ptr(),
                values.len() as c_int,
                values.as_ptr(),
                &mut raw,
            )
        };
        let result = Local { ctx: self.ctx, raw, _engine: PhantomData };

        if result.is_exception() {
            return Err(self.take_exception());
        }
        result
            .text()
            .ok_or_else(|| "the result could not be converted to a string".to_string())
    }

    /// Expose `function` on the global object under `name`.
    ///
    /// `argc` is the `length` JavaScript sees on the function; it does not
    /// limit what actually arrives, so a native function must still check
    /// [`Args::len`].
    pub fn register_global(
        &mut self,
        name: &str,
        argc: i32,
        function: NativeFn,
    ) -> Result<(), String> {
        let Ok(name) = CString::new(name) else {
            return Err("the name contains a NUL byte".to_string());
        };
        let Some(slot) = claim_slot(function) else {
            return Err("no free native function slots".to_string());
        };
        // SAFETY: `name` is NUL-terminated and outlives the call; `slot` is an
        // index the dispatcher will find occupied.
        let rc = unsafe {
            ffi::os101_qjs_define_global_function(
                self.ctx,
                name.as_ptr(),
                argc as c_int,
                slot as c_int,
            )
        };
        if rc < 0 {
            release_slot(slot);
            return Err("the property could not be defined".to_string());
        }
        self.slots.push(slot);
        Ok(())
    }

    /// Bytes and blocks the engine believes it has allocated.
    ///
    /// Exact rather than approximate here: the shim's `malloc_usable_size`
    /// returns the size that was actually asked for, so QuickJS's running total
    /// does not drift. It excludes the shim's own 16-byte block header and
    /// whatever the kernel allocator charges, so it reads a little under
    /// [`crate::allocator::used`].
    pub fn memory_usage(&self) -> (i64, i64) {
        let mut size = 0i64;
        let mut count = 0i64;
        // SAFETY: `rt` is live; both out-params are live locals.
        unsafe { ffi::os101_qjs_memory_usage(self.rt, &mut size, &mut count) };
        (size, count)
    }

    /// Collect what the script no longer references.
    ///
    /// Reference counting reclaims most garbage as it goes; this is for the
    /// cycles it cannot, which in a DOM are the normal case rather than the
    /// exception — a node referring to its parent and back again.
    pub fn run_gc(&self) {
        // SAFETY: `rt` is live for as long as `self` is.
        unsafe { ffi::os101_qjs_run_gc(self.rt) };
    }

    /// Take the pending exception and describe it.
    ///
    /// Two cases here are not hypothetical and both would otherwise produce a
    /// misleading message. An exception whose own `toString` throws is reachable
    /// from a page, because anything at all can be thrown. And a failure with no
    /// exception set is what QuickJS's out-of-memory path leaves behind — it
    /// wants to raise `InternalError: out of memory`, which needs an allocation
    /// it has just been refused, so it raises nothing; reporting that as `"null"`
    /// would send whoever reads it looking in the wrong place entirely.
    fn take_exception(&self) -> String {
        let mut raw = ffi::JsValue { payload: 0, tag: 0 };
        // SAFETY: `raw` is a live local; the call moves the pending exception
        // into it and clears it on the context.
        unsafe { ffi::os101_qjs_get_exception(self.ctx, &mut raw) };
        let error = Local { ctx: self.ctx, raw, _engine: PhantomData };
        if error.is_nullish() {
            return "the engine failed without setting an exception, which is \
                    what running out of memory looks like"
                .to_string();
        }
        error.text().unwrap_or_else(|| "an exception with no message".to_string())
    }
}

/// The number of jobs one [`Engine::pump_jobs`] will run before deciding the
/// queue is not going to drain. Ten thousand microtasks is far past anything a
/// page does on purpose.
const MAX_JOBS_PER_PUMP: usize = 10_000;

/// Evaluate a snippet in a runtime of its own, with no document attached.
///
/// For the shell's `js` command. A fresh engine per line costs about 165 KiB and
/// a few milliseconds, which is nothing against a person typing, and it means one
/// line cannot leave state behind for the next — including a syntax error.
pub fn eval_standalone(source: &str) -> Result<String, String> {
    let engine = Engine::new()?;
    engine.set_time_limit(STANDALONE_BUDGET_MICROS);
    let result = engine.eval_settled(source, "js");
    engine.clear_time_limit();
    result
}

/// How long a line typed at the shell may run. Longer than a page's budget
/// because there is nobody else waiting for the machine.
const STANDALONE_BUDGET_MICROS: u64 = 5_000_000;

// ── the deadline ────────────────────────────────────────────────────────────

/// The instant, in [`crate::clock::micros`], after which a running script is
/// interrupted. [`NO_DEADLINE`] means never.
///
/// One static for every engine, which is correct here for the same reason the
/// rest of the wrapper is single-threaded: OS101 is uniprocessor and an engine
/// may only be used from the stack it was made on, so two of them cannot be
/// inside `eval` at the same time.
static DEADLINE: AtomicU64 = AtomicU64::new(NO_DEADLINE);

const NO_DEADLINE: u64 = u64::MAX;

fn clear_time_limit() {
    DEADLINE.store(NO_DEADLINE, Ordering::Relaxed);
}

/// Whether the script that is running has outstayed its welcome.
///
/// Called by the glue from QuickJS's interrupt hook every ten thousand bytecode
/// operations, so it has to stay cheap: an atomic load and a `rdtsc`.
///
/// # Safety
/// Nothing unsafe happens here; the signature is `extern "C"` because C calls it.
#[no_mangle]
pub extern "C" fn os101_qjs_interrupt_poll() -> c_int {
    let deadline = DEADLINE.load(Ordering::Relaxed);
    if deadline == NO_DEADLINE {
        return 0;
    }
    // `micros` is zero until the clock is calibrated, which would otherwise read
    // as "the deadline passed long ago" for every script during early boot.
    let now = crate::clock::micros();
    if now == 0 {
        return 0;
    }
    (now > deadline) as c_int
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Order matters: JS_FreeRuntime asserts that every context is gone.
        // SAFETY: both handles came from this engine and are freed once.
        unsafe {
            ffi::os101_qjs_free_context(self.ctx);
            ffi::os101_qjs_free_runtime(self.rt);
        }
        // Only after the context is gone, so that nothing can still reach a
        // slot that has been handed back.
        for slot in self.slots.drain(..) {
            release_slot(slot);
        }
        // A deadline outliving the engine it was set for would interrupt the
        // next one before it had run an instruction.
        clear_time_limit();
    }
}
