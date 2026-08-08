# OS101 — Learning Roadmap and Project Status

Back to project overview: [README.md](README.md)

OS101 is a teaching-first operating system project designed for students and hobby developers who want to understand OS construction from first principles.

Initiated by **SM Mamunur Rahaman** from curiosity and built with **100% vibe coding**.

- LinkedIn: https://www.linkedin.com/in/sm-mamunur-rahman/

## Project architecture

The project is split into independent parts:

- `kernel/` — core OS kernel (`no_std`, `x86_64-os101`)
- `tools/` — host-side utility to create a bootable disk image
- `applications/` — app manifests and app binaries/sources
- `os101-user/` — syscall library for userspace apps
- `os101-sdk/` — template + build helper for apps
- `run.sh` — full build-and-boot orchestration

This separation keeps the learning path clean and makes debugging easier.

## One-time setup

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

rustup toolchain install nightly
rustup component add rust-src llvm-tools-preview --toolchain nightly

sudo apt install qemu-system-x86
```

Then verify:

```bash
./check.sh
```

## How to run

```bash
./run.sh
```

## Phase-by-phase status

### Phase 1 — Boot and framebuffer text
- [x] Bootable kernel image
- [x] Framebuffer console output
- [x] Panic handling

### Phase 2 — Debugging infrastructure
- [x] Serial COM1 output
- [x] QEMU debug-exit integration
- [x] Basic in-VM test harness

### Phase 3 — Interrupts and input
- [x] GDT + TSS setup
- [x] IDT exception stubs
- [x] Timer IRQ
- [x] Keyboard IRQ input path

### Phase 4 — Shell
- [x] Command prompt and line editing
- [x] Built-in commands (`help`, `echo`, `clear`, `uptime`, `mem`, `reboot`, `panic`)

### Phase 5 — Memory management
- [x] Memory-map parsing
- [x] Paging + frame allocator
- [x] Kernel heap allocator
- [x] `alloc` support in kernel

### Phase 6 — Mouse
- [x] PS/2 mouse init
- [x] IRQ12 packets
- [x] Software cursor + click tracking

### Phase 7 — Graphics
- [x] Double buffer
- [x] Dirty-rect updates
- [x] 2D primitives (line/rect/circle)

### Phase 8 — GUI and windows
- [x] Window manager
- [x] Z-order and dragging
- [x] Widgets (label/button/textbox/textarea/checkbox/canvas)

### Phase 9 — Userspace and syscalls
- [x] ELF loading
- [x] Ring3 transition path
- [x] Syscall entry/return
- [x] Cooperative process scheduling

### Phase 10 — Filesystem
- [x] Block abstraction
- [x] FAT32 read support
- [x] VFS-style command access
- [x] initramfs integration

### Phase 11 — App platform
- [x] App registry via manifests
- [x] SDK and template flow
- [x] Launcher model for built-in and ELF apps

### Phase 12 — GUI apps
- [x] Calculator
- [x] Notepad
- [x] Paint
- [x] Snake
- [x] Image viewer
- [x] Settings
- [x] About
- [x] File manager and terminal improvements
- [x] Procedural desktop wallpaper, cached and redrawn only when the theme changes
- [x] Paint: bitmap canvas, 16-colour palette, four brush sizes, eraser, flood fill

### Phase 13 — Runtime app installation
- [x] `.opk` package format (header + manifest + ELF payload, CRC-checked)
- [x] Shared `os101-package` crate so kernel and host tooling agree
- [x] Runtime app registry replacing the compile-time-only table
- [x] `pkg install/list/info/run/remove` shell commands
- [x] `/apps` writable mount, scanned at boot
- [x] `os101-tools pack` / `inspect` for building packages on the host
- [x] Package validation unit tests (`cd os101-package && cargo test`)
- [ ] Persist `/apps` across reboots (needs a writable block device)
- [ ] Package signing

### Phase 14 — Memory isolation
- [x] Per-process address spaces (userspace owns level-4 slot 1)
- [x] Address space swapped on context switch
- [x] Full teardown on exit/fault: pages, page tables, frames reclaimed
- [x] `vm` command to inspect the live layout
- [ ] Copy-on-write and demand paging
- [ ] Swap out to disk

### Phase 15 — Networking
- [x] PCI bus enumeration and BAR decoding
- [x] Contiguous DMA allocation for device rings
- [x] e1000 NIC driver (polled RX/TX)
- [x] Ethernet + ARP with a resolution cache
- [x] IPv4 with checksums, and ICMP echo in both directions
- [x] UDP, plus DHCP for address configuration
- [x] DNS resolver with a cache
- [x] TCP client state machine
- [x] HTTP/1.1 client, redirects and chunked encoding
- [x] `pci`, `net`, `ping`, `host`, `wget` shell commands
- [x] Boot-time self-test over the stack's pure logic
- [x] TLS 1.3, for HTTPS (see phase 19)
- [ ] Socket syscalls so userspace apps can use the network
- [ ] Interrupt-driven receive (needs an IRQ-safe allocation path)
- [ ] TCP retransmission and out-of-order reassembly

### Phase 16 — Browser rendering engine
- [x] DOM tree and a forgiving HTML parser (void elements, implicit closes)
- [x] HTML character reference decoding
- [x] CSS parser: selector lists, longhand expansion, colours, lengths
- [x] Viewport-relative units resolved against the real canvas
- [x] Style tree: user-agent sheet, author sheet, inline styles, specificity, inheritance
- [x] Box layout: block stacking, margins, padding, borders, inline line breaking
- [x] Display list and painting, with bold text and underlined links
- [x] Pixel scrolling and link hit-testing in the browser window
- [x] Boot-time self-test covering every stage
- [x] Tables laid out as grids rather than stacked blocks
- [x] Multiple font sizes, so headings step up through four real faces
- [x] Descendant and child combinators, and attribute selectors
- [x] Collapsed margins, `max-width` and auto-margin centring
- [x] Images decoded and drawn inline, with alt text as the fallback
- [ ] Floats
- [ ] Forms: text fields, checkboxes and submission

### Phase 17 — Scripting
- [x] QuickJS compiled into the kernel, against a freestanding libc of our own
- [x] The whole language: classes, generators, `async`/`await`, Promises, `BigInt`,
      real regular expressions — the hand-written interpreter is deleted
- [x] A time budget enforced by QuickJS's interrupt handler, uncatchably, so a
      runaway script cannot hang the kernel
- [x] DOM bindings: queries, text, `innerHTML`, attributes, classes, `dataset`,
      inline styles, the mutation family, `getBoundingClientRect`
- [x] DOM events: capture and bubble phases, a cancellable event object,
      `addEventListener`, `on...` properties and attributes; click, input, change,
      submit, keydown, DOMContentLoaded, load
- [x] `setTimeout`, `setInterval` and `requestAnimationFrame` on the kernel clock,
      driven from the browser's event loop
- [x] Promise jobs drained after every script, every event and from the idle path
- [x] `location`, `navigator`, `localStorage` (persisted to `/disk`), `console`
- [x] Batched relayout: a hundred appends in a loop cost one layout
- [x] External `<script src>` files, under a shared fetch budget
- [x] Boot-time self-tests for the engine and for the bindings
- [ ] `fetch`/`XMLHttpRequest` (both are present but always fail)
- [ ] `document.write`, `MutationObserver`, computed styles from the cascade

### Phase 18 — Searching, saving, and the desktop
- [x] Address bar that searches when what you typed is not a URL
- [x] HTTPS, at first through plain-HTTP gateways and now directly (phase 19)
- [x] Image search, and pulling every picture on a page into one view
- [x] PNG and JPEG decoders
- [x] ATA PIO driver and O1FS, a small persistent filesystem at `/disk`
- [x] A data disk the build never overwrites, so saved files survive a rebuild
- [x] Right-click menus in the browser and the file manager
- [x] Saving a picture from the web to `/disk/downloads`
- [x] Setting a picture as the wallpaper, remembered across reboots
- [x] Rounded, per-app-coloured icons, buttons, title bars and taskbar
- [x] A TSC-derived clock, so timeouts still expire with interrupts disabled
- [ ] Copy, move and rename in the file manager
- [ ] Directories in O1FS beyond the flat table

### Phase 19 — TLS, and the real web
- [x] SHA-256, HMAC and HKDF, checked against the FIPS and RFC vectors
- [x] ChaCha20-Poly1305, checked against RFC 8439 including its carry cases
- [x] X25519 with a constant-time Montgomery ladder, checked against RFC 7748
- [x] An entropy pool: RDRAND where it exists, TSC jitter where it does not
- [x] TLS 1.3 record layer and 1-RTT handshake, with the server's Finished verified
- [x] The key schedule checked against the published trace in RFC 8448
- [x] `https://` in the HTTP client, and a plain-HTTP retry for hosts without TLS
- [x] `www.google.com` loading directly, with no proxy anywhere in the path
- [x] Google's script-only results page detected, explained, and answered from
      an engine that serves HTML
