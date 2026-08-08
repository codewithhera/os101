//! OS101 kernel entry point.

#![no_std]
#![no_main]

extern crate alloc;

mod framebuffer;
mod vbe;
mod color;
mod theme;
mod wallpaper;
mod gdt;
mod interrupts;
mod clock;
mod rtc;
mod keyboard;
mod memory;
mod allocator;
mod mouse;
mod usb;
mod serial;
mod shell;
mod input;
mod widgets;
mod compositor;
mod window;
mod kids;
mod sound;
mod boot_splash;
mod syscall;
mod process;
mod fs;
mod ata;
mod diskfs;
mod fat32;
mod install;
mod image;
mod app_registry;
mod apps;
mod pci;
mod crypto;
mod net;
mod browser;
mod quickjs;
mod tcc;
mod highlight;
mod selftest;

/// The `.opk` format lives in its own crate so host tooling builds packages
/// with exactly the code the kernel uses to read them.
use os101_package as package;
mod icons;

use bootloader_api::{
    config::{BootloaderConfig, Mapping},
    entry_point,
    info::BootInfo,
};
use core::panic::PanicInfo;

pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut c = BootloaderConfig::new_default();
    c.mappings.physical_memory = Some(Mapping::Dynamic);
    // The default 80 KiB is not enough for the recursive passes the browser
    // runs: styling, layout and the script interpreter all walk trees whose
    // depth is bounded by the document rather than by us. A megabyte is
    // nothing against 512 MiB of guest memory and turns a double fault into
    // headroom.
    c.kernel_stack_size = 1024 * 1024;
    c
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

/// Resolution the kernel asks the display adapter for at boot.
///
/// QEMU's macOS (cocoa) backend renders one guest pixel per *backing* pixel,
/// not per point, so the on-screen window is `guest_size / backingScaleFactor`
/// points, plus a title bar (measured at 32pt) added to the height. What
/// matters is the screen's logical point size and scale factor — which on a
/// Mac set to a scaled ("more space") resolution is *not* the display's native
/// panel resolution — not the marketing spec. Measured on this machine via
/// `NSScreen.mainScreen`: 1496x967 points at a 2x scale factor. Using the
/// panel's native 3456x2234 here instead once produced a window bigger than
/// the screen itself (clipped and pushed partly off-screen by macOS).
///
/// 2696x1680 is ~90% of that screen in both dimensions once the title bar is
/// accounted for, so the window is large and readable without covering the
/// host menu bar and dock. Re-derive this if the display changes:
///   guest_w = round8(0.9 * screen_pt_w * scale)
///   guest_h = round8((0.9 * screen_pt_h - titlebar_pt) * scale)
///
/// Both dimensions are multiples of 8: the Bochs VBE interface stores the scan
/// stride in whole 8-pixel groups and quietly rounds anything else.
///
/// This is the *physical* VBE mode only — the resolution the window occupies
/// on screen. Everything the kernel draws (fonts, icons, widget layouts) is
/// tuned in plain pixel counts for a normal-density desktop, and this mode is
/// 2x that density on a Retina host, so `framebuffer::FramebufferWriter`
/// keeps a back buffer at half this size and nearest-neighbour doubles it in
/// `present()`. See that struct's `info` field doc for the full picture.
///
/// Changing this needs three other things checked: the card's video memory
/// (see `tools/qemu.sh`), the heap (the back buffer is width*height (not
/// `DISPLAY_WIDTH`/`HEIGHT`) *4 bytes), and `GUEST_WIDTH`/`GUEST_HEIGHT` in
/// `tools/qemu-runner/drive.py` (half of this, not equal to it).
const DISPLAY_WIDTH: u16 = 2696;
const DISPLAY_HEIGHT: u16 = 1680;

/// Linux fbcon-style: pointer chrome from device coordinates (not move events).
fn shell_sync_hover_only(
    x: usize,
    y: usize,
    buttons: &mut [widgets::Button; 4],
    hovered: &mut Option<usize>,
) {
    let new_hover = widgets::hit(buttons, x, y);
    if new_hover == *hovered {
        return;
    }
    if let Some(i) = *hovered {
        if buttons[i].state != widgets::State::Pressed {
            buttons[i].state = widgets::State::Normal;
            widgets::render(&buttons[i]);
        }
    }
    if let Some(i) = new_hover {
        if buttons[i].state != widgets::State::Pressed {
            buttons[i].state = widgets::State::Hover;
            widgets::render(&buttons[i]);
        }
    }
    *hovered = new_hover;
}

