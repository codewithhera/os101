//! Phase 9 — minimal process/userspace support.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

const ELF_MAGIC: &[u8; 4] = b"\x7FELF";
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;

const PF_X: u32 = 1;
const PF_W: u32 = 2;

const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;

const DT_NULL: i64 = 0;
const DT_SYMTAB: i64 = 6;
const DT_RELA: i64 = 7;
const DT_RELASZ: i64 = 8;
const DT_RELAENT: i64 = 9;
const DT_SYMENT: i64 = 11;
const DT_RELR: i64 = 36; // not handled yet
const DT_RELRSZ: i64 = 35;

const R_X86_64_NONE: u32 = 0;
const R_X86_64_64: u32 = 1;
const R_X86_64_GLOB_DAT: u32 = 6;
const R_X86_64_JUMP_SLOT: u32 = 7;
const R_X86_64_RELATIVE: u32 = 8;

// Userspace virtual-address window.
//
// Userspace occupies level-4 slot 1, which the kernel leaves empty (see the
// `vm` shell command for the live map: the kernel holds slots 0, 2–7 and
// 136). Owning a whole top-level slot is what makes per-process isolation a
// single page-table entry swap — see `memory::AddressSpace`.
//
// Within the slot the layout mirrors a small Linux-like address space:
//
//   0x0000_0080_1000_0000  USER_BASE        (slot 1 + 256 MiB)
//   0x0000_0080_2000_0000  USER_HEAP_BASE   (grows up, to USER_HEAP_MAX)
//   0x0000_0080_7000_0000  USER_STACK_TOP   (grows down, 256 KiB)
//   0x0000_0080_8000_0000  USER_LIMIT       (slot 1 + 2 GiB)
//
// Applications are linked at USER_BASE by `applications/*/user.ld`; changing
// the constants here means changing those linker scripts too.
const USER_SLOT_BASE: usize = (crate::memory::USER_L4_SLOT) << 39;
pub const USER_BASE: usize = USER_SLOT_BASE + 0x1000_0000;
pub const USER_LIMIT: usize = USER_SLOT_BASE + 0x8000_0000;
const USER_STACK_TOP: usize = USER_SLOT_BASE + 0x7000_0000;
const USER_STACK_SIZE: usize = 256 * 1024;

// See `map_user_stack` for why the entry stack pointer is eight below the top
// rather than on the boundary.
const _: () = assert!((USER_STACK_TOP - 8) % 16 == 8);

// Where a process's heap starts, and how far it may grow.
//
// A C program's `malloc` has to get its memory from somewhere, and the image
// is loaded at a fixed address with no room above it, so the heap is given a
// window of its own: 256 MiB in, which leaves the image a clear 256 MiB, and
// stopping well short of the stack so that a runaway allocation is refused
// rather than quietly overwriting the stack from below.
const USER_HEAP_BASE: usize = USER_SLOT_BASE + 0x2000_0000;
const USER_HEAP_MAX: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Process {
    pub pid: u64,
    pub rip: u64,
    pub rsp: u64,
    pub rax_return: u64, // The value userspace expects in RAX when it resumes
    /// `u64::MAX` = not waiting; otherwise the window handle.
    /// Plain integer (not `Option`) so the struct layout stays obvious under
    /// SIMD copies into the run queue — a packed `Option` previously made
    /// `rip`/`rsp` land in the wrong slots after a yield.
    pub waiting_window: u64,
    /// Callee-saved GPRs the user held at the syscall that parked this
    /// process. Restored by `enter_user_mode` before `iretq`.
    pub callee: crate::syscall::SavedCalleeRegs,
    /// The process's private user address space. Installed before its code
    /// runs and removed when it stops, so no two processes are ever mapped
    /// at once even though they share the same virtual addresses.
    pub space: crate::memory::AddressSpace,
}

const NOT_WAITING: u64 = u64::MAX;

static NEXT_PID: AtomicU64 = AtomicU64::new(1);
static RUN_QUEUE: Mutex<Vec<Process>> = Mutex::new(Vec::new());
static SLEEP_QUEUE: Mutex<Vec<Process>> = Mutex::new(Vec::new());
pub static CURRENT_PROCESS: Mutex<Option<Process>> = Mutex::new(None);

/// Live processes' address spaces, keyed by pid, with the page count kept
/// alongside for logging.
///
/// Teardown paths (`exit`, a fault, the user closing a window) only get a
/// pid, so the space has to be findable from one. It is not stored in
/// `Process` alone because a dying process has usually already been removed
/// from every queue by the time its memory is released.
static PROCESS_SPACES: Mutex<BTreeMap<u64, (crate::memory::AddressSpace, usize)>> =
    Mutex::new(BTreeMap::new());

