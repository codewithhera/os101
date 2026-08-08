//! OS101 Interactive Shell.
//!
//! Handles a line buffer, backspace, and dispatches commands to built-in
//! functions.

use crate::{print, println};

const HISTORY_SIZE: usize = 16;

pub struct Shell {
    buffer: [char; 1024],
    len: usize,
    // History storage
    history: [[char; 1024]; HISTORY_SIZE],
    history_lens: [usize; HISTORY_SIZE],
    history_count: usize,
    history_cursor: usize, // 0 = current live line, 1 = last history item, etc.
    temp_buffer: [char; 1024],
    temp_len: usize,
}

impl Shell {
    pub const fn new() -> Self {
        Self {
            buffer: ['\0'; 1024],
            len: 0,
            history: [['\0'; 1024]; HISTORY_SIZE],
            history_lens: [0; HISTORY_SIZE],
            history_count: 0,
            history_cursor: 0,
            temp_buffer: ['\0'; 1024],
            temp_len: 0,
        }
    }

    pub fn prompt(&self) {
        print!("os101> ");
    }

    /// Show or hide the cursor at the current end of the line.
    pub fn enable_cursor(&self, visible: bool) {
        crate::framebuffer::set_cursor_visible(visible);
    }

    /// Process a single key input.
    pub fn handle_key(&mut self, k: pc_keyboard::DecodedKey) {
        use pc_keyboard::{DecodedKey, KeyCode};
        // Erase cursor before we move or print anything.
        self.enable_cursor(false);
        match k {
            DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => {
                println!();
                if self.len > 0 {
                    self.push_history();
                    self.execute();
                    self.len = 0;
                }
                self.history_cursor = 0;
                self.prompt();
            }
            DecodedKey::Unicode('\x08') => { // Backspace
                if self.len > 0 {
                    self.len -= 1;
                    print!("\x08");
                }
            }
            DecodedKey::Unicode(c) => {
                if self.len < self.buffer.len() {
                    self.buffer[self.len] = c;
                    self.len += 1;
                    print!("{}", c);
                }
            }
            DecodedKey::RawKey(KeyCode::ArrowUp) => self.navigate_history(1),
            DecodedKey::RawKey(KeyCode::ArrowDown) => self.navigate_history(-1),
            _ => {}
        }
    }

    fn push_history(&mut self) {
        // Don't push duplicates
        if self.history_count > 0 {
            let last_idx = (self.history_count - 1) % HISTORY_SIZE;
            if self.history_lens[last_idx] == self.len && 
               self.history[last_idx][..self.len] == self.buffer[..self.len] {
                return;
            }
        }

        let idx = self.history_count % HISTORY_SIZE;
        self.history[idx] = self.buffer;
        self.history_lens[idx] = self.len;
        self.history_count += 1;
    }

    fn navigate_history(&mut self, delta: i32) {
        if self.history_count == 0 { return; }

        let old_cursor = self.history_cursor;
        let new_cursor = if delta > 100 { // Reset to current
            0
        } else {
            let res = self.history_cursor as i32 + delta;
            if res < 0 { 0 }
            else if res > self.history_count as i32 || res > HISTORY_SIZE as i32 {
                core::cmp::min(self.history_count, HISTORY_SIZE) as usize
            } else {
                res as usize
            }
        };

        if new_cursor == old_cursor { return; }

        // Save current line if moving away from cursor 0
        if old_cursor == 0 {
            self.temp_buffer = self.buffer;
            self.temp_len = self.len;
        }

        // Erase current line
        for _ in 0..self.len {
            print!("\x08");
        }

        self.history_cursor = new_cursor;
        if new_cursor == 0 {
            self.buffer = self.temp_buffer;
            self.len = self.temp_len;
        } else {
            let idx = (self.history_count - new_cursor) % HISTORY_SIZE;
            self.buffer = self.history[idx];
            self.len = self.history_lens[idx];
        }

        // Print new line
        for i in 0..self.len {
            print!("{}", self.buffer[i]);
        }
    }

