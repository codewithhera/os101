//! Global Descriptor Table with a Task State Segment.
//!
//! 64-bit long mode mostly ignores segmentation, but we still need a GDT for:
//! 1. A kernel code segment (CS must point to a valid 64-bit code descriptor).
//! 2. A TSS so the CPU can switch to a known-good stack on double-fault via
//!    the Interrupt Stack Table (IST).
//!
//! All structures are defined manually to stay on stable Rust without the
//! `x86_64` crate's nightly-only features.

/// IST array index (0-based) used for the double-fault stack.
/// The IDT entry uses this + 1 (IST field 1–7; 0 means "don't use IST").
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

// ---------------------------------------------------------------------------
// Task State Segment (104 bytes in 64-bit mode)
// ---------------------------------------------------------------------------

#[repr(C, packed)]
struct Tss {
    reserved_0: u32,
    /// Privilege-level stacks (RSP0–RSP2). RSP0 is used when entering ring 0
    /// from ring 3 — unused in Phase 3 (everything is ring 0).
    rsp: [u64; 3],
    reserved_1: u64,
    /// Interrupt Stack Table entries (IST1–IST7). The CPU switches to the
    /// stack at IST[n-1] when an IDT entry specifies IST index n.
    ist: [u64; 7],
    reserved_2: u64,
    reserved_3: u16,
    iomap_base: u16,
}

/// A stack the CPU may switch to on its own.
///
/// The alignment is the point of the wrapper: a bare `[u8; N]` has an
/// alignment of one, so the linker is free to place it anywhere, and the
/// handlers that run on these stacks are compiled with hardware SSE2 — one
/// `movaps` against an address that is 8 mod 16 is a #GP inside an exception
/// handler, which is a triple fault and a silent reboot. Intel's own advice
/// for IST entries (SDM 6.14.5) is the same: keep them 16-byte aligned.
#[repr(align(16))]
struct Stack<const N: usize>([u8; N]);

/// 20 KiB stack dedicated to the double-fault handler so a kernel stack
/// overflow doesn't triple-fault.
const DOUBLE_FAULT_STACK_SIZE: usize = 4096 * 5;
static mut DOUBLE_FAULT_STACK: Stack<DOUBLE_FAULT_STACK_SIZE> = Stack([0; DOUBLE_FAULT_STACK_SIZE]);
const PRIVILEGE_STACK_SIZE: usize = 4096 * 8;
static mut PRIVILEGE_STACK: Stack<PRIVILEGE_STACK_SIZE> = Stack([0; PRIVILEGE_STACK_SIZE]);

// What goes into the TSS is the *top* of each stack, so the size has to be a
// multiple of 16 as well for `Stack`'s alignment to reach the pointer the CPU
// actually loads.
const _: () = assert!(DOUBLE_FAULT_STACK_SIZE % 16 == 0);
const _: () = assert!(PRIVILEGE_STACK_SIZE % 16 == 0);

static mut TSS: Tss = Tss {
    reserved_0: 0,
    rsp: [0; 3],
    reserved_1: 0,
    ist: [0; 7],
    reserved_2: 0,
    reserved_3: 0,
    iomap_base: 104, // size of the TSS itself (no I/O map)
};

// ---------------------------------------------------------------------------
// GDT (4 entries = 32 bytes: null, kernel code, TSS low, TSS high)
// ---------------------------------------------------------------------------

/// Kernel code segment selector (entry 1 in GDT).
const KERNEL_CS: u16 = 1 << 3; // 0x08

/// Kernel data segment selector (entry 2 in GDT).
const KERNEL_DS: u16 = 2 << 3; // 0x10

/// TSS segment selector (entry 5 in GDT; entries 3–4 are user data/code for
/// the SYSRET STAR layout).
const TSS_SEL: u16 = 5 << 3; // 0x28
// SYSRET loads SS = STAR[63:48]+8 and CS = STAR[63:48]+16 (both with RPL 3).
// That requires user *data* before user *code* in the GDT — the opposite of
// the naive code-then-data order. With this layout and STAR[63:48] = 0x10:
//   SS = 0x1b (user data), CS = 0x23 (user code).
const USER_DS: u16 = (3 << 3) | 0b11; // 0x1b
const USER_CS: u16 = (4 << 3) | 0b11; // 0x23

#[repr(C, align(16))]
struct Gdt {
    null: u64,
    kernel_code: u64,
    kernel_data: u64,
    user_data: u64,
    user_code: u64,
    tss_lo: u64,
    tss_hi: u64,
}

