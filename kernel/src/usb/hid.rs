//! HID boot-protocol keyboard and mouse report parsers.
//!
//! Boot protocol is fixed-size and layout-stable (8-byte keyboard, 3+ byte
//! mouse), so we avoid a full HID report descriptor parser.

use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use pc_keyboard::{DecodedKey, KeyCode};

/// Modifier bits in boot keyboard report byte 0.
const MOD_LCTRL: u8 = 1 << 0;
const MOD_LSHIFT: u8 = 1 << 1;
const MOD_LALT: u8 = 1 << 2;
const MOD_LGUI: u8 = 1 << 3;
const MOD_RCTRL: u8 = 1 << 4;
const MOD_RSHIFT: u8 = 1 << 5;
const MOD_RALT: u8 = 1 << 6;

/// Typematic repeat for the USB boot-protocol keyboard.
///
/// Unlike PS/2 (see `keyboard.rs`), a HID keyboard does not resend a distinct
/// "repeat" scancode on its own, and — unlike a fresh poll of an unchanged
/// analog value — QEMU's emulated interrupt endpoint only completes a
/// transfer when the report's *contents* actually change (a real NAK-until-
/// there's-something-new device), so `handle_keyboard_report` simply never
/// runs again while a key is held steady. Repeat can't be driven by new
/// reports arriving, then: [`tick`] is called unconditionally from the main
/// loop's USB poll (whether or not a transfer completed) and fires off of a
/// wall-clock timer armed by the most recent key press instead. Only one key
/// repeats at a time, same as real typematic hardware.
const REPEAT_DELAY_US: u64 = 500_000;
const REPEAT_INTERVAL_US: u64 = 40_000;

static REPEAT_CODE: AtomicU8 = AtomicU8::new(0);
static REPEAT_MODS: AtomicU8 = AtomicU8::new(0);
static REPEAT_AT_US: AtomicU64 = AtomicU64::new(0);

/// Diff `prev` → `report` and push newly pressed keys into the input queue,
/// arming (or disarming) the repeat timer that [`tick`] drives.
pub fn handle_keyboard_report(prev: &[u8; 8], report: &[u8; 8]) {
    let mods = report[0];
    crate::keyboard::set_ctrl_held(mods & (MOD_LCTRL | MOD_RCTRL) != 0);
    let now = crate::clock::micros();

    for &code in &report[2..] {
        if code == 0 || code == 1 /* Rollover */ {
            continue;
        }
        if prev[2..].contains(&code) {
            continue; // Already accounted for; shouldn't normally happen (see `tick`'s doc).
        }
        if let Some(key) = hid_usage_to_key(code, mods) {
            crate::input::push(crate::input::InputEvent::Key(key));
        }
        REPEAT_CODE.store(code, Ordering::Relaxed);
        REPEAT_MODS.store(mods, Ordering::Relaxed);
        REPEAT_AT_US.store(now + REPEAT_DELAY_US, Ordering::Relaxed);
    }

    let repeating = REPEAT_CODE.load(Ordering::Relaxed);
    if repeating != 0 && !report[2..].contains(&repeating) {
        // The key that was repeating has been released.
        REPEAT_CODE.store(0, Ordering::Relaxed);
    }
}

/// Fires the next queued typematic repeat, if one is armed and due. Must be
/// called on every USB poll regardless of whether a report was just
/// received — see [`REPEAT_CODE`]'s module doc for why that matters.
pub fn tick() {
    let code = REPEAT_CODE.load(Ordering::Relaxed);
    if code == 0 {
        return;
    }
    let now = crate::clock::micros();
    if now < REPEAT_AT_US.load(Ordering::Relaxed) {
        return;
    }
    let mods = REPEAT_MODS.load(Ordering::Relaxed);
    if let Some(key) = hid_usage_to_key(code, mods) {
        crate::input::push(crate::input::InputEvent::Key(key));
    }
    REPEAT_AT_US.store(now + REPEAT_INTERVAL_US, Ordering::Relaxed);
}

/// Parse a boot mouse report (3 bytes minimum; 4th is optional wheel).
pub fn handle_mouse_report(report: &[u8]) {
    if report.len() < 3 {
        return;
    }
    let buttons = report[0];
    let dx = report[1] as i8 as i32;
    let dy = report[2] as i8 as i32;
    let wheel = if report.len() >= 4 {
        report[3] as i8
    } else {
        0
    };
    let left = buttons & 1 != 0;
    let right = buttons & 2 != 0;
    crate::mouse::inject_relative(dx, dy, left, right, wheel);
}

fn shift(mods: u8) -> bool {
    mods & (MOD_LSHIFT | MOD_RSHIFT) != 0
}

