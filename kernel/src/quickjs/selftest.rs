//! Boot-time checks for the QuickJS engine.
//!
//! The engine was proved on the host before it was wired in here, and that
//! harness covers the language far more thoroughly than this does. What it could
//! not cover is anything that needs the real machine, so this file is weighted
//! towards those: the clock the kernel actually keeps, the stack guard against
//! the kernel's actual 1 MiB stack, an allocation failure against the kernel's
//! actual allocator, and whether twenty runtimes in a row give their memory
//! back. The language checks are here as a smoke test — if `libunicode`'s tables
//! or `dtoa`'s integer arithmetic behaved differently on the metal, they are
//! what would notice.
//!
//! It costs a few hundred milliseconds of boot, which is reported on the serial
//! line alongside the heap figures.

use super::{Arg, Args, Engine, Return};
use super::ffi;
use alloc::format;
use alloc::string::String;

/// The recursion the guard allowed, and the exception it raised, recorded so
/// the serial line can report them rather than only whether they were within
/// bounds.
struct Findings {
    heap_cost: usize,
    engine_bytes: i64,
    engine_blocks: i64,
    recursion_depth: i64,
    overflow_error: String,
    json_depth_refused: usize,
    oom_error: String,
    leak_delta: isize,
    /// How long a loop with a 50 ms budget actually ran, and what stopped it.
    interrupt_micros: u64,
    interrupt_error: String,
    micros: u64,
}

/// Boot-time checks.
pub fn run() -> crate::selftest::Report {
    let started = crate::clock::micros();
    let mut report = crate::selftest::Report::new();
    let mut found = Findings {
        heap_cost: 0,
        engine_bytes: 0,
        engine_blocks: 0,
        recursion_depth: 0,
        overflow_error: String::new(),
        json_depth_refused: 0,
        oom_error: String::new(),
        leak_delta: 0,
        interrupt_micros: 0,
        interrupt_error: String::new(),
        micros: 0,
    };

    layout(&mut report);

    // Everything below needs an engine, and if one cannot be built there is
    // nothing to say beyond that — reporting it as a single failure is more
    // useful than fifty consequential ones.
    let heap_before = crate::allocator::used();
    let mut engine = match Engine::new() {
        Ok(engine) => engine,
        Err(why) => {
            report.check(why, false);
            return report;
        }
    };
    found.heap_cost = crate::allocator::used().saturating_sub(heap_before);
    let (bytes, blocks) = engine.memory_usage();
    found.engine_bytes = bytes;
    found.engine_blocks = blocks;
    report.check("a runtime and a context exist", bytes > 0 && blocks > 0);
    // 165 KiB is what the host measured for a full context. An order of
    // magnitude either side of that means something is very different here.
    report.check("the runtime costs what it did on the host", bytes > 100_000 && bytes < 400_000);

    language(&engine, &mut report);
    numbers(&engine, &mut report);
    date(&engine, &mut report);
    natives(&mut engine, &mut report);
    calling_in(&engine, &mut report);
    jobs(&engine, &mut report);
    collection(&engine, &mut report);
    stack_guard(&engine, &mut report, &mut found);
    deadline(&engine, &mut report, &mut found);
    drop(engine);

    out_of_memory(&mut report, &mut found);
    sharing(&mut report);
    leaks(&mut report, &mut found);

    found.micros = crate::clock::micros().saturating_sub(started);
    announce(&found, &report);
    report
}

/// The numbers that are worth knowing and that no check can express as a pass
/// or a fail. Serial only: the boot screen gets the one-line verdict like every
/// other subsystem, and these are for whoever is reading the log.
fn announce(found: &Findings, report: &crate::selftest::Report) {
    crate::serial_println!(
        "quickjs: {} checks in {}.{:03} s",
        report.passed + report.failed,
        found.micros / 1_000_000,
        (found.micros / 1_000) % 1_000,
    );
    crate::serial_println!(
        "quickjs: a runtime and a full context cost {} KiB of kernel heap; the engine \
         counts {} bytes in {} blocks",
        found.heap_cost / 1024,
        found.engine_bytes,
        found.engine_blocks,
    );
    crate::serial_println!(
        "quickjs: recursion reached {} frames before \"{}\"; the parser and JSON.parse \
         refused {} levels of nesting the same way",
        found.recursion_depth,
        found.overflow_error,
        found.json_depth_refused,
    );
    crate::serial_println!(
        "quickjs: exhausting a JS_SetMemoryLimit budget — {}",
        found.oom_error
    );
    crate::serial_println!(
        "quickjs: {} bytes of kernel heap outstanding after 20 create/destroy cycles",
        found.leak_delta,
    );
    crate::serial_println!(
        "quickjs: a loop given 50 ms was stopped after {} ms with \"{}\"",
        found.interrupt_micros / 1_000,
        found.interrupt_error,
    );
}

