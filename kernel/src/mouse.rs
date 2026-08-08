//! PS/2 Mouse Driver.
//!
//! Communicates with the 8042 controller's auxiliary port to initialize the
//! mouse and parse 3-byte movement packets.

use core::sync::atomic::{AtomicI32, AtomicBool, AtomicU8, AtomicU64, Ordering};

// PS/2 Controller Ports
const PS2_DATA: u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const PS2_CMD: u16 = 0x64;

// Port helpers
unsafe fn outb(port: u16, val: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") val,
                     options(nomem, nostack, preserves_flags));
}

unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    core::arch::asm!("in al, dx", in("dx") port, out("al") val,
                     options(nomem, nostack, preserves_flags));
    val
}

fn wait_write() -> bool {
    let mut timeout = 100_000;
    while unsafe { inb(PS2_STATUS) } & 2 != 0 {
        if timeout == 0 { return false; }
        timeout -= 1;
    }
    true
}

fn wait_read() -> bool {
    let mut timeout = 100_000;
    while unsafe { inb(PS2_STATUS) } & 1 == 0 {
        if timeout == 0 { return false; }
        timeout -= 1;
    }
    true
}

fn mouse_write(data: u8) {
    if !wait_write() { return; }
    unsafe { outb(PS2_CMD, 0xD4); }
    if !wait_write() { return; }
    unsafe { outb(PS2_DATA, data); }
}

fn mouse_read() -> Option<u8> {
    if wait_read() {
        Some(unsafe { inb(PS2_DATA) })
    } else {
        None
    }
}

// ── Mouse state — atomics (no spin-lock in interrupt context) ───────────

static SCREEN_W: AtomicI32 = AtomicI32::new(1280);
static SCREEN_H: AtomicI32 = AtomicI32::new(720);

/// Fixed-point 1.0 for [`POINTER_SCALE`]. The scale carries 8 bits of fraction
/// so a ratio like 1.25 survives integer arithmetic.
pub const POINTER_SCALE_ONE: i32 = 256;

/// The resolution pointer speed is calibrated against. A mouse reports counts,
/// and one count moved one pixel, so every increase in resolution used to make
/// the pointer feel slower — the screen grew but the hand movement did not.
pub const POINTER_BASELINE_W: i32 = 1280;

/// How much quicker the pointer is than the one-count-one-pixel default, on
/// top of the resolution correction. This is the knob to turn if the pointer
/// feels wrong; 1.5 crosses the screen in two thirds of the hand movement.
///
/// The speed is deliberately a plain multiplier rather than the usual
/// threshold acceleration: `tools/qemu-runner/drive.py` aims at absolute
/// coordinates using relative packets, and can only do that if the mapping
/// from counts to pixels is linear.
pub const POINTER_SENSITIVITY: i32 = POINTER_SCALE_ONE * 3 / 2;

/// Speed multiplier applied to every relative delta, in [`POINTER_SCALE_ONE`]
/// units. Recomputed whenever the screen size changes.
static POINTER_SCALE: AtomicI32 = AtomicI32::new(POINTER_SCALE_ONE);

/// Sub-pixel movement left over from the last packet, kept so that scaling
/// never rounds slow movement away to nothing.
static REMAINDER_X: AtomicI32 = AtomicI32::new(0);
static REMAINDER_Y: AtomicI32 = AtomicI32::new(0);

static MOUSE_X: AtomicI32 = AtomicI32::new(0);
static MOUSE_Y: AtomicI32 = AtomicI32::new(0);
static BTN_LEFT: AtomicBool = AtomicBool::new(false);
static BTN_RIGHT: AtomicBool = AtomicBool::new(false);
static BTN_MIDDLE: AtomicBool = AtomicBool::new(false);
static LAST_LEFT_TICK: AtomicU64 = AtomicU64::new(0);

