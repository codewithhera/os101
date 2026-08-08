# QuickJS on OS101

This directory holds a real JavaScript engine and the freestanding C environment
it needs in order to run inside the kernel. It is wired in and it runs on the
metal: `kernel/src/quickjs/` is the embedder, and its boot self-test evaluates
JavaScript on every boot. The browser runs its page scripts on it —
`kernel/src/browser/script.rs` and `browser/domjs.js` are the DOM binding — and
the hand-written interpreter it used to use has been deleted. There is one
engine.

```
third_party/
  quickjs/            the vendored engine, byte-for-byte upstream
  libc-shim/
    include/          the <stdlib.h>, <string.h>, <math.h> and friends it compiles against
    src/              the parts of that libc that are pure computation,
                      plus quickjs_glue.c — the embedder's side of the API
    rust/             the parts that need the kernel: heap, clock, serial, panic
  host-test/          the same engine and shim, built and run on the host
  check-symbols.sh    proves the bare-metal build has nothing left to resolve
```

## The engine

QuickJS **2026-06-04**, from <https://bellard.org/quickjs/>.

```
quickjs-2026-06-04.tar.xz
sha256  b376e839b322978313d929fd20663b11ba58b75df5a46c126dd19ea2fa70ad2a
```

Bellard's original rather than the `quickjs-ng` fork, because the original turned
out to need nothing from us that we did not already have to write: with the shim's
headers on the include path it compiles for a bare-metal target **with no source
modifications at all**, which is the property that matters most for keeping it
updatable. `quickjs-ng` is the better choice if the project later wants its CMake
build, its `JS_AddIntrinsic*` granularity or its more active issue tracker, and
nothing in the shim is specific to the original — but there was no problem here
for the fork to solve.

`quickjs/src/` holds `quickjs.c`, `dtoa.c`, `libregexp.c`, `libunicode.c` and
`cutils.c` plus their headers, which is exactly the `QJS_LIB_OBJS` list from
upstream's Makefile. Note that `dtoa.c` is on it: this release moved the
double-to-string and string-to-double conversions out of the engine into their
own file, and it is not optional. Being separate is useful to know, because it
means **no JavaScript number is ever formatted by our printf** — the engine does
all of that itself, with integer arithmetic and its own correct rounding.

`quickjs-libc.c` is deliberately absent. That is the file that wants `fopen`,
`popen`, `dlopen`, threads and a working `/dev/urandom`, and it provides nothing
the browser needs. Leaving it out is most of the reason this works at all.

`quickjs/patches/` does not exist, because nothing needed patching. If that ever
changes, put the patch in a file there rather than editing the sources, so the
next release can still be dropped in.

To move to a newer release: download and unpack the tarball, copy those ten files
and `LICENSE`, `VERSION` and `Changelog` over the existing ones, and run the three
checks below. `VERSION` is read by the build recipe and becomes
`CONFIG_VERSION`, so it has to be updated with them.

## What the shim provides

QuickJS was compiled for the kernel's target and every undefined symbol was
looked at; the list below is exactly that list, with nothing added. Fifty-six
symbols, and rather more than half of them were already in the kernel's link.

**Written in C** (`libc-shim/src/`) — the parts that are pure computation and
have no business knowing about a kernel:

| symbol | file |
| --- | --- |
| `printf` `fprintf` `vfprintf` `snprintf` `vsnprintf` | `printf.c` |
| `fputc` `fputs` `fwrite` `putchar` `fflush` | `printf.c` |
| `os101_shim_stdout` `os101_shim_stderr` | `printf.c` |
| `memchr` `strcmp` `strchr` `strrchr` | `string.c` |
| `abs` | `stdlib.c` |

`libc-shim/src/quickjs_glue.c` is in the same directory and on the same compile
list, but it is not part of the libc. It is the embedder's side of the API:
`<quickjs.h>` declares `JS_FreeValue`, `JS_IsException`, `JS_NewInt32`,
`JS_ToCString`, `JS_NewCFunction` and every tag predicate as `static inline`, so
there is no symbol for Rust to bind to, and the glue exports real `os101_qjs_*`
functions for the ones the kernel needs. It also keeps `JSValue` — sixteen bytes
of union plus tag, passed and returned by value throughout the API — from ever
crossing the boundary by value: every function there takes and returns them
through pointers, including the trampoline that lets a Rust function be called
from JavaScript without having `JSValue` in its signature. That trampoline is the
one symbol here the kernel has to supply, `os101_qjs_native_dispatch`, which
`kernel/src/quickjs/mod.rs` defines.

