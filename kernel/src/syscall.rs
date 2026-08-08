//! Phase 9 — syscall/sysret interface.
//!
//! Minimal x86_64 syscall entry using the CPU `syscall` instruction and
//! `sysretq` return path.

use core::sync::atomic::{AtomicBool, Ordering};

const IA32_EFER: u32 = 0xC000_0080;
const IA32_STAR: u32 = 0xC000_0081;
const IA32_LSTAR: u32 = 0xC000_0082;
const IA32_FMASK: u32 = 0xC000_0084;
/// Intel: SYSCALL loads RSP from MSR `IA32_KERNEL_RSP` when non‑zero — the CPU
/// then switches to a kernel stack **without** preserving the user RSP in any
/// register (see SYSCALL pseudocode). Our stub assumes RSP is still the user
/// stack so `lea`/pushes capture the correct return frame; keep this at 0 until
/// we implement a proper per‑CPU kernel stack + saved user RSP.
const IA32_KERNEL_RSP: u32 = 0x175;

const EFER_SCE: u64 = 1; // System Call Extensions enable

pub const SYS_WRITE: u64 = 1;
pub const SYS_EXIT: u64 = 2;
pub const SYS_YIELD: u64 = 3;
/// Move the heap break, for a C library's `malloc`. See [`crate::process::sbrk`].
pub const SYS_SBRK: u64 = 4;
/// Milliseconds since the Unix epoch, for `time` and `gettimeofday`.
pub const SYS_TIME_MS: u64 = 5;
pub const SYS_GUI_CREATE_WINDOW: u64 = 10;
pub const SYS_GUI_ADD_BUTTON: u64 = 11;
pub const SYS_GUI_ADD_LABEL: u64 = 12;
pub const SYS_GUI_GET_EVENT: u64 = 13;
pub const SYS_GUI_UPDATE_WIDGET: u64 = 14;
pub const SYS_GUI_SET_FOOTER: u64 = 15;

static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Captured on **every** syscall entry before any `push`, while `RSP` is still
/// the ring‑3 stack pointer (`IA32_KERNEL_RSP` must remain 0).
#[no_mangle]
static mut SYSCALL_USER_RSP_SAVE: u64 = 0;

// Per-call save area for user-preserved registers.
//
// Linux's syscall ABI (and `os101-user::syscall6`'s `in("rdi") …` constraints)
// guarantee that rdi/rsi/rdx/r10/r8/r9 are unchanged across `syscall`. The
// SysV C ABI we use to call `dispatch_syscall` is the OPPOSITE: those are
// caller-saved scratch. Without saving + restoring them ourselves, the user
// compiler keeps using stale values it assumed survived — which is exactly
// what we caught when calc's grid loop sent garbage `(w, h)` from the second
// `gui_add_button` call onwards.
//
// Single-CPU, single-syscall-at-a-time (we're not reentrant — syscall can't
// interrupt itself), so a static save area is fine.
#[no_mangle]
static mut SYSCALL_SAVE_RDI: u64 = 0;
#[no_mangle]
static mut SYSCALL_SAVE_RSI: u64 = 0;
#[no_mangle]
static mut SYSCALL_SAVE_RDX: u64 = 0;
#[no_mangle]
static mut SYSCALL_SAVE_R10: u64 = 0;
#[no_mangle]
static mut SYSCALL_SAVE_R8: u64 = 0;
#[no_mangle]
static mut SYSCALL_SAVE_R9: u64 = 0;
#[no_mangle]
static mut SYSCALL_SAVE_RIP: u64 = 0;
#[no_mangle]
static mut SYSCALL_SAVE_RFLAGS: u64 = 0;
// SysV callee-saved: the compiler for `dispatch_syscall` may freely use these
// inside the kernel frame. Normal sysret is fine (the Rust epilogue restores
// them before we return to the asm stub). Yield/sleep abandon that frame via
// a longjmp-style jump, so the user values must be parked here at entry and
// written back into the process on resume — otherwise C apps that keep
// function pointers in r12–r15 (WinGUI) resume into garbage and page-fault.
#[no_mangle]
static mut SYSCALL_SAVE_RBX: u64 = 0;
#[no_mangle]
static mut SYSCALL_SAVE_RBP: u64 = 0;
#[no_mangle]
static mut SYSCALL_SAVE_R12: u64 = 0;
#[no_mangle]
static mut SYSCALL_SAVE_R13: u64 = 0;
#[no_mangle]
static mut SYSCALL_SAVE_R14: u64 = 0;
#[no_mangle]
static mut SYSCALL_SAVE_R15: u64 = 0;

