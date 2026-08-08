//! The CMOS real-time clock — the only wall clock this machine has.
//!
//! [`crate::clock`] and [`crate::interrupts`] both count how long the kernel
//! has been running, which is enough to give up on a socket and no use at all
//! for telling a script what day it is. The battery-backed clock in the
//! chipset is the other half: it knows the date, it survives a reboot, and it
//! is reached through two I/O ports.
//!
//! # Reading it
//!
//! Port 0x70 selects a register and port 0x71 carries its value. Nothing is
//! latched, so a read that lands in the middle of the chip's own once-a-second
//! update can come back with fields from either side of it — 11:59:60 on its
//! way to 12:00:00, or a minute that has rolled over next to an hour that has
//! not. Two defences, both standard: wait for the update-in-progress flag in
//! status register A to clear, then accept a reading only once two consecutive
//! ones agree.
//!
//! Neither the number base nor the hour format is fixed. Status register B
//! says whether the fields are BCD — which is what QEMU reports — or plain
//! binary, and whether hours run 0-23 or 1-12 with a flag in the top bit.
//! Both are decoded rather than assumed, and every field is clamped on the way
//! out, because a chip with a dead battery will happily report month 0 or hour
//! 99 and nothing downstream should have to survive that.
//!
//! # The cache
//!
//! A script calling `Date.now()` in a loop must not reach the ports on every
//! call: a read waits out the update flag and runs with interrupts masked. So
//! the hardware is read once, together with the value of [`crate::clock::ticks`]
//! at that moment, and every later answer is that reading plus the ticks
//! since — a wall clock laid on top of the monotonic one. After
//! [`RESYNC_TICKS`] the pair is taken again, which bounds the error from the
//! calibrated tick rate to a minute's worth of drift.
//!
//! Re-synchronising never moves the clock backwards. The chip only has
//! one-second resolution, so a fresh reading can legitimately land a little
//! behind what this module has already reported, and a clock that goes
//! backwards turns every script measuring a duration into one computing a
//! negative duration.
//!
//! # Time zones
//!
//! Everything here is UTC. There is no zone database, nothing to configure an
//! offset with, and no `Date` method that would read one. QEMU sets the
//! emulated chip from the host clock in UTC, which is the case that matters;
//! a real machine holding local time in CMOS will read as an offset UTC and
//! there is nothing here that could know it.

use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

/// Register select. Written before every data-port access.
const CMOS_INDEX: u16 = 0x70;
/// The value of whichever register was last selected.
const CMOS_DATA: u16 = 0x71;

const REG_SECOND: u8 = 0x00;
const REG_MINUTE: u8 = 0x02;
const REG_HOUR: u8 = 0x04;
const REG_DAY: u8 = 0x07;
const REG_MONTH: u8 = 0x08;
const REG_YEAR: u8 = 0x09;
const REG_STATUS_A: u8 = 0x0A;
const REG_STATUS_B: u8 = 0x0B;

/// Status A: the chip is part-way through its once-a-second update, and the
/// time registers are not to be trusted.
const STATUS_A_UPDATE_IN_PROGRESS: u8 = 0x80;
/// Status B: hours run 0-23, rather than 1-12 with a PM flag.
const STATUS_B_24_HOUR: u8 = 0x02;
/// Status B: the fields are plain binary, rather than BCD.
const STATUS_B_BINARY: u8 = 0x04;
/// The PM flag, in the top bit of the hour register in 12-hour mode.
const HOUR_PM: u8 = 0x80;

/// Two-digit years below this belong to the twenty-first century.
///
/// This is a guess. The century register at 0x32 is not part of any standard:
/// it is absent on some chipsets and holds whatever the firmware felt like on
/// others, so reading it would trade a predictable guess for an
/// unpredictable one. The guess is acceptable because the only thing it can
/// get wrong is a hundred years, nothing here does arithmetic that a century
/// would break, and the window covers every year the machine can plausibly be
/// run in. A clock that has lost its battery reports 70 and reads as 1970,
/// which is exactly the answer an epoch-based clock should give for "no idea".
const CENTURY_PIVOT: u8 = 70;

