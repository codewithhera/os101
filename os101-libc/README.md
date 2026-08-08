# os101-libc

Back to project overview: [../README.md](../README.md)

A C library and a C++ runtime for OS101 userspace applications, so that an
ordinary C or C++ program can be cross-compiled into an OS101 app:

```bash
tools/os101-cc  -o build/hello.elf hello.c
tools/os101-c++ -o build/hello.elf hello.cpp other.cpp
```

Two worked examples live in [`applications/hello-c`](../applications/hello-c)
and [`applications/hello-cpp`](../applications/hello-cpp). Both put a window on
the desktop, which is the part that proves the syscall argument packing is
right.

## What this is not

**There is no C++ standard library.** No `std::vector`, no `std::string`, no
`std::map`, no iostreams, no `<algorithm>`. A full libc++ is a much larger
project than this one and it is out of scope.

"C++" here means *freestanding* C++, which is still most of what makes C++ worth
writing:

- classes, inheritance and virtual functions
- templates
- RAII — constructors and destructors, including for objects with static
  storage duration
- `new`, `new[]`, `delete`, `delete[]`, and the `std::nothrow` forms
- function-local statics with non-trivial constructors

Applications are compiled with `-fno-exceptions -fno-rtti`, and the driver
passes those flags itself so that what the compiler asks for and what this
library provides cannot drift apart. With exceptions enabled a program would
need `__cxa_throw`, the personality routine and a DWARF unwinder; with RTTI it
would need the `std::type_info` hierarchy. Neither is here.

**There is no filesystem and no standard input.** The kernel has no `open` or
`read` syscall yet, so `fopen` returns `NULL`, `fread` returns 0, `fgets`
returns `NULL` and `getchar` returns `EOF`, all with `errno` set to `ENOSYS`.
They exist so that ordinary code compiles and so that the reason it does not
work is obvious the first time it runs.

## The layout

| Path | What it is |
| --- | --- |
| `include/` | the headers, and only for what is implemented |
| `src/crt0.S` | `_start`: align the stack, run the constructors, call `main`, `exit` |
| `src/syscall.c` | the `syscall` instruction, once per argument count |
| `src/os101.c` | the OS interface: sbrk, console, exit, and the GUI calls |
| `src/init.c` | `__libc_init`, which walks `.init_array` before `main` |
| `src/malloc.c` | a boundary-tag allocator over `sbrk` |
| `src/stdio.c` | the printf engine, and stdio over the console |
| `src/decimal.c` | exact conversion between doubles and decimal digits |
| `src/stdlib.c` | conversions, `qsort`, `bsearch`, `rand`, the exit path |
| `src/string.c`, `src/ctype.c`, `src/errno.c`, `src/assert.c`, `src/time.c` | the rest of the C library |
| `src/math.c` | `math.h`, argument-reduced and series-evaluated |
| `src/cxxrt.cpp` | the C++ runtime support the compiler emits calls to |
| `user.ld` | one linker script for C and C++ applications |
| `tests/` | the host test suite; `tests/run.sh` builds and runs it |

Clang's own freestanding headers provide `stddef.h`, `stdint.h`, `stdarg.h`,
`stdbool.h`, `float.h` and `limits.h`. This library does not reimplement them.

## Headers

`stdio.h`, `stdlib.h`, `string.h`, `ctype.h`, `assert.h`, `errno.h`, `math.h`,
`time.h`, `sys/time.h`, and `os101.h` for the OS-specific calls. For C++:
`new`, and `cstdio`, `cstdlib`, `cstring`, `cmath` as wrappers that also put the
names in namespace `std`.

`os101.h` is the interesting one. It has the raw `os101_syscall*` wrappers and
the GUI calls as ordinary C functions:

```c
uint64_t window = os101_window_create("Hello", 320, 170);
uint64_t label = os101_label_add(window, 16, 16, "Hello from C");
os101_button_add(window, 16, 48, 120, 30, "Press me", 1);

for (;;) {
    os101_event ev = os101_event_poll(window);
    if (ev.kind == OS101_EVENT_CLOSED) {
        return 0;
    }
    if (ev.kind == OS101_EVENT_BUTTON) {
        os101_widget_update(window, label, "Pressed");
        continue;
    }
    os101_yield();   /* the scheduler is cooperative: this is not optional */
}
```

Several of the GUI syscalls pack two 32-bit values into one register; these
wrappers do that packing, so a caller never sees a register. `os101_event_poll`
never blocks — when it returns `OS101_EVENT_NONE` the application must call
`os101_yield()`, or it starves every other process on the machine.

## How the build works

The image is linked static, non-PIE, `ET_EXEC`, at `0x8010000000`, which is
`USER_BASE` in `kernel/src/process.rs`. There is no dynamic loader, no
relocation processing and no PIE support in the kernel, so anything else is a
page fault rather than a warning; `tools/os101-cc` checks the ELF it produced
before reporting success.

Two flags in the driver are worth knowing about:

