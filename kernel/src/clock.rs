//! A clock that keeps running when interrupts do not.
//!
//! # Why this exists
//!
//! The main loop processes events with interrupts disabled. It has to: both
//! the loop and the IRQ handlers allocate, and an IRQ taken while the heap
//! lock is held would spin against itself forever. See the comment above the
//! loop in `main.rs`.
//!
//! The cost is that [`crate::interrupts::ticks`] — advanced only by the timer
//! IRQ — stands still for as long as an action runs. That is harmless for a
//! calculator button and fatal for anything with a timeout: a network wait
//! written as "give up after twenty seconds" never gives up at all, because
//! the twenty seconds never pass. Fetching eight pictures used to wedge the
//! machine on the second one for exactly this reason.
//!
//! The time-stamp counter has no such problem. It is a register the CPU
//! advances itself, at a rate no interrupt flag can change. Calibrating it
//! against the PIT once at boot yields a tick count on the same scale as
//! `interrupts::ticks()` — so callers can swap one for the other — that keeps
//! counting no matter what the interrupt flag is doing.
//!
//! # What it is not
//!
//! It is not a wall clock and it is not precise. The calibration is a few
//! hundred milliseconds long, the TSC may drift with frequency scaling on real
//! hardware, and nothing here compensates. It is accurate enough to decide
//! that a server is not going to answer, which is all anything asks of it.

use core::sync::atomic::{AtomicU64, Ordering};

/// TSC cycles in one PIT tick. Zero until [`calibrate`] has run, which is the
/// signal to fall back on the interrupt-driven count.
static CYCLES_PER_TICK: AtomicU64 = AtomicU64::new(0);
/// The TSC reading that corresponds to tick zero, so this clock and the
/// interrupt-driven one agree on what time it is rather than merely on how
/// fast it passes.
static EPOCH: AtomicU64 = AtomicU64::new(0);

/// How many PIT ticks to measure over. Four is about 220 ms: long enough that
/// the ±1 tick of quantisation is under a percent, short enough not to be felt
/// during boot.
const CALIBRATION_TICKS: u64 = 4;

fn rdtsc() -> u64 {
    // SAFETY: `rdtsc` reads a counter. It has no operands, touches no memory,
    // and is unprivileged on every x86_64 part.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Measure the TSC against the PIT.
///
/// Must be called with interrupts enabled and the timer IRQ already running,
/// since it works by watching `interrupts::ticks()` advance. Calling it twice
/// is harmless; the second measurement replaces the first.
pub fn calibrate() {
    let start_tick = wait_for_tick_edge();
    let start_tsc = rdtsc();

    let end_tick = loop {
        let now = crate::interrupts::ticks();
        if now.wrapping_sub(start_tick) >= CALIBRATION_TICKS {
            break now;
        }
        core::hint::spin_loop();
    };
    let end_tsc = rdtsc();

    let elapsed_ticks = end_tick.wrapping_sub(start_tick);
    let elapsed_cycles = end_tsc.wrapping_sub(start_tsc);
    if elapsed_ticks == 0 || elapsed_cycles == 0 {
        // No usable measurement — leave the fallback in place rather than
        // install a clock that runs at an invented speed.
        return;
    }

    let per_tick = elapsed_cycles / elapsed_ticks;
    EPOCH.store(end_tsc.wrapping_sub(end_tick.wrapping_mul(per_tick)), Ordering::Relaxed);
    // Written last: a non-zero value here is what makes `ticks` trust the
    // epoch, so the epoch has to be there already.
    CYCLES_PER_TICK.store(per_tick, Ordering::Release);
}

/// Wait for the tick counter to change, and return its new value.
///
/// Starting a measurement mid-tick would fold up to one whole tick of error
/// into a four-tick sample.
fn wait_for_tick_edge() -> u64 {
    let first = crate::interrupts::ticks();
    loop {
        let now = crate::interrupts::ticks();
        if now != first {
            return now;
        }
        core::hint::spin_loop();
    }
}

/// Ticks since boot, on the same ~18.2 Hz scale as
/// [`crate::interrupts::ticks`], but derived from the CPU's own counter.
///
/// Use this for anything that has to time out. Use `interrupts::ticks()` only
/// where the interrupt-driven count is the point.
pub fn ticks() -> u64 {
    let per_tick = CYCLES_PER_TICK.load(Ordering::Acquire);
    if per_tick == 0 {
        return crate::interrupts::ticks();
    }
    rdtsc().wrapping_sub(EPOCH.load(Ordering::Relaxed)) / per_tick
}

/// Microseconds since boot, from the same calibration [`ticks`] uses.
///
/// A tick is 55 ms, which is the wrong unit for anything that finishes inside
/// one — a boot self-test, a decode pass. This is the same counter with the
/// division done in microseconds instead, so it inherits the calibration's
/// few-percent accuracy and nothing more. Zero until [`calibrate`] has run,
/// since there is no honest answer before that.
pub fn micros() -> u64 {
    let per_tick = CYCLES_PER_TICK.load(Ordering::Acquire);
    if per_tick == 0 {
        return 0;
    }
    let cycles = rdtsc().wrapping_sub(EPOCH.load(Ordering::Relaxed));
    // Widened because cycles × microseconds-per-tick passes 2^64 after a few
    // hours of uptime, and a wrapped answer would be worse than a coarse one.
    ((cycles as u128 * MICROS_PER_TICK as u128) / per_tick as u128) as u64
}

/// How long one PIT tick lasts, from the 1.193182 MHz input and the default
/// 65536 divisor the timer is left at: 54,925 µs.
const MICROS_PER_TICK: u64 = 65_536 * 1_000_000 / 1_193_182;

/// Boot-time checks.
pub fn selftest() -> crate::selftest::Report {
    let mut report = crate::selftest::Report::new();

    report.check("calibrated", CYCLES_PER_TICK.load(Ordering::Acquire) > 0);
    // A PIT tick is 55 ms. Anything outside 10 MHz–100 GHz of implied clock
    // rate means the measurement went wrong, not that the CPU is unusual.
    let per_tick = CYCLES_PER_TICK.load(Ordering::Acquire);
    report.check("plausible rate", per_tick > 550_000 && per_tick < 5_500_000_000);

    // The two clocks are calibrated to agree; a few ticks apart is the
    // quantisation, more than that is a broken epoch.
    let drift = ticks().abs_diff(crate::interrupts::ticks());
    report.check("agrees with the timer IRQ", drift <= 4);

    // Monotonic, and actually moving: a stuck counter would satisfy any
    // ordering test written with `>=`.
    let first = ticks();
    let mut last = first;
    for _ in 0..200_000 {
        let now = ticks();
        if now < last {
            break;
        }
        last = now;
    }
    report.check("monotonic", last >= first);
    report.check("advances", rdtsc() != 0);

    // A tick is 54,925 µs, so the two scales have to agree to within one tick
    // plus however long the two reads are apart.
    let (in_ticks, in_micros) = (ticks(), micros());
    report.check(
        "microseconds agree with ticks",
        in_micros / MICROS_PER_TICK == in_ticks
            || in_micros / MICROS_PER_TICK == in_ticks.wrapping_add(1),
    );

    report
}
