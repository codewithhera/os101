//! Interrupt Descriptor Table, exception handlers, PIC, and hardware IRQs.
//!
//! Since `extern "x86-interrupt"` and the `x86_64` crate's IDT types require
//! nightly features, we set up the IDT manually and use `global_asm!` stubs
//! that save/restore registers and call plain `extern "C"` Rust handlers.
//!
//! CPU exceptions: breakpoint (#3), double-fault (#8), page-fault (#14).
//! Hardware IRQs via 8259 PIC (remapped): timer (IRQ0 → 32), keyboard (IRQ1 → 33).

use crate::serial_println;
use core::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// IDT entry (16 bytes in long mode)
// ---------------------------------------------------------------------------

#[repr(C, packed)]
#[derive(Copy, Clone)]
struct IdtEntry {
    offset_lo: u16,  // Handler offset bits 0–15
    selector: u16,   // Code segment selector
    ist: u8,         // Bits 0–2: IST index (0 = don't use), bits 3–7: reserved
    type_attr: u8,   // P(1) | DPL(2) | 0(1) | type(4)
    offset_mid: u16, // Handler offset bits 16–31
    offset_hi: u32,  // Handler offset bits 32–63
    reserved: u32,
}

impl IdtEntry {
    const fn empty() -> Self {
        Self {
            offset_lo: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_hi: 0,
            reserved: 0,
        }
    }

    /// Set as a present interrupt gate (DPL 0), no IST.
    fn set_handler(&mut self, handler_addr: u64, code_sel: u16) {
        self.offset_lo = handler_addr as u16;
        self.selector = code_sel;
        self.ist = 0;
        self.type_attr = 0x8E; // P=1, DPL=0, type=0xE (interrupt gate)
        self.offset_mid = (handler_addr >> 16) as u16;
        self.offset_hi = (handler_addr >> 32) as u32;
        self.reserved = 0;
    }

    /// Set as a present interrupt gate (DPL 0) with an IST index (1–7).
    fn set_handler_ist(&mut self, handler_addr: u64, code_sel: u16, ist: u8) {
        self.set_handler(handler_addr, code_sel);
        self.ist = ist & 0x7;
    }
}

// ---------------------------------------------------------------------------
// IDT table (256 entries, 16-byte aligned)
// ---------------------------------------------------------------------------

#[repr(C, align(16))]
struct Idt([IdtEntry; 256]);

static mut IDT: Idt = Idt([IdtEntry::empty(); 256]);

/// IDTR descriptor passed to `lidt`.
#[repr(C, packed)]
struct IdtDescriptor {
    limit: u16,
    base: u64,
}

// ---------------------------------------------------------------------------
// Port I/O helpers
// ---------------------------------------------------------------------------

unsafe fn outb(port: u16, val: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") val,
                     options(nomem, nostack, preserves_flags));
}

unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    core::arch::asm!("in al, dx", in("dx") port, out("al") val,
                     options(nomem, nostack, preserves_flags));
    val
}

// ---------------------------------------------------------------------------
// 8259 PIC
// ---------------------------------------------------------------------------

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;
pub const PIC1_OFFSET: u8 = 32;

fn pic_init() {
    unsafe {
        // ICW1: begin init sequence (cascade, expect ICW4)
        outb(PIC1_CMD, 0x11);
        outb(0x80, 0); // I/O delay
        outb(PIC2_CMD, 0x11);
        outb(0x80, 0);

        // ICW2: remap IRQ vectors
        outb(PIC1_DATA, PIC1_OFFSET);       // IRQ 0–7 → 32–39
        outb(0x80, 0);
        outb(PIC2_DATA, PIC1_OFFSET + 8);   // IRQ 8–15 → 40–47
        outb(0x80, 0);

        // ICW3: wiring between master and slave
        outb(PIC1_DATA, 4); // slave on IRQ2
        outb(0x80, 0);
        outb(PIC2_DATA, 2); // cascade identity
        outb(0x80, 0);

        // ICW4: 8086 mode
        outb(PIC1_DATA, 0x01);
        outb(0x80, 0);
        outb(PIC2_DATA, 0x01);
        outb(0x80, 0);

        // Unmask timer (IRQ0), keyboard (IRQ1), PIC2 (IRQ2), and mouse (IRQ12).
        outb(PIC1_DATA, 0b1111_1000);
        outb(PIC2_DATA, 0b1110_1111);
    }
}

fn pic_eoi(irq: u8) {
    unsafe {
        if irq >= 8 {
            outb(PIC2_CMD, 0x20);
        }
        outb(PIC1_CMD, 0x20);
    }
}