/// The PIT's input frequency and the divisor it runs at.
///
/// [`crate::interrupts`] never reprogrammes the timer, so the divisor is its
/// power-on 65536 and a tick is 65536/1193182 s — the ~18.2 Hz that
/// [`crate::clock`] documents. Milliseconds are derived from these two rather
/// than from a rounded 18 ticks per second, which would put a minute of
/// elapsed ticks about 700 ms out all by itself.
const PIT_INPUT_HZ: u64 = 1_193_182;
const PIT_DIVISOR: u64 = 65_536;

/// How long a reading is trusted before the ports are read again: one minute.
const RESYNC_TICKS: u64 = 60 * PIT_INPUT_HZ / PIT_DIVISOR;

/// How many readings to take before settling for the last one. Two that agree
/// end it early; a chip whose registers never settle is broken, and a handful
/// of tries is enough to establish that without spinning forever with
/// interrupts off.
const MAX_READINGS: usize = 8;

/// How long to wait for the update-in-progress flag to clear. An update lasts
/// under two milliseconds, so this bound is very generous; its only job is to
/// make sure a missing or wedged clock cannot hang the boot.
const UPDATE_SPIN_LIMIT: u32 = 500_000;

/// A field wider than this is nonsense however you read it, and clamping to it
/// is what keeps every multiplication below inside an `i64` without a single
/// overflow check: the largest reachable day count is about 4e10, and a day is
/// 8.64e7 ms.
const FIELD_LIMIT: i64 = 100_000_000;

const MILLIS_PER_SECOND: i64 = 1_000;
const MILLIS_PER_MINUTE: i64 = 60 * MILLIS_PER_SECOND;
const MILLIS_PER_HOUR: i64 = 60 * MILLIS_PER_MINUTE;
const MILLIS_PER_DAY: i64 = 24 * MILLIS_PER_HOUR;

/// Broken-down UTC time from the CMOS clock.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// Epoch milliseconds at the last synchronisation, and the tick count they
/// were taken at. Both are meaningless until `SYNCED` is set.
static BASE_MILLIS: AtomicI64 = AtomicI64::new(0);
static BASE_TICKS: AtomicU64 = AtomicU64::new(0);
static SYNCED: AtomicBool = AtomicBool::new(false);

// ── The public clock ────────────────────────────────────────────────────────

/// The current UTC date and time.
pub fn now() -> DateTime {
    from_unix_millis(unix_millis())
}

/// Milliseconds since the Unix epoch, which is what JavaScript wants.
pub fn unix_millis() -> i64 {
    let ticks = crate::clock::ticks();
    // A tick count below the base reads as an enormous elapsed time here,
    // which forces a re-synchronisation rather than a wild answer.
    if !SYNCED.load(Ordering::Acquire)
        || ticks.wrapping_sub(BASE_TICKS.load(Ordering::Relaxed)) >= RESYNC_TICKS
    {
        return resync(ticks);
    }
    derived(ticks)
}

/// `HH:MM` for the taskbar.
pub fn clock_text() -> String {
    let now = now();
    alloc::format!("{:02}:{:02}", now.hour, now.minute)
}

/// The day of the week for a moment, counting 0 for Sunday as JavaScript does.
pub fn weekday(millis: i64) -> u8 {
    // 1970-01-01 was a Thursday, the fourth day of the week.
    (millis.div_euclid(MILLIS_PER_DAY) + 4).rem_euclid(7) as u8
}