    fn execute(&mut self) {
        // Build a UTF-8 string from the char buffer.  The old code cast
        // `[char; 128]` (4 bytes per element) to `*const u8` which read
        // raw memory — multi-char commands never matched.
        let mut cmd_buf = [0u8; 512];
        let mut cmd_len = 0usize;
        for i in 0..self.len {
            let mut enc = [0u8; 4];
            let s = self.buffer[i].encode_utf8(&mut enc);
            for b in s.as_bytes() {
                if cmd_len < cmd_buf.len() {
                    cmd_buf[cmd_len] = *b;
                    cmd_len += 1;
                }
            }
        }
        let cmd = core::str::from_utf8(&cmd_buf[..cmd_len]).unwrap_or("");
        
        match cmd {
            "help" => {
                println!("OS101 Help:");
                println!(" help      - Show this message");
                println!(" echo STR  - Echo arguments");
                println!(" clear     - Clear the screen");
                println!(" uptime    - Show kernel uptime");
                println!(" ticks     - Show raw timer tick count");
                println!(" date      - Show time since boot (HH:MM:SS)");
                println!(" whoami    - Show current user");
                println!(" version   - Show kernel version");
                println!(" ls [PATH] - List VFS paths (/, /fat, /init, /disk, /usb)");
                println!(" cat PATH  - Print a file from VFS");
                println!(" write PATH TEXT - Write text (@n@ = newline; keeps \\n for C)");
                println!(" cc SRC... - Compile C with the in-kernel TinyCC (-o, -I, -D)");
                println!(" run PATH  - Run an ELF from disk (after cc, or any ELF file)");
                println!(" cpuinfo   - Show CPU vendor and brand string");
                println!(" mem       - Show memory info");
                println!(" vm        - Show virtual address space layout");
                println!(" pkg ...   - Install/list/remove applications");
                println!(" pci       - List devices on the PCI bus");
                println!(" usb       - USB UHCI / HID keyboard & mouse status");
                println!(" install   - List disks / permanently install OS101 onto a disk");
                println!(" net ...   - Network: up | status | arp | dns");
                println!(" ping HOST - Send ICMP echo requests");
                println!(" host NAME - Resolve a hostname via DNS");
                println!(" wget URL  - Fetch a page over HTTP");
                println!(" browse    - Open the web browser (GUI)");
                println!(" js EXPR   - Evaluate a JavaScript expression");
                println!(" gfx       - Demo 2D primitives (rect/line/circle)");
                println!(" gui       - Enter windowed GUI mode (ESC to return)");
                println!(" user      - Run userspace demo (ELF + ring3 + syscall)");
                println!(" reboot    - Reboot the machine");
                println!(" shutdown  - Power off (QEMU exit)");
                println!(" panic     - Cause a kernel panic");
            }
            "clear" => crate::framebuffer::clear_screen(),
            "reboot" => unsafe {
                use core::arch::asm;
                // Standard PS/2 controller reboot
                let mut bit: u8 = 0;
                while bit & 0x02 != 0 {
                    asm!("in al, dx", out("al") bit, in("dx") 0x64u16);
                }
                asm!("out dx, al", in("dx") 0x64u16, in("al") 0xFEu8);
            }
            "shutdown" | "poweroff" => {
                println!("Shutting down...");
                crate::exit_qemu(crate::QemuExitCode::Success);
            }
            "panic" => panic!("User-requested panic!"),
            "uptime" => {
                let ticks = crate::clock::ticks();
                println!("Uptime: {} ticks (~{} seconds)", ticks, ticks / 18);
            }
            "ticks" => {
                println!("{}", crate::clock::ticks());
            }
            "date" => {
                let secs = crate::clock::ticks() / 18;
                let h = secs / 3600;
                let m = (secs / 60) % 60;
                let s = secs % 60;
                println!("Boot+{:02}:{:02}:{:02}", h, m, s);
            }
            "whoami" => {
                println!("os101-user");
            }
            "version" => {
                println!("OS101 kernel v{} (x86_64)", env!("CARGO_PKG_VERSION"));
            }
            s if s.starts_with("ls ") => {
                let path = s.split_whitespace().nth(1).unwrap_or("/");
                match crate::fs::cmd_ls(Some(path)) {
                    Ok(entries) => {
                        for e in entries {
                            println!("{}", e);
                        }
                    }
                    Err(e) => println!("ls: {}", e),
                }
            }
            "ls" => {
                match crate::fs::cmd_ls(Some("/")) {
                    Ok(entries) => {
                        for e in entries {
                            println!("{}", e);
                        }
                    }
                    Err(e) => println!("ls: {}", e),
                }
            }
            s if s.starts_with("cat ") => {
                let path = s.split_whitespace().nth(1).unwrap_or("");
                if path.is_empty() {
                    println!("cat: missing path");
                } else {
                    match crate::fs::cmd_cat(path) {
                        Ok(data) => match core::str::from_utf8(&data) {
                            Ok(text) => print!("{}", text),
                            Err(_) => println!("cat: binary file ({} bytes)", data.len()),
                        },
                        Err(e) => println!("cat: {}", e),
                    }
                }
            }
            s if s.starts_with("write ") => {
                // write /disk/hello.c int main(){...}
                let rest = s["write ".len()..].trim();
                let mut parts = rest.splitn(2, char::is_whitespace);
                let path = parts.next().unwrap_or("");
                let body = parts.next().unwrap_or("").trim_start();
                if path.is_empty() {
                    println!("usage: write <path> <text>");
                } else {
                    // `@n@` is the only line-break marker. Expanding `\n` or `|`
                    // would smash C source (`printf("...\n")`, `a|b`) the moment
                    // someone writes a real program with `write`.
                    let mut expanded = body.replace("@n@", "\n");
                    if !expanded.ends_with('\n') {
                        expanded.push('\n');
                    }
                    match crate::fs::cmd_write_file(path, expanded.into_bytes()) {
                        Ok(()) => println!("wrote {}", path),
                        Err(e) => println!("write: {}", e),
                    }
                }
            }
            s if s == "cc" || s.starts_with("cc ") => {
                let rest = if s.len() > 2 { s[2..].trim() } else { "" };
                if rest.is_empty() {
                    println!("usage: cc [-o out] [-I dir] [-Dsym[=val]] <source.c>...");
                } else {
                    let args: alloc::vec::Vec<&str> = rest.split_whitespace().collect();
                    let result = crate::tcc::compile(&args);
                    if !result.diagnostics.is_empty() {
                        println!("{}", result.diagnostics);
                    }
                    if result.ok {
                        if let Some(out) = result.output_path {
                            println!("cc: wrote {}", out);
                        } else {
                            println!("cc: ok");
                        }
                    } else if result.diagnostics.is_empty() {
                        println!("cc: failed");
                    }
                }
            }
            s if s.starts_with("run ") => {
                let path = s.split_whitespace().nth(1).unwrap_or("");
                if path.is_empty() {
                    println!("run: missing path");
                } else {
                    match crate::fs::cmd_cat(path) {
                        Ok(data) => {
                            if data.len() < 4 || &data[0..4] != b"\x7FELF" {
                                println!("run: not an ELF file");
                            } else {
                                match crate::process::spawn_elf_bytes(&data) {
                                    Ok(pid) => {
                                        println!("spawned userspace process pid={}", pid);
                                        if !crate::process::run_scheduler_once() {
                                            println!("scheduler had no runnable process");
                                        }
                                    }
                                    Err(e) => println!("run: {}", e),
                                }
                            }
                        }
                        Err(e) => println!("run: {}", e),
                    }
                }
            }
            "cpuinfo" => {
                let (vendor, brand) = cpuid_info();
                println!("Vendor: {}", vendor.as_str());
                println!("Brand:  {}", brand.as_str());
            }
            "gfx" => {
                use crate::framebuffer;
                use crate::theme;
                let (sw, sh) = framebuffer::screen_size().unwrap_or((1280, 720));
                // Canvas: a band to the right of the shell, above the buttons.
                let cx0 = sw / 2;
                let cy0 = 100;
                let cw = sw - cx0 - 32;
                let ch = sh.saturating_sub(framebuffer::BOTTOM_RESERVED + cy0 + 16);
                framebuffer::fill_vgradient(cx0, cy0, cw, ch, theme::DESKTOP_TOP, theme::DESKTOP_BOTTOM);
                framebuffer::stroke_rect(cx0, cy0, cw, ch, theme::ACCENT);
                // Filled rectangle.
                framebuffer::fill_rect(cx0 + 20, cy0 + 20, 120, 80, theme::ACCENT);
                framebuffer::stroke_rect(cx0 + 20, cy0 + 20, 120, 80, theme::ACCENT_HOVER);
                // Diagonal lines.
                framebuffer::draw_line(
                    (cx0 + 160) as i32, (cy0 + 20) as i32,
                    (cx0 + 300) as i32, (cy0 + 100) as i32, theme::WARNING);
                framebuffer::draw_line(
                    (cx0 + 300) as i32, (cy0 + 20) as i32,
                    (cx0 + 160) as i32, (cy0 + 100) as i32, theme::ERROR);
                // Outlined circle and filled circle.
                framebuffer::draw_circle(
                    (cx0 + 80) as i32, (cy0 + 180) as i32, 50, theme::SUCCESS);
                framebuffer::fill_circle(
                    (cx0 + 220) as i32, (cy0 + 180) as i32, 50, theme::INFO);
                println!("drew gfx demo — no flicker (double-buffered).");
            }
            "gui" => {
                crate::window::enter_gui_mode();
            }
            "calc" => {
                // DEBUG: directly spawn the Calculator ELF so we can repro the
                // hang without needing mouse input.
                let apps = crate::app_registry::APPS;
                for app in apps {
                    if app.name == "Calculator" {
                        if let crate::app_registry::AppKind::Elf(bytes) = app.kind {
                            crate::window::enter_gui_mode();
                            match crate::process::spawn_elf_bytes(bytes) {
                                Ok(pid) => println!("calc spawned pid={}", pid),
                                Err(e) => println!("calc spawn failed: {}", e),
                            }
                        }
                    }
                }
            }
            "user" => {
                match crate::process::spawn_demo_process() {
                    Ok(pid) => {
                        println!("spawned userspace process pid={}", pid);
                        let ran = crate::process::run_scheduler_once();
                        if !ran {
                            println!("scheduler had no runnable process");
                        } else {
                            println!("userspace process returned to kernel");
                        }
                    }
                    Err(e) => {
                        println!("userspace spawn failed: {}", e);
                    }
                }
            }
            "mem" => {
                let used = crate::allocator::used();
                let total = crate::allocator::size();
                println!("Memory Info:");
                println!(" Used structural:  {} bytes", used);
                println!(" Total available: {} bytes", total);
                println!(" Reclaimed frames: {} ({} KiB)",
                         crate::memory::free_frame_count(),
                         crate::memory::free_frame_count() * 4);
            }
            "vm" => {
                println!("Virtual address space:");
                println!(
                    "  physical memory window : {:#018x}",
                    crate::memory::physical_memory_offset()
                );
                println!(
                    "  user range             : {:#018x} .. {:#018x}",
                    crate::process::USER_BASE,
                    crate::process::USER_LIMIT
                );
                if let Some(fb) = crate::framebuffer::info() {
                    println!(
                        "  framebuffer            : {:#018x} .. {:#018x}  ({}x{})",
                        fb.0,
                        fb.0 + fb.1 as u64,
                        fb.2,
                        fb.3
                    );
                }
                println!();
                println!("  Level-4 slots in use (each covers 512 GiB):");
                for slot in crate::memory::l4_occupancy() {
                    let note = if slot.index == crate::memory::USER_L4_SLOT {
                        "  <- current process"
                    } else {
                        ""
                    };
                    println!(
                        "    [{:>3}] {:#018x}  entry={:#018x}{}",
                        slot.index, slot.base, slot.entry, note
                    );
                }
                println!();
                println!(
                    "  {} live process address space(s); slot {} is swapped on each",
                    crate::process::live_space_count(),
                    crate::memory::USER_L4_SLOT
                );
                println!("  context switch, so only one is ever mapped at a time.");
            }
            s if s == "pkg" || s.starts_with("pkg ") => {
                pkg_command(s["pkg".len()..].trim());
            }
            "pci" => pci_command(),
            "usb" => println!("{}", crate::usb::status_line()),
            "install" => {
                println!("usage: install list");
                println!("       install to master|slave|usb");
                println!("Copies the live boot disk onto the target (destructive).");
            }
            s if s.starts_with("install ") => install_command(s),
            s if s == "net" || s.starts_with("net ") => {
                net_command(s["net".len()..].trim());
            }
            s if s.starts_with("ping ") => ping_command(s[5..].trim()),
            "ping" => println!("usage: ping HOST"),
            s if s.starts_with("host ") || s.starts_with("nslookup ") => {
                let name = s.split_once(' ').map(|(_, r)| r.trim()).unwrap_or("");
                host_command(name);
            }
            s if s.starts_with("wget ") || s.starts_with("curl ") => {
                wget_command(s[5..].trim());
            }
            "wget" | "curl" => println!("usage: wget URL"),
            s if s.starts_with("js ") => match crate::quickjs::eval_standalone(s[3..].trim()) {
                Ok(value) => println!("{}", value),
                Err(e) => println!("{}", e),
            },
            "js" => println!("usage: js EXPRESSION    (no document attached)"),
            "browse" | "browser" => {
                match crate::apps::launch_by_name("Web Browser") {
                    Ok(()) => println!("Opening the browser. Type 'gui' to see it."),
                    Err(e) => println!("browse: {}", e),
                }
            }
            s if s.starts_with("echo ") => {
                println!("{}", &s[5..]);
            }
            "echo" => println!(),
            "" => {}
            _ => println!("Unknown command: {}. Try 'help'.", cmd),
        }
    }
}