**Written in Rust** (`libc-shim/rust/src/`) — the parts that can only come from
the kernel:

| symbol | file |
| --- | --- |
| `malloc` `realloc` `free` `malloc_usable_size` | `malloc.rs` |
| `modf` `lrint` `atanh` | `math.rs` |
| `gettimeofday` `clock_gettime` `localtime_r` | `clock.rs` |
| `os101_shim_write_bytes` | `output.rs` |
| `abort` `os101_shim_assert_fail` | `panic.rs` |

**Already in the kernel's link, from `compiler_builtins`.** These cost nothing
and are not duplicated:

* `memcpy` `memmove` `memset` `memcmp` `strlen` — the `compiler-builtins-mem`
  group, which `kernel/.cargo/config.toml` already enables.
* `__udivti3` — 128-bit unsigned division, which QuickJS uses in `dtoa.c`.
* Thirty libm functions: `acos` `acosh` `asin` `asinh` `atan` `atan2` `cbrt`
  `ceil` `cos` `cosh` `exp` `expm1` `fabs` `floor` `fmax` `fmin` `fmod` `hypot`
  `log` `log10` `log1p` `log2` `pow` `round` `sin` `sinh` `sqrt` `tan` `tanh`
  `trunc`. These are the pure-Rust `libm` port, which now lives inside
  `compiler_builtins` and is exported under C names for any target whose `os` is
  `none`. Taking `libm` as a direct dependency would have meant a registry fetch
  and a second copy of the same code in a 32 MiB machine; using the copy already
  present costs nothing. They are weak symbols, so `math.rs` could override any
  of them if a future release of one turns out to be wrong.

  QuickJS wants thirty-three, and `atanh`, `modf` and `lrint` are not among the
  thirty — `atanh` in particular is missing while its neighbours `asinh` and
  `acosh` are present. That gap is why `check-symbols.sh` exists.

**From the kernel itself**: `__rust_alloc`, `__rust_dealloc`, `__rust_realloc`
and `__rust_no_alloc_shim_is_unstable_v2`, generated by the `#[global_allocator]`
in `kernel/src/allocator.rs`. Every byte QuickJS allocates therefore comes out of
the same 32 MiB heap as the rest of the kernel and shows up in
`allocator::used()`.

Some things the headers supply are not symbols at all, which is why they are not
in the tables: `assert`, `alloca`, `isnan`/`isinf`/`isfinite`/`signbit`/
`fpclassify`/`nan`, the whole of `<ctype.h>`, and all fifteen `pthread_mutex_*`
and `pthread_cond_*` calls. The pthread ones are static inline no-ops — see
`include/pthread.h` for why pretending to have threads was cheaper than
persuading `quickjs.c` not to want them.

## Building it

The recipe is `libc-shim/rust/build.rs`. It compiles the six translation units
with clang and archives them with `llvm-ar`, then tells Cargo to link the result.
It shells out to the compiler rather than using the `cc` crate on purpose: `cc`
would mean a registry fetch, and nothing in this repository's build is allowed to
need the network.

The exact compile command, one per unit:

```
clang --target=x86_64-unknown-none-elf \
      -ffreestanding -fno-stack-protector -mno-red-zone -fPIC \
      -std=gnu11 -O2 -fwrapv -fno-builtin -fno-strict-aliasing \
      -DCONFIG_VERSION="2026-06-04" \
      -Ithird_party/libc-shim/include -Ithird_party/quickjs/src \
      -c <unit>.c -o <unit>.o
```

and then

```
llvm-ar crs libquickjs.a quickjs.o dtoa.o libregexp.o libunicode.o cutils.o \
                         printf.o string.o stdlib.o quickjs_glue.o
```

Three of those flags are load-bearing. `-fwrapv` is in QuickJS's own Makefile
because the engine relies on signed overflow wrapping rather than being undefined.
`-mno-red-zone` is the kernel's ABI: an interrupt taken in kernel mode would
scribble on the red zone. SSE is left **on**, which is the default for this clang
target and is what the kernel's own target now does too.

`llvm-ar` rather than `ar` matters on macOS: Apple's `ar` writes a Mach-O style
symbol table into an archive of ELF objects, and the ELF linker cannot read it.
`rust-toolchain.toml` already requires the `llvm-tools-preview` component that
ships one, and the build script finds it in the toolchain's `bin` directory.

Environment overrides: `OS101_SHIM_CC`, `OS101_SHIM_AR`, `OS101_SHIM_NDEBUG=1`
(compiles out QuickJS's ~160 assertions, which measurement says saves under 8 KiB
of the 900 — so there is no size case for it, only a speed one).