- **`-mcmodel=large`.** `0x8010000000` does not fit in the 32 bits that the
  default code model assumes every address does, and without this the link fails
  with a "relocation R_X86_64_32 out of range" for every string literal in the
  program. The large model also puts code in `.ltext.*` and constants in
  `.lrodata.*`, which is why `user.ld` names those sections: left as orphans,
  lld put all of the code in the read-only segment, the kernel mapped it without
  `PF_X`, and the program faulted on its first instruction.
- **`-mno-red-zone`.** The kernel's syscall entry stub runs on the
  application's own stack and writes 520 bytes below `RSP` (it saves the SSE
  register file there — see `kernel/src/syscall.rs`), so the 128-byte red zone
  the ABI would otherwise allow does not exist on this system.

The linker is `rust-lld` in its GNU flavour, found under the Rust toolchain the
kernel already needs, because Apple's `ld` cannot produce ELF at all. `ld.lld`
and `llvm-ar` are used instead when they happen to be installed. Nothing needs
Homebrew.

Library objects are cached under `build/os101-libc/` and rebuilt only when a
source or a header is newer, so the second build of an application is two or
three compiler runs rather than twenty.

## Accuracy, and other things worth knowing

- **`printf`** implements `%d %i %u %x %X %o %c %s %p %%` and `%f %e %g` with
  their uppercase forms, the `h`, `hh`, `l`, `ll`, `z`, `t` and `j` length
  modifiers, `*` widths and precisions, and the `-`, `+`, space, `0` and `#`
  flags. The float conversions go through `decimal.c`, which converts exactly,
  so the digits agree with a hosted libc byte for byte — including the
  round-half-to-even that a tie is supposed to get. `snprintf` truncates
  correctly and returns the length it would have written.
- **`strtod`** is correctly rounded: the nearest double to the digits, ties to
  even, decided by exact integer comparison rather than by scaled arithmetic. A
  value printed by this library's `printf` reads back as itself. The
  hexadecimal form (`0x1.8p3`) is supported. Beyond 400 decimal exponents the
  answer is only ever zero or infinity and is returned as such with `ERANGE`.
- **`math.h`** is accurate to about one unit in the last place for `exp`, `log`,
  `log2`, `log10`, `exp2`, `expm1`, `log1p`, `cbrt`, `sin`, `cos` and `atan`,
  two for `asin`, `acos`, `atan2`, `cosh`, `tanh` and `pow`, and three for `tan`
  and `sinh`, measured against the build machine's libm by
  `tests/test_math.c`. `sqrt` is a single `sqrtsd` and is correctly rounded.
  `fmod` is exact. The rounding functions are exact and use bit manipulation
  rather than SSE4.1's `roundsd`, which QEMU's default CPU model does not have.
  The weak spot is `sin`, `cos` and `tan` of very large arguments: the
  reduction knows π/2 to 105 bits, so at 10^14 the reduced argument carries an
  absolute error of about 10^-19 — which is tens of ulp where the result is
  near a zero — and past 2^53 the result is a number of the right size and not
  a meaningful sine. That needs Payne-Hanek reduction, which is not here.
- **`malloc`** is a boundary-tag allocator with a free list, splitting,
  coalescing in both directions, and 16-byte alignment. It asks `sbrk` for at
  least 64 KiB at a time and hands memory back when a megabyte has piled up
  free at the top. `realloc` grows in place when the block above it is free.
- **`rand`** is xorshift64\*, reproducible from a seed, and `rand()` before any
  `srand()` behaves as if `srand(1)` had been called.

## Tests

```bash
./os101-libc/tests/run.sh
```

The portable half of the library is compiled for the build machine and linked
into one program with the tests, which call the *host's* `snprintf`, `strtod`
and libm as the reference. Two C libraries in one program only works because
every name this one defines is renamed to `os101_*` (`tests/host_names.h`), and
`run.sh` checks with `nm` that the renaming is complete before it runs anything
— a name left out would silently replace the host function it was supposed to be
compared against.

Roughly 370,000 assertions: tens of thousands of generated `printf` formats
compared byte for byte, `string.h` and `ctype.h` against the host's at every
alignment, `strtol`/`strtoul`/`strtod` including where the end pointer lands and
what `errno` becomes, a thousand `printf`/`strtod` round trips, `qsort` and
`bsearch` against the host's, a malloc torture test with poison patterns and
overlap checking, and a ulp-by-ulp sweep of `math.h`.

What the host cannot test needs the kernel, and is covered by inspecting the
linked ELF and by the two example applications: `crt0.S`, the syscall wrappers,
the GUI calls, `time.c`, the walk of `.init_array`, and the C++ runtime.

## The next things the ABI needs

In rough order of how much they would open up:

1. `open`/`read`/`close`, so `fopen` and `fread` can be real. The kernel has
   FAT32 and O1FS already; only the syscalls are missing.
2. A blocking `get_event`, so an idle application can sleep instead of spinning
   through `os101_yield`. `kernel/src/process.rs` has `sleep_on_window` for
   exactly this and the GUI path does not use it.
3. `argc`/`argv`, which the kernel does not pass at all today — `main` is
   called with `(0, NULL, NULL)`.
4. Per-process CPU time, so `clock()` can mean what it says rather than being
   wall clock since the first call.