/// User callee-saved GPRs captured on the current syscall entry.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct SavedCalleeRegs {
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
}

/// Snapshot of the user registers parked by `syscall_entry_asm`.
pub fn saved_callee_regs() -> SavedCalleeRegs {
    unsafe {
        SavedCalleeRegs {
            rbx: SYSCALL_SAVE_RBX,
            rbp: SYSCALL_SAVE_RBP,
            r12: SYSCALL_SAVE_R12,
            r13: SYSCALL_SAVE_R13,
            r14: SYSCALL_SAVE_R14,
            r15: SYSCALL_SAVE_R15,
        }
    }
}

/// AMD syscall stack pointer MSR (Linux `MSR_AMD64_SYSENTER_ESP`); QEMU/AMD may
/// use this instead of/in addition to `IA32_KERNEL_RSP`.
const MSR_AMD_SYSCALL_RSP: u32 = 0xc000_0101;

#[inline]
unsafe fn wrmsr(msr: u32, value: u64) {
    let lo = value as u32;
    let hi = (value >> 32) as u32;
    core::arch::asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") lo,
        in("edx") hi,
        options(nostack, preserves_flags),
    );
}

#[inline]
unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    core::arch::asm!(
        "rdmsr",
        in("ecx") msr,
        out("eax") lo,
        out("edx") hi,
        options(nostack, preserves_flags),
    );
    ((hi as u64) << 32) | lo as u64
}

pub fn init() {
    if INITIALIZED.swap(true, Ordering::AcqRel) {
        return;
    }

    let kernel_cs = crate::gdt::kernel_code_selector() as u64;
    let user_cs = crate::gdt::user_code_selector() as u64;
    // In 64-bit mode SYSRET loads CS from STAR[63:48] + 16.
    let user_cs_base = user_cs
        .checked_sub(16)
        .expect("user code selector must be >= 16 for STAR encoding");
    let star = (kernel_cs << 32) | (user_cs_base << 48);

    unsafe {
        let efer = rdmsr(IA32_EFER);
        wrmsr(IA32_EFER, efer | EFER_SCE);
        wrmsr(IA32_STAR, star);
        wrmsr(IA32_LSTAR, syscall_entry_asm as *const () as usize as u64);
        // Clear IF on SYSCALL. We still run on the user stack (KERNEL_RSP is
        // 0), so a timer mid-syscall would interrupt CPL0 with a user SS and
        // then #GP on the handler's `iretq` (error_code 0x10). Masking IF
        // here matches Linux's FMASK for that bit; `sysretq` restores the
        // user's RFLAGS from r11, so IF comes back when we leave.
        wrmsr(IA32_FMASK, 0x200);
        // Linux bring-up style: never let SYSCALL replace RSP from a non‑zero
        // kernel‑stack MSR until we have SWAPGS + per‑CPU scratch. Otherwise the
        // user RSP is lost before the entry stub runs.
        wrmsr(IA32_KERNEL_RSP, 0);
        // AMD's syscall-ESP MSR (Linux MSR_AMD64_SYSENTER_ESP). Clear on all
        // CPUs: cheap, and avoids a CPUID round-trip that historically tripped
        // bad LLVM `nostack`/`ebx` asm on some toolchains.
        wrmsr(MSR_AMD_SYSCALL_RSP, 0);
    }
}

#[inline]
fn err() -> u64 {
    u64::MAX
}

