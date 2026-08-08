//! Tiny PC-speaker tones for the kids' games.
//!
//! Uses PIT channel 2 and the motherboard speaker gate on port `0x61` — the
//! same path QEMU and real PCs both honour. Tones are non-blocking: [`beep`]
//! starts the speaker and [`poll`] (called from the GUI tick) turns it off
//! when the note has finished, so a zap never freezes the desktop.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

const PIT_FREQ: u32 = 1_193_182;
const SPEAKER_PORT: u16 = 0x61;
const PIT_CH2_DATA: u16 = 0x42;
const PIT_CMD: u16 = 0x43;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static STOP_AT_US: AtomicU64 = AtomicU64::new(0);
/// Last programmed frequency, for self-test / debugging.
static LAST_FREQ: AtomicU32 = AtomicU32::new(0);

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

fn speaker_off() {
    unsafe {
        let v = inb(SPEAKER_PORT);
        outb(SPEAKER_PORT, v & !0x03);
    }
    ACTIVE.store(false, Ordering::Relaxed);
}

fn speaker_on(freq_hz: u32) {
    let freq = freq_hz.clamp(20, 14_000);
    let divisor = (PIT_FREQ / freq).max(1) as u16;
    unsafe {
        // Channel 2, lobyte/hibyte, mode 3 (square wave), binary.
        outb(PIT_CMD, 0xB6);
        outb(PIT_CH2_DATA, (divisor & 0xFF) as u8);
        outb(PIT_CH2_DATA, (divisor >> 8) as u8);
        let v = inb(SPEAKER_PORT);
        outb(SPEAKER_PORT, v | 0x03);
    }
    LAST_FREQ.store(freq, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
}

/// Start a tone at `freq_hz` for about `duration_ms` milliseconds.
///
/// A new beep replaces any tone still playing. Keep notes short (≤80 ms) so
/// they stay crisp and do not overlap the next game event.
pub fn beep(freq_hz: u32, duration_ms: u32) {
    let ms = duration_ms.clamp(1, 250);
    speaker_on(freq_hz);
    let stop = crate::clock::micros().wrapping_add(ms as u64 * 1_000);
    STOP_AT_US.store(stop, Ordering::Relaxed);
}

/// Silence the speaker once the current note's time is up. Call from the
/// GUI tick so tones stop even if nothing else is happening.
pub fn poll() {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let now = crate::clock::micros();
    let stop = STOP_AT_US.load(Ordering::Relaxed);
    if now.wrapping_sub(stop) < u64::MAX / 2 && now >= stop {
        speaker_off();
    }
}

// ── Cue sheet used by the kids' games ──────────────────────────────────

/// Soft UI / lane change / paddle touch.
pub fn blip() {
    beep(880, 35);
}

/// Laser / fire.
pub fn zap() {
    beep(1200, 40);
}

/// Brick or alien destroyed.
pub fn hit() {
    beep(660, 45);
}

/// Crash, life lost, wrong answer.
pub fn boom() {
    beep(180, 90);
}

/// Win / correct answer.
pub fn cheer() {
    beep(988, 50);
}

/// Speed up (higher pitch).
pub fn accelerate() {
    beep(1100, 30);
}

/// Slow down (lower pitch).
pub fn decelerate() {
    beep(440, 35);
}

/// Wrong letter / miss.
pub fn wrong() {
    beep(220, 70);
}