- [x] `www.google.com` rebuilt as a page that works: the wordmark, a search box
      that submits to Google, and Google's own completions from
      `suggestqueries.google.com` — the one part of Google that still answers a
      browser without JavaScript
- [x] Clicking the address bar selects it, so typing replaces the URL
- [x] Form controls on the page: text fields, password fields, textareas and
      buttons, with a caret, and GET submission of the successful controls
- [x] A CMOS real-time clock, the time of day in the taskbar, and JavaScript's
      `Date` built on it
- [ ] Certificate verification: X.509 parsing, RSA and ECDSA, a root store
- [ ] HelloRetryRequest, so a server can insist on another group
- [ ] Session resumption, and post-handshake key updates
- [ ] AES-GCM, for servers that will not do ChaCha20

### Phase 20 — C and C++, in the kernel and in userspace
- [x] A kernel target with hardware SSE2 (`kernel/x86_64-os101.json`), because
      clang cannot compile C that returns a `double` for a soft-float target
- [x] SSE state saved and restored on interrupt entry and syscall entry, now
      that compiled code is free to use the xmm registers
- [x] `sbrk`, so a process can grow a heap, and `time_ms`, so it can tell the
      time — the two syscalls a C library cannot be written without
- [x] `os101-libc`: crt0, syscall wrappers, an sbrk-backed `malloc`, an
      exactly-rounded `printf`/`strtod` pair, string, ctype, stdlib, `math.h`
      within a few ULP, and the GUI calls wrapped for C