// ---------------------------------------------------------------------------
// Assembly handler stubs (global_asm! — stable Rust)
// ---------------------------------------------------------------------------
//
// No-error-code handlers: CPU pushes 5 qwords (40 B). We push 15 regs
// (120 B) → total 160 B = 16-byte aligned before `call`. Correct.
//
// Error-code handlers: CPU pushes 6 qwords (48 B). We add 8 B alignment
// padding + 15 regs (120 B) → total 176 B = 16-byte aligned. Correct.
//
// Both rely on the CPU delivering the interrupt on a 16-byte aligned RSP,
// which long mode guarantees (Intel SDM 6.14.2). Every total a stub reaches is
// a multiple of 16, which is what the `call` needs and also what `fxsave64`
// needs of its destination — it faults otherwise.
//
// The 512-byte area each stub carves out holds the x87/SSE register file.
// Saving it is not optional now that the kernel is built with hardware SSE:
// the Rust handler bodies are ordinary `extern "C"` functions, so LLVM is
// free to use any of xmm0–xmm15 inside them — a `serial_println!` or the
// mouse driver's packet arithmetic is quite enough — and every one of those
// registers is caller-saved. A timer IRQ arriving in the middle of the
// browser's layout would otherwise return with the layout's floating-point
// intermediates replaced by whatever the handler last computed there. That is
// a wrong answer, not a crash, so nothing would report it.
//
// The area lives on the interrupted stack rather than in a static, which
// costs nothing extra and needs no reasoning about how deeply interrupts can
// nest. The IDT entries here are interrupt gates, so IF is clear inside a
// handler and hardware IRQs cannot nest — but an exception still can, and the
// main loop runs with interrupts disabled and re-enables them around `hlt`,
// so "how many can be live at once" is not a question worth having to answer
// again after the next change.

core::arch::global_asm!(
    // --- Register save/restore macros ---
    ".macro save_regs",
    "push rax",
    "push rcx",
    "push rdx",
    "push rbx",
    "push rbp",
    "push rsi",
    "push rdi",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    ".endm",
    "",
    ".macro restore_regs",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rdi",
    "pop rsi",
    "pop rbp",
    "pop rbx",
    "pop rdx",
    "pop rcx",
    "pop rax",
    ".endm",
    "",
    ".macro save_fpu",
    "sub rsp, 512",
    "fxsave64 [rsp]",
    ".endm",
    "",
    ".macro restore_fpu",
    "fxrstor64 [rsp]",
    "add rsp, 512",
    ".endm",
    "",
    // --- No error code: breakpoint (#3) ---
    ".global breakpoint_handler_asm",
    "breakpoint_handler_asm:",
    "save_regs",
    "cld",
    "save_fpu",
    "call {breakpoint}",
    "restore_fpu",
    "restore_regs",
    "iretq",
    "",
    // --- No error code: timer (IRQ0 = vector 32) ---
    ".global timer_handler_asm",
    "timer_handler_asm:",
    "save_regs",
    "cld",
    "save_fpu",
    "call {timer}",
    "restore_fpu",
    "restore_regs",
    "iretq",
    "",
    // --- No error code: keyboard (IRQ1 = vector 33) ---
    ".global keyboard_handler_asm",
    "keyboard_handler_asm:",
    "save_regs",
    "cld",
    "save_fpu",
    "call {keyboard}",
    "restore_fpu",
    "restore_regs",
    "iretq",
    "",
    // --- No error code: mouse (IRQ12 = vector 44) ---
    ".global mouse_handler_asm",
    "mouse_handler_asm:",
    "save_regs",
    "cld",
    "save_fpu",
    "call {mouse}",
    "restore_fpu",
    "restore_regs",
    "iretq",
    "",
    // --- No error code: invalid opcode (#6) ---
    ".global invalid_opcode_handler_asm",
    "invalid_opcode_handler_asm:",
    "save_regs",
    "cld",
    "save_fpu",
    "call {invalid_opcode}",
    "restore_fpu",
    "restore_regs",
    "iretq",
    "",
    // --- Error code: segment not present (#11) ---
    ".global segment_not_present_handler_asm",
    "segment_not_present_handler_asm:",
    "sub rsp, 8",
    "save_regs",
    "cld",
    "mov rdi, [rsp + 128]",
    "save_fpu",
    "call {segment_not_present}",
    "restore_fpu",
    "restore_regs",
    "add rsp, 16",
    "iretq",
    "",
    // --- Error code: stack-segment fault (#12) ---
    ".global stack_segment_fault_handler_asm",
    "stack_segment_fault_handler_asm:",
    "sub rsp, 8",
    "save_regs",
    "cld",
    "mov rdi, [rsp + 128]",
    "save_fpu",
    "call {stack_segment_fault}",
    "restore_fpu",
    "restore_regs",
    "add rsp, 16",
    "iretq",
    "",
    // --- Error code: general protection fault (#13) ---
    ".global gp_fault_handler_asm",
    "gp_fault_handler_asm:",
    "sub rsp, 8",
    "save_regs",
    "cld",
    "mov rdi, [rsp + 128]",      // error_code
    "mov rsi, [rsp + 136]",      // faulting RIP
    "mov rdx, [rsp + 144]",      // CS
    "mov rcx, [rsp + 160]",      // RSP
    "save_fpu",
    "call {gp_fault}",
    "restore_fpu",
    "restore_regs",
    "add rsp, 16",
    "iretq",
    "",
    // --- Error code: double-fault (#8) ---
    ".global double_fault_handler_asm",
    "double_fault_handler_asm:",
    "sub rsp, 8",                 // alignment padding
    "save_regs",
    "cld",
    "mov rdi, [rsp + 128]",      // error_code at (15 regs + 1 pad) * 8
    "save_fpu",
    "call {double_fault}",
    // double-fault handler diverges (panics), but just in case:
    "restore_fpu",
    "restore_regs",
    "add rsp, 16",               // skip padding + error code
    "iretq",
    "",
    // --- Error code: page-fault (#14) ---
    ".global page_fault_handler_asm",
    "page_fault_handler_asm:",
    "sub rsp, 8",
    "save_regs",
    "cld",
    "mov rdi, [rsp + 128]",      // error_code
    "save_fpu",
    "call {page_fault}",
    "restore_fpu",
    "restore_regs",
    "add rsp, 16",
    "iretq",
    "",
    breakpoint = sym breakpoint_handler_rust,
    timer = sym timer_handler_rust,
    keyboard = sym keyboard_handler_rust,
    mouse = sym mouse_handler_rust,
    double_fault = sym double_fault_handler_rust,
    page_fault = sym page_fault_handler_rust,
    invalid_opcode = sym invalid_opcode_handler_rust,
    segment_not_present = sym segment_not_present_handler_rust,
    stack_segment_fault = sym stack_segment_fault_handler_rust,
    gp_fault = sym gp_fault_handler_rust,
);