/// Prove the Rust declaration of `JSValue` matches the C compiler's.
///
/// This is first because everything else depends on it and nothing else would
/// diagnose it: a payload read at the tag's offset is a tag the engine would
/// follow as a pointer, and the symptom would be a fault somewhere unrelated.
fn layout(report: &mut crate::selftest::Report) {
    // SAFETY: both calls take no arguments and return a constant.
    let (size, align) =
        unsafe { (ffi::os101_qjs_value_size(), ffi::os101_qjs_value_align()) };
    report.check("JSValue is 16 bytes in C", size == 16);
    report.check("JSValue is 16 bytes in Rust", core::mem::size_of::<ffi::JsValue>() == size);
    report.check("JSValue alignment agrees", core::mem::align_of::<ffi::JsValue>() == align);

    // A pattern with different halves, so that a swapped payload and tag fails
    // rather than passing by symmetry.
    const PAYLOAD: u64 = 0x0123_4567_89ab_cdef;
    const TAG: i64 = -7; // JS_TAG_STRING, a real tag rather than a made-up one.

    let mut value = ffi::JsValue { payload: 0, tag: 0 };
    // SAFETY: `value` is a live local.
    unsafe { ffi::os101_qjs_value_roundtrip(PAYLOAD, TAG, &mut value) };
    report.check("C writes the payload where Rust reads it", value.payload == PAYLOAD);
    report.check("C writes the tag where Rust reads it", value.tag == TAG);

    let built = ffi::JsValue { payload: PAYLOAD, tag: TAG };
    // SAFETY: `built` is a live local.
    let agreed = unsafe { ffi::os101_qjs_value_check(&built, PAYLOAD, TAG) };
    report.check("Rust writes the fields where C reads them", agreed != 0);
}

/// Evaluate `source` and check the result stringifies to `want`.
fn expect(
    engine: &Engine,
    report: &mut crate::selftest::Report,
    name: &'static str,
    source: &str,
    want: &str,
) {
    match engine.eval(source, name) {
        Ok(got) => report.check(name, got == want),
        Err(_) => report.check(name, false),
    }
}

fn language(engine: &Engine, report: &mut crate::selftest::Report) {
    expect(engine, report, "arithmetic", "(1 + 2) * 3 / 4 - 5", "-2.75");
    expect(
        engine,
        report,
        "string methods and unicode case",
        "'\\u00c4rger'.toLowerCase() + '/' + 'abc'.padStart(5, '-')",
        "ärger/--abc",
    );
    expect(
        engine,
        report,
        "JSON round trip",
        "JSON.stringify(JSON.parse('{\"a\":[1,2,{\"b\":null}],\"c\":\"x\\\\u00e9\"}'))",
        "{\"a\":[1,2,{\"b\":null}],\"c\":\"xé\"}",
    );
    expect(
        engine,
        report,
        "JSON.stringify with an indent",
        "JSON.stringify({a:1}, null, 2)",
        "{\n  \"a\": 1\n}",
    );
    expect(
        engine,
        report,
        "regexp capture and replace",
        "'2026-06-04'.replace(/(\\d{4})-(\\d{2})-(\\d{2})/, '$3/$2/$1')",
        "04/06/2026",
    );
    expect(
        engine,
        report,
        "regexp named groups",
        "/(?<y>\\d{4})-(?<m>\\d{2})/.exec('2026-06').groups.m",
        "06",
    );
    // The Unicode property tables are 60 KiB of libunicode.o, and this is the
    // only check that touches them.
    expect(
        engine,
        report,
        "regexp unicode property",
        "/^\\p{Lu}\\p{Ll}+$/u.test('\\u00c4rger')",
        "true",
    );
    expect(
        engine,
        report,
        "a class with inheritance",
        "class Point {\
           constructor(x, y) { this.x = x; this.y = y }\
           get length() { return Math.hypot(this.x, this.y) }\
           toString() { return `(${this.x},${this.y})` }\
         }\
         class Named extends Point {\
           constructor(x, y, n) { super(x, y); this.n = n }\
           toString() { return this.n + super.toString() }\
         }\
         String(new Named(3, 4, 'p')) + ' ' + new Named(3, 4, 'p').length",
        "p(3,4) 5",
    );
    expect(
        engine,
        report,
        "a generator",
        "function* fib() { let [a, b] = [0, 1]; while (true) { yield a; [a, b] = [b, a + b] } }\
         const out = []; for (const v of fib()) { if (out.length === 9) break; out.push(v) }\
         out.join(',')",
        "0,1,1,2,3,5,8,13,21",
    );
    expect(
        engine,
        report,
        "closures capture per iteration",
        "const fs = []; for (let i = 0; i < 3; i++) fs.push(() => i); fs.map(f => f()).join(',')",
        "0,1,2",
    );
    expect(
        engine,
        report,
        "Map and Set",
        "const m = new Map([[1, 'a'], [2, 'b']]);\
         `${[...new Set([1, 1, 2, 3])].length}/${m.get(2)}/${typeof Symbol.iterator}`",
        "3/b/symbol",
    );
    expect(engine, report, "BigInt", "(2n ** 64n - 1n).toString()", "18446744073709551615");
    expect(
        engine,
        report,
        "destructuring and spread",
        "const {a, ...rest} = {a: 1, b: 2, c: 3};\
         `${a}/${JSON.stringify(rest)}/${Math.max(...[4, 9, 2])}`",
        "1/{\"b\":2,\"c\":3}/9",
    );
    expect(
        engine,
        report,
        "try, catch and finally in order",
        "let trace = '';\
         try { try { null.x } finally { trace += 'f' } } catch (e) { trace += e.constructor.name }\
         trace",
        "fTypeError",
    );
    expect(
        engine,
        report,
        "sort with a comparator",
        "[10, 9, 1, 100].sort((a, b) => a - b).join(',')",
        "1,9,10,100",
    );

    // A thrown value has to come back as Err, not as a string that happens to
    // look like an error.
    report.check(
        "a throw is reported as an error",
        matches!(engine.eval("null.x", "throwing"), Err(message) if message.contains("TypeError")),
    );
    report.check(
        "a syntax error is reported as an error",
        matches!(engine.eval("function (", "bad syntax"), Err(message) if message.contains("SyntaxError")),
    );
}