/// Current pointer position (updated from IRQ). Same role as Linux input core
/// `ABS_X` / `ABS_Y`: clients should read this at render time, not from a
/// stream of move events.
pub fn position() -> (usize, usize) {
    let x = MOUSE_X.load(Ordering::Relaxed).max(0) as usize;
    let y = MOUSE_Y.load(Ordering::Relaxed).max(0) as usize;
    (x, y)
}

// Packet assembly — only touched from IRQ12, single writer.
static mut PACKET: [u8; 4] = [0; 4];
static PACKET_INDEX: AtomicU8 = AtomicU8::new(0);
/// Set once the mouse has agreed to send four-byte packets with a wheel byte.
static HAS_WHEEL: AtomicBool = AtomicBool::new(false);

/// Set screen dimensions for coordinate clamping, and recalibrate the pointer
/// speed to match. The pointer is never scaled below 1:1.
pub fn set_screen_size(w: usize, h: usize) {
    SCREEN_W.store(w as i32, Ordering::Relaxed);
    SCREEN_H.store(h as i32, Ordering::Relaxed);

    POINTER_SCALE.store(pointer_scale_for(w as i32), Ordering::Relaxed);
}

/// Pixels of pointer travel per mouse count on a screen `w` pixels wide, in
/// [`POINTER_SCALE_ONE`] units.
pub fn pointer_scale_for(w: i32) -> i32 {
    let resolution = (w * POINTER_SCALE_ONE / POINTER_BASELINE_W).max(POINTER_SCALE_ONE);
    resolution * POINTER_SENSITIVITY / POINTER_SCALE_ONE
}

/// Turn one raw delta into pixels of movement, plus the sub-pixel remainder to
/// carry into the next packet.
///
/// Split out from the atomics so it can be checked at boot: an error here is a
/// pointer that drifts on its own or refuses to move at low speed, and neither
/// is visible by reading the code.
fn scale_delta(delta: i32, scale: i32, remainder: i32) -> (i32, i32) {
    if delta == 0 {
        return (0, remainder);
    }
    let total = delta * scale + remainder;
    // Truncating division keeps the remainder's sign matched to the movement,
    // so a reversal cannot leave a carry pushing the pointer the wrong way.
    let whole = total / POINTER_SCALE_ONE;
    (whole, total - whole * POINTER_SCALE_ONE)
}

/// Apply the current pointer speed to `delta`, updating the carried remainder.
fn scaled(delta: i32, remainder: &AtomicI32) -> i32 {
    let (whole, rest) = scale_delta(
        delta,
        POINTER_SCALE.load(Ordering::Relaxed),
        remainder.load(Ordering::Relaxed),
    );
    remainder.store(rest, Ordering::Relaxed);
    whole
}

/// Initialise the PS/2 mouse — simple sequence that works in QEMU.
pub fn init() {
    // Interrupts are already enabled by the time we get here. If we let IRQ1
    // fire while we're asking the controller for its config byte, the
    // keyboard handler will snatch the byte out of port 0x60 (it looks
    // identical to a scancode from outside) and our mouse_read() will time
    // out, returning 0. When we write 0 back we silently drop bit 6
    // (scancode set-2→set-1 translation), and every key the keyboard sends
    // is suddenly decoded wrong — 'a' looks like Enter, 's' looks like
    // LCtrl, etc. Mask interrupts for the whole PS/2 controller dance.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)); }

    // Flush any stale bytes left by the BIOS.
    flush_buffer();

    // Enable auxiliary port
    if !wait_write() {
        unsafe { core::arch::asm!("sti", options(nomem, nostack)); }
        return;
    }
    unsafe { outb(PS2_CMD, 0xA8); }

    // Read-modify-write the controller config byte.
    if !wait_write() {
        unsafe { core::arch::asm!("sti", options(nomem, nostack)); }
        return;
    }
    unsafe { outb(PS2_CMD, 0x20); } // Get config byte
    let mut config = mouse_read().unwrap_or(0);
    crate::serial_println!("MOUSE: config byte before = {:#04x}", config);
    config |= 0x43;    // Bit 0: keyboard IRQ. Bit 1: aux IRQ. Bit 6: set-2→set-1 translation.
    config &= !0x30;   // Bit 4: keyboard clock enable. Bit 5: aux clock enable.
    crate::serial_println!("MOUSE: config byte after  = {:#04x}", config);

    if !wait_write() {
        unsafe { core::arch::asm!("sti", options(nomem, nostack)); }
        return;
    }
    unsafe { outb(PS2_CMD, 0x60); } // Set config byte
    if !wait_write() {
        unsafe { core::arch::asm!("sti", options(nomem, nostack)); }
        return;
    }
    unsafe { outb(PS2_DATA, config); }

    // Mouse defaults and reporting. mouse_write uses 0xD4 + aux byte, so
    // these bytes don't accidentally go to the keyboard.
    mouse_write(0xF6);
    let _ = mouse_read();

    if enable_wheel() {
        HAS_WHEEL.store(true, Ordering::Relaxed);
        crate::serial_println!("MOUSE: scroll wheel enabled");
    }

    mouse_write(0xF4);
    let _ = mouse_read();

    unsafe { core::arch::asm!("sti", options(nomem, nostack)); }
}

