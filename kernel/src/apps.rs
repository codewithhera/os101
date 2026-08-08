//! Runtime application registry — the installed-apps database.
//!
//! `app_registry::APPS` is baked in at compile time by `build.rs`, so adding
//! an app used to mean rebuilding the kernel. This module keeps a mutable
//! registry instead: built-ins are seeded from the static table at boot, then
//! `/apps/*.opk` is scanned and every valid package is registered too. From
//! that point on the launcher, the desktop and `pkg` all read from here, and
//! installing an app makes it appear without a reboot.
//!
//! Installed packages live in the `/apps` RAM layer, so they currently
//! survive until power-off rather than across boots. Persisting them is a
//! disk-driver problem, not a registry one: when a real block device backs
//! `/apps`, this code does not change.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

use crate::package::{self, Package};
use crate::window::WindowKind;

/// Where installed packages are stored in the VFS.
pub const APPS_DIR: &str = "/apps";

/// Cap on concurrently installed packages, so a loop that installs in a
/// retry never exhausts the heap.
const MAX_INSTALLED: usize = 64;

#[derive(Clone)]
pub enum AppSource {
    /// Implemented in the kernel; launching opens a built-in window.
    Builtin(WindowKind),
    /// ELF compiled into the kernel image by `build.rs`.
    Static(&'static [u8]),
    /// ELF from an installed `.opk`.
    Installed(Vec<u8>),
}

#[derive(Clone)]
pub struct App {
    pub name: String,
    pub version: String,
    pub description: String,
    pub source: AppSource,
    /// Path of the backing package, for installed apps.
    pub path: Option<String>,
}

impl App {
    /// Only packages can be uninstalled; built-ins are part of the kernel.
    pub fn removable(&self) -> bool {
        self.path.is_some()
    }

    pub fn kind_label(&self) -> &'static str {
        match self.source {
            AppSource::Builtin(_) => "built-in",
            AppSource::Static(_) => "bundled",
            AppSource::Installed(_) => "installed",
        }
    }
}

static REGISTRY: Mutex<Vec<App>> = Mutex::new(Vec::new());

/// Seed the registry from the compile-time table, then pick up anything
/// already sitting in `/apps`.
pub fn init() {
    {
        let mut reg = REGISTRY.lock();
        reg.clear();
        for entry in crate::app_registry::APPS {
            let source = match entry.kind {
                crate::app_registry::AppKind::Builtin(wk) => AppSource::Builtin(wk),
                crate::app_registry::AppKind::Elf(bytes) => AppSource::Static(bytes),
            };
            reg.push(App {
                name: entry.name.to_string(),
                version: String::from("1.0.0"),
                description: String::new(),
                source,
                path: None,
            });
        }
    }
    let found = scan_apps_dir();
    crate::ok_line(&alloc::format!(
        "App registry: {} app(s), {} installed from {}",
        count(),
        found,
        APPS_DIR
    ));
}

/// Register every valid `.opk` under `/apps`. Invalid packages are reported
/// and skipped rather than aborting the scan — one bad file should not stop
/// the rest of the system from booting.
fn scan_apps_dir() -> usize {
    let Ok(entries) = crate::fs::cmd_ls(Some(APPS_DIR)) else {
        return 0;
    };
    let mut installed = 0;
    for path in entries {
        if !path.ends_with(".opk") {
            continue;
        }
        let Ok(bytes) = crate::fs::cmd_cat(&path) else {
            continue;
        };
        match package::parse(&bytes) {
            Ok(pkg) => {
                if register(pkg, Some(path.clone())).is_ok() {
                    installed += 1;
                }
            }
            Err(e) => {
                crate::warn_line(&alloc::format!("skipping {}: {}", path, e));
            }
        }
    }
    installed
}

pub fn count() -> usize {
    REGISTRY.lock().len()
}

/// Snapshot of the registry for UI rendering.
pub fn list() -> Vec<App> {
    REGISTRY.lock().clone()
}