/// How far each live process has grown its heap, as an offset from
/// [`USER_HEAP_BASE`].
///
/// Kept out of `Process` for the same reason the address spaces are: the
/// break has to be reachable from a pid alone, and the syscall path only ever
/// has the current pid to hand.
static PROCESS_BREAK: Mutex<BTreeMap<u64, usize>> = Mutex::new(BTreeMap::new());

#[no_mangle]
pub static mut SAVED_KERNEL_RSP: u64 = 0;
#[no_mangle]
pub static mut SAVED_KERNEL_RIP: u64 = 0;

#[inline]
fn align_down(v: u64, align: u64) -> u64 {
    v & !(align - 1)
}

#[inline]
fn align_up(v: u64, align: u64) -> u64 {
    (v + align - 1) & !(align - 1)
}

#[inline]
fn rd16(bytes: &[u8], off: usize) -> Result<u16, &'static str> {
    let arr: [u8; 2] = bytes
        .get(off..off + 2)
        .ok_or("ELF read out of bounds")?
        .try_into()
        .map_err(|_| "ELF u16 decode")?;
    Ok(u16::from_le_bytes(arr))
}

#[inline]
fn rd32(bytes: &[u8], off: usize) -> Result<u32, &'static str> {
    let arr: [u8; 4] = bytes
        .get(off..off + 4)
        .ok_or("ELF read out of bounds")?
        .try_into()
        .map_err(|_| "ELF u32 decode")?;
    Ok(u32::from_le_bytes(arr))
}

#[inline]
fn rd64(bytes: &[u8], off: usize) -> Result<u64, &'static str> {
    let arr: [u8; 8] = bytes
        .get(off..off + 8)
        .ok_or("ELF read out of bounds")?
        .try_into()
        .map_err(|_| "ELF u64 decode")?;
    Ok(u64::from_le_bytes(arr))
}

pub fn validate_user_range(ptr: usize, len: usize) -> bool {
    if ptr < USER_BASE || ptr >= USER_LIMIT {
        return false;
    }
    let Some(end) = ptr.checked_add(len) else {
        return false;
    };
    end <= USER_LIMIT
}