/// Break epoch milliseconds down into a UTC date and time.
pub fn from_unix_millis(millis: i64) -> DateTime {
    // Euclidean division so that the time of day before the epoch counts
    // forwards from midnight rather than backwards from it.
    let days = millis.div_euclid(MILLIS_PER_DAY);
    let time_of_day = millis.rem_euclid(MILLIS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    DateTime {
        // Only a timestamp tens of thousands of years out lands outside a
        // `u16`, and there is nothing more useful to say about one that does.
        year: year.clamp(0, u16::MAX as i64) as u16,
        month: month as u8,
        day: day as u8,
        hour: (time_of_day / MILLIS_PER_HOUR) as u8,
        minute: (time_of_day / MILLIS_PER_MINUTE % 60) as u8,
        second: (time_of_day / MILLIS_PER_SECOND % 60) as u8,
    }
}

/// Epoch milliseconds for a UTC date and time whose fields need not be in
/// range: month 13 means January of the next year and day 32 the first of the
/// next month, which is both what JavaScript's `new Date(y, m, d)` does and
/// what makes the function usable for date arithmetic.
pub fn unix_millis_from_parts(
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    millis: i64,
) -> i64 {
    // Only the month needs normalising, since [`days_from_civil`] is defined
    // for 1-12 alone; the day and everything below it are plain offsets that
    // carry themselves.
    let months = clamped(year) * 12 + clamped(month) - 1;
    let days = days_from_civil(months.div_euclid(12), months.rem_euclid(12) + 1, 1)
        + clamped(day)
        - 1;
    days * MILLIS_PER_DAY
        + clamped(hour) * MILLIS_PER_HOUR
        + clamped(minute) * MILLIS_PER_MINUTE
        + clamped(second) * MILLIS_PER_SECOND
        + clamped(millis)
}

/// A field cut down to something the arithmetic above can hold.
fn clamped(value: i64) -> i64 {
    value.clamp(-FIELD_LIMIT, FIELD_LIMIT)
}

// ── The cache ───────────────────────────────────────────────────────────────

/// The time already committed to, carried forward to `ticks`.
fn derived(ticks: u64) -> i64 {
    let elapsed = ticks.wrapping_sub(BASE_TICKS.load(Ordering::Relaxed));
    BASE_MILLIS
        .load(Ordering::Relaxed)
        .saturating_add(ticks_to_millis(elapsed))
}

/// Elapsed ticks as milliseconds.
fn ticks_to_millis(ticks: u64) -> i64 {
    // The multiply overflows only past 2.8e11 ticks, which is 480 years of
    // uptime; it saturates rather than wraps in any case, so an impossible
    // tick count reads as a distant future rather than as a past.
    let millis = ticks.saturating_mul(PIT_DIVISOR * MILLIS_PER_SECOND as u64) / PIT_INPUT_HZ;
    millis.min(i64::MAX as u64) as i64
}

/// Read the hardware and peg the cache to it.
fn resync(ticks: u64) -> i64 {
    let reading = read_hardware();
    let hardware = unix_millis_from_parts(
        reading.year as i64,
        reading.month as i64,
        reading.day as i64,
        reading.hour as i64,
        reading.minute as i64,
        reading.second as i64,
        0,
    );
    // Never step backwards; see the note on the cache at the top of the file.
    // The clock is only ever held back, and only by the second or so of
    // resolution the chip lacks, so it catches up on its own.
    let millis = if SYNCED.load(Ordering::Acquire) {
        let committed = derived(ticks);
        if hardware < committed {
            committed
        } else {
            hardware
        }
    } else {
        hardware
    };

    BASE_MILLIS.store(millis, Ordering::Relaxed);
    BASE_TICKS.store(ticks, Ordering::Relaxed);
    // Written last: the flag is what makes a reader trust the pair above.
    SYNCED.store(true, Ordering::Release);
    millis
}

// ── Talking to the chip ─────────────────────────────────────────────────────

/// The time registers of one reading, exactly as the chip returned them.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Registers {
    second: u8,
    minute: u8,
    hour: u8,
    day: u8,
    month: u8,
    year: u8,
}