/// Cursor + hover + status text; status coordinates throttled so we do not
/// repaint the bar on every PS/2 packet equivalent.
fn shell_paint_pointer_frame(
    x: usize,
    y: usize,
    buttons: &mut [widgets::Button; 4],
    hovered: &mut Option<usize>,
    last_status_tick: &mut u64,
) {
    framebuffer::update_mouse_cursor(x, y);

    let new_hover = widgets::hit(buttons, x, y);
    let hover_changed = new_hover != *hovered;
    if hover_changed {
        if let Some(i) = *hovered {
            if buttons[i].state != widgets::State::Pressed {
                buttons[i].state = widgets::State::Normal;
                widgets::render(&buttons[i]);
            }
        }
        if let Some(i) = new_hover {
            if buttons[i].state != widgets::State::Pressed {
                buttons[i].state = widgets::State::Hover;
                widgets::render(&buttons[i]);
            }
        }
        *hovered = new_hover;
    }

    let t = clock::ticks();
    let refresh_status = hover_changed || t.wrapping_sub(*last_status_tick) >= 5;
    if refresh_status {
        let status_msg = match *hovered {
            Some(i) => alloc::format!("Mouse: ({:4},{:4})  hover: {}", x, y, buttons[i].label),
            None => alloc::format!("Mouse: ({:4},{:4})", x, y),
        };
        framebuffer::update_status_bar(&status_msg);
        *last_status_tick = t;
    }
}

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    // 0. Before anything at all: the kernel is built with hardware SSE2, and
    //    the bootloader leaves CR4.OSFXSR clear, so until this returns every
    //    `xmm` instruction the compiler emitted is a #UD. See
    //    `interrupts::enable_sse`.
    interrupts::enable_sse();

    serial::init();

    // 1. Initialise Memory & Heap — must come first now that the
    //    framebuffer writer allocates its back buffer on the heap.
    let phys_mem_offset = boot_info.physical_memory_offset.into_option().unwrap();
    let l4_table = unsafe { memory::active_l4_table(phys_mem_offset) };
    let mut frame_allocator = unsafe {
        memory::BootInfoFrameAllocator::init(&boot_info.memory_regions)
    };
    allocator::init_heap(l4_table, &mut frame_allocator, phys_mem_offset)
        .expect("heap initialization failed");
    memory::install_runtime(l4_table, frame_allocator, phys_mem_offset);

    // The bootloader will not pick a mode larger than 1280x720. Ask the card
    // for a bigger one first, so the writer below sizes its back buffer to the
    // screen we actually end up with. Falls back to the bootloader's mode on
    // hardware without the Bochs VBE registers.
    match unsafe { vbe::set_mode(DISPLAY_WIDTH, DISPLAY_HEIGHT) } {
        Some((buffer, info)) => framebuffer::init(info, buffer),
        None => {
            if let Some(fb) = boot_info.framebuffer.as_mut() {
                let info = fb.info();
                let buffer = fb.buffer_mut();
                framebuffer::init(info, buffer);
            }
        }
    }

    // 2. Initialise Input Core & GDT
    input::init();
    gdt::init();

    // 3. Initialise Interrupts (Re-enables STI at the end)
    interrupts::init();
    // Straight after, while interrupts are on and nothing else is competing
    // for the CPU: the calibration measures the CPU's own counter against the
    // timer IRQ, so it needs both running and neither busy.
    clock::calibrate();

    // The C environment QuickJS runs in needs a clock and somewhere to write,
    // and neither existed until now. This call is also what keeps the engine in
    // the link at all — see `quickjs::install`.
    quickjs::install();
    tcc::install();

    // 4. Initialise Drivers — tell the mouse driver the actual screen size
    //    so it can clamp coordinates correctly (like Linux's input_abs_set_res).
    if let Some(writer) = framebuffer::WRITER.lock().as_ref() {
        let info = writer.info();
        mouse::set_screen_size(info.width, info.height);
    }
    mouse::init();
    usb::init();
    syscall::init();
    fs::init();
    fs::mount_disk();
    fs::mount_usb();
    // Headers and the TinyCC runtime go on the data disk so `cc` can find them
    // without a host-side package step. Harmless if no disk is attached.
    match tcc::seed_toolchain() {
        Ok(n) if n > 0 => ok_line(&alloc::format!("C toolchain seeded ({} files)", n)),
        Ok(_) => {}
        Err(e) => crate::warn_line(&alloc::format!("C toolchain seed: {}", e)),
    }
    // The desktop is not up yet, but the wallpaper is only generated on the
    // first paint, so restoring the choice now costs nothing and avoids the
    // drawn scene flashing up before the user's picture replaces it.
    window::restore_wallpaper();
    apps::init();

    // A machine with no NIC is perfectly usable, so a failure here is
    // reported and then ignored.
    let net_status = net::init();
    // Seed the random pool once the MAC address exists, so two machines
    // booting the same image do not derive the same TLS keys.
    crypto::random::seed(net::mac());

    ok_line("Memory management online");
    ok_line("Input Core & Mouse driver online");
    ok_line("Interrupts enabled, system ready.");
    ok_line("Syscall interface online");
    match &net_status {
        Ok(()) => ok_line(&alloc::format!(
            "Network: e1000 up, MAC {}",
            net::format_mac(net::mac())
        )),
        Err(e) => warn_line(&alloc::format!("Network unavailable: {}", e)),
    }
    report_selftest("User address space", &process::selftest());
    report_selftest("Heap", &allocator::selftest());
    report_selftest("FPU and SSE", &interrupts::selftest());
    report_selftest("Clock", &clock::selftest());
    report_selftest("Real-time clock", &rtc::selftest());
    report_selftest("Pointer", &mouse::selftest());
    report_selftest("Storage", &diskfs::selftest());
    report_selftest("USB FAT32 driver", &fat32::selftest());
    report_selftest("Installer", &install::selftest());
    report_selftest("Image decoders", &image::selftest());
    report_selftest("Icon images", &icons::selftest());
    report_selftest("Wallpaper", &wallpaper::selftest());
    report_selftest("Cryptography", &crypto::selftest());
    report_selftest("Network stack", &net::selftest::run());
    report_selftest("TLS", &net::tls::selftest::run());
    report_selftest("Address bar", &browser::search::selftest());
    report_selftest("QuickJS", &quickjs::selftest::run());
    report_selftest("TinyCC", &tcc::selftest::run());
    report_selftest("Script bindings", &browser::script::selftest::run());
    report_selftest("Browser engine", &browser::selftest::run());

    // Unattended install: `/usb/autoinst.txt` containing master|slave|usb.
    // Runs after self-tests so a failed clone cannot hide other boot bugs.
    install::try_autoinstall();

    // Neon splash with developer portrait & credits, then the banner/shell.
    boot_splash::play();
    banner();

    let mut sh = shell::Shell::new();

    // Build clickable buttons at the bottom of the screen and paint them once.
    // These are used in shell mode; GUI mode (Phase 8) renders its own UI.
    let (sw, shh) = framebuffer::screen_size().unwrap_or((1280, 720));
    let mut buttons = widgets::build(sw, shh);
    widgets::render_all(&buttons);

    // Track the button the mouse is currently over, and the last left-button
    // state so we only act on the press edge (not on release).
    let mut hovered: Option<usize> = None;
    let mut prev_left = false;
    let mut prev_right = false;
    let mut last_shell_status_tick = 0u64;

    sh.prompt();

    // Blit the initial scene (banner + prompt + buttons) before we start
    // waiting on interrupts, otherwise the user sees a black screen until
    // their first keystroke. Request a full dirty rect so the first `present`
    // always refreshes the real framebuffer (avoids partial-union / 32-bpp
    // padding edge cases on some hosts).
    framebuffer::mark_entire_dirty();
    framebuffer::present();

    // ── Unified Event Loop ───────────────────────────────────────────────
    //
    // Modelled after the Linux kernel's approach to interrupt-safe event
    // processing.  The main loop MUST run with interrupts **disabled**
    // because both the main loop (via `alloc::format!`, `println!`) and
    // interrupt handlers (via `SegQueue::push` → allocator) acquire the
    // heap's spin-lock.  If an IRQ fires while the main loop holds that
    // lock the handler will spin forever → deadlock.
    //
    // Linux solves this with `spin_lock_irqsave()`.  Our simpler pattern:
    //   cli            — disable interrupts while processing events
    //   sti ; hlt      — atomically re-enable & sleep (x86 guarantees
    //                    no IRQ window between STI and HLT)
    //
    // Pointer position (like Linux input ABS coords + fbcon soft cursor): read
    // `mouse::position()` once per loop pass—never from a backlog of move events.
    loop {
        // ── Disable interrupts for the processing phase ─────────────
        unsafe { core::arch::asm!("cli", options(nomem, nostack)); }

        let mut got_event = false;
        let mut gui_before = window::is_gui_mode();

        while let Some(event) = input::pop() {
            got_event = true;
            match event {
                input::InputEvent::Key(k) => {
                    if window::is_gui_mode() {
                        if let Some(action) = window::handle_key(k) {
                            window::apply_action(action);
                        }
                    } else {
                        sh.handle_key(k);
                    }
                }
                input::InputEvent::MouseWheel { delta } => {
                    if window::is_gui_mode() {
                        if let Some(action) = window::handle_mouse_wheel(delta) {
                            window::apply_action(action);
                        }
                    }
                }
                input::InputEvent::MouseButton { left, right, double_clicked: dbl } => {
                    let (mx, my) = mouse::position();
                    if window::is_gui_mode() {
                        window::handle_mouse_move(mx, my);
                    } else {
                        shell_sync_hover_only(mx, my, &mut buttons, &mut hovered);
                    }
                    if window::is_gui_mode() {
                        if let Some(action) =
                            window::handle_mouse_button(left, prev_left, right, prev_right, dbl)
                        {
                            window::apply_action(action);
                        }
                        prev_left = left;
                        prev_right = right;
                    } else {
                        // Only fire on the press edge.
                        let pressed_now = left && !prev_left;
                        prev_left = left;

                        if pressed_now {
                            if let Some(i) = hovered {
                                buttons[i].state = widgets::State::Pressed;
                                widgets::render(&buttons[i]);

                                // Execute the action — single click runs Clear/Help,
                                // Reboot requires a double-click to avoid foot-guns.
                                let action = buttons[i].action;
                                let label = widgets::describe(action);
                                serial::_print(format_args!("WIDGET: clicked {} dbl={}\n", buttons[i].label, dbl));
                                match action {
                                    widgets::Action::Clear => {
                                        framebuffer::clear_screen();
                                        widgets::render_all(&buttons);
                                        sh.prompt();
                                    }
                                    widgets::Action::Help => {
                                        println!();
                                        // Feed the shell a synthetic "help\n".
                                        for c in "help".chars() {
                                            sh.handle_key(pc_keyboard::DecodedKey::Unicode(c));
                                        }
                                        sh.handle_key(pc_keyboard::DecodedKey::Unicode('\n'));
                                    }
                                    widgets::Action::Reboot => {
                                        if dbl {
                                            println!();
                                            println!("Rebooting (double-click confirmed)...");
                                            // Same sequence the `reboot` command uses.
                                            unsafe {
                                                core::arch::asm!(
                                                    "out dx, al",
                                                    in("dx") 0x64u16,
                                                    in("al") 0xFEu8,
                                                    options(nomem, nostack, preserves_flags),
                                                );
                                            }
                                        }
                                    }
                                    widgets::Action::Shutdown => {
                                        if dbl {
                                            println!();
                                            println!("Shutting down...");
                                            crate::exit_qemu(crate::QemuExitCode::Success);
                                        }
                                    }
                                }
                                framebuffer::update_status_bar(&label);
                            }
                        } else if !left {
                            // Release: any pressed button returns to hover/normal.
                            for (i, b) in buttons.iter_mut().enumerate() {
                                if b.state == widgets::State::Pressed {
                                    b.state = if Some(i) == hovered {
                                        widgets::State::Hover
                                    } else {
                                        widgets::State::Normal
                                    };
                                    widgets::render(b);
                                }
                            }
                            let status_msg = alloc::format!(
                                "Click: [ {} ] [ {} ] {}",
                                if left { "L" } else { " " },
                                if right { "R" } else { " " },
                                if dbl { "DOUBLE-CLICK!" } else { "" }
                            );
                            framebuffer::update_status_bar(&status_msg);
                        }
                    }
                }
            }

            // GUI transition hook: restore shell chrome after leaving GUI.
            let gui_now = window::is_gui_mode();
            if gui_before && !gui_now {
                framebuffer::clear_screen();
                widgets::render_all(&buttons);
                hovered = None;
                println!("Exited GUI mode.");
                sh.prompt();
            }
            gui_before = gui_now;
        }

        // The network stack is polled, not interrupt-driven, so the main
        // loop is what actually moves packets. Without this, incoming
        // frames only get processed while a network command is blocking.
        net::poll();
        usb::poll();
        fs::usb_tick();

        let (mx, my) = mouse::position();
        if window::is_gui_mode() {
            window::handle_mouse_move(mx, my);
            window::tick(crate::clock::ticks());
            crate::sound::poll();
            let needs_redraw = window::take_redraw_request();
            let hover_redraw = window::take_hover_redraw_request();
            if needs_redraw {
                framebuffer::invalidate_mouse_cache();
                window::render();
            } else if hover_redraw {
                framebuffer::invalidate_mouse_cache();
                window::render_top_window();
            }
            framebuffer::update_mouse_cursor(mx, my);
        } else {
            shell_paint_pointer_frame(
                mx,
                my,
                &mut buttons,
                &mut hovered,
                &mut last_shell_status_tick,
            );
        }

        if !got_event && !window::is_gui_mode() {
            let visible = if window::cursor_blink_enabled() {
                (crate::clock::ticks() / 10) % 2 == 0
            } else {
                true
            };
            sh.enable_cursor(visible);
        }

        // Blit whatever changed this iteration
        framebuffer::present();

        // Run userspace whenever something is runnable — not only in GUI mode.
        // `pkg run` / `run` from the shell spawn a process and return; without
        // this the process sits forever in the queue until someone types `gui`.
        // One slice per iteration keeps the keyboard responsive.
        let has_tasks = crate::process::run_scheduler_once();

        // ── Atomically re-enable interrupts and halt ────────────────
        // We ONLY halt if there was no input AND no tasks were runnable.
        if !got_event && !has_tasks {
            unsafe { core::arch::asm!("sti", "hlt", options(nomem, nostack)); }
        } else {
            unsafe { core::arch::asm!("sti", options(nomem, nostack)); }
        }
    }
}