static mut GDT: Gdt = Gdt {
    null: 0,
    // 64-bit kernel code: P=1, DPL=0, S=1, Type=Execute/Read(0xA), L=1, D=0.
    // Access byte = 0x9A, flags = 0x20.
    kernel_code: 0x00209A0000000000,
    // 64-bit kernel data: P=1, DPL=0, S=1, Type=Read/Write(0x2).
    // Access byte = 0x92, flags = 0x00.
    kernel_data: 0x0000920000000000,
    // 64-bit user data: P=1, DPL=3, S=1, Type=Read/Write(0x2).
    // Must sit immediately before user_code for SYSRET's STAR+8 / STAR+16.
    user_data: 0x0000F20000000000,
    // 64-bit user code: P=1, DPL=3, S=1, Type=Execute/Read(0xA), L=1.
    user_code: 0x0020FA0000000000,
    // TSS entries are filled at runtime.
    tss_lo: 0,
    tss_hi: 0,
};

/// Raw u16 value of the kernel code segment selector.
pub fn kernel_code_selector() -> u16 {
    KERNEL_CS
}

pub fn user_code_selector() -> u16 {
    USER_CS
}

pub fn user_data_selector() -> u16 {
    USER_DS
}

/// GDTR / IDTR descriptor used by `lgdt` / `lidt`.
#[repr(C, packed)]
struct TableDescriptor {
    limit: u16,
    base: u64,
}

/// Load the GDT, reload CS, and load the TSS. Call once, early in boot.
pub fn init() {
    // --- Fill in the TSS ---
    // SAFETY: single-threaded boot path, no other access.
    unsafe {
        let stack_top = &raw const DOUBLE_FAULT_STACK as u64
            + DOUBLE_FAULT_STACK_SIZE as u64;
        TSS.ist[DOUBLE_FAULT_IST_INDEX as usize] = stack_top;
        let rsp0_top = &raw const PRIVILEGE_STACK as u64 + PRIVILEGE_STACK_SIZE as u64;
        TSS.rsp[0] = rsp0_top;
    }

    // --- Build the TSS descriptor (16 bytes across two GDT entries) ---
    let tss_addr = (&raw const TSS) as u64;
    let tss_limit = (core::mem::size_of::<Tss>() - 1) as u64;

    // Low 8 bytes of the TSS descriptor:
    //   bits  0–15: limit low
    //   bits 16–39: base low (bits 0–23)
    //   bits 40–47: access (P=1, DPL=0, S=0, Type=0x9 = Available 64-bit TSS)
    //   bits 48–51: limit bits 16–19
    //   bits 52–55: flags (G=0, 0, 0, AVL=0)
    //   bits 56–63: base bits 24–31
    let access: u64 = 0x89; // Present, DPL=0, System, Available 64-bit TSS
    let tss_lo = (tss_limit & 0xFFFF)
        | ((tss_addr & 0xFFFFFF) << 16)
        | (access << 40)
        | (((tss_limit >> 16) & 0xF) << 48)
        | (((tss_addr >> 24) & 0xFF) << 56);

    // High 8 bytes: base bits 32–63 in low 32 bits, reserved in high 32 bits.
    let tss_hi = tss_addr >> 32;

    unsafe {
        GDT.tss_lo = tss_lo;
        GDT.tss_hi = tss_hi;

        // Load GDTR
        let gdtr = TableDescriptor {
            limit: (core::mem::size_of::<Gdt>() - 1) as u16,
            base: &raw const GDT as u64,
        };
        core::arch::asm!("lgdt [{}]", in(reg) &gdtr, options(nostack));

        // Reload CS via a far return, and load SS directly.
        core::arch::asm!(
            "mov ds, {ax:x}",
            "mov es, {ax:x}",
            "mov fs, {ax:x}",
            "mov gs, {ax:x}",
            "mov ss, {ax:x}",
            "push {cs:r}",           // push new CS
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",             // push return address
            "retfq",                  // far return → loads CS
            "2:",
            ax = in(reg) KERNEL_DS,
            cs = in(reg) KERNEL_CS as u64,
            tmp = lateout(reg) _,
            options(preserves_flags),
        );

        // Load the TSS
        core::arch::asm!("ltr {sel:x}", sel = in(reg) TSS_SEL, options(nostack));
    }
}
