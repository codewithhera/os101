//! Host-side tooling for OS101.
//!
//! Two jobs:
//!   * `image` — wrap a kernel ELF into bootable disk images (BIOS and/or
//!     UEFI) and a hybrid ISO that USB writers can flash. A thin shim around
//!     `bootloader::DiskImageBuilder`.
//!   * `pack` / `inspect` — build and examine `.opk` application packages,
//!     the format the running OS installs apps from.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn usage(program: &str) {
    eprintln!("usage:");
    eprintln!("  {program} image <kernel-elf> <out-prefix>");
    eprintln!("      writes <out-prefix>-bios.img, <out-prefix>-uefi.img, and <out-prefix>.iso");
    eprintln!("  {program} pack  <binary-elf> <out.opk> [--name N] [--version V] [--description D] [--icon I]");
    eprintln!("  {program} inspect <file.opk>");
    eprintln!();
    eprintln!("  {program} <kernel-elf> <out-image>    (legacy: BIOS image + siblings)");
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let program = args.first().map(String::as_str).unwrap_or("os101-tools");

    match args.get(1).map(String::as_str) {
        Some("image") => match args.get(2..4) {
            Some([kernel, out]) => build_images(Path::new(kernel), Path::new(out)),
            _ => {
                usage(program);
                ExitCode::from(2)
            }
        },
        Some("pack") => pack(&args[2..], program),
        Some("inspect") => match args.get(2) {
            Some(path) => inspect(Path::new(path)),
            None => {
                usage(program);
                ExitCode::from(2)
            }
        },
        // Positional form kept working: run.sh and test.sh call it this way.
        Some(_) if args.len() == 3 => {
            let kernel = Path::new(&args[1]);
            let out = Path::new(&args[2]);
            build_images_from_legacy_path(kernel, out)
        }
        _ => {
            usage(program);
            ExitCode::from(2)
        }
    }
}

/// Legacy `os101-tools kernel.elf build/os101-bios.img` — keep writing that
/// exact path, plus `os101-uefi.img` and `os101.iso` beside it.
fn build_images_from_legacy_path(kernel: &Path, bios_out: &Path) -> ExitCode {
    let parent = bios_out.parent().unwrap_or_else(|| Path::new("."));
    let stem = bios_out
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("os101");
    let prefix_name = stem.strip_suffix("-bios").unwrap_or(stem);
    let prefix = parent.join(prefix_name);
    build_images(kernel, &prefix)
}

fn build_images(kernel: &Path, prefix: &Path) -> ExitCode {
    if !kernel.exists() {
        eprintln!("error: kernel ELF not found at {}", kernel.display());
        return ExitCode::from(1);
    }
    if let Some(parent) = prefix.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("error: cannot create {}: {e}", parent.display());
            return ExitCode::from(1);
        }
    }

    let bios_path = PathBuf::from(format!("{}-bios.img", prefix.display()));
    let uefi_path = PathBuf::from(format!("{}-uefi.img", prefix.display()));
    let iso_path = PathBuf::from(format!("{}.iso", prefix.display()));

    // The framebuffer size is deliberately not configured here. `BootConfig`'s
    // `minimum_framebuffer_*` fields are only read by the bootloader's UEFI
    // stage; the BIOS stage hardcodes a 1280x720 ceiling and skips any VESA
    // mode larger than that.
    let builder = bootloader::DiskImageBuilder::new(PathBuf::from(kernel));

    if let Err(e) = builder.create_bios_image(&bios_path) {
        eprintln!("error: failed to build BIOS disk image: {e}");
        return ExitCode::from(1);
    }
    println!("wrote {}", bios_path.display());

    if let Err(e) = builder.create_uefi_image(&uefi_path) {
        eprintln!("error: failed to build UEFI disk image: {e}");
        return ExitCode::from(1);
    }
    println!("wrote {}", uefi_path.display());

    // Hybrid install ISO: same bytes as the BIOS disk image. Rufus, Etcher,
    // and `dd` all write it to a USB stick as a bootable install medium.
    if let Err(e) = fs::copy(&bios_path, &iso_path) {
        eprintln!("error: failed to write ISO {}: {e}", iso_path.display());
        return ExitCode::from(1);
    }
    println!("wrote {} (hybrid USB/BIOS install medium)", iso_path.display());

    ExitCode::SUCCESS
}

fn pack(args: &[String], program: &str) -> ExitCode {
    let mut positional: Vec<&str> = Vec::new();
    let mut name: Option<String> = None;
    let mut version = String::from("1.0.0");
    let mut description = String::new();
    let mut icon = String::new();

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let mut take_value = |field: &mut String, label: &str| -> bool {
            match args.get(i + 1) {
                Some(v) => {
                    *field = v.clone();
                    i += 2;
                    true
                }
                None => {
                    eprintln!("error: {label} needs a value");
                    false
                }
            }
        };
        match arg {
            "--name" => {
                let mut v = String::new();
                if !take_value(&mut v, "--name") {
                    return ExitCode::from(2);
                }
                name = Some(v);
            }
            "--version" => {
                if !take_value(&mut version, "--version") {
                    return ExitCode::from(2);
                }
            }
            "--description" => {
                if !take_value(&mut description, "--description") {
                    return ExitCode::from(2);
                }
            }
            "--icon" => {
                if !take_value(&mut icon, "--icon") {
                    return ExitCode::from(2);
                }
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown option {other}");
                return ExitCode::from(2);
            }
            other => {
                positional.push(other);
                i += 1;
            }
        }
    }

    let [binary, out] = positional[..] else {
        usage(program);
        return ExitCode::from(2);
    };

    let payload = match std::fs::read(binary) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: reading {binary}: {e}");
            return ExitCode::from(1);
        }
    };

    let name = name.unwrap_or_else(|| {
        Path::new(binary)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| String::from("App"))
    });
    if !os101_package::is_valid_name(&name) {
        eprintln!("error: '{name}' is not a valid app name");
        eprintln!("       use letters, digits, spaces, '-' and '_' (max 32 chars)");
        eprintln!("       pass a different one with --name");
        return ExitCode::from(1);
    }

    let manifest = format!(
        "name = {name}\nversion = {version}\ndescription = {description}\nicon = {icon}\n"
    );
    let image = os101_package::build(&manifest, &payload);

    if let Err(e) = os101_package::parse(&image) {
        eprintln!("error: refusing to write an invalid package: {e}");
        return ExitCode::from(1);
    }

    if let Err(e) = std::fs::write(out, &image) {
        eprintln!("error: writing {out}: {e}");
        return ExitCode::from(1);
    }

    println!("wrote {out}");
    println!("  name:    {name}");
    println!("  version: {version}");
    println!("  payload: {} bytes", payload.len());
    println!("  total:   {} bytes", image.len());
    println!();
    println!(
        "Install it inside OS101 with:  pkg install /fat/{}",
        Path::new(out)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    );
    ExitCode::SUCCESS
}

fn inspect(path: &Path) -> ExitCode {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: reading {}: {e}", path.display());
            return ExitCode::from(1);
        }
    };
    match os101_package::parse(&bytes) {
        Ok(pkg) => {
            println!("{}", path.display());
            println!("  name:        {}", pkg.name);
            println!("  version:     {}", pkg.version);
            println!("  description: {}", pkg.description);
            println!("  icon:        {}", pkg.icon);
            println!("  payload:     {} bytes", pkg.payload.len());
            println!("  valid:       yes");
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("{}: invalid package: {e}", path.display());
            ExitCode::from(1)
        }
    }
}