extern "C" {
    fn breakpoint_handler_asm();
    fn double_fault_handler_asm();
    fn page_fault_handler_asm();
    fn timer_handler_asm();
    fn keyboard_handler_asm();
    fn mouse_handler_asm();
    fn invalid_opcode_handler_asm();
    fn segment_not_present_handler_asm();
    fn stack_segment_fault_handler_asm();
    fn gp_fault_handler_asm();
}

// ---------------------------------------------------------------------------
// Rust handler bodies
// ---------------------------------------------------------------------------

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Monotonic tick count since boot (~18.2 Hz from the default PIT rate).
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

extern "C" fn breakpoint_handler_rust() {
    serial_println!("EXCEPTION: BREAKPOINT");
}

extern "C" fn double_fault_handler_rust(error_code: u64) -> ! {
    panic!("EXCEPTION: DOUBLE FAULT (error_code = {:#x})", error_code);
}

extern "C" fn page_fault_handler_rust(error_code: u64) {
    let cr2: u64;
    // SAFETY: reading CR2 to get the faulting address.
    unsafe {
        core::arch::asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack));
    }

    // Bit 2 of error_code = the fault came from user mode. A bug in user
    // code must NEVER panic the kernel — that's the same contract Linux
    // gives userspace. Drop the offending process and return to the kernel
    // main loop; the user-visible result is "the app crashed", not "the
    // OS crashed".
    let user_mode = (error_code & 0x4) != 0;
    if user_mode {
        let pid = crate::process::CURRENT_PROCESS
            .lock()
            .take()
            .map(|p| p.pid)
            .unwrap_or(0);
        serial_println!(
            "PAGE FAULT in user pid={} at {:#x} (error_code={:#x}) — killing process",
            pid, cr2, error_code
        );
        if pid != 0 {
            // Reclaim the dead process's pages and close its windows;
            // otherwise a crashing app leaks every frame it was given.
            crate::process::kill_process(pid);
        }
        // Same teardown path as a normal `exit(...)` syscall: restore the
        // saved kernel RSP/RIP and jump back into `enter_user_mode`'s post-
        // iretq landing pad. Doesn't return.
        unsafe {
            core::arch::asm!(
                "mov rsp, qword ptr [rip + {saved_rsp}]",
                "mov rax, qword ptr [rip + {saved_rip}]",
                "jmp rax",
                saved_rsp = sym crate::process::SAVED_KERNEL_RSP,
                saved_rip = sym crate::process::SAVED_KERNEL_RIP,
                options(noreturn),
            );
        }
    }

    panic!(
        "PAGE FAULT at {:#x} (error_code = {:#x})",
        cr2, error_code
    );
}

