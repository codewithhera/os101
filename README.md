# OS101

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Arch: x86_64](https://img.shields.io/badge/arch-x86__64-blue.svg)](#what-kind-of-os-is-this)
[![Language: Rust](https://img.shields.io/badge/language-Rust-orange.svg)](https://www.rust-lang.org/)
[![Contributions welcome](https://img.shields.io/badge/contributions-welcome-brightgreen.svg)](#contribution)

OS101 is a from-scratch hobby operating system written in Rust. It is built for students, hobbyists, and developers who want to understand how a modern computer OS is constructed layer by layer — from boot and interrupts to a graphical desktop, networking, and a real web browser.

This project was started by **SM Mamunur Rahaman** out of curiosity and built with **100% vibe coding**.

- Repository: https://github.com/codewithhera/os101
- LinkedIn: https://www.linkedin.com/in/sm-mamunur-rahman/

> **We need contributors.** Help make OS101 more stable, easier to run, and more kid-friendly — better tests, clearer docs, safer defaults, and delightful learning apps. See [Contribution](#contribution).

## Documentation Map

- Project overview: [README.md](README.md)
- Roadmap and progress: [TASKS.md](TASKS.md)
- Applications guide: [applications/README.md](applications/README.md)
- SDK guide: [os101-sdk/README.md](os101-sdk/README.md)

## What kind of OS is this?

OS101 is a **monolithic-style hobby OS** for **x86_64** with:

- a custom kernel (`no_std`)
- a framebuffer-based shell and GUI desktop
- basic userspace process support with ELF loading
- a small app ecosystem (built-in and ELF apps, including kid-friendly games)
- a bootable install ISO and an in-OS permanent installer
- its own IPv4 network stack, TLS 1.3, and an in-kernel web browser

Current architecture focus:

- CPU arch: `x86_64`
- Boot media: BIOS hybrid ISO (`os101.iso`), BIOS raw image, and UEFI GPT image
- Environment: QEMU first (including on Apple Silicon via emulation); real hardware experimental
- Input: PS/2 keyboard/mouse (fallback) plus USB HID via UHCI (boot protocol)
- Storage: ATA/IDE disks, UHCI USB mass storage, FAT32 read/write at `/usb`, O1FS at `/disk`

## Who is this for?

- Computer science students learning OS internals
- Systems programmers exploring low-level Rust
- Hobbyists who want to build an operating system from scratch
- Parents and educators looking for a simple, playful desktop that kids can explore in QEMU
- Open-source contributors who want to harden a teaching OS and grow a kid-friendly app set

## Features completed so far

High-level milestones already implemented:

1. Bootable kernel and framebuffer text output
2. Serial logging and QEMU test harness
3. Interrupts, GDT/TSS, timer, keyboard
4. Interactive shell with built-in commands
5. Memory map handling, paging, frame allocator, kernel heap
6. Mouse input and cursor rendering
7. Graphics stack (double buffer, primitives, dirty rects)
8. Window manager and GUI widgets
9. Userspace process model + syscalls + ELF loading
10. Filesystem stack (VFS + FAT32 + initramfs + persistent `/disk`)
11. SDK/app pipeline and multiple GUI apps
12. Desktop apps (calculator, notepad, paint, snake, image viewer, settings, about, file manager, terminal, monitor)
13. Kid-friendly apps (ABC Fun, Race Cars, Breakout, Space Invaders) with PC-speaker sound
14. Runtime app installation (`.opk` packages, `pkg` command, no reboot needed)
15. Per-process address space isolation
16. 24-bit colour throughout the UI, with a procedurally drawn desktop wallpaper
17. Networking: PCI enumeration, e1000 driver, ARP/IPv4/ICMP/UDP/TCP, DHCP, DNS
18. HTTP client and a web browser with its own HTML/CSS rendering engine
19. QuickJS with real DOM bindings and DOM events, so pages respond to interaction
20. Web search and inline images (HTTPS sites load directly; image proxy where needed)
21. PNG and JPEG decoders, plus an ATA driver with a persistent filesystem at `/disk`
22. Pictures saved from the web, and set as a wallpaper that survives a reboot
23. TLS 1.3 in the kernel — X25519, ChaCha20-Poly1305, SHA-256 — so `https://` sites (including `www.google.com`) load straight from the server
24. USB mass storage (UHCI BOT/SCSI) with writable FAT32 at `/usb`, browsable in My Computer
25. In-kernel TinyCC + C Code Editor (syntax highlight, build, run)
26. C and C++ userspace apps via `os101-libc` and `tools/os101-cc` / `os101-c++`
27. Bootable install ISO (`build/os101.iso`), UEFI image, and in-OS **Install OS101** wizard
28. QEMU install → reboot verification (`./tools/test-install.sh`)

For detailed phase-by-phase status, see [TASKS.md](TASKS.md).

## Applications at a glance

| Category | Apps |
| --- | --- |
| Desktop / productivity | Calculator, Notepad, Paint, File Manager, Terminal, Settings, About, Monitor, Image Viewer |
| Learning / kids | ABC Fun, Race Cars, Breakout, Space Invaders, Snake |
| Development | C Code Editor (TinyCC), Hello ELF / Hello C / Hello C++ samples |
| System | Install OS101, Web Browser, package manager (`pkg`) |

See [applications/README.md](applications/README.md) for manifests and how to add a new app.

## Networking

OS101 has its own IPv4 stack, written from the PCI bus upwards. The layers,
bottom to top, live in `kernel/src/pci.rs` and `kernel/src/net/`:

| Layer | Module | What it does |
| --- | --- | --- |
| PCI | `pci.rs` | Config space over ports `0xCF8`/`0xCFC`, BAR decoding |
| NIC | `net/e1000.rs` | Intel 82540EM: descriptor rings, DMA buffers, TX/RX |
| Link | `net/mod.rs`, `net/arp.rs` | Ethernet framing, ARP cache and resolution |
| Internet | `net/ip.rs`, `net/icmp.rs` | IPv4 with checksums, ICMP echo both ways |
| Transport | `net/udp.rs`, `net/tcp.rs` | UDP datagrams; a TCP client state machine |
| Application | `net/dhcp.rs`, `net/dns.rs`, `net/http.rs` | Address configuration, name resolution, HTTP/1.1 |

The stack is **polled rather than interrupt-driven**. The kernel's IRQ
handlers can be entered while the heap lock is held, and parsing a packet
allocates, so receiving from an interrupt would risk deadlock. Instead the
main loop calls `net::poll()` each iteration and anything waiting for a reply
polls while it waits.

That has a consequence worth knowing about, because it caused a real bug. The
main loop processes events with interrupts *disabled*, for the same
heap-reentrancy reason — so while a fetch is running, the timer IRQ cannot
fire and the tick counter it advances stands still. Every network timeout was
therefore written against a clock that did not move, which is to say it was
not a timeout at all: the second of eight image fetches would wait forever.
`kernel/src/clock.rs` fixes it by calibrating the CPU's time-stamp counter
against the PIT at boot. That is a clock no interrupt flag can stop, and it is
what all the timeouts, the uptime display and the ephemeral port numbers now
read.

Try it from the shell:

```text
os101> pci                  # what is on the bus
os101> net up               # probe the NIC, then DHCP
os101> net status           # address, gateway, resolver, frame counters
os101> ping 10.0.2.2
os101> host example.com
os101> wget http://example.com
os101> browse               # open the browser, then type `gui`
```

`run.sh` starts QEMU with user-mode networking (`-netdev user -device
e1000`), which NATs the guest behind a built-in gateway at 10.0.2.2 and needs
no privileges on the host.

### Web browser

The browser is a real rendering engine, not a markup-to-text converter. It
lives in `kernel/src/browser/` and runs the same pipeline a production browser
does, cut down to what a kernel with one monospace font and no GPU can honour:

| Stage | Module | What it produces |
| --- | --- | --- |
| Parse | `htmlparse.rs`, `entities.rs` | A DOM tree, forgiving of unclosed tags, void elements and comments |
| Parse CSS | `css.rs` | Rules with selectors, longhand declarations, colours and lengths |
| Style | `style.rs` | The style tree: rules matched and cascaded by origin, specificity and document order, with inheritance |
| Layout | `layout.rs` | A box tree: block boxes stacked with collapsed margins, padding and borders; inline content wrapped into line boxes; table rows in aligned columns |
| Paint | `paint.rs` | A flat display list of rectangles and text runs, plus hit-boxes |
| Script | `script.rs`, `domjs.js`, `../quickjs/` | QuickJS, and the DOM bindings that let a page's script change it |

Separating paint from drawing is what keeps scrolling smooth: a page is parsed
and laid out once, and every frame after that just walks the display list with
a pixel offset.

The engine honours a user-agent stylesheet (the default look of HTML), the
page's own `<style>` rules, and inline `style` attributes, in that order.
Selectors may combine tags, classes, ids, attributes and the descendant and
child combinators. Lengths in `px`, `pt`, `em`, `rem`, `ex`, `ch`, `%` and the
viewport units `vw`/`vh`/`vmin`/`vmax` all resolve against the real canvas
size, and `max-width` with `margin: 0 auto` centres a column the way most
pages expect. Headings step through four real font sizes, links are underlined
and clickable, and relative URLs — including `../` — are resolved against the
current page.

### JavaScript: a real engine

**Page scripts run on QuickJS**, Fabrice Bellard's engine, compiled into the
kernel against a freestanding libc written for it — see `third_party/README.md`
for how that is built and what it costs. It is the whole language: classes,
generators, `async`/`await`, Promises, `BigInt`, real regular expressions with
Unicode property escapes, `Map`, `Set`, template literals, destructuring. There
is no second engine and no fallback; the hand-written interpreter that used to
live in `kernel/src/js/` is gone.

`browser/script.rs` and `browser/domjs.js` bind the document to it. Rust exposes
about sixty host functions that speak only in numbers and strings; everything
with shape to it — the element wrappers, the node lists, the event objects, the
timer queue — is built on top of those in JavaScript. A page gets `document` with
the query and creation methods, elements with `textContent`, `innerHTML`,
`classList`, `dataset`, `style`, `getBoundingClientRect` and the whole mutation
family, `addEventListener` with real capture and bubble phases and a cancellable
event object, `window` with `location`, `setTimeout`, `setInterval`,
`requestAnimationFrame`, `localStorage` that survives a reboot, and a `console`
that formats objects onto the serial line.

Mutations are batched: a loop appending a hundred nodes costs one relayout, not a
hundred. Timers, animation callbacks and Promise jobs are driven from the
browser's own event loop, so a page that continues in a `.then` keeps going while
you are looking at it. And a page cannot take the machine: every evaluation is
given a deadline that QuickJS's interrupt handler enforces uncatchably, so
`while (true) {}` ends in an error message and a usable browser.

`os101:scripting` is a page whose entire content is written by its own script —
the shortest way to see all of this working.

Nearly five hundred checks covering every stage run at boot; the results are the
`QuickJS`, `Script bindings` and `Browser engine` self-test lines in the boot log.

### HTTPS: the kernel's own TLS

**OS101 speaks TLS 1.3.** `https://www.google.com` loads from Google's servers
over a connection this kernel negotiated itself — no proxy, no gateway, nothing
in the middle reading the traffic.

The implementation is deliberately the narrowest one that real servers accept.
TLS 1.3 threw out the negotiation sprawl of earlier versions, so a client can
offer exactly one of everything and still interoperate:

| Piece | Choice | Why that one |
| --- | --- | --- |
| Key exchange | X25519 | One curve, ~300 lines of field arithmetic, and universally supported |
| Cipher | ChaCha20-Poly1305 | Add-rotate-xor on 32-bit words: no lookup tables to leak through the cache, and no need for the AES-NI instructions a 2008 machine may not have |
| Hash | SHA-256 | What the suite names, and what the whole key schedule is built from |

`crypto/` holds the primitives and `net/tls.rs` the protocol. The key schedule
is checked at every boot against the published handshake trace in RFC 8448 —
comparing against someone else's numbers rather than its own, because a
cryptographic implementation that is merely self-consistent will fail against
the first real server and give no clue why.

#### What this does not protect against

**Certificates are not verified.** There is no root store, no RSA or ECDSA
signature checking, and no clock trustworthy enough to judge an expiry date.
The server's certificate is parsed exactly far enough to be skipped over.

Stated plainly: this hides your traffic from anyone merely *watching* the
network, and does not stop anyone who can *redirect* it. Whoever runs the
network you are on can present any certificate they like and OS101 will accept
it. That is strictly better than the plaintext gateway it replaces and strictly
worse than a real browser. **Do not type a password into OS101.**

The randomness is weak for the same class of reason — there is no reliable
hardware entropy source on an emulated machine, so the pool leans on
time-stamp-counter jitter. `crypto/random.rs` describes exactly what it has and
what that is worth.

### Searching, and the one thing Google will not do

Typing words in the address bar searches Google. Typing an address goes there.
Typing `images: kittens` searches for pictures you can save or set as wallpaper.

Google's *results*, though, cannot be shown — and not for want of TLS. Google
no longer puts results in the HTML it serves anyone. Every response to
`/search?q=…` is a JavaScript program that fetches the results and builds the
page in the browser. Asking as an older browser gets an "update your browser"
page instead; asking as a modern one gets the script. There is no parameter, no
header and no user agent that produces server-rendered results, and Google's
own no-JavaScript retry flow leads back to the same place.

So `www.google.com` is fetched from Google, over TLS, and then rebuilt from the
parts of Google that still work. What you get is a Google page in Google's
colours with a working search box, and under it Google's own completions for
what you typed — `suggestqueries.google.com` answers a browser like this one
with a small JSON array and no script at all, and it is the last part of Google
that does. Searching from that box, or from the address bar, goes to Google;
when Google answers with its program, the browser says so and shows results for
the same query from DuckDuckGo's script-free interface, under the same Google
header, with a link to Google's own page anyway.

The user asked to search Google. What they get is Google's page whenever Google
will serve one, Google's search box and Google's suggestions when it will not,
an honest sentence about which engine answered, and results either way.

One gateway remains, and it is no longer about TLS: `wsrv.nl` re-encodes
images. The kernel decodes PNG, JPEG, GIF and BMP but not WebP or AVIF, and it
decodes into a plain `Vec<Color>` — so a six-megapixel photograph would cost
twenty-four megabytes of heap to look at. Asking the proxy for a bounded JPEG
solves both problems at once.

So, from a cold boot:

```text
os101> net up               # DHCP
os101> gui                  # the desktop
```

Then double-click **Web**, type a search, press Enter. Follow a result, click
**Images** to pull the pictures on the page, right-click one and choose
**Save picture** or **Set as wallpaper**.

### Saving things: the data disk

Downloads and the wallpaper choice go to a second disk, mounted at `/disk`,
that the build never touches — `./run.sh` regenerates the boot image from
source every run, so anything kept there would not survive a rebuild.

`kernel/src/diskfs.rs` is both the ATA PIO driver and O1FS, a deliberately
small filesystem: a superblock, a free-space bitmap, a flat directory table,
and contiguously allocated files. The disk is formatted on first boot.

| Path | Holds |
| --- | --- |
| `/disk/downloads` | Pictures saved from the browser |
| `/disk/settings` | Which wallpaper to restore at boot |

Saved pictures show up in **Files** under `/disk/downloads`. Right-click one
and **Set as wallpaper** scales it to the screen and remembers it, so it is
still there after a reboot. **Settings → Use the drawn scene** puts the
procedural wallpaper back.

## Installing applications

OS101 installs apps at runtime the way a desktop OS does: one self-contained
package file per app, validated and registered without rebuilding the kernel.

An `.opk` package is a small header, a text manifest, and an ELF payload
(format details in `os101-package/src/lib.rs`).

**Build a package on the host** from any OS101 binary:

```bash
./os101-sdk/build.sh applications/my-app
./tools/target/release/os101-tools pack \
    applications/my-app/target/x86_64-os101/release/my-app \
    build/my-app.opk --name "My App" --version 1.0.0 --description "Does a thing"

# Check what a package contains
./tools/target/release/os101-tools inspect build/my-app.opk
```

**Install it inside OS101** from the shell:

```text
os101> pkg install /fat/demo.opk     # install a package
os101> pkg list                      # show every installed app
os101> pkg info "Demo App"           # inspect one app
os101> pkg run "Demo App"            # launch it
os101> pkg remove "Demo App"         # uninstall it
```

Installed apps appear in the Applications launcher immediately — press `F2`
on the desktop, or click Applications in the taskbar. Packages are copied
into `/apps`, which is scanned at every boot.

Installation is a trust boundary, so every package is validated before it is
registered: magic and version, bounds-checked lengths, a CRC-32 over the
payload, a name restricted to characters safe as a path component, and an
ELF header check for 64-bit x86-64. The format has unit tests
(`cd os101-package && cargo test`) covering each rejection path.

Packages currently live in RAM, so they persist for the session rather than
across reboots; a writable block device is what that is waiting on.

## Writing applications in C and C++

Applications do not have to be written in Rust. `os101-libc/` is a C library
written for this OS: `crt0` and the syscall wrappers, a `malloc` that grows
the process heap through the new `sbrk` syscall, a `printf` whose digits match
a hosted libc byte for byte, the usual string, character, conversion and
`math.h` sets, and the GUI calls wrapped so a C program can put a window on
the screen. On top of it sits the small C++ runtime — `operator new` and
`delete`, guard variables for function-local statics, `__cxa_atexit`, and the
`.init_array` walk that runs global constructors before `main`.

Building one is a single command:

```bash
tools/os101-cc  -o build/hello.elf hello.c
tools/os101-c++ -o build/hello.elf hello.cpp other.cpp
```

The driver cross-compiles with clang, links statically at `USER_BASE` with
`rust-lld`, and needs nothing installed beyond the toolchain this repo already
requires. An app directory with a `manifest.txt` and a `build.sh` is picked up
by the build exactly like a Rust one — see `applications/hello-c/` and
`applications/hello-cpp/`, which exercise `malloc`, `qsort`, floating-point
`printf`, virtual functions, templates, RAII and static construction, and then
open a window.

C++ here means freestanding C++: classes, templates, RAII, virtuals, `new` and
`delete`, compiled with `-fno-exceptions -fno-rtti`. There is no
`std::vector`, no `std::string` and no iostreams, because a full libc++ is a
porting job of its own. File I/O is the other honest gap — `fopen` and friends
return `ENOSYS` until the kernel grows `open`/`read`/`close` syscalls, which is
the next thing that ABI needs.

The library's own tests run on the host, where its answers can be checked
against a libc known to be right: 371,679 assertions comparing `printf`
output byte for byte, `strtod` round trips, an allocator torture test, and
every `math.h` function measured in ULP against the system libm.

```bash
./os101-libc/tests/run.sh
```

## Process isolation

Every process gets a private address space. The kernel occupies level-4 page
table slots 0, 2–7 and 136, leaving slot 1 free, and userspace lives entirely
in slot 1 (`USER_BASE = 0x8010_0000_00`). A process's whole address space is
therefore a single level-3 table, and switching to a process is one page
table entry write plus a TLB flush — kernel mappings are shared implicitly
because no other slot is touched.

The practical effect: two processes both load at the same virtual address
and neither can see the other's memory, because an unscheduled process is
not mapped at all. Run `vm` in the shell to see the live layout.

## Roadmap pending

Main pending areas:
- Certificate verification, so TLS authenticates the server and not just the
  channel — an X.509 parser, RSA and ECDSA signatures, and a root store
- Sockets exposed to userspace apps via syscalls
- Better real-hardware compatibility (AHCI/NVMe beyond IDE + UHCI)
- aarch64 bring-up
- SMP and deeper ACPI integration

Detailed roadmap with checkboxes is in [TASKS.md](TASKS.md).

## Repo structure

- `kernel/` — OS kernel (`no_std`, networking, browser, TLS, GUI, kids apps)
- `tools/` — host tools: bootable disk images, `.opk` packaging, QEMU runner
- `os101-package/` — the `.opk` package format, shared by kernel and tools
- `os101-libc/` — freestanding C/C++ library for userspace apps
- `applications/` — app manifests and app sources/binaries
- `os101-user/` — userspace syscall crate (Rust)
- `os101-sdk/` — helper SDK/templates for user apps
- `third_party/` — QuickJS, TinyCC, and related shims
- `screen_shots/` — README screenshots (`sc-1.png` … `sc-18.png`)
- `video/` — optional local preview media
- `run.sh` — build + image + boot orchestration

## Why OS101 SDK exists

`os101-sdk` exists to make userspace app development for OS101 consistent and beginner-friendly.

Without an SDK, each contributor must manually solve target setup, linker/layout details, `no_std` structure, and expected output conventions. That leads to repeated build and launch failures.

The SDK is necessary for:

1. Consistency: every app follows one standard structure.
2. Reliability: fewer ELF/runtime issues caused by misconfiguration.
3. Learning speed: students focus on OS concepts and app logic, not toolchain debugging.
4. Ecosystem growth: easier for new contributors to add apps.

Typical usage flow:

1. Copy SDK template app.
2. Implement app logic.
3. Build with `./os101-sdk/build.sh <app-dir>`.
4. Register app in `applications/<app>/manifest.txt`.
5. Run `./run.sh` and launch from OS101.

## System requirements

Minimum dev host setup:

- Linux (recommended: Ubuntu/Debian) or macOS
- Rust stable + nightly installed via rustup
- Rust target: `x86_64-os101` (a JSON spec in `kernel/`, built with `-Z build-std`)
- QEMU (`./run.sh` installs one if you have none — see below)
- ~2 GB free disk, ~4 GB RAM on host

Guest input (QEMU / experimental PC):

- **PS/2** keyboard and mouse (always available as fallback)
- **USB HID** via UHCI — boot-protocol keyboard and mouse, including devices
  behind a hub. QEMU is launched with `-usb -device usb-kbd -device usb-mouse`.
  Check status in the shell with `usb`.

Guest display:

- **2696x1680, 32 bits per pixel** — tuned to occupy ~90% of a MacBook Pro's
  built-in Retina display in both dimensions. The bootloader's BIOS stage will
  not select a VESA mode larger than 1280x720, so `vbe.rs` reprograms the
  adapter through the Bochs VBE registers at boot, before the framebuffer is
  sized. Adapters without those registers keep the bootloader's mode.
- Needs at least 18 MiB of video memory, above QEMU's 16 MiB default, so
  `tools/qemu.sh` raises it to 64 MiB. A card that cannot fit the mode does not
  report an error — it simply stays at the bootloader's 1280x720.
- QEMU's macOS (cocoa) backend renders one guest pixel per *backing* pixel, so
  the on-screen window is `guest_size / backingScaleFactor` points, plus a
  title bar. What matters for sizing is the screen's *logical point size* —
  which, once a Mac is set to a scaled ("more space") display mode, is smaller
  than the panel's native resolution, not the marketing spec. Measured with
  `NSScreen.mainScreen` on the dev machine this was built against: 1496x967
  points at a 2x scale factor, with a 32pt title bar. Using the panel's native
  spec here once produced a window bigger than the screen itself, clipped and
  pushed partly off-screen by macOS.
- That 1:1 guest-pixel-to-backing-pixel mapping is also why 2696x1680 is twice
  the pixel density a normal desktop has: fonts, icons and widgets are all
  tuned in plain pixel counts for a normal-density screen, and drawing them
  straight into this many physical pixels would make everything look half its
  intended size. Instead `kernel/src/framebuffer.rs`'s `FramebufferWriter`
  draws into a back buffer at half this resolution — the *virtual* canvas
  every widget's layout is measured against — and its `present()` doubles
  each pixel into a crisp 2x2 block on the way to VRAM: an exact integer
  upscale, so there is no blur, just correctly-sized pixels.
- The physical resolution is set by `DISPLAY_WIDTH`/`DISPLAY_HEIGHT` in
  `kernel/src/main.rs`, along with the derivation and the formula to
  re-target a different screen. QEMU's window follows that size (not
  fullscreen), so the host menu bar and dock stay visible. Changing it means
  checking three other things: the video memory above, the kernel heap
  (`HEAP_SIZE` in `kernel/src/allocator.rs` — the back buffer and the cached
  wallpaper are each `(width/2)*(height/2)*4` bytes, since both live in the
  virtual canvas), and `GUEST_WIDTH`/`GUEST_HEIGHT` (also half this) in
  `tools/qemu-runner/drive.py`.
- The guest is booted with 512 MiB of RAM, comfortably above what the screen
  buffers need: at this resolution they account for about 9 MiB of the
  192 MiB kernel heap.
- Pointer speed scales with the *virtual* resolution (`POINTER_SENSITIVITY` in
  `kernel/src/mouse.rs`), so crossing the screen takes the same hand movement
  no matter how large the display is.
- Kids' games (Race Cars, Breakout, Space Invaders) use the PC speaker for
  short beeps. `tools/qemu.sh` wires QEMU's speaker to CoreAudio (macOS) or
  PulseAudio (Linux) so those tones are audible.

Run `./check.sh` to verify your setup. One-time setup commands are documented
in [TASKS.md](TASKS.md).

## Run OS101

From repo root:

```bash
./run.sh
```

That builds the userspace apps, the kernel and the host tools, writes a
bootable disk image, and opens the running machine in a QEMU window.

```bash
./run.sh --headless     # boot with the serial console on your terminal
./run.sh --build-only   # just produce build/os101-bios.img
./run.sh stop           # stop a VNC session (see below)
```

Inside the OS, try `help`, then `gui` for the desktop (F2 opens the app
launcher, ESC returns to the shell).

### How it finds a QEMU

`tools/qemu.sh` picks the best option available, in order:

1. `qemu-system-x86_64` on `PATH` — Homebrew, MacPorts or a distro package.
2. [pkgx](https://pkgx.sh) — prebuilt binaries unpacked under `$HOME`. This
   needs no administrator password, so `./run.sh` installs it automatically
   when nothing else is present. You can also run `./tools/install-qemu.sh`
   yourself.
3. A container — the last resort. It has no display at all, so the screen is
   exported over VNC (`./tools/vnc.sh`, password `os101`) and `./run.sh stop`
   shuts it down. Prefer options 1 or 2: a native QEMU gives you a real
   window with working keyboard and mouse.

`./tools/qemu.sh --backend` prints which one would be used.

Output images:
- `build/os101-bios.img` — BIOS boot / install medium (also copied as `build/os101.iso`)
- `build/os101-uefi.img` — UEFI GPT + ESP
- `build/os101.iso` — hybrid USB install ISO

Build output is logged to `build/build.log`.

## Capture a screenshot

`tools/screenshot.sh` boots the image headlessly, optionally types a few
commands, and writes a PNG plus the serial log. It works with or without a
native QEMU, so it is also the way to see the OS on a host that can only run
the containerised runner.

```bash
# Just the boot screen
./tools/screenshot.sh build/boot.png

# Install a package, open the desktop, open the launcher (F2)
./tools/screenshot.sh build/launcher.png \
    "type:pkg install /fat/demo.opk" key:ret wait:2 \
    "type:gui" key:ret wait:5 key:f2 wait:4
```

## Install on a physical PC

OS101 builds three install artifacts every time you run `./run.sh --build-only`:

| File | Use |
|------|-----|
| `build/os101.iso` | Hybrid USB/BIOS install medium — write with Rufus, Etcher, or `dd` |
| `build/os101-bios.img` | Same bytes as the ISO (raw BIOS disk image) |
| `build/os101-uefi.img` | GPT + ESP image for UEFI firmware / QEMU+OVMF |

### Write the install USB

```bash
# macOS / Linux — replace /dev/sdX or /dev/rdiskN carefully
sudo dd if=build/os101.iso of=/dev/sdX bs=4M status=progress conv=fsync
sync
```

Or open `build/os101.iso` in [Rufus](https://rufus.ie/) / balenaEtcher (DD / image mode).

### Permanent install (in-OS wizard)

1. Boot the PC from the USB/ISO.
2. Open the desktop (`gui`) → double-click **Install**, or Apps → **Install OS101**.
3. Pick the target disk (another ATA/IDE drive or USB stick).
4. Type `ERASE` and confirm.
5. When finished, remove the install USB and reboot from the target disk.

Shell alternative:

```text
install list
install to slave    # or: master | usb
```

Unattended install (for scripts / testing): put a one-line file on a FAT32 USB stick:

```text
# /usb/autoinst.txt
slave
```

On the next boot OS101 clones itself onto that target, then you reboot from it.

### Verify in QEMU (no USB hardware needed)

```bash
./tools/test-install.sh
```

This boots the install medium, clones onto `build/os101-target.img`, then
reboots from that image and checks that the shell comes up.

Warnings:
- The target disk is **completely overwritten**.
- Supported targets today: primary ATA (IDE) master/slave and UHCI USB mass storage.
  Modern AHCI/NVMe disks are not listed yet.
- Real hardware support is still experimental; prefer QEMU first.
- BIOS/CSM may be required for the hybrid ISO on older firmware; use
  `build/os101-uefi.img` on UEFI-only machines (write with `dd` the same way).

## Contribution

OS101 is an open learning project. Contributions of every size are welcome — from a one-line docs fix to a new kid-friendly app or a kernel hardening patch.

### What we especially need help with

1. **Stability** — crash fixes, regression tests, clearer error messages, safer install paths, and real-hardware bring-up.
2. **Kid-friendly experience** — simpler UI copy, larger hit targets, gentler games, parental-friendly defaults, and short “first five minutes” guides.
3. **Documentation** — student walkthroughs, architecture notes, and screenshots that match the current desktop.
4. **Apps and SDK** — new learning apps, package examples, and smoother C/C++/Rust app templates.
5. **Roadmap items** — open checkboxes in [TASKS.md](TASKS.md) (TLS certificate verification, socket syscalls, AHCI/NVMe, aarch64, and more).

### How to contribute

1. Fork the repository: https://github.com/codewithhera/os101
2. Read [TASKS.md](TASKS.md) and pick an open item (or open an issue to discuss an idea).
3. Create a branch for your change.
4. Keep changes small, focused, and testable.
5. Run `./check.sh`, `./run.sh`, and `./test.sh` before opening a pull request.
6. Prefer clear commit messages that name the phase or feature.

Beginners are especially encouraged to start with documentation, tests, screenshot updates, and small GUI or kids-app improvements. Please contribute respectfully — this project is meant to be a friendly space for people who love OS development as a hobby or a study path.

If you are unsure where to begin, open an issue titled **“Looking for a first contribution”** and we can suggest a good starter task.

## Screenshots and Video

### Screenshots

1. Boot shell — USB mounted, ready for commands  
   ![OS101 Screenshot 1 — boot shell](screen_shots/sc-1.png)

2. Desktop with space wallpaper and sidebar icons  
   ![OS101 Screenshot 2 — desktop](screen_shots/sc-2.png)

3. Web browser — Wikipedia page with inline images over HTTPS  
   ![OS101 Screenshot 3 — web browser](screen_shots/sc-3.png)

4. Browser image search / gallery view  
   ![OS101 Screenshot 4 — image search](screen_shots/sc-4.png)

5. Applications launcher (21 apps) on a custom wallpaper  
   ![OS101 Screenshot 5 — applications launcher](screen_shots/sc-5.png)

6. Breakout — kid-friendly arcade game  
   ![OS101 Screenshot 6 — Breakout](screen_shots/sc-6.png)

7. C Code Editor (build & run) plus Calculator  
   ![OS101 Screenshot 7 — Code Editor and Calculator](screen_shots/sc-7.png)

8. Image Viewer previewing an embedded bitmap  
   ![OS101 Screenshot 8 — Image Viewer](screen_shots/sc-8.png)

9. Notepad and Clock & Monitor (live heap stats)  
   ![OS101 Screenshot 9 — Notepad and Monitor](screen_shots/sc-9.png)

10. Notepad file dialog on persistent `/disk/downloads`  
    ![OS101 Screenshot 10 — file dialog](screen_shots/sc-10.png)

11. Paint — brush, eraser, fill, and colour palette  
    ![OS101 Screenshot 11 — Paint](screen_shots/sc-11.png)

12. Race Cars — kid-friendly racing game  
    ![OS101 Screenshot 12 — Race Cars](screen_shots/sc-12.png)

13. Settings — theme and drawn-scene wallpaper  
    ![OS101 Screenshot 13 — Settings](screen_shots/sc-13.png)

14. Snake — big on-screen controls for kids  
    ![OS101 Screenshot 14 — Snake](screen_shots/sc-14.png)

15. Space Invaders  
    ![OS101 Screenshot 15 — Space Invaders](screen_shots/sc-15.png)

16. Terminal app — `help`, `mem`, and `version`  
    ![OS101 Screenshot 16 — Terminal](screen_shots/sc-16.png)

17. File Explorer browsing `/disk/downloads`  
    ![OS101 Screenshot 17 — File Explorer](screen_shots/sc-17.png)

18. Install OS101 wizard (permanent disk install)  
    ![OS101 Screenshot 18 — Install OS101](screen_shots/sc-18.png)

You can also capture screenshots automatically:

```bash
./tools/screenshot.sh screen_shots/sc-1.png
./tools/screenshot.sh screen_shots/sc-2.png "type:gui" key:ret wait:6
./tools/screenshot.sh screen_shots/sc-5.png "type:gui" key:ret wait:5 key:f2 wait:4
```

### Video Preview

Click the image below to watch OS101 on YouTube:

[![OS101 Video Preview](screen_shots/sc-2.png)](https://youtu.be/rim-SwxtjpI)

Direct link: https://youtu.be/rim-SwxtjpI

---

If you are learning OS development as a hobby, this project is intentionally structured so you can understand each layer in sequence and build confidence step by step. Star the repo, try `./run.sh`, and — if you can — help make it more stable and more welcoming for kids who are just starting out.