/// `pci` — what is on the bus.
fn pci_command() {
    let devices = crate::pci::scan();
    println!("{} PCI device(s):", devices.len());
    for d in devices {
        println!(
            "  {:02x}:{:02x}.{}  {:04x}:{:04x}  {}",
            d.bus,
            d.slot,
            d.func,
            d.vendor_id,
            d.device_id,
            crate::pci::class_name(d.class, d.subclass)
        );
    }
}

/// `net` — bring the interface up and inspect it.
fn net_command(args: &str) {
    use crate::net;

    let (sub, _rest) = match args.split_once(char::is_whitespace) {
        Some((a, b)) => (a, b.trim()),
        None => (args, ""),
    };

    match sub {
        "" | "status" => {
            if !net::is_up() {
                println!("No network interface. The kernel found no supported NIC.");
                println!("Start QEMU with:  -netdev user,id=n0 -device e1000,netdev=n0");
                return;
            }
            let (configured, mac, ip, mask, gw, dns) = net::config();
            let (rx, tx, dropped) = net::stats();
            println!("Interface eth0:");
            println!("  MAC address : {}", net::format_mac(mac));
            println!("  Link        : {}", if net::link_up() { "up" } else { "down" });
            if configured {
                println!("  IPv4        : {}", ip);
                println!("  Netmask     : {}", mask);
                println!("  Gateway     : {}", gw);
                println!("  DNS         : {}", dns);
            } else {
                println!("  IPv4        : not configured — run 'net up'");
            }
            println!("  Frames      : {} received, {} sent, {} dropped", rx, tx, dropped);
        }
        "up" => {
            if !net::is_up() {
                if let Err(e) = net::init() {
                    println!("net: {}", e);
                    return;
                }
                println!("Found an e1000 NIC, MAC {}.", net::format_mac(net::mac()));
            }
            println!("Requesting an address over DHCP...");
            match net::dhcp::configure() {
                Ok(()) => {
                    let (_, _, ip, mask, gw, dns) = net::config();
                    println!("Configured: {} netmask {} gateway {} dns {}", ip, mask, gw, dns);
                }
                Err(e) => println!("net: DHCP failed: {}", e),
            }
        }
        "arp" => {
            let entries = net::arp::entries();
            if entries.is_empty() {
                println!("The ARP cache is empty.");
                return;
            }
            println!("ARP cache:");
            for (ip, mac) in entries {
                println!("  {:<15}  {}", alloc::format!("{}", ip), net::format_mac(mac));
            }
        }
        "dns" => {
            let entries = net::dns::cached();
            if entries.is_empty() {
                println!("No hostnames resolved yet.");
                return;
            }
            println!("Resolver cache:");
            for (name, ip) in entries {
                println!("  {:<30}  {}", name, ip);
            }
        }
        "flush" => {
            net::dns::clear_cache();
            println!("Resolver cache cleared.");
        }
        other => {
            println!("net: unknown subcommand '{}'.", other);
            println!("Try: up | status | arp | dns | flush");
        }
    }
}