fn map_and_load_elf(elf: &[u8], pages: &mut Vec<u64>) -> Result<u64, &'static str> {
    if elf.len() < 64 || &elf[0..4] != ELF_MAGIC {
        return Err("not an ELF");
    }
    if elf[4] != 2 || elf[5] != 1 {
        return Err("ELF must be 64-bit little-endian");
    }
    let e_type = rd16(elf, 16)?;
    if e_type != ET_EXEC && e_type != ET_DYN {
        return Err("ELF must be ET_EXEC or ET_DYN");
    }
    let e_machine = rd16(elf, 18)?;
    if e_machine != 0x3e {
        return Err("ELF is not x86_64");
    }
    let e_entry = rd64(elf, 24)?;
    let e_phoff = rd64(elf, 32)? as usize;
    let e_phentsize = rd16(elf, 54)? as usize;
    let e_phnum = rd16(elf, 56)? as usize;
    if e_phentsize < 56 {
        return Err("ELF program header too small");
    }

    // First pass: compute LOAD span so we can rebase low-linked ELFs
    // (e.g. 0x0040_0000) into our user VA window.
    let mut have_load = false;
    let mut load_min = u64::MAX;
    let mut load_max = 0u64;
    for i in 0..e_phnum {
        let ph = e_phoff + i * e_phentsize;
        let p_type = rd32(elf, ph)?;
        if p_type != PT_LOAD {
            continue;
        }
        let p_vaddr = rd64(elf, ph + 16)?;
        let p_memsz = rd64(elf, ph + 40)? as usize;
        if p_memsz == 0 {
            continue;
        }
        have_load = true;
        let seg_lo = align_down(p_vaddr, 4096);
        let seg_hi = align_up(p_vaddr + p_memsz as u64, 4096);
        if seg_lo < load_min {
            load_min = seg_lo;
        }
        if seg_hi > load_max {
            load_max = seg_hi;
        }
    }

    let mut load_bias: i128 = 0;
    if have_load {
        let fits_native =
            load_min >= USER_BASE as u64 && load_max <= USER_LIMIT as u64 && e_entry >= USER_BASE as u64;
        if !fits_native {
            // Rebase entire image so lowest LOAD page starts at USER_BASE.
            load_bias = USER_BASE as i128 - load_min as i128;
            let rebased_top = load_max as i128 + load_bias;
            if rebased_top > USER_LIMIT as i128 {
                return Err("ELF image too large for user range");
            }
        }
    }

    let entry_rebased_i = e_entry as i128 + load_bias;
    if entry_rebased_i < 0 {
        return Err("ELF entry underflow");
    }
    let entry_rebased = entry_rebased_i as u64;

    crate::serial_println!(
        "spawn: e_phnum={} bias={:#x} entry={:#x}->{:#x}",
        e_phnum,
        load_bias as i64,
        e_entry,
        entry_rebased
    );
    for i in 0..e_phnum {
        let ph = e_phoff + i * e_phentsize;
        let p_type = rd32(elf, ph)?;
        if p_type != PT_LOAD {
            continue;
        }
        crate::serial_println!("spawn: ph[{}] LOAD", i);

        let p_flags = rd32(elf, ph + 4)?;
        let p_offset = rd64(elf, ph + 8)? as usize;
        let p_vaddr = rd64(elf, ph + 16)?;
        let p_filesz = rd64(elf, ph + 32)? as usize;
        let p_memsz = rd64(elf, ph + 40)? as usize;

        if p_memsz < p_filesz {
            return Err("ELF memsz < filesz");
        }
        if p_memsz == 0 {
            continue;
        }
        let seg_start_i = p_vaddr as i128 + load_bias;
        if seg_start_i < 0 {
            return Err("ELF segment underflow");
        }
        let seg_start = seg_start_i as usize;
        if !validate_user_range(seg_start, p_memsz) {
            return Err("ELF segment outside user range");
        }
        if p_offset.checked_add(p_filesz).ok_or("ELF overflow")? > elf.len() {
            return Err("ELF segment file bounds invalid");
        }

        let rel_vaddr = seg_start as u64;
        let page_start = align_down(rel_vaddr, 4096);
        let page_end = align_up(rel_vaddr + p_memsz as u64, 4096);
        let _segment_writable = (p_flags & PF_W) != 0;
        // During load the kernel must copy bytes into every mapped page,
        // including RX text. Map writable for now; tightening permissions can
        // be added once we support remapping/updating PTE flags.
        let writable = true;
        let executable = (p_flags & PF_X) != 0;
        crate::serial_println!("spawn:   map {:#x}..{:#x} (memsz={:#x})", page_start, page_end, p_memsz);
        for page in (page_start..page_end).step_by(4096) {
            let _ = crate::memory::map_user_page(page, writable, executable)?;
            pages.push(page);
        }
        crate::serial_println!("spawn:   zeroing {:#x} bytes @ {:#x}", p_memsz, seg_start);

        unsafe {
            core::ptr::write_bytes(seg_start as *mut u8, 0, p_memsz);
            core::ptr::copy_nonoverlapping(
                elf[p_offset..p_offset + p_filesz].as_ptr(),
                seg_start as *mut u8,
                p_filesz,
            );
        }
        crate::serial_println!("spawn:   segment done");
    }

    // Apply dynamic relocations when the image was linked for a different
    // base (static PIE from TinyCC, or any ET_DYN). Without this, the small
    // code model's absolute fixups still point at the link address.
    if load_bias != 0 || e_type == ET_DYN {
        apply_elf_relocations(elf, load_bias)?;
    }

    Ok(entry_rebased)
}