/// Boot line with a green `[ OK ]` tag, systemd style.
/// `[ OK ]` status line, with the tag in the success colour.
pub fn ok_line(msg: &str) {
    print!(" [");
    framebuffer::with_text_color(theme::SUCCESS, || print!(" OK "));
    println!("] {}", msg);
}

/// One boot line summarising a subsystem's self-test.
fn report_selftest(subsystem: &str, report: &selftest::Report) {
    if report.failed == 0 {
        ok_line(&alloc::format!(
            "{} self-test passed ({} checks)",
            subsystem, report.passed
        ));
    } else {
        warn_line(&alloc::format!(
            "{} self-test: {} of {} failed — {}",
            subsystem,
            report.failed,
            report.passed + report.failed,
            report.failure_summary()
        ));
    }
}

/// `[WARN]` status line, for subsystems that came up degraded.
pub fn warn_line(msg: &str) {
    print!(" [");
    framebuffer::with_text_color(theme::WARNING, || print!("WARN"));
    println!("] {}", msg);
}

fn banner() {
    println!();
    framebuffer::with_text_color(theme::ACCENT, || {
        println!("   ___  ____  _ ___  _");
        println!("  / _ \\/ ___|/ |  _ \\| |");
        println!(" | | | \\___ \\| | | | | |");
        println!(" | |_| |___) | | |_| | |");
        println!("  \\___/|____/|_|\\___/|_|");
    });
    println!();
    println!(" OS101 v{}   (x86_64, phase 12)", env!("CARGO_PKG_VERSION"));
    framebuffer::with_text_color(theme::TEXT_MUTED, || {
        println!(" A tiny OS that wants to grow up.");
    });
    println!();
    framebuffer::with_text_color(theme::ACCENT, || {
        println!(" Developed by SM Mamunur Rahaman Hera");
    });
    framebuffer::with_text_color(theme::TEXT_MUTED, || {
        println!(" (Father of: Inaaya & Aayan)");
        println!(" Software Engineer, Bangladesh");
        println!(" https://www.linkedin.com/in/sm-mamunur-rahman/");
    });
}