/// `ping` — four ICMP echo requests, like the real thing.
fn ping_command(target: &str) {
    use crate::net;

    if target.is_empty() {
        println!("usage: ping HOST");
        return;
    }
    let addr = match net::dns::resolve(target) {
        Ok(a) => a,
        Err(e) => {
            println!("ping: {}", e);
            return;
        }
    };

    println!("PING {} ({}):", target, addr);
    let mut received = 0;
    for seq in 1..=4u16 {
        match net::icmp::ping(addr, seq, net::icmp::default_timeout()) {
            Some(ticks) => {
                received += 1;
                // The PIT ticks about every 55 ms, which is the best
                // resolution available here.
                println!("  reply from {}: seq={} time~{}ms", addr, seq, ticks * 55);
            }
            None => println!("  no reply: seq={} (timed out)", seq),
        }
    }
    println!("4 sent, {} received, {}% loss", received, (4 - received) * 25);
}

/// `host` — resolve a name.
fn host_command(name: &str) {
    if name.is_empty() {
        println!("usage: host NAME");
        return;
    }
    match crate::net::dns::resolve(name) {
        Ok(addr) => println!("{} has address {}", name, addr),
        Err(e) => println!("host: {}", e),
    }
}

/// `wget` — fetch a URL and print it.
fn wget_command(url: &str) {
    if url.is_empty() {
        println!("usage: wget URL");
        return;
    }
    println!("Fetching {} ...", url);
    match crate::net::http::get(url) {
        Ok(resp) => {
            println!(
                "HTTP {} — {} ({} bytes)",
                resp.status,
                if resp.content_type.is_empty() { "unknown type" } else { &resp.content_type },
                resp.body.len()
            );
            println!();
            // Printing a whole page would scroll the useful part away, so
            // show the opening and say how much was left.
            let text = resp.body_text();
            let mut shown = 0;
            for line in text.lines().take(24) {
                println!("{}", line);
                shown += 1;
            }
            let total = text.lines().count();
            if total > shown {
                println!();
                println!("... {} more lines. Use the browser for the full page.", total - shown);
            }
        }
        Err(e) => println!("wget: {}", e),
    }
}