/// Numbers, which are the one part of the language this OS could plausibly get
/// wrong on its own: every JavaScript number is formatted by `dtoa.c` and every
/// `Math` call but three is the pure-Rust `libm` inside `compiler_builtins`.
fn numbers(engine: &Engine, report: &mut crate::selftest::Report) {
    expect(engine, report, "float addition is not decimal", "String(0.1 + 0.2)", "0.30000000000000004");
    expect(
        engine,
        report,
        "number formatting at the edges",
        "[1/3, 1e21, 1e-7, -0, 2 ** 53].map(String).join('|')",
        "0.3333333333333333|1e+21|1e-7|0|9007199254740992",
    );
    expect(
        engine,
        report,
        "toFixed, toPrecision and toString(radix)",
        "[(1234.5678).toFixed(2), (0.000123).toPrecision(2), (255).toString(16)].join('|')",
        "1234.57|0.00012|ff",
    );
    expect(
        engine,
        report,
        "parsing back",
        "[parseFloat('3.25e2'), parseInt('ff', 16), Number('  42  ')].join(',')",
        "325,255,42",
    );
    // atanh is the interesting one: it is not among the thirty libm functions
    // compiler_builtins exports, so it is the shim's own implementation.
    expect(
        engine,
        report,
        "Math against libm and the shim",
        "[Math.sqrt(2), Math.hypot(3, 4), Math.cbrt(27), Math.log2(1024), Math.atanh(0.5)]\
           .map(x => x.toFixed(6)).join(',')",
        "1.414214,5.000000,3.000000,10.000000,0.549306",
    );
}

fn date(engine: &Engine, report: &mut crate::selftest::Report) {
    expect(
        engine,
        report,
        "a fixed instant formats",
        "new Date(1780531200000).toISOString()",
        "2026-06-04T00:00:00.000Z",
    );
    expect(
        engine,
        report,
        "Date.parse round trips",
        "new Date(Date.parse('2026-06-04T12:34:56.789Z')).getTime()",
        "1780576496789",
    );
    // The shim reports UTC because the CMOS clock is UTC and this OS has no
    // timezone database, so zero is the right answer rather than a stub.
    expect(engine, report, "the clock is UTC", "new Date().getTimezoneOffset()", "0");

    // The point of the whole exercise: Date.now() has to be the kernel's clock,
    // not the shim's fallback. Bracket the evaluation with two RTC readings so
    // the comparison cannot fail merely because time passed.
    let before = crate::rtc::unix_millis();
    let reported = engine.eval("Date.now()", "Date.now").ok().and_then(|text| text.parse::<i64>().ok());
    let after = crate::rtc::unix_millis();
    match reported {
        Some(now) => {
            report.check("Date.now is the kernel's clock", now >= before && now <= after);
            // 2026-06-04T00:00:00Z is what clock.rs answers when nothing
            // installed a clock, and it is the one value that would otherwise
            // look plausible.
            report.check("Date.now is not the shim's fallback", now != 1_780_531_200_000);
        }
        None => {
            report.check("Date.now is the kernel's clock", false);
            report.check("Date.now is not the shim's fallback", false);
        }
    }

    // And the formatting of that same instant has to agree with rtc.rs, which
    // computes the civil date by an entirely separate route.
    let now = crate::rtc::now();
    let stamp = engine.eval("new Date(Date.now()).toISOString()", "toISOString");
    let expected = format!("{:04}-{:02}-{:02}", now.year, now.month, now.day);
    report.check(
        "toISOString agrees with the RTC's own date",
        matches!(&stamp, Ok(text) if text.starts_with(&expected)),
    );
}