/// When set, `SYS_WRITE` appends to this buffer instead of the framebuffer
/// text console. The Code Editor's Run action uses this to capture a
/// program's `printf` output into its own GUI window rather than letting it
/// draw over the desktop, which is what happens if a GUI-launched process
/// writes to the classic console that only the shell normally uses.
static OUTPUT_CAPTURE: spin::Mutex<Option<alloc::string::String>> = spin::Mutex::new(None);
/// Cap on a captured run's output so a program stuck in a print loop cannot
/// grow this buffer without bound (the kernel aborts on allocation failure).
const CAPTURE_LIMIT: usize = 16 * 1024;

pub fn begin_capture() {
    *OUTPUT_CAPTURE.lock() = Some(alloc::string::String::new());
}

/// Stop capturing and return everything collected since [`begin_capture`].
pub fn end_capture() -> alloc::string::String {
    OUTPUT_CAPTURE.lock().take().unwrap_or_default()
}

extern "C" fn dispatch_syscall(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64, user_rip: u64, user_rsp: u64) -> u64 {
    match nr {
        SYS_WRITE => {
            let ptr = a1 as usize;
            let len = a2 as usize;
            if !crate::process::validate_user_range(ptr, len) {
                return err();
            }
            let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
            let mut capture = OUTPUT_CAPTURE.lock();
            if let Some(buf) = capture.as_mut() {
                for &b in bytes {
                    if buf.len() >= CAPTURE_LIMIT {
                        break;
                    }
                    let c = match b {
                        b'\n' | b'\r' | b'\t' => b as char,
                        0x20..=0x7e => b as char,
                        _ => '?',
                    };
                    buf.push(c);
                }
            } else {
                drop(capture);
                for &b in bytes {
                    let c = match b {
                        b'\n' | b'\r' | b'\t' => b as char,
                        0x20..=0x7e => b as char,
                        _ => '?',
                    };
                    crate::print!("{}", c);
                }
            }
            a2
        }
        SYS_EXIT => {
            let code = a1;
            crate::serial_println!("USER: exit({})", code);
            crate::process::exit_current(code)
        }
        SYS_YIELD => {
            crate::process::yield_current(user_rip, user_rsp, 0);
            0
        }
        SYS_SBRK => {
            // The increment is signed, and arrives in a register: a shrink is
            // a large unsigned value that has to be read back as negative,
            // exactly as Unix `sbrk` takes an `intptr_t`.
            match crate::process::sbrk(a1 as i64) {
                Some(previous) => previous as u64,
                None => err(),
            }
        }
        SYS_TIME_MS => crate::rtc::unix_millis() as u64,
        SYS_GUI_CREATE_WINDOW => {
            let title_ptr = a1 as usize;
            let title_len = a2 as usize;
            let w = (a3 >> 32) as usize;
            let h = (a3 & 0xFFFFFFFF) as usize;
            if !crate::process::validate_user_range(title_ptr, title_len) {
                return err();
            }
            let title_bytes = unsafe { core::slice::from_raw_parts(title_ptr as *const u8, title_len) };
            let title = core::str::from_utf8(title_bytes).unwrap_or("User App");
            crate::window::sys_create_window(title, w, h)
        }
        SYS_GUI_ADD_BUTTON => {
            let win_handle = a1;
            let x = (a2 >> 32) as usize;
            let y = (a2 & 0xFFFFFFFF) as usize;
            let w = (a3 >> 32) as usize;
            let h = (a3 & 0xFFFFFFFF) as usize;
            let text_ptr = a4 as usize;
            let text_len = a5 as usize;
            let action_id = a6;
            
            if !crate::process::validate_user_range(text_ptr, text_len) {
                return err();
            }
            let text_bytes = unsafe { core::slice::from_raw_parts(text_ptr as *const u8, text_len) };
            let text = core::str::from_utf8(text_bytes).unwrap_or("");
            crate::window::sys_add_button(win_handle, x, y, w, h, text, action_id)
        }
        SYS_GUI_ADD_LABEL => {
            let win_handle = a1;
            let x = (a2 >> 32) as usize;
            let y = (a2 & 0xFFFFFFFF) as usize;
            let text_ptr = a3 as usize;
            let text_len = a4 as usize;
            
            if !crate::process::validate_user_range(text_ptr, text_len) {
                return err();
            }
            let text_bytes = unsafe { core::slice::from_raw_parts(text_ptr as *const u8, text_len) };
            let text = core::str::from_utf8(text_bytes).unwrap_or("");
            crate::window::sys_add_label(win_handle, x, y, text)
        }
        SYS_GUI_GET_EVENT => {
            let win_handle = a1;
            crate::window::sys_get_event(win_handle, user_rip, user_rsp)
        }
        SYS_GUI_UPDATE_WIDGET => {
            let win_handle = a1;
            let widget_handle = a2;
            let text_ptr = a3 as usize;
            let text_len = a4 as usize;

            if !crate::process::validate_user_range(text_ptr, text_len) {
                return err();
            }
            let text_bytes = unsafe { core::slice::from_raw_parts(text_ptr as *const u8, text_len) };
            let text = core::str::from_utf8(text_bytes).unwrap_or("");
            crate::window::sys_update_widget(win_handle, widget_handle, text)
        }
        SYS_GUI_SET_FOOTER => {
            let win_handle = a1;
            let text_ptr = a2 as usize;
            let text_len = a3 as usize;
            if !crate::process::validate_user_range(text_ptr, text_len) {
                return err();
            }
            let text_bytes = unsafe { core::slice::from_raw_parts(text_ptr as *const u8, text_len) };
            let text = core::str::from_utf8(text_bytes).unwrap_or("");
            crate::window::sys_set_footer(win_handle, text)
        }
        _ => {
            let _ = (a3, a4, a5, a6);
            err()
        }
    }
}

