//! TinyCC embedder — compile C inside OS101 with no host toolchain.
//!
//! Routing decision (see the module docs in `mod.rs`): TCC emits static PIE
//! (`ET_DYN`); the process loader applies `R_X86_64_RELATIVE` (and friends) so
//! the image can live at `USER_BASE` despite the small code model.

pub mod ffi;
pub mod selftest;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_void};
use core::sync::atomic::{AtomicBool, Ordering};

static SEEDED: AtomicBool = AtomicBool::new(false);

/// Install VFS hooks the libc shim needs before TCC opens any file.
pub fn install() {
    os101_libc_shim::install_vfs(vfs_read, vfs_write, vfs_remove, vfs_exists);
    // Keep the shim crate in the link graph even if nothing else names it
    // beyond quickjs::install — naming install_vfs is enough, but be explicit.
    let _ = core::mem::size_of_val(&SEEDED);
}

fn vfs_read(path: &str) -> Result<Vec<u8>, &'static str> {
    crate::fs::cmd_cat(path)
}

fn vfs_write(path: &str, data: &[u8]) -> Result<(), &'static str> {
    crate::fs::cmd_write_file(path, data.to_vec())
}

fn vfs_remove(path: &str) -> Result<(), &'static str> {
    crate::fs::cmd_remove(path)
}

fn vfs_exists(path: &str) -> bool {
    crate::fs::cmd_cat(path).is_ok()
}

/// Seed `/disk/include` and `/disk/lib` with headers and the minimal runtime
/// TCC links against. Idempotent; skips files that already exist so a user's
/// edits survive reboot.
pub fn seed_toolchain() -> Result<usize, &'static str> {
    if SEEDED.swap(true, Ordering::SeqCst) {
        return Ok(0);
    }
    let mut n = 0usize;
    for (path, data) in runtime::FILES.iter() {
        if !path.starts_with("/disk/") {
            continue;
        }
        if crate::fs::cmd_cat(path).is_ok() {
            continue;
        }
        if let Some(parent) = path.rsplit_once('/').map(|(p, _)| p) {
            let mut built = alloc::string::String::new();
            for part in parent.split('/').filter(|p| !p.is_empty()) {
                built.push('/');
                built.push_str(part);
                let _ = crate::fs::cmd_mkdir(&built);
            }
        }
        crate::fs::cmd_write_file(path, data.as_bytes().to_vec())?;
        n += 1;
    }
    Ok(n)
}

/// Result of a `cc` invocation.
pub struct CompileResult {
    pub ok: bool,
    pub diagnostics: String,
    pub output_path: Option<String>,
}

struct DiagCapture {
    buf: String,
}

extern "C" fn diag_callback(opaque: *mut c_void, msg: *const c_char) {
    if opaque.is_null() || msg.is_null() {
        return;
    }
    let cap = unsafe { &mut *(opaque as *mut DiagCapture) };
    let bytes = unsafe {
        let mut len = 0usize;
        while *msg.add(len) != 0 {
            len += 1;
            if len > 4096 {
                break;
            }
        }
        core::slice::from_raw_parts(msg as *const u8, len)
    };
    if let Ok(s) = core::str::from_utf8(bytes) {
        if !cap.buf.is_empty() {
            cap.buf.push('\n');
        }
        cap.buf.push_str(s);
    }
}