### How it is integrated

Two lines, and both are already there:

```toml
# kernel/Cargo.toml
os101-libc-shim = { path = "../third_party/libc-shim/rust" }
```

```rust
// kernel/src/main.rs, once the heap and the RTC are up — this is quickjs::install()
os101_libc_shim::install(rtc::unix_millis, |_stream, bytes| serial::write(bytes));
```

**The second line is not optional, and not just for the clock.** If nothing in
the kernel names a Rust item from the crate, rustc drops the crate from the graph
entirely and every `JS_*` symbol goes undefined — even though the archive is
bundled inside the crate's own rlib. This was the first thing that went wrong when
proving the link, and the error message points nowhere near the cause.

Do not add the C compile to `kernel/build.rs`. It belongs to the shim crate, and
keeping it there is what makes the integration two lines.

Everything above that line is in `kernel/src/quickjs/`: an `Engine` that owns a
runtime and a context and frees them together, `eval` and `eval_settled`,
`pump_jobs`, and `register_global` for exposing a Rust function to JavaScript.
`kernel/src/quickjs/selftest.rs` runs on every boot.

## Checking it

Three commands, none of which needs the network:

```bash
cd third_party/libc-shim/rust && cargo test          # the Rust half, 23 tests
cd third_party/host-test && ./run.sh                 # real JavaScript, on the host
cd third_party/host-test && ./run.sh x86_64-apple-darwin   # again, on the kernel's architecture
cd third_party && ./check-symbols.sh                 # nothing left undefined for the kernel
```

The host harness deserves a note, because it would be easy to mistake for a test
of macOS's libc. It compiles QuickJS, the shim's `printf.c`, `string.c` and
`stdlib.c` and the driver itself with `-ffreestanding` and *this* directory's
headers. Only four things are borrowed from the host — the real allocator, the
real clock, `write(2)` and `abort` — and they come through `host_platform.c`,
which is the one file compiled against the system headers. `host_glue.c` stands
in for the Rust crate at that boundary, and mirrors its sixteen-byte allocation
header so that the memory numbers the harness prints transfer to the kernel.

It takes a target triple, because the stack figures are architecture-specific and
the kernel's architecture is x86_64. On an Apple Silicon machine
`./run.sh x86_64-apple-darwin` runs the same harness under Rosetta.

## What it costs

Measured against `kernel/x86_64-os101.json`, release, assertions left on.

| | bytes |
| --- | --- |
| `libquickjs.a` | 1,417,184 |
| of which code and constants (`.text` + `.data`) | 928,567 |
| `quickjs.o` alone | 805,075 |
| `libunicode.o` (the Unicode tables) | 60,402 |
| everything the shim itself adds (`printf.o` `string.o` `stdlib.o`) | 10,096 |
| `quickjs_glue.o` | 5,912 |

Measured against the kernel that is actually built, before and after adding the
dependency:

| | before | after | growth |
| --- | --- | --- | --- |
| `os101-kernel` ELF | 2,616,672 | 3,756,528 | +1,139,856 |
| `.text` | 2,212,871 | 3,240,036 | +1,027,165 |
| `.data` | 298,850 | 323,344 | +24,494 |
| `.bss` | 59,208 | 60,788 | +1,580 |
| `build/os101-bios.img` | 4,686,848 | 5,735,424 | +1,048,576 |

The `.text` growth is a little more than `libquickjs.a`'s own 929 KiB of code and
constants, because pulling in the shim's Rust half also pulls in the parts of
`core` and `alloc` it needs that the kernel was not already using.

So the image grows by exactly 1 MiB, which is the bootable image's padding
granularity rather than a coincidence. The bootloader and QEMU load it without
complaint and boot time is unchanged.

Runtime memory, as QuickJS's own accounting reports it — which is exact here,
because `malloc_usable_size` returns the size that was actually asked for:

| | bytes | blocks |
| --- | --- | --- |
| `JS_NewRuntime` alone | 36,264 | 10 |
| `+ JS_NewContextRaw` and `JS_AddIntrinsicBaseObjects` | 122,784 | 37 |
| `+ JS_NewContext` (every intrinsic) | 165,776 | 56 |
| `+ a small script, after a GC` | 159,512 | 49 |

A runtime and a full context is **162 KiB**, half a percent of the heap. A page
with a moderate amount of script should be budgeted at a megabyte or two;
`JS_SetMemoryLimit` is there to make that a decision rather than a discovery, and
`kernel/src/quickjs/` sets it — 8 MiB of a 32 MiB heap.