extern "C" fn invalid_opcode_handler_rust() {
    serial_println!("EXCEPTION: INVALID OPCODE");
    loop { unsafe { core::arch::asm!("hlt"); } }
}

extern "C" fn segment_not_present_handler_rust(error_code: u64) {
    serial_println!("EXCEPTION: SEGMENT NOT PRESENT (error_code = {:#x})", error_code);
    loop { unsafe { core::arch::asm!("hlt"); } }
}

extern "C" fn stack_segment_fault_handler_rust(error_code: u64) {
    serial_println!("EXCEPTION: STACK SEGMENT FAULT (error_code = {:#x})", error_code);
    loop { unsafe { core::arch::asm!("hlt"); } }
}

extern "C" fn gp_fault_handler_rust(error_code: u64, rip: u64, cs: u64, rsp: u64) {
    serial_println!(
        "EXCEPTION: GENERAL PROTECTION FAULT (error_code = {:#x}) rip={:#x} cs={:#x} rsp={:#x}",
        error_code, rip, cs, rsp
    );
    loop { unsafe { core::arch::asm!("hlt"); } }
}

extern "C" fn timer_handler_rust() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    pic_eoi(0);
}

extern "C" fn keyboard_handler_rust() {
    // The PS/2 data port (0x60) is shared between keyboard and mouse.
    // Status bit 0: output buffer full. Bit 5: AUX (mouse) data.
    // Only consume the byte if it's keyboard data, otherwise we'd feed
    // mouse packets into the keyboard decoder and produce garbage keys.
    let status = unsafe { inb(0x64) };
    if status & 0x01 != 0 && status & 0x20 == 0 {
        let scancode: u8 = unsafe { inb(0x60) };
        crate::keyboard::handle_scancode(scancode);
    }
    pic_eoi(1);
}

extern "C" fn mouse_handler_rust() {
    crate::mouse::handle_interrupt();
    pic_eoi(12);
}

// ---------------------------------------------------------------------------
// Public init
// ---------------------------------------------------------------------------

/// Turn on the FPU and the SSE register file.
///
/// This has to be the first thing the kernel does. With `CR4.OSFXSR` clear
/// every instruction that names an `xmm` register raises #UD, and the kernel
/// is compiled for a target with hardware SSE2, so the compiler emits them
/// freely — a `f64` multiply in the wallpaper generator, a 16-byte `memcpy`,
/// a struct copy. The BIOS bootloader that hands us control sets only
/// `CR4.PAE`, so nothing else has done this for us; anything scheduled before
/// this call is one register allocation away from a triple fault at boot.
///
/// `OSFXSR` is also what makes `fxsave64`/`fxrstor64` cover the xmm registers
/// rather than just the x87 stack, which the interrupt stubs depend on.
pub fn enable_sse() {
    // SAFETY: writing CR0/CR4 with the FPU and SSE bits the CPU has supported
    // since the Pentium III. Runs once, before anything else.
    unsafe {
        let mut cr0: u64;
        let mut cr4: u64;
        core::arch::asm!("mov {}, cr0", out(reg) cr0);
        cr0 &= !(1 << 2); // clear EM (bit 2)
        cr0 |= 1 << 1;  // set MP (bit 1)
        core::arch::asm!("mov cr0, {}", in(reg) cr0);

        core::arch::asm!("mov {}, cr4", out(reg) cr4);
        cr4 |= 1 << 9;  // set OSFXSR (bit 9)
        cr4 |= 1 << 10; // set OSXMMEXCPT (bit 10)
        core::arch::asm!("mov cr4, {}", in(reg) cr4);
    }
}

