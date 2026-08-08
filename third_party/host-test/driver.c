/*
 * Runs real JavaScript through the vendored QuickJS, on the host, against the
 * same freestanding headers and the same C shim the kernel will use.
 *
 * This exists to answer three questions before anyone spends time on DOM
 * bindings: does the engine work at all on this libc, what does a runtime cost
 * in bytes, and how much C stack does it need. The last one decides whether the
 * whole idea is viable, because the kernel has a 1 MiB stack and QuickJS's
 * default stack limit is exactly 1 MiB.
 *
 * Build and run it with ./run.sh.
 */
#include <stdio.h>
#include <stdlib.h>
#include <inttypes.h>
#include <string.h>

#include "quickjs.h"

static int checks_run;
static int checks_failed;

static void report(const char *label, const char *got, const char *want)
{
    int ok = want == NULL || (got != NULL && strcmp(got, want) == 0);

    checks_run++;
    if (!ok)
        checks_failed++;
    printf("  %-4s %-34s %s\n", ok ? "ok" : "FAIL", label,
           got == NULL ? "<no value>" : got);
    if (!ok)
        printf("       expected: %s\n", want);
}

/*
 * Evaluate one expression and compare its string form against `want`. Passing
 * NULL for `want` means "print whatever it says" — used where the exact wording
 * is QuickJS's business rather than something we should pin down, such as the
 * text of a TypeError.
 */
static void check_eval(JSContext *ctx, const char *label, const char *source,
                       const char *want)
{
    JSValue value = JS_Eval(ctx, source, strlen(source), label,
                            JS_EVAL_TYPE_GLOBAL);
    const char *text;

    if (JS_IsException(value)) {
        JSValue error = JS_GetException(ctx);
        const char *message = JS_ToCString(ctx, error);
        checks_run++;
        checks_failed++;
        printf("  %-4s %-34s threw: %s\n", "FAIL", label,
               message == NULL ? "?" : message);
        JS_FreeCString(ctx, message);
        JS_FreeValue(ctx, error);
        JS_FreeValue(ctx, value);
        return;
    }

    text = JS_ToCString(ctx, value);
    report(label, text, want);
    JS_FreeCString(ctx, text);
    JS_FreeValue(ctx, value);
}

/* Drain the microtask queue, the way an embedder's event loop has to. */
static int pump_jobs(JSRuntime *rt)
{
    int pumped = 0;

    for (;;) {
        JSContext *unused;
        int status = JS_ExecutePendingJob(rt, &unused);
        if (status <= 0)
            return status < 0 ? -1 : pumped;
        pumped++;
    }
}

/* ── the shim's own printf ─────────────────────────────────────────────────── */

/*
 * QuickJS reaches snprintf for every exception message and fprintf for the
 * memory dump, so the formatter is on the path of anything that goes wrong. The
 * expectations below were taken from Apple's libc for the same format and
 * arguments, which is the whole point: this is the shim's printf being compared
 * against a real one, not against itself.
 */
static void expect_format(const char *want, const char *shown, const char *got)
{
    checks_run++;
    if (strcmp(got, want) != 0) {
        checks_failed++;
        printf("  %-4s %-34s [%s]\n", "FAIL", shown, got);
        printf("       expected: [%s]\n", want);
    } else {
        printf("  %-4s %-34s [%s]\n", "ok", shown, got);
    }
}

#define CHECK_FORMAT(want, fmt, ...)                                          \
    do {                                                                      \
        char buffer[160];                                                      \
        snprintf(buffer, sizeof(buffer), fmt, __VA_ARGS__);                    \
        expect_format(want, fmt, buffer);                                      \
    } while (0)