/// `pkg` — the package manager front end.
///
/// Mirrors what a real system offers: list what is installed, inspect one
/// app, install a package file, remove it again.
fn pkg_command(args: &str) {
    let (sub, rest) = match args.split_once(char::is_whitespace) {
        Some((a, b)) => (a, b.trim()),
        None => (args, ""),
    };
    match sub {
        "" | "list" | "ls" => {
            let apps = crate::apps::list();
            println!("{} application(s):", apps.len());
            for app in &apps {
                println!(
                    "  {:<20} {:<8} {:<10} {}",
                    app.name,
                    app.version,
                    app.kind_label(),
                    app.description
                );
            }
            println!();
            println!("Install with: pkg install /fat/<file>.opk");
        }
        "info" => {
            if rest.is_empty() {
                println!("usage: pkg info <name>");
                return;
            }
            match crate::apps::find(rest) {
                Some(app) => {
                    println!("Name:        {}", app.name);
                    println!("Version:     {}", app.version);
                    println!("Type:        {}", app.kind_label());
                    println!("Description: {}", app.description);
                    match &app.path {
                        Some(p) => println!("Package:     {}", p),
                        None => println!("Package:     (part of the kernel image)"),
                    }
                }
                None => println!("pkg: no application named '{}'", rest),
            }
        }
        "install" | "add" => {
            if rest.is_empty() {
                println!("usage: pkg install <path-to-.opk>");
                return;
            }
            match crate::apps::install_from_path(rest) {
                Ok(name) => {
                    println!("Installed '{}'.", name);
                    println!("It is now in the Applications launcher.");
                }
                Err(e) => println!("pkg: install failed: {}", e),
            }
        }
        "remove" | "uninstall" | "rm" => {
            if rest.is_empty() {
                println!("usage: pkg remove <name>");
                return;
            }
            match crate::apps::uninstall(rest) {
                Ok(()) => println!("Removed '{}'.", rest),
                Err(e) => println!("pkg: {}", e),
            }
        }
        "run" | "launch" => {
            if rest.is_empty() {
                println!("usage: pkg run <name>");
                return;
            }
            match crate::apps::launch_by_name(rest) {
                Ok(()) => println!("Launched '{}'.", rest),
                Err(e) => println!("pkg: {}", e),
            }
        }
        other => {
            println!("pkg: unknown subcommand '{}'", other);
            println!("Try: pkg list | info <name> | install <file> | remove <name> | run <name>");
        }
    }
}