/// Ask a plain PS/2 mouse to become an IntelliMouse, which reports a wheel.
///
/// The protocol has no feature negotiation: setting the sample rate to 200,
/// then 100, then 80 is a knock that a wheel mouse recognises, after which it
/// identifies itself as device 3 and starts sending four-byte packets. A
/// mouse that does not know the knock keeps its id of 0 and carries on with
/// three, so trying costs nothing.
fn enable_wheel() -> bool {
    for rate in [200u8, 100, 80] {
        mouse_write(0xF3);
        let _ = mouse_read();
        mouse_write(rate);
        let _ = mouse_read();
    }

    mouse_write(0xF2);
    let _ = mouse_read();
    matches!(mouse_read(), Some(0x03))
}

/// Drain any stale bytes from the PS/2 output buffer.
fn flush_buffer() {
    for _ in 0..32 {
        if unsafe { inb(PS2_STATUS) } & 0x01 == 0 {
            break;
        }
        let stale = unsafe { inb(PS2_DATA) };
        crate::serial_println!("MOUSE: flushed stale byte {:#04x}", stale);
    }
}

/// Called from IRQ12 handler.
pub fn handle_interrupt() {
    // Status bit 0: output buffer full. Bit 5: AUX (mouse) data.
    // Skip if the pending byte is keyboard data — otherwise we'd feed
    // scancodes into the mouse packet parser and go permanently out of sync.
    let status = unsafe { inb(PS2_STATUS) };
    if status & 0x21 != 0x21 {
        return;
    }
    let byte = unsafe { inb(PS2_DATA) };

    let last = if HAS_WHEEL.load(Ordering::Relaxed) { 3 } else { 2 };

    // SAFETY: PACKET is only written from this handler; x86 guarantees
    // IRQ12 does not re-enter itself.
    unsafe {
        let index = PACKET_INDEX.load(Ordering::Relaxed);
        match index {
            0 => {
                // Byte 0: bit 3 must always be set in a valid first byte.
                if byte & 0x08 != 0 {
                    PACKET[0] = byte;
                    PACKET_INDEX.store(1, Ordering::Relaxed);
                }
                // else: out of sync, wait for a valid first byte
            }
            i if i < last => {
                PACKET[i as usize] = byte;
                PACKET_INDEX.store(i + 1, Ordering::Relaxed);
            }
            i if i == last => {
                PACKET[i as usize] = byte;
                PACKET_INDEX.store(0, Ordering::Relaxed);
                let wheel = if last == 3 { PACKET[3] } else { 0 };
                parse_packet(PACKET[0], PACKET[1], PACKET[2], wheel);
            }
            _ => PACKET_INDEX.store(0, Ordering::Relaxed),
        }
    }
}