static void test_printf(void)
{
    printf("\nthe shim's printf\n");

    CHECK_FORMAT("-42", "%d", -42);
    CHECK_FORMAT("    7|7    |00007", "%5d|%-5d|%05d", 7, 7, 7);
    CHECK_FORMAT("+5 -5  5", "%+d %+d % d", 5, -5, 5);
    CHECK_FORMAT("4294967295 beef BEEF 0xbeef 10", "%u %x %X %#x %o",
                 4294967295u, 48879, 48879, 48879, 8);
    CHECK_FORMAT("0000beef 00123456789abc", "%08x %014" PRIx64, 0xbeefu,
                 (uint64_t)0x123456789abcULL);
    CHECK_FORMAT("-1234567890123 9007199254740993", "%ld %" PRId64,
                 (long)-1234567890123L, (int64_t)9007199254740993LL);
    CHECK_FORMAT("abcdef|        ab|ab        |abc|   ab|ab",
                 "%s|%10s|%-10s|%.3s|%*s|%.*s", "abcdef", "ab", "ab", "abcdef",
                 5, "ab", 2, "abcdef");
    CHECK_FORMAT("ok", "%c%c", 'o', 'k');
    CHECK_FORMAT("%literal 1", "%%literal %d", 1);
    CHECK_FORMAT(" 3 [(null)]", "%2.0d [%s]", 3, (char *)NULL);
    CHECK_FORMAT("44 4464", "%hhd %hd", 300, 70000);

    /* Ties round to even, which is where a naive implementation gives 3 and 2.3. */
    CHECK_FORMAT("12.3  3.14      -0.1", "%0.1f %5.2f %9.1f", 12.34, 3.14159,
                 -0.05);
    CHECK_FORMAT("0.333333 2 0.3333333333", "%f %.0f %.10f", 1.0 / 3.0, 2.5,
                 1.0 / 3.0);
    CHECK_FORMAT("  2.2|2.25    |-0002.25", "%5.1f|%-8.2f|%08.2f", 2.25, 2.25,
                 -2.25);
    CHECK_FORMAT("1.234568e+04 1.234568E+04 1.23e-04", "%e %E %.2e",
                 12345.6789, 12345.6789, 0.000123);
    CHECK_FORMAT("0.0001 123.456 1e+21 100000", "%g %.14g %g %g", 0.0001,
                 123.456, 1e21, 100000.0);
    CHECK_FORMAT("inf -inf nan", "%f %f %f", 1.0 / 0.0, -1.0 / 0.0, 0.0 / 0.0);

    /* snprintf reports the length it wanted, not the length it wrote. */
    {
        char small[4];
        int wanted = snprintf(small, sizeof(small), "%s", "truncated");
        char detail[32];
        snprintf(detail, sizeof(detail), "%d wanted, wrote [%s]", wanted, small);
        report("truncation reports full length", detail,
               "9 wanted, wrote [tru]");
    }
}

/* ── the language ──────────────────────────────────────────────────────────── */