fn apply_elf_relocations(elf: &[u8], load_bias: i128) -> Result<(), &'static str> {
    let e_phoff = rd64(elf, 32)? as usize;
    let e_phentsize = rd16(elf, 54)? as usize;
    let e_phnum = rd16(elf, 56)? as usize;

    let mut dyn_vaddr = None;
    let mut dyn_filesz = 0usize;
    let mut dyn_offset = 0usize;
    for i in 0..e_phnum {
        let ph = e_phoff + i * e_phentsize;
        let p_type = rd32(elf, ph)?;
        if p_type != PT_DYNAMIC {
            continue;
        }
        dyn_offset = rd64(elf, ph + 8)? as usize;
        dyn_vaddr = Some(rd64(elf, ph + 16)?);
        dyn_filesz = rd64(elf, ph + 32)? as usize;
        break;
    }
    let Some(dyn_vaddr) = dyn_vaddr else {
        // Static PIE from TinyCC is often fully RIP-relative with every symbol
        // resolved at link time and no PT_DYNAMIC left behind. Rebasing by
        // copying the LOAD segments is then enough. If an absolute pointer
        // remains, the process will fault and the kernel will kill it.
        if load_bias != 0 {
            crate::serial_println!(
                "spawn: no PT_DYNAMIC; rebasing by {:#x} assuming PIC",
                load_bias as i64
            );
        }
        return Ok(());
    };
    let _ = dyn_vaddr;

    if dyn_offset.checked_add(dyn_filesz).ok_or("dyn overflow")? > elf.len() {
        return Err("PT_DYNAMIC out of bounds");
    }

    let mut rela_addr = 0u64;
    let mut rela_size = 0u64;
    let mut rela_ent = 24u64; // sizeof(Elf64_Rela)
    let mut symtab_addr = 0u64;
    let mut syment = 24u64; // sizeof(Elf64_Sym)
    let mut off = dyn_offset;
    while off + 16 <= dyn_offset + dyn_filesz {
        let tag = i64::from_le_bytes(
            elf[off..off + 8]
                .try_into()
                .map_err(|_| "dyn tag")?,
        );
        let val = u64::from_le_bytes(
            elf[off + 8..off + 16]
                .try_into()
                .map_err(|_| "dyn val")?,
        );
        off += 16;
        match tag {
            DT_NULL => break,
            DT_SYMTAB => symtab_addr = val,
            DT_SYMENT => syment = val,
            DT_RELA => rela_addr = val,
            DT_RELASZ => rela_size = val,
            DT_RELAENT => rela_ent = val,
            DT_RELR | DT_RELRSZ => {}
            _ => {}
        }
    }

    if rela_size == 0 {
        if load_bias != 0 {
            crate::serial_println!("spawn: warning: bias={:#x} but empty RELA", load_bias as i64);
        }
        return Ok(());
    }
    if rela_ent < 24 {
        return Err("DT_RELAENT too small");
    }
    if syment < 24 {
        return Err("DT_SYMENT too small");
    }

    let bias = load_bias as u64;
    let count = rela_size / rela_ent;
    crate::serial_println!("spawn: applying {} RELA entries (bias={:#x})", count, bias);
    let mut applied = 0u64;
    for i in 0..count {
        let entry_vaddr = rela_addr + i * rela_ent;
        let entry_ptr = (entry_vaddr as i128 + load_bias) as usize;
        if !validate_user_range(entry_ptr, 24) {
            return Err("RELA entry outside user range");
        }
        let r_offset = unsafe { core::ptr::read_unaligned(entry_ptr as *const u64) };
        let r_info = unsafe { core::ptr::read_unaligned((entry_ptr + 8) as *const u64) };
        let r_addend = unsafe { core::ptr::read_unaligned((entry_ptr + 16) as *const i64) };
        let r_type = (r_info & 0xffff_ffff) as u32;
        let r_sym = (r_info >> 32) as u32;
        let dest = (r_offset as i128 + load_bias) as usize;
        if !validate_user_range(dest, 8) {
            return Err("reloc target outside user range");
        }

        let sym_value = if r_sym != 0 {
            if symtab_addr == 0 {
                return Err("reloc needs dynsym but DT_SYMTAB missing");
            }
            let sym_ptr =
                (symtab_addr as i128 + load_bias + (r_sym as i128) * (syment as i128)) as usize;
            if !validate_user_range(sym_ptr, 24) {
                return Err("dynsym entry outside user range");
            }
            // Elf64_Sym: st_value at offset 8
            unsafe { core::ptr::read_unaligned((sym_ptr + 8) as *const u64) }
        } else {
            0u64
        };

        match r_type {
            R_X86_64_NONE => {}
            R_X86_64_RELATIVE => {
                let value = (r_addend as i128 + load_bias) as u64;
                unsafe {
                    core::ptr::write_unaligned(dest as *mut u64, value);
                }
                applied += 1;
            }
            R_X86_64_64 | R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT => {
                let value = (sym_value as i128 + load_bias + r_addend as i128) as u64;
                unsafe {
                    core::ptr::write_unaligned(dest as *mut u64, value);
                }
                applied += 1;
            }
            other => {
                crate::serial_println!("spawn: unsupported reloc type {}", other);
                return Err("unsupported ELF relocation type");
            }
        }
    }
    crate::serial_println!("spawn: applied {} relocations", applied);
    Ok(())
}

fn map_user_stack(pages: &mut Vec<u64>) -> Result<u64, &'static str> {
    let stack_start = USER_STACK_TOP - USER_STACK_SIZE;
    for page in (stack_start as u64..USER_STACK_TOP as u64).step_by(4096) {
        let _ = crate::memory::map_user_page(page, true, false)?;
        pages.push(page);
    }
    // Eight bytes below the top, not sixteen. `_start` is an `extern "C"`
    // function and the SysV ABI promises it that `rsp + 8` is a multiple of
    // 16 on entry — the eight being the return address a `call` would have
    // pushed, which our `iretq` does not. Now that apps are built with
    // hardware SSE2 that promise has teeth: the compiler places 16-byte
    // aligned slots relative to the entry `rsp`, and getting it wrong faults
    // on the first `movaps` rather than merely wasting a word.
    Ok((USER_STACK_TOP - 8) as u64)
}