/// Movement is a 9-bit signed value: eight bits in the data byte and its sign
/// in the flags byte.
fn delta(flags: u8, sign_bit: u8, value: u8) -> i32 {
    if flags & sign_bit != 0 {
        value as i32 - 256
    } else {
        value as i32
    }
}

/// Wheel movement from the fourth byte of an IntelliMouse packet.
///
/// Only the low nibble is movement, as a 4-bit signed value; the upper bits
/// carry the fourth and fifth buttons on mice that have them.
fn wheel_delta(z: u8) -> i8 {
    ((z & 0x0F) as i8) << 4 >> 4
}

fn parse_packet(f: u8, x: u8, y: u8, z: u8) {
    let dx = scaled(delta(f, 0x10, x), &REMAINDER_X);
    let dy = scaled(delta(f, 0x20, y), &REMAINDER_Y);

    // Update position
    let max_x = SCREEN_W.load(Ordering::Relaxed) - 1;
    let max_y = SCREEN_H.load(Ordering::Relaxed) - 1;

    let old_x = MOUSE_X.load(Ordering::Relaxed);
    let old_y = MOUSE_Y.load(Ordering::Relaxed);
    let new_x = (old_x + dx).clamp(0, max_x);
    let new_y = (old_y - dy).clamp(0, max_y);
    MOUSE_X.store(new_x, Ordering::Relaxed);
    MOUSE_Y.store(new_y, Ordering::Relaxed);

    // Decode buttons
    let left = f & 0x01 != 0;
    let right = f & 0x02 != 0;
    let middle = f & 0x04 != 0;

    let prev_left = BTN_LEFT.swap(left, Ordering::Relaxed);
    let prev_right = BTN_RIGHT.swap(right, Ordering::Relaxed);
    let _prev_middle = BTN_MIDDLE.swap(middle, Ordering::Relaxed);

    let buttons_changed = left != prev_left || right != prev_right;

    // Do not enqueue MouseMove: position is authoritative in `position()`.
    // The main loop reads atomics once per frame (fbcon-style), avoiding IRQ→
    // queue backlog and allocator pressure.

    let wheel = wheel_delta(z);
    if wheel != 0 {
        crate::input::push(crate::input::InputEvent::MouseWheel { delta: wheel });
    }

    if buttons_changed {
        // Double-click detection
        let mut double_clicked = false;
        if left && !prev_left {
            let now = crate::clock::ticks();
            let last = LAST_LEFT_TICK.load(Ordering::Relaxed);
            if now.wrapping_sub(last) < 10 {
                double_clicked = true;
            }
            LAST_LEFT_TICK.store(now, Ordering::Relaxed);
        }
        crate::input::push(crate::input::InputEvent::MouseButton {
            left,
            right,
            double_clicked,
        });
    }
}

/// Apply a relative move / button change from a non-PS/2 source (USB HID).
///
/// HID boot-protocol Y is positive downward (screen space). PS/2 Y is the
/// opposite, so callers must not feed HID deltas through [`parse_packet`].
pub fn inject_relative(dx: i32, dy: i32, left: bool, right: bool, wheel: i8) {
    let dx = scaled(dx, &REMAINDER_X);
    let dy = scaled(dy, &REMAINDER_Y);

    let max_x = SCREEN_W.load(Ordering::Relaxed) - 1;
    let max_y = SCREEN_H.load(Ordering::Relaxed) - 1;
    let old_x = MOUSE_X.load(Ordering::Relaxed);
    let old_y = MOUSE_Y.load(Ordering::Relaxed);
    MOUSE_X.store((old_x + dx).clamp(0, max_x), Ordering::Relaxed);
    MOUSE_Y.store((old_y + dy).clamp(0, max_y), Ordering::Relaxed);

    let prev_left = BTN_LEFT.swap(left, Ordering::Relaxed);
    let prev_right = BTN_RIGHT.swap(right, Ordering::Relaxed);

    if wheel != 0 {
        crate::input::push(crate::input::InputEvent::MouseWheel { delta: wheel });
    }
    if left != prev_left || right != prev_right {
        let mut double_clicked = false;
        if left && !prev_left {
            let now = crate::clock::ticks();
            let last = LAST_LEFT_TICK.load(Ordering::Relaxed);
            if now.wrapping_sub(last) < 10 {
                double_clicked = true;
            }
            LAST_LEFT_TICK.store(now, Ordering::Relaxed);
        }
        crate::input::push(crate::input::InputEvent::MouseButton {
            left,
            right,
            double_clicked,
        });
    }
}