/// Read the clock, taking readings until two agree.
fn read_hardware() -> DateTime {
    let (registers, status_b) = without_interrupts(|| {
        // SAFETY: interrupts are masked for the whole exchange, so nothing can
        // slip a write to the index port between selecting a register and
        // reading its value back. The ports themselves are the CMOS pair and
        // reading them has no effect beyond returning a byte.
        unsafe {
            wait_for_update();
            let mut previous = read_registers();
            for _ in 1..MAX_READINGS {
                wait_for_update();
                let current = read_registers();
                if current == previous {
                    break;
                }
                previous = current;
            }
            (previous, cmos_read(REG_STATUS_B))
        }
    });
    decode(registers, status_b)
}

/// Spin until the chip is not part-way through an update, or until the bound
/// runs out and we have to assume there is no working clock behind the ports.
unsafe fn wait_for_update() {
    for _ in 0..UPDATE_SPIN_LIMIT {
        if cmos_read(REG_STATUS_A) & STATUS_A_UPDATE_IN_PROGRESS == 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

unsafe fn read_registers() -> Registers {
    Registers {
        second: cmos_read(REG_SECOND),
        minute: cmos_read(REG_MINUTE),
        hour: cmos_read(REG_HOUR),
        day: cmos_read(REG_DAY),
        month: cmos_read(REG_MONTH),
        year: cmos_read(REG_YEAR),
    }
}

/// Select a CMOS register and read it.
///
/// Sound only while nothing else can interleave its own two-port exchange,
/// which is what the caller's interrupt masking is for.
unsafe fn cmos_read(register: u8) -> u8 {
    // Bit 7 of the index port masks the NMI. Keep it clear, which is the state
    // the firmware hands the machine over in: leaving NMIs masked as a
    // side-effect of reading the time would be a lasting change made by
    // accident.
    outb(CMOS_INDEX, register & 0x7F);
    inb(CMOS_DATA)
}

// Port helpers, in the same shape as the ones in `mouse.rs`. This kernel does
// not depend on the `x86_64` crate — `gdt.rs` explains why — so port access is
// two lines of inline assembly rather than a `Port` type.

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    core::arch::asm!("in al, dx", in("dx") port, out("al") value,
                     options(nomem, nostack, preserves_flags));
    value
}

unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") value,
                     options(nomem, nostack, preserves_flags));
}

/// Run `f` with interrupts masked, leaving the flag exactly as it was found.
///
/// `mouse::init` brackets its port sequence with a bare `cli`/`sti` pair,
/// which is right for something that runs once during boot with interrupts on.
/// This is also called from the main loop, which runs with interrupts already
/// disabled so that it can allocate without an IRQ handler deadlocking against
/// the heap lock, and an unconditional `sti` here would punch a hole straight
/// through that.
fn without_interrupts<T>(f: impl FnOnce() -> T) -> T {
    let flags: u64;
    // SAFETY: this copies RFLAGS to a register by way of the stack and clears
    // the interrupt flag. Neither has any effect beyond the flag, and the flag
    // is put back below.
    unsafe {
        core::arch::asm!("pushfq", "pop {}", "cli", out(reg) flags);
    }

    let result = f();

    // Bit 9 of RFLAGS is the interrupt flag.
    if flags & (1 << 9) != 0 {
        // SAFETY: interrupts were on when this was entered, so switching them
        // back on restores the caller's state rather than changing it.
        unsafe {
            core::arch::asm!("sti", options(nomem, nostack));
        }
    }
    result
}

// ── Decoding ────────────────────────────────────────────────────────────────