/// Move the current process's heap break by `increment` bytes and return
/// where it was, the way Unix `sbrk` does.
///
/// This exists for C. Rust applications bring their own allocator over a
/// static array, but a C library's `malloc` expects to ask the kernel for
/// more memory as it runs, and every C program of any size does so through
/// this call.
///
/// Growing maps whole pages as they are first needed. Shrinking only lowers
/// the break: the pages stay mapped, because a C library that shrinks and
/// grows again in a loop would otherwise pay a page fault each time, and the
/// whole space is freed at exit regardless.
pub fn sbrk(increment: i64) -> Option<usize> {
    let pid = CURRENT_PROCESS.lock().as_ref()?.pid;

    let mut breaks = PROCESS_BREAK.lock();
    let current = *breaks.get(&pid).unwrap_or(&0);
    let requested = if increment >= 0 {
        current.checked_add(increment as usize)?
    } else {
        current.checked_sub(increment.unsigned_abs() as usize)?
    };
    if requested > USER_HEAP_MAX {
        return None;
    }

    // Only the pages between the old and new high-water marks are new; a
    // break that has been here before is already backed.
    let mapped = align_up(current as u64, 4096);
    let needed = align_up(requested as u64, 4096);
    let mut grown = 0usize;
    for offset in (mapped..needed).step_by(4096) {
        let page = USER_HEAP_BASE as u64 + offset;
        if crate::memory::map_user_page(page, true, false).is_err() {
            // Out of frames partway: hand back the pages just taken by
            // leaving the break where it was, so the caller sees a clean
            // failure rather than a half-grown heap.
            return None;
        }
        grown += 1;
    }
    if grown > 0 {
        if let Some((_, count)) = PROCESS_SPACES.lock().get_mut(&pid) {
            *count += grown;
        }
    }

    breaks.insert(pid, requested);
    Some(USER_HEAP_BASE + current)
}

/// Check that the pieces of the user address space still fit together.
///
/// The image, the heap and the stack are placed by hand-written constants
/// here and by linker scripts in `applications/*`. Nothing enforces that they
/// stay out of each other's way, and an overlap would not fault: it would let
/// a growing heap quietly scribble on the stack. So the arithmetic is checked
/// once, at boot, where a mistake is visible.
pub fn selftest() -> crate::selftest::Report {
    let mut report = crate::selftest::Report::new();

    report.check("user base below heap", USER_BASE < USER_HEAP_BASE);
    report.check(
        "heap ends below stack",
        USER_HEAP_BASE + USER_HEAP_MAX <= USER_STACK_TOP - USER_STACK_SIZE,
    );
    report.check("stack inside user window", USER_STACK_TOP <= USER_LIMIT);
    report.check(
        "heap window page aligned",
        USER_HEAP_BASE % 4096 == 0 && USER_HEAP_MAX % 4096 == 0,
    );
    report.check(
        "heap validates as user memory",
        validate_user_range(USER_HEAP_BASE, USER_HEAP_MAX),
    );
    report.check(
        "image has room before the heap",
        USER_HEAP_BASE - USER_BASE >= 16 * 1024 * 1024,
    );

    report
}

fn push_run_queue(
    rip: u64,
    rsp: u64,
    pages: Vec<u64>,
    space: crate::memory::AddressSpace,
) -> u64 {
    let pid = NEXT_PID.fetch_add(1, Ordering::SeqCst);
    PROCESS_SPACES.lock().insert(pid, (space, pages.len()));
    RUN_QUEUE.lock().push(Process {
        pid,
        rip,
        rsp,
        rax_return: 0,
        waiting_window: NOT_WAITING,
        callee: crate::syscall::SavedCalleeRegs::default(),
        space,
    });
    pid
}

/// Free everything a process owned: its pages, its page tables, and the
/// address space itself.
///
/// Safe to call more than once for the same pid: the second call finds no
/// entry and does nothing.
fn free_process_memory(pid: u64) {
    PROCESS_BREAK.lock().remove(&pid);
    let Some((space, count)) = PROCESS_SPACES.lock().remove(&pid) else {
        return;
    };
    // Unhook the space before freeing its frames, so the active page tables
    // never point at memory that is back on the free list.
    if current_space() == space {
        crate::memory::activate_address_space(crate::memory::AddressSpace::NONE);
    }
    crate::memory::destroy_address_space(space);
    crate::serial_println!("proc: pid={} released {} pages", pid, count);
}

/// How many processes currently hold a private address space.
pub fn live_space_count() -> usize {
    PROCESS_SPACES.lock().len()
}

/// The address space currently installed in the page tables.
fn current_space() -> crate::memory::AddressSpace {
    CURRENT_PROCESS
        .lock()
        .map(|p| p.space)
        .unwrap_or(crate::memory::AddressSpace::NONE)
}

pub fn spawn_demo_process() -> Result<u64, &'static str> {
    let elf = demo_elf_bytes();
    spawn_into_new_space(&elf, |elf, pages| {
        let entry = map_and_load_elf(elf, pages)?;
        let stack_top = map_user_stack(pages)?;
        Ok((entry, stack_top))
    })
}

