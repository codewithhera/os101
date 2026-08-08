//! PS/2 keyboard driver — scancode set 1, US 104-key layout.
//!
//! Decodes raw scancodes into `pc_keyboard::DecodedKey` and pushes
//! them to the unified `input` core. Typematic auto-repeat is allowed to
//! pass through so that holding e.g. Backspace or an arrow key repeats
//! naturally.

use core::sync::atomic::{AtomicBool, Ordering};
use lazy_static::lazy_static;
use pc_keyboard::{layouts, HandleControl, KeyCode, KeyState, Keyboard, ScancodeSet1};
use spin::Mutex;

lazy_static! {
    static ref DECODER: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> = Mutex::new(
        Keyboard::new(ScancodeSet1::new(), layouts::Us104Key, HandleControl::Ignore)
    );
}

/// Whether either Ctrl key is currently held, for chords like Ctrl+C that
/// `HandleControl::Ignore` deliberately keeps out of the decoded key stream
/// (so Ctrl+C still decodes as plain `'c'`, not a control character, for
/// widgets that don't care about chords). Tracked from the *raw* key event
/// rather than `pc_keyboard`'s internal modifier state, since that state
/// isn't exposed publicly — Ctrl-down always decodes to a `RawKey` event, and
/// Ctrl-up doesn't decode to anything at all, so this has to look at the
/// event's `KeyState` directly instead of relying on `process_keyevent`.
static CTRL_HELD: AtomicBool = AtomicBool::new(false);

pub fn ctrl_held() -> bool {
    CTRL_HELD.load(Ordering::Relaxed)
}

/// Also called from the USB HID keyboard path, which reports modifier keys
/// as bits in every report rather than as discrete press/release events.
pub fn set_ctrl_held(held: bool) {
    CTRL_HELD.store(held, Ordering::Relaxed);
}

/// Called from the keyboard interrupt handler with a raw scancode byte.
pub fn handle_scancode(scancode: u8) {
    let mut kb = DECODER.lock();
    if let Ok(Some(event)) = kb.add_byte(scancode) {
        if matches!(event.code, KeyCode::LControl | KeyCode::RControl) {
            set_ctrl_held(event.state != KeyState::Up);
        }
        if let Some(key) = kb.process_keyevent(event) {
            crate::input::push(crate::input::InputEvent::Key(key));
        }
    }
}