/// Turn a reading into a date, clamping every field into range on the way.
///
/// A clock with a dead battery, or a chipset that answers 0xFF because there
/// is nothing there, has to produce a wrong date rather than an index off the
/// end of the month table.
fn decode(registers: Registers, status_b: u8) -> DateTime {
    let binary = status_b & STATUS_B_BINARY != 0;

    let two_digit_year = from_bcd(registers.year, binary);
    let year = if two_digit_year < CENTURY_PIVOT {
        2000 + two_digit_year as u16
    } else {
        1900 + two_digit_year as u16
    };
    let month = from_bcd(registers.month, binary).clamp(1, 12);

    DateTime {
        year,
        month,
        day: from_bcd(registers.day, binary).clamp(1, days_in_month(year, month)),
        hour: decode_hour(registers.hour, status_b).min(23),
        minute: from_bcd(registers.minute, binary).min(59),
        second: from_bcd(registers.second, binary).min(59),
    }
}

/// The hour register, as the 0-23 the rest of this module works in.
///
/// In 12-hour mode the top bit is the PM flag, and it has to come off *before*
/// the conversion: noon arrives as 0x92, and reading that as BCD would take
/// the flag for part of the tens digit and answer 92.
fn decode_hour(register: u8, status_b: u8) -> u8 {
    let binary = status_b & STATUS_B_BINARY != 0;
    if status_b & STATUS_B_24_HOUR != 0 {
        return from_bcd(register, binary);
    }
    let pm = register & HOUR_PM != 0;
    // The twelve wraps to zero either way round: 12 AM is hour 0 and 12 PM is
    // hour 12.
    let hour = from_bcd(register & !HOUR_PM, binary) % 12;
    if pm {
        hour + 12
    } else {
        hour
    }
}

/// A register as a number, converting from BCD unless the chip is already
/// reporting binary.
///
/// Nonsense in gives nonsense out rather than an overflow: the largest result
/// is 0xFF's 15 * 10 + 15, which is 165 and fits in the byte.
fn from_bcd(value: u8, binary: bool) -> u8 {
    if binary {
        return value;
    }
    (value >> 4) * 10 + (value & 0x0F)
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        // Unreachable from [`decode`], which clamps the month first, but a
        // month table with a hole in it is not worth the risk.
        _ => 31,
    }
}

/// The Gregorian rule: every fourth year, except centuries, except every
/// fourth century.
fn is_leap_year(year: u16) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

// ── The calendar ────────────────────────────────────────────────────────────

/// Days from 1970-01-01 to a civil date, by Howard Hinnant's
/// `days_from_civil`.
///
/// Shifting the year to start in March puts the leap day at the end of it,
/// which is what makes the 4/100/400 rules fall out of plain division instead
/// of needing a table or a special case. It is exact for every proleptic
/// Gregorian date, which matters more than it sounds: the approximation this
/// replaces is `365.25 * years`, and that is a day out within a century in
/// either direction.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    // Rust truncates division towards zero, as C++ does, so the negative case
    // needs the same nudge Hinnant's original gives it.
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400; // 0..=399
    let shifted_month = (month + 9) % 12; // 0..=11, March first
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    // 146097 days to a 400-year era, and 719468 days from 0000-03-01 to the
    // Unix epoch.
    era * 146_097 + day_of_era - 719_468
}

/// The inverse, Hinnant's `civil_from_days`: the year, month and day a count
/// of days since the epoch lands on.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097; // 0..=146096
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524
        - day_of_era / 146_096)
        / 365; // 0..=399
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153; // 0..=11, March first
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1; // 1..=31
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

// ── Boot-time checks ────────────────────────────────────────────────────────