/// Small fixed-size string so we can return CPUID results without `alloc`.
pub struct FixedStr {
    bytes: [u8; 64],
    len: usize,
}

impl FixedStr {
    fn new() -> Self { Self { bytes: [0; 64], len: 0 } }
    fn push_u32_le(&mut self, v: u32) {
        for i in 0..4 {
            if self.len < self.bytes.len() {
                self.bytes[self.len] = ((v >> (i * 8)) & 0xFF) as u8;
                self.len += 1;
            }
        }
    }
    pub fn as_str(&self) -> &str {
        // Trim trailing NULs and non-printable bytes.
        let mut end = self.len;
        while end > 0 {
            let b = self.bytes[end - 1];
            if b == 0 || b == b' ' { end -= 1; } else { break; }
        }
        core::str::from_utf8(&self.bytes[..end]).unwrap_or("?")
    }
}

fn cpuid_raw(leaf: u32) -> (u32, u32, u32, u32) {
    let (eax, ebx, ecx, edx);
    unsafe {
        // Preserve rbx — LLVM reserves it in PIC code.
        core::arch::asm!(
            "mov {tmp:r}, rbx",
            "cpuid",
            "mov {b:r}, rbx",
            "mov rbx, {tmp:r}",
            tmp = out(reg) _,
            b = out(reg) ebx,
            inout("eax") leaf => eax,
            out("ecx") ecx,
            out("edx") edx,
            options(nostack, preserves_flags),
        );
    }
    (eax, ebx, ecx, edx)
}