core::arch::global_asm!(
    // ---- Intel syntax: rust `global_asm!` / LLVM defaults to AT&T (see
    //    `interrupts.rs`), so `mov r12, [rsp+16]` was assembled backwards and
    //    corrupted SYSRET state (RIP 0 / user #PF). Linux entry uses explicit
    //    dialect or AT&T throughout; we pin Intel for this block.
    ".intel_syntax noprefix",
    ".global syscall_entry_asm",
    "syscall_entry_asm:",

    // CPU on entry: rax=nr, rdi/rsi/rdx/r10/r8/r9=a1..a6,
    //               rcx=user RIP, r11=user RFLAGS, rsp=user RSP
    // (we keep IA32_KERNEL_RSP == 0 so RSP stays ring-3 here — see init()).

    // 1. Park every register the user expects preserved. This is the heart
    //    of the fix: dispatch_syscall is a regular SysV-ABI Rust function
    //    and may scribble over rdi/rsi/rdx/rcx/r8/r9/r10/r11. We dump
    //    everything to global scratch first so the order in which we then
    //    rearrange registers for the call doesn't matter.
    "mov [rip + {save_rsp}], rsp",
    "mov [rip + {save_rip}], rcx",       // user_rip (CPU put it in rcx)
    "mov [rip + {save_rflags}], r11",    // user_rflags
    "mov [rip + {save_rdi}], rdi",       // a1
    "mov [rip + {save_rsi}], rsi",       // a2
    "mov [rip + {save_rdx}], rdx",       // a3
    "mov [rip + {save_r10}], r10",       // a4
    "mov [rip + {save_r8}],  r8",        // a5
    "mov [rip + {save_r9}],  r9",        // a6
    "mov [rip + {save_rbx}], rbx",
    "mov [rip + {save_rbp}], rbp",
    "mov [rip + {save_r12}], r12",
    "mov [rip + {save_r13}], r13",
    "mov [rip + {save_r14}], r14",
    "mov [rip + {save_r15}], r15",

    // 2. Marshal args for dispatch_syscall(nr, a1..a6, user_rip, user_rsp).
    //    SysV: rdi rsi rdx rcx r8 r9 in regs; args 7..9 on stack (right-to-left).
    "mov rdi, rax",                      // nr
    "mov rsi, [rip + {save_rdi}]",       // a1
    "mov rdx, [rip + {save_rsi}]",       // a2
    "mov rcx, [rip + {save_rdx}]",       // a3
    "mov r8,  [rip + {save_r10}]",       // a4
    "mov r9,  [rip + {save_r8}]",        // a5

    // 3. Put the stack on a 16-byte boundary for the call.
    //
    //    We deliberately keep running on the ring-3 stack (IA32_KERNEL_RSP is
    //    0, see init), so `dispatch_syscall` and the whole GUI and filesystem
    //    tree it reaches execute on whatever RSP the application happened to
    //    have. The kernel is built with hardware SSE2 and LLVM spills xmm
    //    registers with `movaps`, which faults on any address that is not a
    //    multiple of 16, so that inherited RSP is now load-bearing. SysV wants
    //    RSP at 0 mod 16 immediately before the `call`; the three stack
    //    arguments and the pad below add 32 bytes, so what we need underneath
    //    them is a boundary. The bytes this steps over are below the
    //    application's own RSP and the target disables the red zone, so there
    //    is nothing down there to lose.
    "and rsp, -16",

    //    The same argument that made us park rdi/rsi/rdx/r10/r8/r9 above
    //    applies to xmm0–xmm15: `os101-user`'s `asm!("syscall")` declares no
    //    clobber for them, so the application's compiler is entitled to keep a
    //    `f64` in one across the call, and `dispatch_syscall` will happily use
    //    every one of them. 512 bytes of the user's own stack, 16-byte aligned
    //    by the line above, is where the register file goes.
    "sub rsp, 512",
    "fxsave64 [rsp]",
    "sub rsp, 8",

    // Stack args (push 9th first so 7th ends up on top).
    "push qword ptr [rip + {save_rsp}]", // 9th: user_rsp
    "push qword ptr [rip + {save_rip}]", // 8th: user_rip
    "push qword ptr [rip + {save_r9}]",  // 7th: a6

    "call {dispatch}",

    "add rsp, 32",                       // drop the 3 stack args and the pad
    "fxrstor64 [rsp]",

    // 4. Restore user-preserved registers in place for the sysretq trip.
    //    RSP itself comes back wholesale from the saved copy, which is why
    //    nothing above has to unwind the alignment adjustment.
    //    sysretq itself loads RIP from rcx and RFLAGS from r11.
    "mov rdi, [rip + {save_rdi}]",
    "mov rsi, [rip + {save_rsi}]",
    "mov rdx, [rip + {save_rdx}]",
    "mov r10, [rip + {save_r10}]",
    "mov r8,  [rip + {save_r8}]",
    "mov r9,  [rip + {save_r9}]",
    "mov rcx, [rip + {save_rip}]",
    "mov r11, [rip + {save_rflags}]",
    "mov rsp, [rip + {save_rsp}]",
    "sysretq",
    ".att_syntax prefix",

    dispatch = sym dispatch_syscall,
    save_rsp    = sym SYSCALL_USER_RSP_SAVE,
    save_rip    = sym SYSCALL_SAVE_RIP,
    save_rflags = sym SYSCALL_SAVE_RFLAGS,
    save_rdi    = sym SYSCALL_SAVE_RDI,
    save_rsi    = sym SYSCALL_SAVE_RSI,
    save_rdx    = sym SYSCALL_SAVE_RDX,
    save_r10    = sym SYSCALL_SAVE_R10,
    save_r8     = sym SYSCALL_SAVE_R8,
    save_r9     = sym SYSCALL_SAVE_R9,
    save_rbx    = sym SYSCALL_SAVE_RBX,
    save_rbp    = sym SYSCALL_SAVE_RBP,
    save_r12    = sym SYSCALL_SAVE_R12,
    save_r13    = sym SYSCALL_SAVE_R13,
    save_r14    = sym SYSCALL_SAVE_R14,
    save_r15    = sym SYSCALL_SAVE_R15,
);

unsafe extern "C" {
    fn syscall_entry_asm();
}
