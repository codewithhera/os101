//! Compiles the vendored QuickJS sources, TinyCC, and the C half of the libc
//! shim into one static archive, and tells Cargo to link it.
//!
//! This is done with `std::process::Command` rather than the `cc` crate on
//! purpose. `cc` would be the obvious choice, but pulling it in means a registry
//! fetch, and nothing in this repository's build is allowed to need the network.
//!
//! Environment overrides:
//!   OS101_SHIM_CC   — the C compiler to use (default: clang)
//!   OS101_SHIM_AR   — the archiver (default: llvm-ar from the Rust toolchain)
//!   OS101_SHIM_NDEBUG — set to 1 to compile out QuickJS's ~160 assertions
//!   OS101_SHIM_NO_TCC — set to 1 to skip TinyCC (emergency / size experiments)

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const QUICKJS_UNITS: &[&str] =
    &["quickjs.c", "dtoa.c", "libregexp.c", "libunicode.c", "cutils.c"];

const SHIM_UNITS: &[&str] = &[
    "printf.c",
    "string.c",
    "string_extra.c",
    "stdlib.c",
    "stdlib_extra.c",
    "fileio.c",
    "tcc_helpers.c",
    "quickjs_glue.c",
];

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("..")
        .join("..");
    let quickjs_src = root.join("quickjs").join("src");
    let shim_src = root.join("libc-shim").join("src");
    let shim_include = root.join("libc-shim").join("include");
    let tcc_root = root.join("tcc");
    let tcc_src = tcc_root.join("src");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    for dir in [&quickjs_src, &shim_src, &shim_include, &tcc_src, &tcc_root] {
        println!("cargo:rerun-if-changed={}", dir.display());
    }
    println!("cargo:rerun-if-env-changed=OS101_SHIM_CC");
    println!("cargo:rerun-if-env-changed=OS101_SHIM_AR");
    println!("cargo:rerun-if-env-changed=OS101_SHIM_NDEBUG");
    println!("cargo:rerun-if-env-changed=OS101_SHIM_NO_TCC");

    let version = std::fs::read_to_string(root.join("quickjs").join("VERSION"))
        .expect("third_party/quickjs/VERSION is missing");
    let version = version.trim();

    let cc = env::var("OS101_SHIM_CC").unwrap_or_else(|_| "clang".into());
    let mut flags = base_flags(version);
    flags.extend(target_flags());
    if env::var("OS101_SHIM_NDEBUG").as_deref() == Ok("1") {
        flags.push("-DNDEBUG".into());
    }
    flags.push(format!("-I{}", shim_include.display()));
    flags.push(format!("-I{}", quickjs_src.display()));

    let mut objects = Vec::new();
    for (dir, units) in [(&quickjs_src, QUICKJS_UNITS), (&shim_src, SHIM_UNITS)] {
        for unit in units {
            objects.push(compile(&cc, &flags, &dir.join(unit), &out_dir));
        }
    }

    // setjmp.S — real x86-64 implementation for TCC error recovery.
    objects.push(assemble(&cc, &flags, &shim_src.join("setjmp.S"), &out_dir));

    if env::var("OS101_SHIM_NO_TCC").as_deref() != Ok("1") {
        let mut tcc_flags = flags.clone();
        // TCC's own include dir first for tccdefs.h / stdarg, then the shim.
        tcc_flags.insert(0, format!("-I{}", tcc_src.display()));
        tcc_flags.insert(0, format!("-I{}", tcc_root.join("include").display()));
        tcc_flags.push("-DONE_SOURCE=1".into());
        tcc_flags.push("-DCONFIG_TCC_SEMLOCK=0".into());
        // Suppress noisy-but-harmless TCC warnings under -Werror-less clang.
        tcc_flags.push("-Wno-unused-parameter".into());
        tcc_flags.push("-Wno-sign-compare".into());
        tcc_flags.push("-Wno-unused-function".into());
        tcc_flags.push("-Wno-missing-field-initializers".into());
        tcc_flags.push("-Wno-pointer-to-int-cast".into());
        tcc_flags.push("-Wno-int-to-pointer-cast".into());
        tcc_flags.push("-Wno-unused-variable".into());
        tcc_flags.push("-Wno-unused-but-set-variable".into());
        objects.push(compile(
            &cc,
            &tcc_flags,
            &tcc_src.join("libtcc.c"),
            &out_dir,
        ));
    }

    let archive = out_dir.join("libquickjs.a");
    archive_objects(&archive, &objects);

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=quickjs");
}