/// Look an app up by display name, or by its slug so that names with spaces
/// stay typeable at the shell (`pkg run hello-elf` finds "Hello ELF").
pub fn find(name: &str) -> Option<App> {
    let reg = REGISTRY.lock();
    if let Some(app) = reg.iter().find(|a| a.name.eq_ignore_ascii_case(name)) {
        return Some(app.clone());
    }
    let slug = crate::package::slugify(name);
    reg.iter()
        .find(|a| crate::package::slugify(&a.name) == slug)
        .cloned()
}

/// Add a parsed package to the registry, replacing any app of the same name.
fn register(pkg: Package, path: Option<String>) -> Result<(), &'static str> {
    let mut reg = REGISTRY.lock();
    if reg.len() >= MAX_INSTALLED {
        return Err("too many installed applications");
    }
    let app = App {
        name: pkg.name,
        version: pkg.version,
        description: pkg.description,
        source: AppSource::Installed(pkg.payload),
        path,
    };
    // Upgrading in place keeps launcher indices stable for everything else.
    if let Some(slot) = reg.iter_mut().find(|a| a.name.eq_ignore_ascii_case(&app.name)) {
        if !slot.removable() {
            return Err("an app with that name is built in");
        }
        *slot = app;
    } else {
        reg.push(app);
    }
    Ok(())
}

/// Install a package that is already in the filesystem.
///
/// Reads it, validates it, copies it into `/apps` and registers it. Returns
/// the installed app's name.
pub fn install_from_path(path: &str) -> Result<String, &'static str> {
    let bytes = crate::fs::cmd_cat(path)?;
    install_bytes(&bytes)
}

/// Install from an in-memory package image.
pub fn install_bytes(bytes: &[u8]) -> Result<String, &'static str> {
    let pkg = package::parse(bytes)?;
    let name = pkg.name.clone();

    // Store the package before registering it, so a write failure does not
    // leave a registered app with no backing file.
    let dest = alloc::format!("{}/{}.opk", APPS_DIR, package::slugify(&name));
    crate::fs::cmd_mkdir(APPS_DIR)?;
    crate::fs::cmd_write_file(&dest, bytes.to_vec())?;

    if let Err(e) = register(pkg, Some(dest.clone())) {
        // Roll back the copy so a rejected install leaves nothing behind.
        let _ = crate::fs::cmd_remove(&dest);
        return Err(e);
    }
    crate::window::request_redraw();
    Ok(name)
}

/// Remove an installed app and delete its package.
pub fn uninstall(name: &str) -> Result<(), &'static str> {
    let path = {
        let mut reg = REGISTRY.lock();
        let Some(idx) = reg.iter().position(|a| a.name.eq_ignore_ascii_case(name)) else {
            return Err("no such application");
        };
        if !reg[idx].removable() {
            return Err("built-in applications cannot be removed");
        }
        let app = reg.remove(idx);
        app.path
    };
    if let Some(p) = path {
        let _ = crate::fs::cmd_remove(&p);
    }
    crate::window::request_redraw();
    Ok(())
}

/// Launch the app at `index` in the registry.
pub fn launch(index: usize) -> Result<(), &'static str> {
    // Clone out of the lock first: spawning touches the window manager and
    // the process tables, and holding the registry across that invites a
    // lock-order deadlock.
    let app = {
        let reg = REGISTRY.lock();
        reg.get(index).cloned().ok_or("no such application")?
    };
    launch_app(&app)
}

pub fn launch_by_name(name: &str) -> Result<(), &'static str> {
    let app = find(name).ok_or("no such application")?;
    launch_app(&app)
}

fn launch_app(app: &App) -> Result<(), &'static str> {
    match &app.source {
        AppSource::Builtin(kind) => {
            crate::window::launch_builtin(*kind);
            Ok(())
        }
        AppSource::Static(bytes) => crate::window::launch_elf_app(&app.name, bytes),
        AppSource::Installed(bytes) => crate::window::launch_elf_app(&app.name, bytes),
    }
}
