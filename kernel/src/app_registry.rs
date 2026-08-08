//! Plug-and-play application registry.
//!
//! Populated at compile time by `kernel/build.rs`, which scans
//! `../applications/*/manifest.txt`. The generated file at
//! `$OUT_DIR/app_registry_generated.rs` defines `AppKind`, `AppEntry` and
//! the `APPS` slice. This file just includes it so the rest of the kernel
//! can talk to it as `crate::app_registry::APPS`.

include!(concat!(env!("OUT_DIR"), "/app_registry_generated.rs"));
