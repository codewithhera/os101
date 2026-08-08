# OS101 SDK

Back to project overview: [README.md](../README.md)

`os101-sdk` helps you build userspace ELF apps for OS101.

This is intended for students and hobby OS developers who want to write small apps and understand syscall-driven userspace.

## Why this SDK was made

The SDK was created so app developers do not need to repeatedly set up low-level build details by hand.

It standardizes:
- target/toolchain expectations
- project template shape
- build output conventions for launcher/process integration

This is necessary because OS userspace apps are more fragile than normal desktop apps: one wrong target/linker/layout choice can produce a binary that does not run.

With SDK, contributors can focus on app behavior instead of fighting configuration.

## Create a starter app

```bash
cp -r os101-sdk/templates/app my-os101-app
```

Then edit:
- `my-os101-app/src/main.rs`

## Build an app

```bash
./os101-sdk/build.sh my-os101-app
```

Expected output is an ELF binary in:
- `my-os101-app/target/x86_64-os101/release/`

## Register the app in OS101 launcher

Create `applications/my-os101-app/manifest.txt`:

```txt
name = My OS101 App
kind = elf
binary = target/x86_64-os101/release/my-os101-app
```

Then run:

```bash
./run.sh
```

## Notes

- Target must be `x86_64-os101` (`kernel/x86_64-os101.json`), the same one the kernel uses
- Apps should be `#![no_std]`
- Keep heap and allocations small in early versions

For full project roadmap and contribution info, see:
- [README.md](../README.md)
- [TASKS.md](../TASKS.md)

## How people should use it going forward

1. Start from `os101-sdk/templates/app`.
2. Keep the app minimal and verify basic syscall behavior first.
3. Build with SDK script.
4. Register via app manifest.
5. Test in QEMU with `./run.sh`.
6. Iterate feature-by-feature.