fn hid_usage_to_key(usage: u8, mods: u8) -> Option<DecodedKey> {
    let sh = shift(mods);
    // Letters A–Z (usage 0x04..=0x1D)
    if (0x04..=0x1D).contains(&usage) {
        let base = b'a' + (usage - 0x04);
        let ch = if sh {
            (base as char).to_ascii_uppercase()
        } else {
            base as char
        };
        return Some(DecodedKey::Unicode(ch));
    }
    // Digits and symbols on the number row
    match usage {
        0x1E => Some(DecodedKey::Unicode(if sh { '!' } else { '1' })),
        0x1F => Some(DecodedKey::Unicode(if sh { '@' } else { '2' })),
        0x20 => Some(DecodedKey::Unicode(if sh { '#' } else { '3' })),
        0x21 => Some(DecodedKey::Unicode(if sh { '$' } else { '4' })),
        0x22 => Some(DecodedKey::Unicode(if sh { '%' } else { '5' })),
        0x23 => Some(DecodedKey::Unicode(if sh { '^' } else { '6' })),
        0x24 => Some(DecodedKey::Unicode(if sh { '&' } else { '7' })),
        0x25 => Some(DecodedKey::Unicode(if sh { '*' } else { '8' })),
        0x26 => Some(DecodedKey::Unicode(if sh { '(' } else { '9' })),
        0x27 => Some(DecodedKey::Unicode(if sh { ')' } else { '0' })),
        0x28 => Some(DecodedKey::Unicode('\n')),
        0x29 => Some(DecodedKey::RawKey(KeyCode::Escape)),
        0x2A => Some(DecodedKey::Unicode('\u{8}')), // Backspace
        0x2B => Some(DecodedKey::Unicode('\t')),
        0x2C => Some(DecodedKey::Unicode(' ')),
        0x2D => Some(DecodedKey::Unicode(if sh { '_' } else { '-' })),
        0x2E => Some(DecodedKey::Unicode(if sh { '+' } else { '=' })),
        0x2F => Some(DecodedKey::Unicode(if sh { '{' } else { '[' })),
        0x30 => Some(DecodedKey::Unicode(if sh { '}' } else { ']' })),
        0x31 => Some(DecodedKey::Unicode(if sh { '|' } else { '\\' })),
        0x33 => Some(DecodedKey::Unicode(if sh { ':' } else { ';' })),
        0x34 => Some(DecodedKey::Unicode(if sh { '"' } else { '\'' })),
        0x35 => Some(DecodedKey::Unicode(if sh { '~' } else { '`' })),
        0x36 => Some(DecodedKey::Unicode(if sh { '<' } else { ',' })),
        0x37 => Some(DecodedKey::Unicode(if sh { '>' } else { '.' })),
        0x38 => Some(DecodedKey::Unicode(if sh { '?' } else { '/' })),
        0x3A => Some(DecodedKey::RawKey(KeyCode::F1)),
        0x3B => Some(DecodedKey::RawKey(KeyCode::F2)),
        0x3C => Some(DecodedKey::RawKey(KeyCode::F3)),
        0x3D => Some(DecodedKey::RawKey(KeyCode::F4)),
        0x3E => Some(DecodedKey::RawKey(KeyCode::F5)),
        0x3F => Some(DecodedKey::RawKey(KeyCode::F6)),
        0x40 => Some(DecodedKey::RawKey(KeyCode::F7)),
        0x41 => Some(DecodedKey::RawKey(KeyCode::F8)),
        0x42 => Some(DecodedKey::RawKey(KeyCode::F9)),
        0x43 => Some(DecodedKey::RawKey(KeyCode::F10)),
        0x44 => Some(DecodedKey::RawKey(KeyCode::F11)),
        0x45 => Some(DecodedKey::RawKey(KeyCode::F12)),
        0x4F => Some(DecodedKey::RawKey(KeyCode::ArrowRight)),
        0x50 => Some(DecodedKey::RawKey(KeyCode::ArrowLeft)),
        0x51 => Some(DecodedKey::RawKey(KeyCode::ArrowDown)),
        0x52 => Some(DecodedKey::RawKey(KeyCode::ArrowUp)),
        0x4C => Some(DecodedKey::RawKey(KeyCode::Delete)),
        0x4A => Some(DecodedKey::RawKey(KeyCode::Home)),
        0x4D => Some(DecodedKey::RawKey(KeyCode::End)),
        0x4B => Some(DecodedKey::RawKey(KeyCode::PageUp)),
        0x4E => Some(DecodedKey::RawKey(KeyCode::PageDown)),
        // Modifier-only presses produce no character; Ctrl/Alt chords ignored for now.
        _ if usage == 0xE0
            || usage == 0xE1
            || usage == 0xE2
            || usage == 0xE3
            || usage == 0xE4
            || usage == 0xE5
            || usage == 0xE6
            || usage == 0xE7 =>
        {
            let _ = (MOD_LCTRL, MOD_LALT, MOD_LGUI, MOD_RCTRL, MOD_RALT);
            None
        }
        _ => None,
    }
}