fn hlt_loop() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }
}

pub fn exit_qemu(exit_code: QemuExitCode) -> ! {
    // Try every common QEMU power-off path. Which one works depends on how
    // the VM was launched; falling through all of them ends in `hlt` so the
    // machine at least stops burning CPU if nothing listened.
    unsafe {
        // isa-debug-exit (added by ./run.sh) — clean exit with a status code.
        core::arch::asm!(
            "out dx, eax",
            in("dx") 0xf4u16,
            in("eax") exit_code as u32,
            options(nomem, nostack, preserves_flags),
        );
        // QEMU ACPI power button (i440fx / q35).
        core::arch::asm!(
            "out dx, ax",
            in("dx") 0x604u16,
            in("ax") 0x2000u16,
            options(nomem, nostack, preserves_flags),
        );
        // Bochs / older QEMU ACPI port.
        core::arch::asm!(
            "out dx, ax",
            in("dx") 0xb004u16,
            in("ax") 0x2000u16,
            options(nomem, nostack, preserves_flags),
        );
        // PIIX4 power port used by some QEMU configs.
        core::arch::asm!(
            "out dx, ax",
            in("dx") 0x4004u16,
            in("ax") 0x3400u16,
            options(nomem, nostack, preserves_flags),
        );
    }
    hlt_loop();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failure = 0x11,
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    framebuffer::set_text_color(theme::ERROR, theme::CONSOLE_BG);
    println!();
    println!(" !!! KERNEL PANIC !!!");
    println!(" {}", info);
    hlt_loop();
}