- [x] The C++ runtime: `operator new`/`delete`, guard variables, `__cxa_atexit`,
      and `.init_array` run before `main` — freestanding C++, no libc++
- [x] `tools/os101-cc` and `tools/os101-c++`: source to a runnable app in one
      command, with only clang and the Rust toolchain installed
- [x] C and C++ example apps built by the same build as the Rust ones
- [x] 371,679 host assertions for the library, `printf` compared byte for byte
      against a hosted libc and every math function measured in ULP
- [x] QuickJS vendored unpatched, with a freestanding shim, running in the
      kernel and self-testing at boot
- [ ] `open`/`read`/`close` syscalls, so `fopen` and `fread` can be real
- [ ] `argc`/`argv`, and a blocking window event wait instead of a spin
- [ ] A C compiler running *on* OS101, so code can be built without a host

### Phase 21 — Portability and hardware maturity (pending)
- [x] UEFI output image path (`build/os101-uefi.img`)
- [x] Hybrid install ISO (`build/os101.iso`) + in-OS permanent installer wizard
- [x] Install → reboot verification in QEMU (`tools/test-install.sh`)
- [ ] Better real-hardware compatibility (AHCI/NVMe, broader USB host controllers)
- [ ] aarch64 bring-up
- [ ] ACPI + SMP expansion

## What students learn here

By working through OS101, you can learn:

- CPU privilege levels and interrupt flow
- Paging and memory safety boundaries
- ABI and syscall bridge design
- Event loops, GUI input routing, and rendering
- ELF loading and process control basics
- Filesystem integration in a small kernel

## Bootable disk and direct install notes

`run.sh` creates three install artifacts:

- `build/os101.iso` — hybrid USB/BIOS install medium (Rufus / Etcher / `dd`)
- `build/os101-bios.img` — raw BIOS disk image (same bytes as the ISO)
- `build/os101-uefi.img` — GPT + ESP image for UEFI firmware

Write the ISO to USB:

```bash
sudo dd if=build/os101.iso of=/dev/sdX bs=4M status=progress conv=fsync
sync
```

Then boot the stick, open **Install OS101**, pick a target disk, type `ERASE`,
and reboot from the installed drive. QEMU verification:

```bash
./tools/test-install.sh
```

Use with caution:
- verify `/dev/sdX` first
- this erases the target device
- supported targets today are ATA/IDE and UHCI USB MSC (not AHCI/NVMe yet)
- hardware boot support is still experimental

## Contribution invitation

Contributors are warmly welcome — especially help that makes OS101 **more stable** and **more kid-friendly**.

Suggested starting points:

1. docs cleanup, student guides, and up-to-date screenshots
2. testing and regression checks
3. small kernel subsystems (input / filesystem / GUI)
4. kid-friendly apps, larger UI targets, and gentler game defaults
5. pending roadmap items in the later phases (TLS certs, sockets, AHCI/NVMe, aarch64)

Please contribute respectfully. This project is an open learning space for people who love OS development as a hobby or a study path. See the [Contribution](README.md#contribution) section in the README for the full workflow.