/// `install list` / `install to master|slave|usb` — permanent disk clone.
fn install_command(cmd: &str) {
    let mut parts = cmd.split_whitespace();
    let _ = parts.next(); // "install"
    match parts.next() {
        None | Some("list") => {
            let disks = crate::install::list_disks();
            if disks.is_empty() {
                println!("install: no disks detected");
                return;
            }
            let src = crate::install::default_source(&disks);
            for d in &disks {
                let mark = if Some(d.id) == src { " (source)" } else { "" };
                println!("{}{}", d.describe(), mark);
            }
        }
        Some("to") => {
            let target = match parts.next() {
                Some("master") => crate::install::DiskId::AtaMaster,
                Some("slave") => crate::install::DiskId::AtaSlave,
                Some("usb") => crate::install::DiskId::Usb,
                Some(other) => {
                    println!("install: unknown target '{}' (use master, slave, or usb)", other);
                    return;
                }
                None => {
                    println!("usage: install to master|slave|usb");
                    return;
                }
            };
            let disks = crate::install::list_disks();
            let Some(source) = crate::install::default_source(&disks) else {
                println!("install: no source disk");
                return;
            };
            println!(
                "install: copying {} -> {} …",
                source.label(),
                target.label()
            );
            match crate::install::install(source, target, None) {
                Ok(()) => println!("install: done — reboot from the target disk"),
                Err(e) => println!("install: {}", e),
            }
        }
        Some(other) => {
            println!("install: unknown subcommand '{}'", other);
            println!("usage: install list | install to master|slave|usb");
        }
    }
}

/// Return (vendor, brand). Vendor is 12 chars; brand up to 48 chars.
fn cpuid_info() -> (FixedStr, FixedStr) {
    let mut vendor = FixedStr::new();
    let (_, ebx, ecx, edx) = cpuid_raw(0);
    vendor.push_u32_le(ebx);
    vendor.push_u32_le(edx);
    vendor.push_u32_le(ecx);

    let mut brand = FixedStr::new();
    // Check that extended leaves are supported.
    let (max_ext, _, _, _) = cpuid_raw(0x8000_0000);
    if max_ext >= 0x8000_0004 {
        for leaf in 0x8000_0002u32..=0x8000_0004 {
            let (a, b, c, d) = cpuid_raw(leaf);
            brand.push_u32_le(a);
            brand.push_u32_le(b);
            brand.push_u32_le(c);
            brand.push_u32_le(d);
        }
    } else {
        for &b in b"unknown" { if brand.len < 64 { brand.bytes[brand.len] = b; brand.len += 1; } }
    }
    (vendor, brand)
}