/// Load the IDT, initialise the PIC, and enable hardware interrupts.
/// Must be called after `gdt::init()`.
pub fn init() {
    let cs = crate::gdt::kernel_code_selector();

    // SAFETY: called once, single-threaded boot path. Assembly symbols are
    // valid function pointers defined in the global_asm! block above.
    unsafe {
        IDT.0[3].set_handler(breakpoint_handler_asm as *const () as u64, cs);
        // Double-fault uses IST 1 (= TSS.IST[0], where our separate stack lives).
        IDT.0[8].set_handler_ist(
            double_fault_handler_asm as *const () as u64,
            cs,
            (crate::gdt::DOUBLE_FAULT_IST_INDEX + 1) as u8,
        );
        IDT.0[14].set_handler(page_fault_handler_asm as *const () as u64, cs);
        IDT.0[PIC1_OFFSET as usize].set_handler(timer_handler_asm as *const () as u64, cs);
        IDT.0[(PIC1_OFFSET + 1) as usize].set_handler(keyboard_handler_asm as *const () as u64, cs);
        IDT.0[(PIC1_OFFSET + 12) as usize].set_handler(mouse_handler_asm as *const () as u64, cs);

        // Additional exceptions
        IDT.0[6].set_handler(invalid_opcode_handler_asm as *const () as u64, cs);
        IDT.0[11].set_handler(segment_not_present_handler_asm as *const () as u64, cs);
        IDT.0[12].set_handler(stack_segment_fault_handler_asm as *const () as u64, cs);
        IDT.0[13].set_handler(gp_fault_handler_asm as *const () as u64, cs);

        // Spurious IRQs
        // IDT.0[39].set_handler(...) // IRQ7
        // IDT.0[47].set_handler(...) // IRQ15

        let desc = IdtDescriptor {
            limit: (core::mem::size_of::<Idt>() - 1) as u16,
            base: &raw const IDT as u64,
        };
        core::arch::asm!("lidt [{}]", in(reg) &desc, options(nostack));
    }

    pic_init();

    // Enable interrupts.
    unsafe {
        core::arch::asm!("sti", options(nostack));
    }
}

/// Boot-time checks.
///
/// What the interrupt stubs do with the SSE register file cannot fail loudly.
/// If `fxsave64`/`fxrstor64` do not really cover `xmm0`–`xmm15` then a handler
/// returns with the register file it left behind rather than the one it found,
/// and the interrupted arithmetic is quietly wrong — no fault, no log line.
/// The way that happens in practice is `CR4.OSFXSR` being clear, in which case
/// both instructions restrict themselves to the x87 area and report nothing.
///
/// So exercise the exact pair the stubs use, on an area with the same
/// alignment, and check the register contents really made the round trip.
pub fn selftest() -> crate::selftest::Report {
    let mut report = crate::selftest::Report::new();

    #[repr(align(16))]
    struct SaveArea([u8; 512]);
    let mut area = SaveArea([0; 512]);
    let mut mxcsr: u32 = 0;

    let pattern: [u64; 4] = [
        0x0123_4567_89ab_cdef,
        0xfedc_ba98_7654_3210,
        0x1111_2222_3333_4444,
        0x5555_6666_7777_8888,
    ];
    let mut seen = [0u64; 4];

    // SAFETY: every pointer addresses a local of at least the size the block
    // writes, the save area is 16-byte aligned as `fxsave64` requires, and both
    // xmm registers the block touches are declared as clobbers.
    unsafe {
        core::arch::asm!(
            "movups xmm0, [{pat}]",
            "movups xmm15, [{pat} + 16]",
            "fxsave64 [{area}]",
            "xorps xmm0, xmm0",
            "xorps xmm15, xmm15",
            "fxrstor64 [{area}]",
            "movups [{seen}], xmm0",
            "movups [{seen} + 16], xmm15",
            "stmxcsr [{mxcsr}]",
            pat = in(reg) pattern.as_ptr(),
            area = in(reg) &raw mut area.0,
            seen = in(reg) seen.as_mut_ptr(),
            mxcsr = in(reg) &raw mut mxcsr,
            out("xmm0") _,
            out("xmm15") _,
        );
    }

    report.check("save area is 16-byte aligned", (&raw const area.0 as usize) % 16 == 0);
    report.check("xmm0 survives the round trip", seen[0..2] == pattern[0..2]);
    report.check("xmm15 survives the round trip", seen[2..4] == pattern[2..4]);
    // The FXSAVE image puts MXCSR at offset 24 and xmm0 at offset 160. Reading
    // them back is what distinguishes "the registers were saved" from "the
    // registers were never touched and happened to still hold the pattern".
    report.check("MXCSR is in the image", area.0[24..28] == mxcsr.to_le_bytes());
    report.check(
        "xmm0 is in the image",
        area.0[160..168] == pattern[0].to_le_bytes()
            && area.0[168..176] == pattern[1].to_le_bytes(),
    );

    report
}
