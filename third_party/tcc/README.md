# TinyCC on OS101

Vendored [TinyCC](https://bellard.org/tcc/) (mob branch, `0.9.28rc`) so the kernel
can compile C programs at runtime with no host toolchain involved.

```
third_party/tcc/
  src/          upstream sources (x86_64 only), plus config.h
  include/      TCC's freestanding headers (stddef, stdarg, tccdefs, …)
  lib/          libtcc1 helpers shipped onto /disk for the linker
  patches/      any diffs against upstream (kept empty while unpatched)
  README.md     this file
```

## Provenance

```
https://github.com/TinyCC/tinycc  (mob)
VERSION  0.9.28rc
```

The 0.9.27 Savannah tarball was unreachable when this was vendored; mob is the
actively maintained line and already emits `ET_DYN` / `DF_1_PIE` for executables
when `CONFIG_TCC_PIE` is set — which is what lets the kernel load TCC output at
`USER_BASE` (`0x8010000000`) despite the small code model.

Sources are copied, not submodule'd, matching `third_party/quickjs/`. Prefer
updating by replacing `src/` and `include/` from a newer mob snapshot and
re-running the boot self-test; put any unavoidable edits in `patches/`.

## How it is built

`third_party/libc-shim/rust/build.rs` compiles `libtcc.c` as a single translation
unit (`ONE_SOURCE`) with the same freestanding clang flags used for QuickJS, and
archives it into the shim's static library. The kernel never talks to TCC's
sources directly — only through `kernel/src/tcc/`.

## What the shim had to grow

TCC needs a real `setjmp`/`longjmp`, `FILE*` I/O (`fopen`/`fread`/`fwrite`/
`fseek`/`ftell`/`fclose`/`fdopen`) and POSIX `open`/`read`/`write`/`close`/
`lseek`/`unlink`. Those are implemented in the libc shim against the kernel VFS
so TCC can read `/disk/hello.c` and write `/disk/hello`. See
`third_party/libc-shim/README.md`.