static void test_language(JSContext *ctx, JSRuntime *rt)
{
    printf("\nlanguage\n");

    check_eval(ctx, "arithmetic", "(1 + 2) * 3 / 4 - 5", "-2.75");
    check_eval(ctx, "integer overflow to double",
               "String(2 ** 53) + ' ' + String(0.1 + 0.2)",
               "9007199254740992 0.30000000000000004");
    check_eval(ctx, "number formatting",
               "[1/3, 1e21, 1e-7, -0].map(String).join('|')",
               "0.3333333333333333|1e+21|1e-7|0");

    check_eval(ctx, "JSON round trip",
               "JSON.stringify(JSON.parse('{\"a\":[1,2,{\"b\":null}],\"c\":\"x\\\\u00e9\"}'))",
               "{\"a\":[1,2,{\"b\":null}],\"c\":\"xé\"}");
    check_eval(ctx, "JSON.stringify with indent",
               "JSON.stringify({a:1}, null, 2)", "{\n  \"a\": 1\n}");

    check_eval(ctx, "regexp capture and replace",
               "'2026-06-04'.replace(/(\\d{4})-(\\d{2})-(\\d{2})/, '$3/$2/$1')",
               "04/06/2026");
    check_eval(ctx, "regexp named groups",
               "/(?<y>\\d{4})-(?<m>\\d{2})/.exec('2026-06').groups.m", "06");
    check_eval(ctx, "regexp unicode property",
               "/^\\p{Lu}\\p{Ll}+$/u.test('Ärger')", "true");

    check_eval(ctx, "map with an arrow function",
               "[1,2,3].map(x => x * x).join(',')", "1,4,9");
    check_eval(ctx, "reduce and closures",
               "[1,2,3,4].reduce((a, b) => a + b, 0)", "10");

    check_eval(ctx, "class with a method",
               "class Point {"
               "  constructor(x, y) { this.x = x; this.y = y; }"
               "  get length() { return Math.hypot(this.x, this.y); }"
               "  toString() { return `(${this.x},${this.y})`; }"
               "}"
               "class Named extends Point {"
               "  constructor(x, y, n) { super(x, y); this.n = n; }"
               "  toString() { return this.n + super.toString(); }"
               "}"
               "String(new Named(3, 4, 'p')) + ' ' + new Named(3, 4, 'p').length",
               "p(3,4) 5");

    check_eval(ctx, "template literal",
               "((n) => `n is ${n} and ${n * 2}`)(7)", "n is 7 and 14");
    check_eval(ctx, "tagged template",
               "((s, ...v) => s.raw.join('|') + '/' + v.join(','))`a${1}b${2}c`",
               "a|b|c/1,2");

    check_eval(ctx, "try/catch/finally",
               "let trace = '';"
               "try { try { null.x } finally { trace += 'f' } }"
               "catch (e) { trace += e.constructor.name }"
               "trace",
               "fTypeError");
    check_eval(ctx, "thrown message", "try { null.x } catch (e) { e.message }",
               NULL);
    check_eval(ctx, "custom error subclass",
               "class Oops extends Error {};"
               "try { throw new Oops('bad') } catch (e) { e.name + ':' + e.message + ':' + (e instanceof Error) }",
               "Error:bad:true");

    printf("\nmodern syntax\n");
    check_eval(ctx, "destructuring and spread",
               "const {a, ...rest} = {a:1, b:2, c:3};"
               "`${a}/${JSON.stringify(rest)}/${Math.max(...[4,9,2])}`",
               "1/{\"b\":2,\"c\":3}/9");
    check_eval(ctx, "optional chaining and nullish",
               "const o = {p:{q:0}}; `${o?.p?.q ?? 'none'}|${o?.z?.q ?? 'none'}`",
               "0|none");
    check_eval(ctx, "generators and iterators",
               "function* fib() { let [a,b]=[0,1]; while (true) { yield a; [a,b]=[b,a+b] } }"
               "[...(function*(){ let n=0; for (const v of fib()) { if (n++ === 8) return; yield v } })()].join(',')",
               "0,1,1,2,3,5,8,13");
    check_eval(ctx, "Map, Set and symbols",
               "const m = new Map([[1,'a'],[2,'b']]);"
               "`${[...new Set([1,1,2,3])].length}/${m.get(2)}/${typeof Symbol.iterator}`",
               "3/b/symbol");
    check_eval(ctx, "BigInt", "(2n ** 64n - 1n).toString()",
               "18446744073709551615");
    check_eval(ctx, "getters, Proxy and Reflect",
               "const p = new Proxy({}, { get: (t, k) => k === 'x' ? 42 : Reflect.get(t, k) }); p.x",
               "42");
    check_eval(ctx, "labelled break and for-of",
               "let s = 0; outer: for (const i of [1,2,3]) { for (const j of [1,2,3]) { if (j === 2) continue outer; s += i * j } } s",
               "6");

    printf("\npromises and the job queue\n");
    check_eval(ctx, "a promise stays pending",
               "globalThis.trail = '';"
               "Promise.resolve(1)"
               "  .then(v => { trail += 'a' + v; return v + 1 })"
               "  .then(v => { trail += 'b' + v; throw new Error('x') })"
               "  .catch(e => { trail += 'c' + e.message });"
               "trail",
               "");
    {
        int pumped = pump_jobs(rt);
        char detail[64];
        snprintf(detail, sizeof(detail), "%d job(s) executed", pumped);
        report("JS_ExecutePendingJob drains", detail, NULL);
    }
    check_eval(ctx, "the chain ran in order", "trail", "a1b2cx");

    check_eval(ctx, "async/await starts",
               "globalThis.awaited = 'pending';"
               "(async () => { const v = await Promise.resolve(20); awaited = String(v + 22) })();"
               "awaited",
               "pending");
    pump_jobs(rt);
    check_eval(ctx, "async/await finished", "awaited", "42");

    printf("\nDate\n");
    check_eval(ctx, "a fixed instant formats",
               "new Date(1780531200000).toISOString()",
               "2026-06-04T00:00:00.000Z");
    check_eval(ctx, "parsing round trips",
               "new Date(Date.parse('2026-06-04T12:34:56.789Z')).getTime()",
               "1780576496789");
    check_eval(ctx, "field accessors",
               "const d = new Date(Date.UTC(2024, 1, 29, 13, 5, 6));"
               "[d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate(), d.getUTCDay()].join(',')",
               "2024,1,29,4");
    check_eval(ctx, "the shim reports UTC",
               "new Date().getTimezoneOffset()", "0");
    check_eval(ctx, "Date.now moves forward",
               "const t = Date.now(); t > 1780531200000 && t < 4102444800000",
               "true");
}