/// Boot-time checks.
///
/// The hardware itself can only be sanity-checked — nothing here knows what
/// time it really is — so the weight is on the two pure parts that are easy to
/// get quietly wrong: decoding a register, and counting days.
pub fn selftest() -> crate::selftest::Report {
    let mut report = crate::selftest::Report::new();

    // ── Registers ───────────────────────────────────────────────────────────

    report.check("bcd converts", from_bcd(0x59, false) == 59);
    report.check("bcd tens and units", from_bcd(0x12, false) == 12);
    report.check("binary passes through", from_bcd(0x17, true) == 23);
    report.check("nonsense bcd cannot overflow", from_bcd(0xFF, false) == 165);

    report.check("a 24-hour bcd hour", decode_hour(0x23, STATUS_B_24_HOUR) == 23);
    report.check(
        "a 24-hour binary hour",
        decode_hour(17, STATUS_B_24_HOUR | STATUS_B_BINARY) == 17,
    );
    report.check("midnight in 12-hour mode", decode_hour(0x12, 0) == 0);
    report.check("morning in 12-hour mode", decode_hour(0x07, 0) == 7);
    report.check("noon in 12-hour mode", decode_hour(0x92, 0) == 12);
    report.check("one in the afternoon", decode_hour(0x81, 0) == 13);
    report.check("the pm flag comes off before the digits", decode_hour(0x89, 0) == 21);
    report.check(
        "a 12-hour binary hour",
        decode_hour(9 | HOUR_PM, STATUS_B_BINARY) == 21,
    );

    // A whole reading of 2023-11-14 22:13:20, in 12-hour BCD.
    let afternoon = Registers {
        second: 0x20,
        minute: 0x13,
        hour: 0x10 | HOUR_PM,
        day: 0x14,
        month: 0x11,
        year: 0x23,
    };
    report.check(
        "a whole reading decodes",
        decode(afternoon, 0)
            == DateTime { year: 2023, month: 11, day: 14, hour: 22, minute: 13, second: 20 },
    );

    let zeroed = Registers { second: 0, minute: 0, hour: 0, day: 0, month: 0, year: 0 };
    report.check(
        "a zeroed clock clamps up into range",
        decode(zeroed, STATUS_B_24_HOUR)
            == DateTime { year: 2000, month: 1, day: 1, hour: 0, minute: 0, second: 0 },
    );

    // Ports with nothing behind them read as 0xFF, which converts to 165 in
    // every field. Each one has to clamp: the month into the table, the day
    // into the month, and the hour past a PM flag that is also set.
    let dead = Registers {
        second: 0xFF,
        minute: 0xFF,
        hour: 0xFF,
        day: 0xFF,
        month: 0xFF,
        year: 0xFF,
    };
    report.check(
        "dead ports clamp down into range",
        decode(dead, 0)
            == DateTime { year: 2065, month: 12, day: 31, hour: 13, minute: 59, second: 59 },
    );

    report.check("february is short", days_in_month(2023, 2) == 28);
    report.check("february is longer in a leap year", days_in_month(2024, 2) == 29);
    report.check("1900 was not a leap year", !is_leap_year(1900));
    report.check("2000 was", is_leap_year(2000));
    report.check("2100 will not be", !is_leap_year(2100));

    // ── The calendar ────────────────────────────────────────────────────────

    report.check("the epoch is day zero", days_from_civil(1970, 1, 1) == 0);
    report.check("the day after it", days_from_civil(1970, 1, 2) == 1);
    report.check("the day before it", days_from_civil(1969, 12, 31) == -1);
    report.check(
        "a year boundary is one day wide",
        days_from_civil(2001, 1, 1) - days_from_civil(2000, 12, 31) == 1,
    );
    report.check("2000-02-29 is day 11016", days_from_civil(2000, 2, 29) == 11_016);
    report.check("2024-02-29 is day 19782", days_from_civil(2024, 2, 29) == 19_782);
    report.check(
        "2000 had a leap day",
        days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28) == 2,
    );
    report.check(
        "1900 had none",
        days_from_civil(1900, 3, 1) - days_from_civil(1900, 2, 28) == 1,
    );
    report.check(
        "2100 will have none",
        days_from_civil(2100, 3, 1) - days_from_civil(2100, 2, 28) == 1,
    );

    report.check(
        "zero milliseconds is the epoch",
        from_unix_millis(0)
            == DateTime { year: 1970, month: 1, day: 1, hour: 0, minute: 0, second: 0 },
    );
    report.check(
        "a known timestamp breaks down",
        from_unix_millis(1_700_000_000_000)
            == DateTime { year: 2023, month: 11, day: 14, hour: 22, minute: 13, second: 20 },
    );
    report.check(
        "a millisecond before the epoch",
        from_unix_millis(-1)
            == DateTime { year: 1969, month: 12, day: 31, hour: 23, minute: 59, second: 59 },
    );
    report.check(
        "a day before the epoch",
        from_unix_millis(-MILLIS_PER_DAY)
            == DateTime { year: 1969, month: 12, day: 31, hour: 0, minute: 0, second: 0 },
    );

    report.check(
        "the same timestamp built back up",
        unix_millis_from_parts(2023, 11, 14, 22, 13, 20, 0) == 1_700_000_000_000,
    );
    report.check(
        "a leap day survives the round trip",
        from_unix_millis(unix_millis_from_parts(2024, 2, 29, 12, 0, 0, 0))
            == DateTime { year: 2024, month: 2, day: 29, hour: 12, minute: 0, second: 0 },
    );
    report.check(
        "a thirteenth month is next january",
        unix_millis_from_parts(2023, 13, 1, 0, 0, 0, 0)
            == unix_millis_from_parts(2024, 1, 1, 0, 0, 0, 0),
    );
    report.check(
        "a day past the end of february rolls over",
        from_unix_millis(unix_millis_from_parts(2023, 2, 29, 0, 0, 0, 0))
            == DateTime { year: 2023, month: 3, day: 1, hour: 0, minute: 0, second: 0 },
    );
    report.check(
        "absurd fields do not overflow",
        unix_millis_from_parts(i64::MAX, i64::MAX, i64::MAX, i64::MAX, i64::MAX, i64::MAX, i64::MAX)
            > 0,
    );
    report.check(
        "absurd negative fields do not either",
        unix_millis_from_parts(i64::MIN, i64::MIN, i64::MIN, i64::MIN, i64::MIN, i64::MIN, i64::MIN)
            < 0,
    );

    report.check("the epoch was a thursday", weekday(0) == 4);
    report.check("the day before it was a wednesday", weekday(-1) == 3);
    report.check("a known timestamp was a tuesday", weekday(1_700_000_000_000) == 2);

    // ── The live clock ──────────────────────────────────────────────────────
    //
    // Nothing here knows the real time, so these only pin down that the answer
    // is a date at all and that the cache is telling the same story as the
    // chip.

    let reading = now();
    report.check("the year is plausible", (1970..=2100).contains(&reading.year));
    report.check(
        "the fields are in range",
        (1..=12).contains(&reading.month)
            && (1..=31).contains(&reading.day)
            && reading.hour <= 23
            && reading.minute <= 59
            && reading.second <= 59,
    );

    let first = unix_millis();
    let second = unix_millis();
    report.check("two readings do not go backwards", second >= first);

    // The cache is pegged to the hardware, so the two must not have drifted
    // further apart than the second of resolution the chip lacks.
    let hardware = read_hardware();
    let hardware_millis = unix_millis_from_parts(
        hardware.year as i64,
        hardware.month as i64,
        hardware.day as i64,
        hardware.hour as i64,
        hardware.minute as i64,
        hardware.second as i64,
        0,
    );
    report.check(
        "the cache agrees with the chip",
        unix_millis().abs_diff(hardware_millis) < 2 * MILLIS_PER_SECOND as u64,
    );

    let text = clock_text();
    let bytes = text.as_bytes();
    report.check(
        "the taskbar text is hh:mm",
        bytes.len() == 5
            && bytes[..2].iter().all(u8::is_ascii_digit)
            && bytes[2] == b':'
            && bytes[3..].iter().all(u8::is_ascii_digit),
    );

    report
}