/// Compile one or more sources into an ELF at `output`, linking the seeded
/// OS101 runtime. `args` is a shell-style argument list already split
/// (sources, `-o out`, `-I`, `-D`, `-O`, `-W`, `-l` …).
pub fn compile(args: &[&str]) -> CompileResult {
    let _ = seed_toolchain();

    let mut sources: Vec<String> = Vec::new();
    let mut output: Option<String> = None;
    let mut extras: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if a == "-o" {
            if i + 1 >= args.len() {
                return CompileResult {
                    ok: false,
                    diagnostics: String::from("cc: -o needs an argument"),
                    output_path: None,
                };
            }
            output = Some(args[i + 1].to_string());
            i += 2;
            continue;
        }
        if a.starts_with("-o") && a.len() > 2 {
            output = Some(a[2..].to_string());
            i += 1;
            continue;
        }
        if a.starts_with('-') {
            extras.push(a.to_string());
            if matches!(a, "-I" | "-D" | "-L" | "-l" | "-include") && i + 1 < args.len() {
                extras.push(args[i + 1].to_string());
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        sources.push(a.to_string());
        i += 1;
    }

    if sources.is_empty() {
        return CompileResult {
            ok: false,
            diagnostics: String::from("cc: no input files"),
            output_path: None,
        };
    }
    let output = output.unwrap_or_else(|| {
        // Default: strip extension and write beside the first source.
        let src = &sources[0];
        if let Some((base, _)) = src.rsplit_once('.') {
            base.to_string()
        } else {
            alloc::format!("{}.elf", src)
        }
    });

    let mut diag = DiagCapture {
        buf: String::new(),
    };

    unsafe {
        let s = ffi::tcc_new();
        if s.is_null() {
            return CompileResult {
                ok: false,
                diagnostics: String::from("cc: tcc_new failed"),
                output_path: None,
            };
        }
        ffi::tcc_set_error_func(
            s,
            &mut diag as *mut DiagCapture as *mut c_void,
            Some(diag_callback),
        );
        ffi::tcc_set_lib_path(s, cstr_ptr(b"/disk/lib/tcc\0"));
        // PIE EXE without -static so TCC emits PT_DYNAMIC / RELA for load-time
        // rebase to USER_BASE. -nostdlib keeps host libc out; we add our runtime.
        ffi::tcc_set_options(s, cstr_ptr(b"-nostdlib\0"));
        for e in &extras {
            let c = alloc::ffi::CString::new(e.as_str()).unwrap_or_default();
            ffi::tcc_set_options(s, c.as_ptr());
        }
        ffi::tcc_add_sysinclude_path(s, cstr_ptr(b"/disk/lib/tcc/include\0"));
        ffi::tcc_add_sysinclude_path(s, cstr_ptr(b"/disk/include\0"));
        ffi::tcc_add_include_path(s, cstr_ptr(b"/disk/include\0"));
        ffi::tcc_add_library_path(s, cstr_ptr(b"/disk/lib\0"));

        if ffi::tcc_set_output_type(s, ffi::TCC_OUTPUT_EXE) < 0 {
            let msg = diag.buf.clone();
            ffi::tcc_delete(s);
            return CompileResult {
                ok: false,
                diagnostics: if msg.is_empty() {
                    String::from("cc: tcc_set_output_type failed")
                } else {
                    msg
                },
                output_path: None,
            };
        }

        // Runtime objects / sources, then user sources.
        for rt in runtime::LINK_SOURCES {
            let c = alloc::ffi::CString::new(*rt).unwrap_or_default();
            if ffi::tcc_add_file(s, c.as_ptr()) < 0 {
                let msg = diag.buf.clone();
                ffi::tcc_delete(s);
                return CompileResult {
                    ok: false,
                    diagnostics: if msg.is_empty() {
                        alloc::format!("cc: failed to add runtime {}", rt)
                    } else {
                        msg
                    },
                    output_path: None,
                };
            }
        }
        for src in &sources {
            let c = alloc::ffi::CString::new(src.as_str()).unwrap_or_default();
            if ffi::tcc_add_file(s, c.as_ptr()) < 0 {
                let msg = diag.buf.clone();
                ffi::tcc_delete(s);
                return CompileResult {
                    ok: false,
                    diagnostics: if msg.is_empty() {
                        alloc::format!("cc: failed to compile {}", src)
                    } else {
                        msg
                    },
                    output_path: None,
                };
            }
        }

        let out_c = alloc::ffi::CString::new(output.as_str()).unwrap_or_default();
        let rc = ffi::tcc_output_file(s, out_c.as_ptr());
        ffi::tcc_delete(s);
        if rc < 0 {
            return CompileResult {
                ok: false,
                diagnostics: if diag.buf.is_empty() {
                    String::from("cc: link failed")
                } else {
                    diag.buf
                },
                output_path: None,
            };
        }
    }

    CompileResult {
        ok: true,
        diagnostics: diag.buf,
        output_path: Some(output),
    }
}

/// Compile a C string in memory, relocate, and call `int name(void)`.
/// Used by the boot self-test. Runs in ring 0 — not for user programs.
pub fn eval_i32(source: &str, entry: &str) -> Result<i32, String> {
    let mut diag = DiagCapture {
        buf: String::new(),
    };
    unsafe {
        let s = ffi::tcc_new();
        if s.is_null() {
            return Err(String::from("tcc_new failed"));
        }
        ffi::tcc_set_error_func(
            s,
            &mut diag as *mut DiagCapture as *mut c_void,
            Some(diag_callback),
        );
        ffi::tcc_set_lib_path(s, cstr_ptr(b"/disk/lib/tcc\0"));
        ffi::tcc_add_sysinclude_path(s, cstr_ptr(b"/disk/lib/tcc/include\0"));
        ffi::tcc_set_options(s, cstr_ptr(b"-nostdlib\0"));
        if ffi::tcc_set_output_type(s, ffi::TCC_OUTPUT_MEMORY) < 0 {
            ffi::tcc_delete(s);
            return Err(diag_or(&diag, "set_output_type failed"));
        }
        let src = alloc::ffi::CString::new(source).map_err(|_| String::from("source NUL"))?;
        if ffi::tcc_compile_string(s, src.as_ptr()) < 0 {
            let e = diag_or(&diag, "compile failed");
            ffi::tcc_delete(s);
            return Err(e);
        }
        if ffi::tcc_relocate(s, ffi::TCC_RELOCATE_AUTO) < 0 {
            let e = diag_or(&diag, "relocate failed");
            ffi::tcc_delete(s);
            return Err(e);
        }
        let name = alloc::ffi::CString::new(entry).map_err(|_| String::from("entry NUL"))?;
        let sym = ffi::tcc_get_symbol(s, name.as_ptr());
        if sym.is_null() {
            ffi::tcc_delete(s);
            return Err(alloc::format!("undefined symbol '{}'", entry));
        }
        let f: extern "C" fn() -> i32 = core::mem::transmute(sym);
        let v = f();
        ffi::tcc_delete(s);
        Ok(v)
    }
}

/// Compile source that may call a few injected symbols (printf sink, etc.).
pub fn eval_i32_with_printf(source: &str, entry: &str) -> Result<(i32, String), String> {
    use core::sync::atomic::AtomicPtr;
    static CAPTURE: AtomicPtr<String> = AtomicPtr::new(core::ptr::null_mut());

    let mut out = String::new();
    CAPTURE.store(&mut out as *mut String, Ordering::SeqCst);

    extern "C" fn shim_putchar(c: c_int) -> c_int {
        let p = CAPTURE.load(Ordering::SeqCst);
        if !p.is_null() && c >= 0 && c <= 255 {
            unsafe { (*p).push(c as u8 as char) };
        }
        c
    }
    extern "C" fn shim_printf(fmt: *const c_char) -> c_int {
        // Extremely small printf: only handles plain text and %d / %s with one arg ignored
        // beyond the format for self-test purposes — self-tests use putchar loops instead.
        if fmt.is_null() {
            return 0;
        }
        let p = CAPTURE.load(Ordering::SeqCst);
        if p.is_null() {
            return 0;
        }
        unsafe {
            let mut i = 0usize;
            while *fmt.add(i) != 0 {
                (*p).push(*fmt.add(i) as u8 as char);
                i += 1;
                if i > 2048 {
                    break;
                }
            }
            i as c_int
        }
    }

    let mut diag = DiagCapture {
        buf: String::new(),
    };
    let result = unsafe {
        let s = ffi::tcc_new();
        if s.is_null() {
            CAPTURE.store(core::ptr::null_mut(), Ordering::SeqCst);
            return Err(String::from("tcc_new failed"));
        }
        ffi::tcc_set_error_func(
            s,
            &mut diag as *mut DiagCapture as *mut c_void,
            Some(diag_callback),
        );
        ffi::tcc_set_lib_path(s, cstr_ptr(b"/disk/lib/tcc\0"));
        ffi::tcc_add_sysinclude_path(s, cstr_ptr(b"/disk/lib/tcc/include\0"));
        ffi::tcc_set_options(s, cstr_ptr(b"-nostdlib\0"));
        if ffi::tcc_set_output_type(s, ffi::TCC_OUTPUT_MEMORY) < 0 {
            ffi::tcc_delete(s);
            CAPTURE.store(core::ptr::null_mut(), Ordering::SeqCst);
            return Err(diag_or(&diag, "set_output_type failed"));
        }
        ffi::tcc_add_symbol(s, cstr_ptr(b"putchar\0"), shim_putchar as *const c_void);
        ffi::tcc_add_symbol(s, cstr_ptr(b"printf\0"), shim_printf as *const c_void);

        let src = alloc::ffi::CString::new(source).map_err(|_| String::from("source NUL"))?;
        if ffi::tcc_compile_string(s, src.as_ptr()) < 0 {
            let e = diag_or(&diag, "compile failed");
            ffi::tcc_delete(s);
            CAPTURE.store(core::ptr::null_mut(), Ordering::SeqCst);
            return Err(e);
        }
        if ffi::tcc_relocate(s, ffi::TCC_RELOCATE_AUTO) < 0 {
            let e = diag_or(&diag, "relocate failed");
            ffi::tcc_delete(s);
            CAPTURE.store(core::ptr::null_mut(), Ordering::SeqCst);
            return Err(e);
        }
        let name = alloc::ffi::CString::new(entry).map_err(|_| String::from("entry NUL"))?;
        let sym = ffi::tcc_get_symbol(s, name.as_ptr());
        if sym.is_null() {
            ffi::tcc_delete(s);
            CAPTURE.store(core::ptr::null_mut(), Ordering::SeqCst);
            return Err(alloc::format!("undefined symbol '{}'", entry));
        }
        let f: extern "C" fn() -> i32 = core::mem::transmute(sym);
        let v = f();
        ffi::tcc_delete(s);
        Ok(v)
    };
    CAPTURE.store(core::ptr::null_mut(), Ordering::SeqCst);
    result.map(|v| (v, out))
}

fn diag_or(diag: &DiagCapture, fallback: &str) -> String {
    if diag.buf.is_empty() {
        fallback.to_string()
    } else {
        diag.buf.clone()
    }
}

fn cstr_ptr(s: &[u8]) -> *const c_char {
    s.as_ptr() as *const c_char
}

mod runtime {
    //! Headers and minimal userspace runtime seeded onto `/disk`.

    pub const LINK_SOURCES: &[&str] = &[
        "/disk/lib/crt0.c",
        "/disk/lib/syscall.c",
        "/disk/lib/os101.c",
        "/disk/lib/stdio.c",
        "/disk/lib/stdlib.c",
        "/disk/lib/string.c",
        "/disk/lib/malloc.c",
        "/disk/lib/va_list.c",
    ];

    pub const FILES: &[(&str, &str)] = &[
        ("/disk/include/stdio.h", include_str!("runtime/include/stdio.h")),
        ("/disk/include/stdlib.h", include_str!("runtime/include/stdlib.h")),
        ("/disk/include/string.h", include_str!("runtime/include/string.h")),
        ("/disk/include/stddef.h", include_str!("runtime/include/stddef.h")),
        ("/disk/include/stdint.h", include_str!("runtime/include/stdint.h")),
        ("/disk/include/stdarg.h", include_str!("runtime/include/stdarg.h")),
        ("/disk/include/stdbool.h", include_str!("runtime/include/stdbool.h")),
        ("/disk/include/errno.h", include_str!("runtime/include/errno.h")),
        ("/disk/include/os101.h", include_str!("runtime/include/os101.h")),
        ("/disk/lib/crt0.c", include_str!("runtime/crt0.c")),
        ("/disk/lib/syscall.c", include_str!("runtime/syscall.c")),
        ("/disk/lib/os101.c", include_str!("runtime/os101.c")),
        ("/disk/lib/stdio.c", include_str!("runtime/stdio.c")),
        ("/disk/lib/stdlib.c", include_str!("runtime/stdlib.c")),
        ("/disk/lib/string.c", include_str!("runtime/string.c")),
        ("/disk/lib/malloc.c", include_str!("runtime/malloc.c")),
        ("/disk/lib/va_list.c", include_str!("runtime/va_list.c")),
        (
            "/disk/lib/tcc/include/tccdefs.h",
            include_str!("../../../third_party/tcc/include/tccdefs.h"),
        ),
    ];
}