/* ── what it costs ─────────────────────────────────────────────────────────── */

static void report_bytes(const char *label, int64_t bytes, int64_t count)
{
    printf("  %-38s %9lld bytes", label, (long long)bytes);
    if (count >= 0)
        printf("  in %lld blocks", (long long)count);
    printf("\n");
}

static void measure_footprint(void)
{
    JSMemoryUsage usage;
    JSRuntime *rt;
    JSContext *ctx;

    printf("\nmemory footprint\n");

    rt = JS_NewRuntime();
    JS_ComputeMemoryUsage(rt, &usage);
    report_bytes("runtime only", usage.malloc_size, usage.malloc_count);

    ctx = JS_NewContextRaw(rt);
    JS_AddIntrinsicBaseObjects(ctx);
    JS_ComputeMemoryUsage(rt, &usage);
    report_bytes("+ raw context, base objects only", usage.malloc_size,
                 usage.malloc_count);
    JS_FreeContext(ctx);

    ctx = JS_NewContext(rt);
    JS_ComputeMemoryUsage(rt, &usage);
    report_bytes("+ full context, all intrinsics", usage.malloc_size,
                 usage.malloc_count);

    {
        const char *warm =
            "class C { m(x) { return x * 2 } };"
            "[...Array(64).keys()].map(i => new C().m(i)).reduce((a,b)=>a+b,0);"
            "JSON.parse(JSON.stringify({a:[1,2,3]}));"
            "/(\\w+)-(\\d+)/.exec('abc-123');";
        JSValue v = JS_Eval(ctx, warm, strlen(warm), "warm",
                            JS_EVAL_TYPE_GLOBAL);
        JS_FreeValue(ctx, v);
        JS_RunGC(rt);
        JS_ComputeMemoryUsage(rt, &usage);
        report_bytes("+ a small script, after a GC", usage.malloc_size,
                     usage.malloc_count);
    }

    JS_FreeContext(ctx);
    JS_FreeRuntime(rt);
}

/* ── how much stack it needs ───────────────────────────────────────────────── */

/*
 * The lowest stack address the engine has been seen at, recorded by a native
 * function that JavaScript calls from the bottom of its recursion. It is a
 * lower bound on the true peak — the interpreter's own frames below the call
 * are not counted — which is the right direction to be wrong in for a headroom
 * calculation only if we then treat it as a floor, so the report below quotes
 * it against the limit that produced it rather than as an absolute.
 */
static char *deepest_frame;
static char *reference_frame;

static JSValue js_stack_probe(JSContext *ctx, JSValueConst this_val, int argc,
                              JSValueConst *argv)
{
    char *here = (char *)__builtin_frame_address(0);

    (void)this_val;
    (void)argc;
    (void)argv;
    if (deepest_frame == NULL || here < deepest_frame)
        deepest_frame = here;
    return JS_NewInt32(ctx, 0);
}

/* Run `source` and return the depth it reported, or -1 if it threw. */
static long run_depth_probe(JSContext *ctx, const char *source)
{
    JSValue value = JS_Eval(ctx, source, strlen(source), "probe",
                            JS_EVAL_TYPE_GLOBAL);
    long depth = -1;

    if (JS_IsException(value)) {
        JSValue error = JS_GetException(ctx);
        const char *message = JS_ToCString(ctx, error);
        printf("       the probe itself threw: %s\n",
               message == NULL ? "?" : message);
        JS_FreeCString(ctx, message);
        JS_FreeValue(ctx, error);
    } else {
        int64_t n = 0;
        JS_ToInt64(ctx, &n, value);
        depth = (long)n;
    }
    JS_FreeValue(ctx, value);
    return depth;
}

/*
 * The parser and JSON.parse recurse on the same C stack as the interpreter, and
 * a web page is a perfectly ordinary way to receive input designed to make them
 * do so. These two build the pathological input; `deepest_accepted` finds where
 * the guard starts refusing it.
 */