/// Build a fresh address space, load an image into it, and queue the process.
///
/// The new space has to be installed in the page tables before any mapping
/// happens, because that is what the mapper walks. Whatever was mapped
/// before is restored afterwards, so spawning never disturbs the process
/// that asked for the spawn.
fn spawn_into_new_space(
    elf: &[u8],
    load: impl FnOnce(&[u8], &mut Vec<u64>) -> Result<(u64, u64), &'static str>,
) -> Result<u64, &'static str> {
    let space = crate::memory::create_address_space()?;
    let previous = current_space();
    crate::memory::activate_address_space(space);

    let mut pages = Vec::new();
    let result = load(elf, &mut pages);

    crate::memory::activate_address_space(previous);

    match result {
        Ok((entry, stack_top)) => Ok(push_run_queue(entry, stack_top, pages, space)),
        Err(e) => {
            // A partially loaded image must not leak the frames it did get.
            crate::memory::destroy_address_space(space);
            Err(e)
        }
    }
}

pub fn spawn_elf_bytes(elf: &[u8]) -> Result<u64, &'static str> {
    crate::serial_println!("spawn: load ELF ({} bytes)", elf.len());
    // Copy the image to the kernel heap before loading it. The caller's
    // slice may point into the kernel's .rodata (`include_bytes!`) or into a
    // buffer owned by an installed package, and loading swaps the active
    // user address space underneath us. Parsing from the kernel's own copy
    // — the same discipline as Linux's `copy_from_user` — keeps the loader
    // independent of whatever the mapping step does to the address space.
    let buf: alloc::vec::Vec<u8> = elf.to_vec();
    spawn_into_new_space(&buf, |elf, pages| {
        let entry = map_and_load_elf(elf, pages)?;
        crate::serial_println!("spawn: entry={:#x}, mapping stack", entry);
        let stack_top = map_user_stack(pages)?;
        crate::serial_println!("spawn: stack_top={:#x}, enqueue", stack_top);
        Ok((entry, stack_top))
    })
}

#[no_mangle]
static mut RESUME_RIP: u64 = 0;
#[no_mangle]
static mut RESUME_RSP: u64 = 0;
#[no_mangle]
static mut RESUME_RAX: u64 = 0;
#[no_mangle]
static mut RESUME_RBX: u64 = 0;
#[no_mangle]
static mut RESUME_RBP: u64 = 0;
#[no_mangle]
static mut RESUME_R12: u64 = 0;
#[no_mangle]
static mut RESUME_R13: u64 = 0;
#[no_mangle]
static mut RESUME_R14: u64 = 0;
#[no_mangle]
static mut RESUME_R15: u64 = 0;

pub fn run_scheduler_once() -> bool {
    let proc = {
        let mut q = RUN_QUEUE.lock();
        if q.is_empty() {
            return false;
        }
        q.remove(0)
    };
    // Park resume state in RESUME_* statics, then call the naked enter helper.
    // Callee-saved user GPRs must come too: yield abandons the syscall Rust
    // frame, so they are not restored by a normal function epilogue.
    unsafe {
        RESUME_RIP = proc.rip;
        RESUME_RSP = proc.rsp;
        RESUME_RAX = proc.rax_return;
        RESUME_RBX = proc.callee.rbx;
        RESUME_RBP = proc.callee.rbp;
        RESUME_R12 = proc.callee.r12;
        RESUME_R13 = proc.callee.r13;
        RESUME_R14 = proc.callee.r14;
        RESUME_R15 = proc.callee.r15;
    }
    let space = proc.space;
    *CURRENT_PROCESS.lock() = Some(proc);
    crate::memory::activate_address_space(space);
    unsafe {
        core::arch::asm!(
            "call {enter}",
            enter = sym enter_user_mode,
            clobber_abi("C"),
        );
    }
    // Control lands here once the process yields, sleeps, exits or faults.
    // Drop its mappings so kernel code never runs with userspace visible.
    crate::memory::activate_address_space(crate::memory::AddressSpace::NONE);
    true
}

pub fn yield_current(rip: u64, rsp: u64, rax_return: u64) {
    if let Some(mut proc) = CURRENT_PROCESS.lock().take() {
        proc.rip = rip;
        proc.rsp = rsp;
        proc.rax_return = rax_return;
        proc.callee = crate::syscall::saved_callee_regs();
        proc.waiting_window = NOT_WAITING;
        RUN_QUEUE.lock().push(proc);
    }
    unsafe {
        core::arch::asm!(
            "mov rsp, qword ptr [rip + {saved_rsp}]",
            "mov rax, qword ptr [rip + {saved_rip}]",
            "jmp rax",
            saved_rsp = sym SAVED_KERNEL_RSP,
            saved_rip = sym SAVED_KERNEL_RIP,
            options(noreturn)
        );
    }
}