// ── native functions ────────────────────────────────────────────────────────

/// A `print` that goes where the rest of the kernel's diagnostics go.
///
/// The DOM bindings will be built on this mechanism, so it is the mechanism
/// rather than the function that is being checked. This one is not meant to
/// survive: a page's `console.log` needs to reach the browser's own console,
/// not the serial port.
fn print(args: &Args) -> Return {
    let mut line = String::new();
    for index in 0..args.len() {
        if index > 0 {
            line.push(' ');
        }
        line.push_str(&args.string(index).unwrap_or_default());
    }
    crate::serial_println!("quickjs print: {}", line);
    Return::Undefined
}

fn sum(args: &Args) -> Return {
    let mut total = 0.0;
    for index in 0..args.len() {
        total += args.number(index).unwrap_or(0.0);
    }
    Return::Number(total)
}

fn shout(args: &Args) -> Return {
    if args.is_empty() {
        return Return::Undefined;
    }
    match args.string(0) {
        Some(text) => Return::Text(format!("kernel says {}", text)),
        None => Return::Undefined,
    }
}

fn count(args: &Args) -> Return {
    Return::Int(args.len() as i32)
}

/// Returns a real JavaScript boolean rather than a truthy number, which is the
/// distinction a `hasAttribute`-style binding will depend on.
fn even(args: &Args) -> Return {
    match args.number(0) {
        Some(value) => Return::Bool(value as i64 % 2 == 0),
        None => Return::Bool(false),
    }
}

fn natives(engine: &mut Engine, report: &mut crate::selftest::Report) {
    report.check("print registers", engine.register_global("print", 1, print).is_ok());
    report.check("sum registers", engine.register_global("kernelSum", 2, sum).is_ok());
    report.check("shout registers", engine.register_global("kernelShout", 1, shout).is_ok());
    report.check("count registers", engine.register_global("kernelCount", 0, count).is_ok());
    report.check("even registers", engine.register_global("kernelEven", 1, even).is_ok());

    let engine = &*engine;
    expect(engine, report, "a native is a function", "typeof kernelSum", "function");
    expect(engine, report, "a native returns a number", "kernelSum(2, 40)", "42");
    expect(engine, report, "a native returns a string", "kernelShout('hello')", "kernel says hello");
    expect(engine, report, "a native returns a real boolean", "[kernelEven(4), typeof kernelEven(4)].join(',')", "true,boolean");
    expect(engine, report, "a native sees every argument", "kernelCount(1, 2, 3, 4)", "4");
    expect(engine, report, "a native sees no arguments", "kernelCount()", "0");
    expect(engine, report, "a native returning nothing is undefined", "String(print('from js'))", "undefined");
    // A native called from inside JavaScript rather than at the top level, which
    // is the shape every DOM call will have.
    expect(
        engine,
        report,
        "a native called from a closure",
        "[1, 2, 3].map(n => kernelSum(n, 10)).join(',')",
        "11,12,13",
    );
    expect(engine, report, "declared arity is visible", "kernelSum.length", "2");
}

// ── calling into JavaScript ─────────────────────────────────────────────────