static char *nested_parens(size_t depth)
{
    char *source = malloc(depth * 2 + 2);
    size_t i;

    for (i = 0; i < depth; i++)
        source[i] = '(';
    source[depth] = '1';
    for (i = 0; i < depth; i++)
        source[depth + 1 + i] = ')';
    source[depth * 2 + 1] = '\0';
    return source;
}

static char *nested_json(size_t depth)
{
    char *source = malloc(depth * 2 + 32);
    size_t at = 0;
    size_t i;

    at += (size_t)snprintf(source, 32, "JSON.parse('");
    for (i = 0; i < depth; i++)
        source[at++] = '[';
    for (i = 0; i < depth; i++)
        source[at++] = ']';
    source[at++] = '\'';
    source[at++] = ')';
    source[at] = '\0';
    return source;
}

/*
 * Bisect for the deepest input the engine accepts. A refusal has to be a clean
 * JavaScript exception; if the guard were missing, the process would die here
 * rather than return a number, which is itself the result.
 */
static long deepest_accepted(JSContext *ctx, char *(*build)(size_t))
{
    size_t low = 1;
    size_t high = 1 << 20;

    while (low < high) {
        size_t mid = low + (high - low + 1) / 2;
        char *source = build(mid);
        JSValue v = JS_Eval(ctx, source, strlen(source), "nested",
                            JS_EVAL_TYPE_GLOBAL);
        int refused = JS_IsException(v);
        if (refused)
            JS_FreeValue(ctx, JS_GetException(ctx));
        JS_FreeValue(ctx, v);
        free(source);
        if (refused)
            high = mid - 1;
        else
            low = mid;
    }
    return (long)low;
}

static void measure_stack(size_t limit)
{
    JSRuntime *rt;
    JSContext *ctx;
    JSValue global;
    long depth;

    reference_frame = (char *)__builtin_frame_address(0);
    deepest_frame = NULL;

    rt = JS_NewRuntime();
    JS_SetMaxStackSize(rt, limit);
    ctx = JS_NewContext(rt);
    global = JS_GetGlobalObject(ctx);
    JS_SetPropertyStr(ctx, global, "probe",
                      JS_NewCFunction(ctx, js_stack_probe, "probe", 0));
    JS_FreeValue(ctx, global);

    /*
     * Recurse until the guard fires, then report the depth reached. The probe()
     * call on the way down is what records how far the C stack actually went;
     * calling it on every level would dominate the measurement, so it is called
     * only when the recursion is about to give up.
     */
    depth = run_depth_probe(ctx,
                            "globalThis.reached = 0;"
                            "function down(n) {"
                            "  reached = n;"
                            "  try { return down(n + 1) }"
                            "  catch (e) { probe(); throw e }"
                            "}"
                            "try { down(0) } catch (e) {}"
                            "reached");
    printf("  limit %7zu KiB: %6ld JS frames", limit / 1024, depth);
    if (deepest_frame != NULL)
        printf(", C stack reached %6ld KiB",
               (long)(reference_frame - deepest_frame) / 1024);
    printf("\n");

    printf("                   parser survives %6ld nested parens,"
           " JSON.parse %6ld levels\n",
           deepest_accepted(ctx, nested_parens),
           deepest_accepted(ctx, nested_json));

    JS_FreeContext(ctx);
    JS_FreeRuntime(rt);
}

int main(void)
{
    JSRuntime *rt = JS_NewRuntime();
    JSContext *ctx;

    printf("QuickJS %s on the OS101 libc shim\n", CONFIG_VERSION);

    /*
     * The kernel will set a limit; the functional tests use a generous one so
     * that a deep-but-legitimate script is not what fails.
     */
    JS_SetMaxStackSize(rt, 4 << 20);
    ctx = JS_NewContext(rt);

    test_printf();
    test_language(ctx, rt);

    JS_FreeContext(ctx);
    JS_FreeRuntime(rt);

    measure_footprint();

    printf("\nstack budget\n");
    measure_stack(64 * 1024);
    measure_stack(128 * 1024);
    measure_stack(256 * 1024);
    measure_stack(512 * 1024);

    printf("\n%d checks, %d failed\n", checks_run, checks_failed);
    return checks_failed == 0 ? 0 : 1;
}