/// Packet decoding, checked at boot.
///
/// Both fields are signed in an unusual way — nine bits for movement, four for
/// the wheel — and a sign-extension mistake shows up as a pointer that drifts
/// or a page that scrolls the wrong way, neither of which is obvious from the
/// code.
pub fn selftest() -> crate::selftest::Report {
    let mut r = crate::selftest::Report::new();

    r.check("delta is positive without the sign bit", delta(0x08, 0x10, 5) == 5);
    r.check("delta is negative with it", delta(0x18, 0x10, 0xFB) == -5);
    r.check("delta reaches the full range", delta(0x18, 0x10, 0x00) == -256);
    r.check("x and y use their own sign bits", delta(0x20, 0x10, 7) == 7);

    r.check("no wheel movement", wheel_delta(0x00) == 0);
    r.check("wheel down is positive", wheel_delta(0x01) == 1);
    r.check("wheel up is negative", wheel_delta(0x0F) == -1);
    r.check("side buttons are not movement", wheel_delta(0x30) == 0);
    r.check("side buttons do not mask movement", wheel_delta(0x3F) == -1);

    // Pointer speed. `scale_delta` runs on every packet from the IRQ, so a
    // remainder that leaks or a sign that flips shows up as a pointer that
    // creeps across the screen on its own.
    const ONE: i32 = POINTER_SCALE_ONE;
    r.check("1:1 scaling moves a pixel per count", scale_delta(1, ONE, 0) == (1, 0));
    r.check("a still mouse stays still", scale_delta(0, 2 * ONE, 0) == (0, 0));
    r.check("no movement keeps the carry", scale_delta(0, ONE, 7) == (0, 7));
    r.check("a wider screen moves further", scale_delta(2, ONE + ONE / 4, 0) == (2, ONE / 2));
    r.check(
        "the carry completes the next pixel",
        scale_delta(2, ONE + ONE / 4, ONE / 2) == (3, 0),
    );
    r.check("negative movement stays negative", scale_delta(-1, ONE, 0) == (-1, 0));
    r.check(
        "a slow drag is not rounded away",
        scale_delta(1, ONE / 2, 0) == (0, ONE / 2) && scale_delta(1, ONE / 2, ONE / 2) == (1, 0),
    );

    // The mapping has to stay linear, or `drive.py` cannot aim a relative
    // device at an absolute coordinate. Eight slow counts must land exactly
    // where one fast packet of eight would, carry included.
    let scale = pointer_scale_for(1920);
    let mut carried = 0;
    let mut crept = 0;
    for _ in 0..8 {
        let (pixels, rest) = scale_delta(1, scale, carried);
        crept += pixels;
        carried = rest;
    }
    let (swept, _) = scale_delta(8, scale, 0);
    r.check("speed does not depend on how fast the mouse moves", crept == swept);
    r.check("the baseline width is left at 1:1 speed", pointer_scale_for(POINTER_BASELINE_W) == POINTER_SENSITIVITY);
    r.check("a narrower screen is never slowed down", pointer_scale_for(640) == POINTER_SENSITIVITY);
    r.check("a wider screen is quicker", pointer_scale_for(1920) > pointer_scale_for(1280));

    r
}