/// The other direction: Rust calling a function the script defined.
///
/// This is how the browser dispatches an event and runs the timers that are due,
/// and the reason it exists rather than a generated source string is that the
/// arguments are page data — an element's text, a key that was pressed — and
/// splicing those into a program would be an injection into the page's own
/// script.
fn calling_in(engine: &Engine, report: &mut crate::selftest::Report) {
    report.check(
        "a function to call can be defined",
        engine
            .eval(
                "globalThis.roundTrip = function (a, b, c, d) {\
                   return [typeof a, a, typeof b, b, typeof c, c, typeof d].join('|') };\
                 globalThis.thrower = function () { throw new TypeError('from js') };\
                 1",
                "call_global",
            )
            .is_ok(),
    );

    report.check(
        "every argument type arrives as itself",
        matches!(
            engine.call_global(
                "roundTrip",
                &[
                    Arg::Text("text"),
                    Arg::Int(-7),
                    Arg::Bool(true),
                    Arg::Undefined,
                ],
            ),
            Ok(answer) if answer == "string|text|number|-7|boolean|true|undefined"
        ),
    );
    report.check(
        "a float keeps its fraction",
        matches!(
            engine.eval("globalThis.echo = function (x) { return x * 2 }; 1", "call_global")
                .and_then(|_| engine.call_global("echo", &[Arg::Number(0.25)])),
            Ok(answer) if answer == "0.5"
        ),
    );
    report.check(
        "no arguments is allowed",
        matches!(
            engine
                .eval("globalThis.count = function () { return arguments.length }; 1", "call_global")
                .and_then(|_| engine.call_global("count", &[])),
            Ok(answer) if answer == "0"
        ),
    );
    report.check(
        "a throw comes back as an error",
        matches!(engine.call_global("thrower", &[]), Err(why) if why.contains("from js")),
    );
    // Not a crash and not a silent zero: the browser calls these by name, and a
    // name that is not there is a bug worth a message.
    report.check(
        "calling something that is not a function is an error",
        matches!(engine.call_global("noSuchThing", &[]), Err(why) if why.contains("not a function")),
    );
    report.check(
        "and the engine is unharmed",
        engine.eval("'still here'", "call_global").as_deref() == Ok("still here"),
    );
}

// ── the time budget ─────────────────────────────────────────────────────────

/// A script that never returns has to be stopped, and stopped by the clock
/// rather than by a step count — because a step count cannot tell a slow page
/// from a runaway one.
///
/// Note what the interruption is: an `InternalError` with QuickJS's *uncatchable*
/// flag set, which is what keeps a page from wrapping its own infinite loop in a
/// `try` and carrying on. That is the whole reason this is worth more than
/// counting instructions.
fn deadline(engine: &Engine, report: &mut crate::selftest::Report, found: &mut Findings) {
    const BUDGET: u64 = 50_000;

    let started = crate::clock::micros();
    engine.set_time_limit(BUDGET);
    let outcome = engine.eval(
        "globalThis.spun = 0; try { while (true) { spun++ } } catch (e) { 'caught' }",
        "runaway",
    );
    found.interrupt_micros = crate::clock::micros().saturating_sub(started);
    engine.clear_time_limit();

    report.check("a loop that never ends is stopped", outcome.is_err());
    found.interrupt_error = match &outcome {
        Err(why) => why.clone(),
        Ok(text) => alloc::format!("nothing — it returned {}", text),
    };
    report.check(
        "the page cannot catch the interruption",
        !matches!(&outcome, Ok(text) if text.contains("caught")),
    );
    // Bracketed loosely on purpose: the handler is polled every ten thousand
    // operations, so it overshoots, and the clock is a calibrated TSC.
    report.check(
        "it was stopped at about the budget",
        found.interrupt_micros >= BUDGET / 4 && found.interrupt_micros < BUDGET * 20,
    );
    report.check(
        "it really did run first",
        matches!(engine.eval("spun > 1000", "runaway"), Ok(text) if text == "true"),
    );
    report.check(
        "the engine works afterwards",
        engine.eval("1 + 1", "runaway").as_deref() == Ok("2"),
    );
    // And the flag QuickJS leaves behind must not make the *next* script's own
    // exception uncatchable, which would be a very confusing page to debug.
    report.check(
        "an ordinary throw is catchable again",
        matches!(
            engine.eval("try { null.x } catch (e) { 'ok' }", "runaway"),
            Ok(text) if text == "ok"
        ),
    );
    // A cleared limit has to mean no limit, or every later evaluation inherits
    // a deadline that has already passed.
    report.check(
        "a cleared budget lets a slow loop finish",
        matches!(
            engine.eval("let total = 0; for (let i = 0; i < 3e6; i++) total += i; total > 0", "slow"),
            Ok(text) if text == "true"
        ),
    );
}

// ── promises ────────────────────────────────────────────────────────────────

