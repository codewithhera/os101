/*
 * Non-inline wrappers for the parts of QuickJS's API that Rust cannot reach.
 *
 * Two separate problems are solved here, and neither is about the kernel.
 *
 * The first is that a good half of the names in <quickjs.h> are `static inline`
 * — JS_FreeValue, JS_IsException, JS_NewInt32, JS_ToCString, JS_NewCFunction and
 * every tag predicate among them. They have no symbol to declare `extern` in
 * Rust, so each one that the embedder needs gets a real function here.
 *
 * The second is `JSValue` itself. It is a sixteen-byte struct of a union and an
 * int64, passed and returned by value throughout the API, and while Rust's
 * `#[repr(C)]` does classify it the same way clang does on x86-64, a mistake
 * there produces wrong numbers rather than a link error. So no JSValue ever
 * crosses this boundary by value: everything below takes and returns them
 * through pointers, and `os101_qjs_value_roundtrip` exists so the Rust side can
 * prove at boot that the two halves agree about the layout.
 *
 * The naming is `os101_qjs_*` rather than `JS_*` so that nothing here can be
 * mistaken for part of the vendored engine, which is unpatched and stays that
 * way.
 */
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#include "quickjs.h"

/* ── runtime and context ─────────────────────────────────────────────────── */

/*
 * The only way to get a runtime.
 *
 * JS_SetMaxStackSize is called here, in the same function, because the default
 * QuickJS compiles in is JS_DEFAULT_STACK_SIZE — one megabyte — and the kernel
 * stack is also exactly one megabyte. A runtime left at the default is
 * permitted to eat the whole stack, and the failure is a page fault taken
 * inside the interpreter with no frame left to report it from. Splitting this
 * into two calls would make forgetting the second one possible; not splitting
 * it makes it impossible.
 *
 * `stack_size` of 0 would disable the check, which is exactly what this exists
 * to prevent, so it is rejected rather than honoured.
 *
 * `memory_limit` of 0 leaves QuickJS's default, which is no limit.
 *
 * The stack top the engine measures against is captured by
 * __builtin_frame_address(0) inside JS_NewRuntime, i.e. this call's frame. A
 * runtime created in a shallow frame and used from a deep one therefore has
 * less room than it thinks; one created deep and used shallow has more. Create
 * it near the top of the stack and leave JS_UpdateStackTop alone.
 */
JSRuntime *os101_qjs_new_runtime(size_t stack_size, size_t memory_limit)
{
    JSRuntime *rt;

    if (stack_size == 0)
        return NULL;

    rt = JS_NewRuntime();
    if (rt == NULL)
        return NULL;
    JS_SetMaxStackSize(rt, stack_size);
    if (memory_limit != 0)
        JS_SetMemoryLimit(rt, memory_limit);
    return rt;
}

void os101_qjs_free_runtime(JSRuntime *rt)
{
    JS_FreeRuntime(rt);
}

JSContext *os101_qjs_new_context(JSRuntime *rt)
{
    return JS_NewContext(rt);
}

void os101_qjs_free_context(JSContext *ctx)
{
    JS_FreeContext(ctx);
}

void os101_qjs_run_gc(JSRuntime *rt)
{
    JS_RunGC(rt);
}

/*
 * Bound how long a script may run.
 *
 * QuickJS calls the handler every JS_INTERRUPT_COUNTER_INIT (10,000) bytecode
 * operations and on every backward branch, and a non-zero answer raises
 * `InternalError: interrupted` with the uncatchable flag set — so a page cannot
 * wrap its own infinite loop in a try/catch and carry on. The Rust side decides
 * against a deadline; all that is needed here is somewhere for the function
 * pointer to live, since JSInterruptHandler takes a JSRuntime * that Rust has
 * no reason to see.
 */
extern int os101_qjs_interrupt_poll(void);

static int os101_qjs_interrupt(JSRuntime *rt, void *opaque)
{
    (void)rt;
    (void)opaque;
    return os101_qjs_interrupt_poll();
}

void os101_qjs_install_interrupt_handler(JSRuntime *rt)
{
    JS_SetInterruptHandler(rt, os101_qjs_interrupt, NULL);
}

/* The engine's own accounting, which is exact against this shim because
   malloc_usable_size returns the size that was actually requested. */
void os101_qjs_memory_usage(JSRuntime *rt, int64_t *size, int64_t *count)
{
    JSMemoryUsage usage;

    JS_ComputeMemoryUsage(rt, &usage);
    if (size != NULL)
        *size = usage.malloc_size;
    if (count != NULL)
        *count = usage.malloc_count;
}

/* 1 if a job ran, 0 if the queue was empty, negative if the job threw. */
int os101_qjs_execute_pending_job(JSRuntime *rt)
{
    JSContext *unused;

    return JS_ExecutePendingJob(rt, &unused);
}

int os101_qjs_is_job_pending(JSRuntime *rt)
{
    return JS_IsJobPending(rt) ? 1 : 0;
}

/* ── values ──────────────────────────────────────────────────────────────── */

size_t os101_qjs_value_size(void)
{
    return sizeof(JSValue);
}

size_t os101_qjs_value_align(void)
{
    return _Alignof(JSValue);
}

/*
 * Build a JSValue from raw halves and read one back, so that the Rust
 * declaration can be checked against this compiler's idea of the struct rather
 * than assumed to match it. A wrong offset here is the difference between a
 * tag and a payload, which the engine would follow as a pointer.
 */
void os101_qjs_value_roundtrip(uint64_t payload, int64_t tag, JSValue *out)
{
    JSValue v;

    v.u.uint64 = payload;
    v.tag = tag;
    *out = v;
}

