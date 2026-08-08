//! The only place the shim answers "what time is it".
//!
//! `Date` and the seed for `Math.random()` both reach `gettimeofday`, and
//! `Atomics.wait` reaches `clock_gettime`; all three end up in [`unix_millis`]
//! below. Until the kernel calls [`set_clock`], that returns a fixed timestamp,
//! which is wrong but harmless — a `Date` that does not advance is a much
//! smaller problem than a kernel that cannot boot because the shim insisted on a
//! clock before one existed.
//!
//! `localtime_r` is also here, and it reports UTC. That is not a placeholder:
//! `rtc.rs` reads the CMOS clock as UTC and this OS has no timezone database, so
//! UTC is genuinely the local time. QuickJS reads exactly one field of the
//! `struct tm` it passes in — `tm_gmtoff` — which is why the rest is left zeroed.

use core::sync::atomic::{AtomicUsize, Ordering};

/// A source of milliseconds since the Unix epoch. `kernel/src/rtc.rs` has one
/// with this exact signature, `rtc::unix_millis`.
pub type ClockFn = fn() -> i64;

/// 2026-06-04T00:00:00Z, the release date of the vendored QuickJS. Any `Date`
/// showing this exact instant means the kernel never installed a clock.
const FALLBACK_UNIX_MILLIS: i64 = 1_780_531_200_000;

static CLOCK: AtomicUsize = AtomicUsize::new(0);

/// Hand the shim the kernel's clock. Called once, from kernel start-up.
pub fn set_clock(clock: ClockFn) {
    CLOCK.store(clock as usize, Ordering::Release);
}

/// Milliseconds since the Unix epoch, from the kernel if it has told us how and
/// from [`FALLBACK_UNIX_MILLIS`] if it has not.
pub fn unix_millis() -> i64 {
    let raw = CLOCK.load(Ordering::Acquire);
    if raw == 0 {
        return FALLBACK_UNIX_MILLIS;
    }
    let clock: ClockFn = unsafe { core::mem::transmute::<usize, ClockFn>(raw) };
    clock()
}

#[repr(C)]
pub struct Timeval {
    tv_sec: i64,
    tv_usec: i64,
}

#[repr(C)]
pub struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

#[repr(C)]
pub struct Tm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
    tm_gmtoff: i64,
    tm_zone: *const u8,
}

/// Split epoch milliseconds into whole seconds and a remainder, using Euclidean
/// division so that a pre-epoch instant counts forward from the second boundary
/// rather than backwards from it — the same convention `rtc.rs` uses.
fn split(millis: i64) -> (i64, i64) {
    (millis.div_euclid(1000), millis.rem_euclid(1000))
}

/// # Safety
/// `tv` must be a valid, aligned, writable `Timeval`. `tz` is ignored, as it is
/// on every platform that still has this function.
#[no_mangle]
pub unsafe extern "C" fn gettimeofday(tv: *mut Timeval, _tz: *mut u8) -> i32 {
    if tv.is_null() {
        return 0;
    }
    let (seconds, remainder) = split(unix_millis());
    unsafe {
        core::ptr::write(
            tv,
            Timeval { tv_sec: seconds, tv_usec: remainder * 1000 },
        );
    }
    0
}

/// # Safety
/// `ts` must be a valid, aligned, writable `Timespec`.
#[no_mangle]
pub unsafe extern "C" fn clock_gettime(_clock_id: i32, ts: *mut Timespec) -> i32 {
    if ts.is_null() {
        return 0;
    }
    // CLOCK_MONOTONIC and CLOCK_REALTIME are the same thing here: the RTC is the
    // only clock, and the single caller (Atomics.wait's timeout, which this OS
    // cannot reach because JS_NewRuntime leaves can_block false) would not
    // notice the difference.
    let (seconds, remainder) = split(unix_millis());
    unsafe {
        core::ptr::write(
            ts,
            Timespec { tv_sec: seconds, tv_nsec: remainder * 1_000_000 },
        );
    }
    0
}

/// # Safety
/// `out` must be a valid, aligned, writable `Tm`.
#[no_mangle]
pub unsafe extern "C" fn localtime_r(_t: *const i64, out: *mut Tm) -> *mut Tm {
    if out.is_null() {
        return out;
    }
    unsafe {
        core::ptr::write(
            out,
            Tm {
                tm_sec: 0,
                tm_min: 0,
                tm_hour: 0,
                tm_mday: 1,
                tm_mon: 0,
                tm_year: 70,
                tm_wday: 4,
                tm_yday: 0,
                tm_isdst: 0,
                tm_gmtoff: 0,
                tm_zone: c"UTC".as_ptr() as *const u8,
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fallback_is_the_quickjs_release_date() {
        // 20608 days from 1970-01-01 is 2026-06-04.
        assert_eq!(FALLBACK_UNIX_MILLIS, 20_608 * 86_400_000);
    }

    #[test]
    fn seconds_and_microseconds_split_cleanly() {
        assert_eq!(split(1_700_000_000_123), (1_700_000_000, 123));
        assert_eq!(split(0), (0, 0));
    }

    #[test]
    fn a_pre_epoch_instant_counts_forward_from_the_second() {
        // -1 ms is one millisecond before the epoch: second -1, remainder 999.
        assert_eq!(split(-1), (-1, 999));
        assert_eq!(split(-1000), (-1, 0));
    }

    #[test]
    fn gettimeofday_reports_the_installed_clock() {
        fn fixed() -> i64 {
            1_234_567_891
        }
        set_clock(fixed);

        let mut tv = Timeval { tv_sec: 0, tv_usec: 0 };
        assert_eq!(unsafe { gettimeofday(&mut tv, core::ptr::null_mut()) }, 0);
        assert_eq!(tv.tv_sec, 1_234_567);
        assert_eq!(tv.tv_usec, 891_000);
    }

    #[test]
    fn localtime_r_reports_utc() {
        let mut tm = Tm {
            tm_sec: -1,
            tm_min: -1,
            tm_hour: -1,
            tm_mday: -1,
            tm_mon: -1,
            tm_year: -1,
            tm_wday: -1,
            tm_yday: -1,
            tm_isdst: -1,
            tm_gmtoff: 12345,
            tm_zone: core::ptr::null(),
        };
        let t: i64 = 1_700_000_000;
        assert!(!unsafe { localtime_r(&t, &mut tm) }.is_null());
        assert_eq!(tm.tm_gmtoff, 0);
    }
}