fn jobs(engine: &Engine, report: &mut crate::selftest::Report) {
    // The chain must not have run when eval returns; if it had, the pump would
    // be unnecessary and the browser's event loop would not need to know about
    // it.
    expect(
        engine,
        report,
        "a promise chain does not run inside eval",
        "globalThis.trail = '';\
         Promise.resolve(1)\
           .then(v => { trail += 'a' + v; return v + 1 })\
           .then(v => { trail += 'b' + v; throw new Error('x') })\
           .catch(e => { trail += 'c' + e.message });\
         trail",
        "",
    );
    report.check("jobs are pending", engine.has_pending_jobs());
    let pumped = engine.pump_jobs();
    report.check("the pump ran jobs", matches!(pumped, Ok(count) if count >= 3));
    report.check("the queue is empty afterwards", !engine.has_pending_jobs());
    expect(engine, report, "the chain ran in order", "trail", "a1b2cx");

    expect(
        engine,
        report,
        "an async function suspends at await",
        "globalThis.awaited = 'pending';\
         (async () => { const v = await Promise.resolve(20); awaited = String(v + 22) })();\
         awaited",
        "pending",
    );
    report.check("the pump resumes it", engine.pump_jobs().is_ok());
    expect(engine, report, "await finished", "awaited", "42");

    // eval_settled is the convenience that does both, and is what the browser
    // should call unless it has its own loop.
    report.check(
        "eval_settled drains before returning",
        matches!(
            engine.eval_settled(
                "globalThis.settled = 'no'; Promise.resolve().then(() => { settled = 'yes' }); 1",
                "settled",
            ),
            Ok(text) if text == "1"
        ),
    );
    expect(engine, report, "and the side effect happened", "settled", "yes");
}

// ── the collector ───────────────────────────────────────────────────────────

/// Reference counting handles everything a script drops on the floor; the cycle
/// collector handles the rest, and a DOM is nothing but cycles — every child
/// knows its parent. So the browser will depend on this, and the check is that
/// a reference cycle really does go away.
fn collection(engine: &Engine, report: &mut crate::selftest::Report) {
    let (baseline, _) = engine.memory_usage();
    let built = engine.eval(
        "globalThis.ring = [];\
         for (let i = 0; i < 2000; i++) { const a = {}, b = { a }; a.b = b; ring.push(a) }\
         ring.length",
        "cycles",
    );
    report.check("a cycle-heavy script runs", matches!(&built, Ok(text) if text == "2000"));
    let (peak, _) = engine.memory_usage();
    report.check("the cycles cost memory", peak > baseline);

    report.check("dropping the root succeeds", engine.eval("ring = null; 1", "cycles").is_ok());
    engine.run_gc();
    let (collected, _) = engine.memory_usage();
    report.check("the collector reclaims them", collected < peak);
    // Not back to exactly the baseline: the compiled code for the loop above,
    // and the atoms its property names created, stay behind on purpose.
    report.check("and gets most of it back", collected - baseline < (peak - baseline) / 2);
}

// ── the stack guard ─────────────────────────────────────────────────────────

/// The most important checks in the file, and the ones the host harness could
/// not stand in for: a runaway script has to come back as a JavaScript
/// exception rather than as a page fault on the kernel's guard page.
///
/// Note what the exception actually is. Bellard's QuickJS raises
/// `InternalError: stack overflow` from the interpreter and
/// `SyntaxError: stack overflow` from the parser — **not** the `RangeError`
/// that V8 raises and that `quickjs-ng` changed to. Page script that tests
/// `e instanceof RangeError` will not recognise it. That is a deviation worth
/// knowing about rather than one worth patching the vendored engine for.
fn stack_guard(
    engine: &Engine,
    report: &mut crate::selftest::Report,
    found: &mut Findings,
) {
    // Recurse until the guard fires, remembering how deep it got. `reached` is
    // assigned before the recursive call, so it survives the unwind.
    let depth = engine.eval(
        "globalThis.reached = 0; globalThis.caught = '';\
         function down(n) { reached = n; return down(n + 1) }\
         try { down(1) } catch (e) { caught = e.constructor.name + ': ' + e.message }\
         reached",
        "recursion",
    );
    found.recursion_depth = depth.ok().and_then(|text| text.parse::<i64>().ok()).unwrap_or(-1);
    found.overflow_error = engine.eval("caught", "recursion").unwrap_or_default();

    report.check("deep recursion is caught, not fatal", found.recursion_depth > 0);
    // The host measured 258 frames at this limit. Anything under a hundred
    // would mean the budget is being consumed by something other than the
    // interpreter; anything over a few thousand would mean the check is not
    // firing where we think it is.
    report.check(
        "the depth is in the range the limit implies",
        found.recursion_depth > 100 && found.recursion_depth < 2_000,
    );
    report.check(
        "the overflow is a catchable stack-overflow error",
        found.overflow_error.contains("stack overflow"),
    );
    // And the engine has to be usable afterwards, which is what makes it a
    // caught exception rather than a wedged runtime.
    expect(engine, report, "the engine still works after an overflow", "1 + 1", "2");

    // The parser recurses on the same stack, and untrusted markup is the
    // obvious way to reach it. 5,000 levels is far past the ~220 the host
    // measured at this limit.
    let deep_parens = format!("{}1{}", "(".repeat(5_000), ")".repeat(5_000));
    report.check(
        "deeply nested expressions are refused by the parser",
        matches!(
            engine.eval(&deep_parens, "nested parens"),
            Err(message) if message.contains("stack overflow")
        ),
    );

    // JSON.parse is the same hazard reached through data rather than code, and
    // is exactly what a page's fetch response would look like. The host
    // measured 1,619 levels at this limit.
    const JSON_DEPTH: usize = 20_000;
    found.json_depth_refused = JSON_DEPTH;
    let deep_json = format!(
        "JSON.parse('{}{}')",
        "[".repeat(JSON_DEPTH),
        "]".repeat(JSON_DEPTH)
    );
    report.check(
        "deeply nested JSON is refused",
        matches!(
            engine.eval(&deep_json, "nested json"),
            Err(message) if message.contains("stack overflow")
        ),
    );
    // A depth the guard does allow still has to parse correctly, or the check
    // above would pass just as well with a broken JSON parser.
    expect(
        engine,
        report,
        "JSON at a legitimate depth still parses",
        "JSON.parse('[[[[[[[[[[1]]]]]]]]]]').flat(9)[0]",
        "1",
    );
    expect(engine, report, "and the engine survived that too", "'ok'", "ok");
}