int os101_qjs_value_check(const JSValue *v, uint64_t payload, int64_t tag)
{
    return v->u.uint64 == payload && v->tag == tag;
}

void os101_qjs_free_value(JSContext *ctx, const JSValue *v)
{
    JS_FreeValue(ctx, *v);
}

int os101_qjs_is_exception(const JSValue *v)
{
    return JS_IsException(*v) ? 1 : 0;
}

/*
 * True for null and for undefined, which is what a pending exception reads as
 * when the engine failed without managing to set one — its out-of-memory path
 * suppresses the InternalError it would otherwise raise, because raising it
 * needs an allocation it has just been refused.
 */
int os101_qjs_is_nullish(const JSValue *v)
{
    return (JS_IsNull(*v) || JS_IsUndefined(*v)) ? 1 : 0;
}

void os101_qjs_undefined(JSValue *out)
{
    *out = JS_UNDEFINED;
}

/* Distinct from undefined on purpose: `getAttribute` on an attribute that is
   not there is null, and pages test for it. */
void os101_qjs_null(JSValue *out)
{
    *out = JS_NULL;
}

void os101_qjs_new_int32(JSContext *ctx, JSValue *out, int32_t value)
{
    *out = JS_NewInt32(ctx, value);
}

void os101_qjs_new_float64(JSContext *ctx, JSValue *out, double value)
{
    *out = JS_NewFloat64(ctx, value);
}

void os101_qjs_new_bool(JSValue *out, int value)
{
    *out = value ? JS_TRUE : JS_FALSE;
}

void os101_qjs_new_string(JSContext *ctx, const char *bytes, size_t len,
                          JSValue *out)
{
    *out = JS_NewStringLen(ctx, bytes, len);
}

/* The UTF-8 form of any value, allocated by the engine; free it with
   os101_qjs_free_cstring. NULL if the conversion itself threw. */
const char *os101_qjs_to_cstring(JSContext *ctx, const JSValue *v, size_t *plen)
{
    return JS_ToCStringLen2(ctx, plen, *v, 0);
}

void os101_qjs_free_cstring(JSContext *ctx, const char *text)
{
    JS_FreeCString(ctx, text);
}

int os101_qjs_to_float64(JSContext *ctx, const JSValue *v, double *out)
{
    return JS_ToFloat64(ctx, out, *v);
}

/* ── eval and exceptions ─────────────────────────────────────────────────── */

void os101_qjs_eval(JSContext *ctx, const char *input, size_t len,
                    const char *filename, int flags, JSValue *out)
{
    *out = JS_Eval(ctx, input, len, filename, flags);
}

void os101_qjs_get_exception(JSContext *ctx, JSValue *out)
{
    *out = JS_GetException(ctx);
}

/*
 * Call a function on the global object.
 *
 * The alternative — building a source string and evaluating it — means escaping
 * every argument correctly for a JavaScript literal, and getting that wrong on
 * a page's own text is an injection into the page's own script. Passing the
 * arguments as values instead makes the question not arise.
 *
 * `argv` is borrowed: the caller built those values and still owns them. The
 * result is written to *out and is owned by the caller, exception included.
 */
void os101_qjs_call_global(JSContext *ctx, const char *name, int argc,
                          const JSValue *argv, JSValue *out)
{
    JSValue global = JS_GetGlobalObject(ctx);
    JSValue fn = JS_GetPropertyStr(ctx, global, name);

    if (!JS_IsFunction(ctx, fn)) {
        JS_FreeValue(ctx, fn);
        JS_FreeValue(ctx, global);
        *out = JS_ThrowTypeError(ctx, "%s is not a function", name);
        return;
    }
    *out = JS_Call(ctx, fn, JS_UNDEFINED, argc, (JSValueConst *)argv);
    JS_FreeValue(ctx, fn);
    JS_FreeValue(ctx, global);
}

/* ── native functions ────────────────────────────────────────────────────── */

/*
 * Rust's side of the trampoline. It is given the slot number it was registered
 * under rather than a function pointer, so that no Rust `extern "C" fn` ever
 * has to have the by-value JSValue signature that JSCFunction demands.
 *
 * Returns 0 having written *out, or non-zero if the slot holds nothing — which
 * can only happen if a context outlived the registration table, and is turned
 * into a JavaScript exception rather than ignored.
 */
extern int os101_qjs_native_dispatch(JSContext *ctx, int slot, int argc,
                                     const JSValue *argv, JSValue *out);

static JSValue os101_qjs_trampoline(JSContext *ctx, JSValueConst this_val,
                                    int argc, JSValueConst *argv, int slot)
{
    JSValue out = JS_UNDEFINED;

    (void)this_val;
    if (os101_qjs_native_dispatch(ctx, slot, argc, argv, &out) != 0)
        return JS_ThrowInternalError(ctx,
                                     "no native function in slot %d", slot);
    return out;
}

/* Install `name` on the global object as a function that dispatches to `slot`.
   0 on success, -1 if the property could not be defined. */
int os101_qjs_define_global_function(JSContext *ctx, const char *name, int argc,
                                     int slot)
{
    JSValue global = JS_GetGlobalObject(ctx);
    JSValue fn = JS_NewCFunctionMagic(ctx, os101_qjs_trampoline, name, argc,
                                      JS_CFUNC_generic_magic, slot);
    int rc;

    if (JS_IsException(fn)) {
        JS_FreeValue(ctx, global);
        return -1;
    }
    rc = JS_SetPropertyStr(ctx, global, name, fn);
    JS_FreeValue(ctx, global);
    return rc < 0 ? -1 : 0;
}