On the metal the same measurement, taken from `allocator::used()` either side of
`Engine::new()`, is **148 KiB**, against 151,328 bytes in 47 blocks by the
engine's own count. Both numbers are a little under the host's, because the
kernel's `JS_NewContext` builds the same intrinsics against a heap whose
allocations round differently; the shape is the same and the budget above holds.
Twenty create/destroy cycles return the heap to **exactly** where it started.

If 162 KiB ever matters, `JS_NewContextRaw` plus a chosen subset of
`JS_AddIntrinsic*` gets a working context in 123 KiB, and that is with the base
objects, which include `Object`, `Function`, `Error` and `Array`.

## The stack is the thing to worry about

`JS_DEFAULT_STACK_SIZE` in `quickjs.h` is **1 MiB**, and
`kernel/src/main.rs` sets `kernel_stack_size = 1024 * 1024`. Those are the same
number. A runtime left at its default is permitted to consume the entire kernel
stack, and what happens then is a page fault on the guard page from inside the
interpreter — or, if the fault handler needs a frame it cannot get, a triple fault
and a silent reboot.

The good news is that the limit is honoured tightly. QuickJS checks the stack
pointer against `stack_top - stack_size` at every function entry, every parser
recursion and every `alloca`, with the `alloca` size included in the check.
Measured on x86_64:

| `JS_SetMaxStackSize` | C stack actually reached | JS frames | nested parens the parser accepts | `JSON.parse` nesting |
| --- | --- | --- | --- | --- |
| 64 KiB | 63 KiB | 63 | 53 | 390 |
| 128 KiB | 127 KiB | 128 | 109 | 800 |
| 256 KiB | 255 KiB | 258 | 219 | 1,619 |
| 512 KiB | 511 KiB | 518 | 441 | 3,257 |

Every overrun in that table is a clean, catchable JavaScript exception — the
engine never got near the guard page, and never crashed. Note **which** exception,
because it is not the one other engines raise: the interpreter throws
`InternalError: stack overflow` and the parser throws `SyntaxError: stack
overflow`. V8 and SpiderMonkey both raise a `RangeError` here, and so does
`quickjs-ng`, so page script that tests `e instanceof RangeError` will not
recognise this. Changing it would mean patching the vendored engine, which is not
worth the property it would cost.

On the metal, `kernel/src/quickjs/`'s 256 KiB limit gives **251 frames** of
JavaScript recursion before the exception, against the 258 the host measured at
the same limit — the difference being the frames between `kernel_main` and the
`JS_NewRuntime` that captured the stack top.

**The recommendation is `JS_SetMaxStackSize(rt, 256 * 1024)`**, and it should be
called immediately after `JS_NewRuntime`. `os101_qjs_new_runtime` in
`quickjs_glue.c` does both in one function precisely so that the second cannot be
forgotten, and refuses a size of zero. That leaves three quarters of the stack
for everything else, which the browser needs: the call path from the compositor
down through `window.rs` and `browser::layout` to `JS_Eval` is already several
frames deep before the engine starts, and the DOM bindings that JavaScript calls
back into will add frames of their own *above* the limit that QuickJS is checking
against — a native callback's stack use is not counted until the next re-entry
into the interpreter. 256 KiB buys 258 levels of JavaScript recursion and 219
levels of expression nesting, which is far more than a real page uses; anything
that needs more is a runaway or an attack.

512 KiB is the absolute ceiling and should only be considered if a real page turns
out to need it. Anything above that is asking for the failure this table exists to
prevent.

Two smaller points that come from the same mechanism:

* `stack_top` is captured by `__builtin_frame_address(0)` inside
  `JS_NewRuntime`. If the runtime is created on one stack and used from another —
  a different task, an interrupt handler — the limit is measured from the wrong
  place and means nothing. `JS_UpdateStackTop(rt)` re-captures it, and must be
  called on the stack the engine is about to run on.
* The parser is the hungriest part, at roughly 1.2 KiB of stack per level of
  expression nesting against 1 KiB per interpreted call. Untrusted markup from a
  web page is the obvious way to reach it, and the guard is what stands in the
  way.

## Known deviations, and what was not verified

* **`Atomics.wait` with an infinite timeout** would return `"ok"` immediately
  instead of blocking, because the pthread condition variables are no-ops. The
  path is unreachable: it first checks `JSRuntime::can_block`, which
  `JS_NewRuntime` leaves false and nothing in this OS sets.