// ── running out of memory ───────────────────────────────────────────────────

/// A runtime with a deliberately small budget, to see what an allocation-heavy
/// script does when it cannot have any more.
///
/// The limit is QuickJS's own accounting rather than the kernel allocator's, so
/// this tests the graceful path — the engine refusing before it asks. What it
/// does *not* test is the kernel heap genuinely running dry, which would have
/// `malloc` return null; that path exists in the shim but exercising it at boot
/// would mean filling 32 MiB.
fn out_of_memory(report: &mut crate::selftest::Report, found: &mut Findings) {
    // A full context needs about 150 KiB, so both budgets clear that; the rest
    // is what the script gets to play with. Two of them, because the size of the
    // budget turns out to change what the failure says: see `announce`.
    let heap_before = crate::allocator::used();

    let tight = exhaust(report, "512 KiB", 512 * 1024);
    let roomy = exhaust(report, "4 MiB", 4 * 1024 * 1024);
    found.oom_error = format!(
        "512 KiB: {}; 4 MiB: {}",
        describe(&tight),
        describe(&roomy)
    );

    // The interesting question is not only whether the engine coped but whether
    // the kernel's heap came back, since both runtimes were freed while they
    // believed they had no memory left.
    let leaked = crate::allocator::used().saturating_sub(heap_before);
    report.check("the kernel heap recovers from it", leaked < 64 * 1024);
    // And a fresh engine with a normal budget still works, which is what says
    // the failure was contained in the runtime that hit it.
    report.check(
        "a later engine is unaffected",
        matches!(Engine::new().map(|next| next.eval("2 ** 10", "after oom")), Ok(Ok(text)) if text == "1024"),
    );
}

/// Build a runtime with `budget` bytes to spend and make a script spend them.
fn exhaust(
    report: &mut crate::selftest::Report,
    label: &'static str,
    budget: usize,
) -> Result<String, String> {
    let engine = match Engine::with_limits(super::DEFAULT_STACK_SIZE, budget) {
        Ok(engine) => engine,
        Err(_) => {
            report.check("a runtime fits in the budget", false);
            return Err(format!("no runtime fits in {}", label));
        }
    };
    report.check("a runtime fits in the budget", true);
    report.check("and has room to run something", engine.eval("1 + 1", "oom").is_ok());

    let outcome = engine.eval(
        "const held = []; for (let i = 0; i < 1e7; i++) held.push({ index: i, pad: 'xxxxxxxx' }); held.length",
        "allocate until it hurts",
    );
    report.check("an allocation-heavy script fails rather than succeeding", outcome.is_err());
    // QuickJS does not manage to say what went wrong: raising
    // `InternalError: out of memory` needs an allocation it has just been
    // refused, so it raises nothing and leaves the failure with no exception
    // attached. `Engine::take_exception` recognises that case and says so, which
    // is what this checks — a page that hits its budget has to produce a
    // diagnosable message rather than the word "null".
    report.check(
        "the failure explains itself",
        matches!(&outcome, Err(message) if message.contains("out of memory")),
    );
    outcome
}