/// Park the current process on SLEEP_QUEUE until an event arrives on
/// `win_handle`. Does not return — jumps back to the kernel main loop.
pub fn sleep_on_window(rip: u64, rsp: u64, rax_return: u64, win_handle: u64) -> ! {
    if let Some(mut proc) = CURRENT_PROCESS.lock().take() {
        proc.rip = rip;
        proc.rsp = rsp;
        proc.rax_return = rax_return;
        proc.callee = crate::syscall::saved_callee_regs();
        proc.waiting_window = win_handle;
        SLEEP_QUEUE.lock().push(proc);
    }
    unsafe {
        core::arch::asm!(
            "mov rsp, qword ptr [rip + {saved_rsp}]",
            "mov rax, qword ptr [rip + {saved_rip}]",
            "jmp rax",
            saved_rsp = sym SAVED_KERNEL_RSP,
            saved_rip = sym SAVED_KERNEL_RIP,
            options(noreturn)
        );
    }
}

/// Move every process parked on `win_handle` from SLEEP_QUEUE to RUN_QUEUE.
pub fn wake_on_window(win_handle: u64) {
    let mut sleep = SLEEP_QUEUE.lock();
    let mut run = RUN_QUEUE.lock();
    let mut i = 0;
    while i < sleep.len() {
        if sleep[i].waiting_window == win_handle {
            let mut p = sleep.remove(i);
            p.waiting_window = NOT_WAITING;
            run.push(p);
        } else {
            i += 1;
        }
    }
}

/// Windows a process still owns when it dies are closed by
/// `close_windows_owned_by`; this lets the window manager ask the scheduler
/// to drop a process whose window the user closed.
pub fn terminate(pid: u64) {
    RUN_QUEUE.lock().retain(|p| p.pid != pid);
    SLEEP_QUEUE.lock().retain(|p| p.pid != pid);
    let mut current = CURRENT_PROCESS.lock();
    if current.map(|p| p.pid) == Some(pid) {
        *current = None;
    }
    drop(current);
    free_process_memory(pid);
}

pub fn exit_current(_code: u64) -> ! {
    // Release the process's memory before we leave its context. Previously
    // this jumped straight back to the kernel loop, leaking every code and
    // stack frame the process had been given.
    //
    // Take the process out in its own statement: an `if let` would hold the
    // CURRENT_PROCESS guard across the teardown calls below, which both take
    // further locks.
    let dying = CURRENT_PROCESS.lock().take();
    if let Some(proc) = dying {
        free_process_memory(proc.pid);
        close_windows_owned_by(proc.pid);
    }
    return_to_kernel();
}

/// Tear down the current process after a fault. Called from the page-fault
/// handler, which has already taken it out of `CURRENT_PROCESS`.
pub fn kill_process(pid: u64) {
    free_process_memory(pid);
    close_windows_owned_by(pid);
}

fn close_windows_owned_by(pid: u64) {
    crate::window::close_windows_for_pid(pid);
}

/// Restore the saved kernel stack/instruction pointer and resume the main
/// loop. Shared by yield, sleep, exit and the fault path.
fn return_to_kernel() -> ! {
    unsafe {
        core::arch::asm!(
            "mov rsp, qword ptr [rip + {saved_rsp}]",
            "mov rax, qword ptr [rip + {saved_rip}]",
            "jmp rax",
            saved_rsp = sym SAVED_KERNEL_RSP,
            saved_rip = sym SAVED_KERNEL_RIP,
            options(noreturn)
        );
    }
}

/// Drop into ring 3 using the resume state parked in `RESUME_*` statics.
///
/// On the way in we stash the kernel's own callee-saved GPRs on the kernel
/// stack and point `SAVED_KERNEL_*` at the restore/`ret` epilogue. Yield then
/// longjmps back here with the stack intact, so the kernel recovers its
/// registers instead of inheriting the user's r12–r15 (that bug caused a #GP
/// right after the first successful C-app yield).
#[unsafe(naked)]
unsafe extern "C" fn enter_user_mode() {
    core::arch::naked_asm!(
        "push rbx",
        "push rbp",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "lea r10, [rip + 2f]",
        "mov qword ptr [rip + {saved_rip}], r10",
        "mov qword ptr [rip + {saved_rsp}], rsp",
        "mov rcx, qword ptr [rip + {resume_rip}]",
        "mov rsi, qword ptr [rip + {resume_rsp}]",
        "mov rax, qword ptr [rip + {resume_rax}]",
        "mov rbx, qword ptr [rip + {resume_rbx}]",
        "mov rbp, qword ptr [rip + {resume_rbp}]",
        "mov r12, qword ptr [rip + {resume_r12}]",
        "mov r13, qword ptr [rip + {resume_r13}]",
        "mov r14, qword ptr [rip + {resume_r14}]",
        "mov r15, qword ptr [rip + {resume_r15}]",
        "push {user_ss}",
        "push rsi",
        "push {rflags}",
        "push {user_cs}",
        "push rcx",
        "iretq",
        "2:",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbp",
        "pop rbx",
        "ret",
        saved_rip = sym SAVED_KERNEL_RIP,
        saved_rsp = sym SAVED_KERNEL_RSP,
        resume_rip = sym RESUME_RIP,
        resume_rsp = sym RESUME_RSP,
        resume_rax = sym RESUME_RAX,
        resume_rbx = sym RESUME_RBX,
        resume_rbp = sym RESUME_RBP,
        resume_r12 = sym RESUME_R12,
        resume_r13 = sym RESUME_R13,
        resume_r14 = sym RESUME_R14,
        resume_r15 = sym RESUME_R15,
        user_ss = const 0x1bu64, // USER_DATA | RPL3 — must match gdt.rs (SYSRET layout)
        user_cs = const 0x23u64, // USER_CODE | RPL3
        rflags = const 0x202u64,
    );
}