* **`localtime_r` reports UTC and fills in only `tm_gmtoff`.** QuickJS reads only
  that field. UTC is also the honest answer — `rtc.rs` reads the CMOS clock as
  UTC and this OS has no timezone database — so `getTimezoneOffset()` returning
  zero is correct rather than a stub.
* **The shim's `printf` is not a complete printf.** It covers what the vendored
  sources use, which is `d i u o x X c s p f e g` with the `- 0 + space #` flags,
  `*` widths and precisions, and the `hh h l ll z t j` length modifiers, and it is
  checked against Apple's libc for eighteen cases including tie-rounding. `%a`,
  `%n` and wide characters are absent. In the exponential form a remainder of
  exactly 0.5 is taken at face value rather than tested for being a true tie, so
  `%e` and `%g` can disagree with glibc in the last digit; `%f` does the tie test
  exactly. Nothing JavaScript-visible depends on any of this, because `dtoa.c`
  formats every number itself.
* **`clang --target=x86_64-unknown-none-elf` is not literally the kernel's
  target.** It is the `llvm-target` that `kernel/x86_64-os101.json` names, with
  the same data layout, the same red-zone setting and the same SSE2-and-no-more
  feature set, which is what the ABI depends on. There is no way to hand a Rust
  target JSON to clang, so the two lists have to be kept in step by hand — if the
  kernel's `features` line changes, `target_flags()` in `build.rs` has to change
  with it.
* **A soft-float target is refused outright.** `build.rs` fails the build if
  `sse2` is missing from the target's features, because Rust would then be using
  the soft-float ABI — passing doubles in general-purpose registers where clang
  passes them in `xmm0`. The two halves link without complaint and disagree about
  every `double` crossing the boundary, which is most of QuickJS's API. This is
  not hypothetical: the stock `x86_64-unknown-none` target is declared
  `"rustc-abi": "softfloat"`, and it is what the kernel used until
  `x86_64-os101.json` landed.
* **Running out of memory produces no exception object.** When
  `JS_SetMemoryLimit` refuses an allocation, QuickJS wants to raise
  `InternalError: out of memory` — which needs an allocation it has just been
  refused, so `JS_ThrowOutOfMemory` suppresses itself and returns `JS_EXCEPTION`
  with nothing pending. `JS_GetException` then answers `null`. This was measured
  at both a 512 KiB and a 4 MiB budget and happens at both, because the
  allocation that finally fails is a small one either way.
  `Engine::take_exception` recognises the case and says so rather than reporting
  the word `null`, but a page that exhausts its budget cannot be told *what* went
  wrong, only that something did. The failure itself is clean: the script stops,
  the runtime frees, the kernel heap comes back and later runtimes are
  unaffected.
* **`check.sh` and `test.sh` still do not run the three commands above.** They
  are run by hand.

## What running on the metal added

The bare-metal side was originally proved only by linking. It now runs:
`kernel/src/quickjs/selftest.rs` is 86 checks and 0.20 s of every boot, and
covers the things the host harness could not reach — `Date.now()` against the
kernel's own RTC, the stack guard against the kernel's actual 1 MiB stack, an
exhausted `JS_SetMemoryLimit`, a Rust function called from JavaScript through the
trampoline, the cycle collector, and twenty runtimes created and destroyed in a
row with the kernel heap measured either side.

What is still unverified is concurrency: the self-test runs at boot from
`kernel_main` with only the timer interrupt live, and nothing has yet evaluated
JavaScript from inside the main event loop, from a second task, or with a page
fault taken mid-interpretation. `JSRuntime` is not thread-safe and
`JS_UpdateStackTop` would have to be reconsidered if an engine were ever used
from a stack other than the one it was created on.

## Not to be confused with `os101-libc/`

There is a second libc in this tree, and the two are not alternatives. `os101-libc/`
is for **userspace**: it has a `crt0.S`, a syscall layer and a linker script, and
its malloc asks the kernel for memory through a system call. This shim is for
**inside** the kernel: no syscalls, no process, `malloc` reaching straight into
`allocator.rs`, and no entry point because the kernel already has one.

Neither can be built with the other's headers, and neither include directory ever
appears on the other's command line. They would become the same problem only if
the browser were moved out of the kernel into a userspace process, at which point
`os101-libc/` is the environment QuickJS should be built against and this
directory's `libc-shim/` becomes unnecessary. That is a much larger decision than
this work makes.

## Licences

QuickJS is MIT; `quickjs/LICENSE` is upstream's copy, unmodified. Everything
under `libc-shim/` and `host-test/` was written for this repository and is covered
by the licence at the root of the tree.
