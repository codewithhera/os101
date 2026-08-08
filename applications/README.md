# OS101 Applications Guide

Back to project overview: [README.md](../README.md)

This folder contains all apps shown by the OS101 launcher.

At kernel build time, `kernel/build.rs` scans `applications/*/manifest.txt` and generates the app registry automatically.

## App types

OS101 supports two app styles.

## 1. Built-in apps

Built-in apps run inside kernel GUI code (`kernel/src/window.rs`) and are launched by `WindowKind`.

Example manifest:

```txt
name = Calculator
kind = builtin
window_kind = Calculator
```

Fields:
- `name`: launcher label
- `kind = builtin`
- `window_kind`: must match an existing `WindowKind` variant

Use built-in apps when you want maximum stability for core GUI utilities.

## 2. ELF userspace apps

ELF apps are separate binaries loaded by the process subsystem.

Example manifest:

```txt
name = Hello ELF
kind = elf
binary = target/x86_64-os101/release/hello-elf
```

Fields:
- `name`: launcher label
- `kind = elf`
- `binary`: relative path inside the app folder

ELF apps can be written in Rust against [`os101-user`](../os101-user), or in C
or C++ against [`os101-libc`](../os101-libc). The language makes no difference
to the kernel: it loads a static, non-PIE ELF linked at the userspace base
address either way.

## 3. C and C++ apps

`tools/os101-cc` and `tools/os101-c++` turn a source file into an app in one
command:

```bash
tools/os101-cc  -o applications/my-app/target/my-app.elf applications/my-app/main.c
tools/os101-c++ -o applications/my-app/target/my-app.elf applications/my-app/main.cpp
```

Working examples, both with a window, a self-test and a `build.sh`:

- [`hello-c`](hello-c) — printf, malloc, qsort, math and a GUI event loop
- [`hello-cpp`](hello-cpp) — virtual functions, templates, RAII, `new`/`delete`

C++ here is freestanding: classes, templates, RAII and virtuals, but no
`std::vector` or `std::string`. See
[os101-libc/README.md](../os101-libc/README.md) for what is and is not
provided.

## Manifest format

- Plain `key = value` lines
- `#` comments supported
- No nested syntax needed

## Add a new app quickly

1. Create an app folder under `applications/`.
2. Add `manifest.txt`.
3. For ELF apps, place/build the binary at the declared `binary` path.
4. Rebuild with `./run.sh`.

The launcher updates automatically from manifests.

## Remove an app

Delete its folder and rebuild.
