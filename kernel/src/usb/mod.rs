//! USB host support — UHCI + HID boot keyboard/mouse.
//!
//! There is no full USB stack yet: just enough of UHCI to enumerate ports,
//! speak control transfers, and poll HID boot-protocol interrupt endpoints.
//! Events are pushed into [`crate::input`] so shell and GUI need no changes.
//! PS/2 remains the fallback when no UHCI controller or USB HID device is
//! present.

mod hid;
pub mod msc;
mod uhci;

use alloc::string::String;
use spin::Mutex;

pub use msc::UsbDisk;

static CONTROLLER: Mutex<Option<uhci::Uhci>> = Mutex::new(None);

/// Probe PCI for a UHCI controller and attach any HID boot devices on its ports.
pub fn init() {
    match uhci::Uhci::probe() {
        Some(mut ctrl) => {
            let n = ctrl.enumerate_ports();
            crate::ok_line(&alloc::format!(
                "USB: UHCI online ({} HID device{})",
                n,
                if n == 1 { "" } else { "s" }
            ));
            *CONTROLLER.lock() = Some(ctrl);
        }
        None => {
            crate::warn_line("USB: no UHCI controller found — PS/2 input only");
        }
    }
}

/// Poll interrupt endpoints. Call from the main loop (same cadence as `net::poll`).
pub fn poll() {
    if let Some(ctrl) = CONTROLLER.lock().as_mut() {
        ctrl.poll();
    }
    // Unconditional: typematic repeat is timer-driven, not report-driven —
    // see `hid::tick`'s doc comment for why it can't just live in the branch
    // above alongside fresh HID reports.
    hid::tick();
}

/// Short status string for the shell `usb` command.
pub fn status_line() -> String {
    match CONTROLLER.lock().as_ref() {
        Some(c) => c.status_line(),
        None => String::from("USB: no UHCI controller"),
    }
}

/// Is a mass-storage (USB drive) device currently attached?
pub fn has_msc() -> bool {
    CONTROLLER.lock().as_ref().is_some_and(|c| c.has_msc())
}

/// Consume the "a drive just appeared" flag set by enumeration/hotplug.
/// `fs::usb_tick` polls this once per main-loop tick to decide whether it is
/// worth attempting a mount, instead of remounting every tick.
pub fn take_new_msc() -> bool {
    CONTROLLER
        .lock()
        .as_mut()
        .is_some_and(|c| c.take_msc_just_attached())
}