fn base_flags(version: &str) -> Vec<String> {
    vec![
        "-std=gnu11".into(),
        "-O2".into(),
        "-fwrapv".into(),
        "-fno-builtin".into(),
        "-fno-strict-aliasing".into(),
        format!("-DCONFIG_VERSION=\"{version}\""),
    ]
}

fn target_flags() -> Vec<String> {
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if os == "none" {
        assert_eq!(arch, "x86_64", "the shim has only been built for x86_64");
        require_hardware_float();
        vec![
            "--target=x86_64-unknown-none-elf".into(),
            "-ffreestanding".into(),
            "-fno-stack-protector".into(),
            "-mno-red-zone".into(),
            "-fPIC".into(),
        ]
    } else {
        let triple = env::var("TARGET").unwrap_or_default();
        vec![format!("--target={triple}"), "-ffreestanding".into()]
    }
}

fn require_hardware_float() {
    let features = env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
    if features.split(',').any(|f| f == "sse2") {
        return;
    }
    if env::var("OS101_SHIM_ALLOW_SOFT_FLOAT").as_deref() == Ok("1") {
        println!(
            "cargo:warning=building QuickJS against a soft-float target; the \
             library must not be called from Rust, only measured"
        );
        return;
    }
    panic!(
        "this target has no sse2, so Rust is using the soft-float ABI and \
         doubles would be passed differently on each side of the C boundary. \
         Build the kernel for a hardware-float bare-metal target. Set \
         OS101_SHIM_ALLOW_SOFT_FLOAT=1 only to measure the library, never to \
         run it."
    );
}

fn compile(cc: &str, flags: &[String], source: &Path, out_dir: &Path) -> PathBuf {
    let object = out_dir.join(format!(
        "{}.o",
        source.file_stem().unwrap().to_string_lossy()
    ));
    let status = Command::new(cc)
        .args(flags)
        .arg("-c")
        .arg(source)
        .arg("-o")
        .arg(&object)
        .status()
        .unwrap_or_else(|e| panic!("could not run {cc}: {e}"));
    assert!(status.success(), "compiling {} failed", source.display());
    object
}

fn assemble(cc: &str, flags: &[String], source: &Path, out_dir: &Path) -> PathBuf {
    let object = out_dir.join(format!(
        "{}.o",
        source.file_stem().unwrap().to_string_lossy()
    ));
    // Assembly still needs the target triple / freestanding flags.
    let status = Command::new(cc)
        .args(flags.iter().filter(|f| {
            let s = f.as_str();
            s.starts_with("--target=")
                || s == "-ffreestanding"
                || s == "-fPIC"
                || s == "-mno-red-zone"
        }))
        .arg("-c")
        .arg(source)
        .arg("-o")
        .arg(&object)
        .status()
        .unwrap_or_else(|e| panic!("could not run {cc}: {e}"));
    assert!(status.success(), "assembling {} failed", source.display());
    object
}

fn archive_objects(archive: &Path, objects: &[PathBuf]) {
    let ar = env::var("OS101_SHIM_AR").unwrap_or_else(|_| llvm_ar().to_string_lossy().into());
    let _ = std::fs::remove_file(archive);
    let status = Command::new(&ar)
        .arg("crs")
        .arg(archive)
        .args(objects)
        .status()
        .unwrap_or_else(|e| panic!("could not run {ar}: {e}"));
    assert!(status.success(), "archiving {} failed", archive.display());
}

fn llvm_ar() -> PathBuf {
    let sysroot = Command::new(env::var("RUSTC").unwrap_or_else(|_| "rustc".into()))
        .arg("--print")
        .arg("sysroot")
        .output()
        .expect("could not ask rustc for its sysroot");
    let sysroot = String::from_utf8_lossy(&sysroot.stdout).trim().to_string();
    let host = env::var("HOST").expect("HOST is not set");
    PathBuf::from(sysroot)
        .join("lib/rustlib")
        .join(host)
        .join("bin/llvm-ar")
}