fn describe(outcome: &Result<String, String>) -> String {
    match outcome {
        Err(message) if message.contains("without setting an exception") => {
            String::from("a failure with no exception attached")
        }
        Err(message) => format!("\"{}\"", message),
        Ok(text) => format!("no failure at all — it returned {}", text),
    }
}

// ── several engines at once ──────────────────────────────────────────────────

/// The slot table is global and fixed, and the DOM binding fills sixty of it. So
/// eight engines each registering the same sixty functions has to cost sixty
/// slots rather than four hundred and eighty — which it did, until the table
/// started sharing a slot between engines that register the same function.
///
/// This is not hypothetical: the browser's own self-test holds a page open for
/// each thing it is checking, and every one of them wants the whole binding.
fn sharing(report: &mut crate::selftest::Report) {
    const ENGINES: usize = 8;

    let baseline = super::registered_natives();
    let mut engines = alloc::vec::Vec::new();
    let mut all_worked = true;
    for _ in 0..ENGINES {
        let Ok(mut engine) = Engine::new() else {
            all_worked = false;
            break;
        };
        // Two names for one function and one name for another, so that both the
        // sharing and the counting have something to get wrong.
        all_worked &= engine.register_global("kernelSum", 2, sum).is_ok();
        all_worked &= engine.register_global("alsoSum", 2, sum).is_ok();
        all_worked &= engine.register_global("kernelEven", 1, even).is_ok();
        all_worked &= engine.eval("kernelSum(1, 2) + alsoSum(3, 4)", "sharing").as_deref() == Ok("10");
        engines.push(engine);
    }

    report.check("eight engines can each have the same natives", all_worked);
    report.check(
        "and they share the slots rather than taking one each",
        super::registered_natives() == baseline + 2,
    );
    // Each of the eight has to be able to reach it, not just the first — a shared
    // slot released too early would leave the others dispatching to nothing.
    report.check(
        "every one of them still works",
        engines.iter().all(|engine| engine.eval("kernelEven(4)", "sharing").as_deref() == Ok("true")),
    );

    // Dropping seven of the eight must not take the slot away from the eighth.
    let last = engines.pop();
    drop(engines);
    report.check(
        "the survivor keeps its natives",
        matches!(&last, Some(engine) if engine.eval("kernelSum(20, 22)", "sharing").as_deref() == Ok("42")),
    );
    drop(last);
    report.check("and the slots go back when the last one goes", super::registered_natives() == baseline);
}

// ── one runtime per page, twenty pages ──────────────────────────────────────

/// The browser will build a runtime per page and drop it on navigation. A leak
/// there is not a slow leak: it is 165 KiB every time someone follows a link,
/// which a 32 MiB heap notices within a couple of hundred pages.
fn leaks(report: &mut crate::selftest::Report, found: &mut Findings) {
    const CYCLES: usize = 20;

    let before = crate::allocator::used();
    let mut all_worked = true;
    for cycle in 0..CYCLES {
        let Ok(mut engine) = Engine::new() else {
            all_worked = false;
            break;
        };
        // Enough to make the runtime do real work — compile a class, allocate
        // objects, run a regexp, resolve a promise — so that the cycle exercises
        // the structures a page would leave behind rather than an empty context.
        let script = format!(
            "class C {{ constructor(n) {{ this.n = n }} double() {{ return this.n * 2 }} }}\
             const xs = [...Array(64).keys()].map(i => new C(i).double());\
             JSON.parse(JSON.stringify({{ xs }})).xs.length + /(\\w+)-(\\d+)/.exec('page-{}')[2]",
            cycle
        );
        if engine.eval_settled(&script, "cycle").is_err() {
            all_worked = false;
        }
        // Registering on every cycle is what the browser would do, and is the
        // case where a per-registration slot would leak.
        if engine.register_global("kernelSum", 2, sum).is_err() {
            all_worked = false;
        }
        if engine.eval("kernelSum(1, 2)", "cycle").is_err() {
            all_worked = false;
        }
    }
    let after = crate::allocator::used();
    found.leak_delta = after as isize - before as isize;

    report.check("twenty runtimes in a row all worked", all_worked);
    // Not exactly zero: the allocator's own bookkeeping and the strings this
    // loop built can leave the total a little either side of where it started.
    // A leaked runtime would be 165 KiB × 20, which is nowhere near this.
    report.check("the heap returns to where it started", found.leak_delta.abs() < 16 * 1024);
    // Every engine above claimed a native slot and every one of them was
    // dropped, so the table has to be empty rather than twenty deep.
    report.check("no native slots are left claimed", super::registered_natives() == 0);
}