fn push_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn push_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn push_u64(v: &mut Vec<u8>, x: u64) {
    v.extend_from_slice(&x.to_le_bytes());
}

pub fn demo_elf_bytes() -> Vec<u8> {
    // Tiny static userspace payload:
    // write(1, msg, msg_len); exit(0);
    let msg = b"hello from userspace via syscall\n";
    let mut code = Vec::new();
    // mov rax, SYS_WRITE
    code.extend_from_slice(&[0x48, 0xC7, 0xC0, crate::syscall::SYS_WRITE as u8, 0, 0, 0]);
    // mov rdi, 1 (stdout-like fd)
    code.extend_from_slice(&[0x48, 0xC7, 0xC7, 1, 0, 0, 0]);
    // lea rsi, [rip + disp32]
    code.extend_from_slice(&[0x48, 0x8D, 0x35, 0, 0, 0, 0]);
    // mov rdx, msg_len
    code.extend_from_slice(&[0x48, 0xC7, 0xC2, msg.len() as u8, 0, 0, 0]);
    // syscall
    code.extend_from_slice(&[0x0F, 0x05]);
    // mov rax, SYS_EXIT
    code.extend_from_slice(&[0x48, 0xC7, 0xC0, crate::syscall::SYS_EXIT as u8, 0, 0, 0]);
    // xor rdi, rdi
    code.extend_from_slice(&[0x48, 0x31, 0xFF]);
    // syscall
    code.extend_from_slice(&[0x0F, 0x05]);
    // hlt (shouldn't be reached)
    code.push(0xF4);

    let msg_off = code.len();
    code.extend_from_slice(msg);

    // Patch lea displacement (from RIP after lea instruction).
    let lea_next_ip = 21usize;
    let disp = (msg_off as i32) - (lea_next_ip as i32);
    code[17..21].copy_from_slice(&disp.to_le_bytes());

    let code_file_off = 0x80usize;
    let entry = (USER_BASE + code_file_off) as u64;
    let filesz = code.len() as u64;

    let mut elf = Vec::with_capacity(code_file_off + code.len());
    // ELF header
    elf.extend_from_slice(ELF_MAGIC); // e_ident[0..4]
    elf.extend_from_slice(&[2, 1, 1, 0, 0]); // class, data, version, osabi, abiversion
    elf.extend_from_slice(&[0; 7]); // e_ident padding
    push_u16(&mut elf, 2); // ET_EXEC
    push_u16(&mut elf, 0x3e); // x86_64
    push_u32(&mut elf, 1); // e_version
    push_u64(&mut elf, entry); // e_entry
    push_u64(&mut elf, 64); // e_phoff
    push_u64(&mut elf, 0); // e_shoff
    push_u32(&mut elf, 0); // e_flags
    push_u16(&mut elf, 64); // e_ehsize
    push_u16(&mut elf, 56); // e_phentsize
    push_u16(&mut elf, 1); // e_phnum
    push_u16(&mut elf, 0); // e_shentsize
    push_u16(&mut elf, 0); // e_shnum
    push_u16(&mut elf, 0); // e_shstrndx

    // Program header
    push_u32(&mut elf, PT_LOAD);
    push_u32(&mut elf, PF_X); // RX
    push_u64(&mut elf, code_file_off as u64); // p_offset
    push_u64(&mut elf, entry); // p_vaddr
    push_u64(&mut elf, entry); // p_paddr
    push_u64(&mut elf, filesz); // p_filesz
    push_u64(&mut elf, filesz); // p_memsz
    push_u64(&mut elf, 0x1000); // p_align

    while elf.len() < code_file_off {
        elf.push(0);
    }
    elf.extend_from_slice(&code);
    elf
}
