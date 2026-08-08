//! The three libm functions QuickJS needs that the kernel's link does not
//! already contain.
//!
//! QuickJS references thirty-three libm symbols. Thirty of them — acos, acosh,
//! asin, asinh, atan, atan2, cbrt, ceil, cos, cosh, exp, expm1, fabs, floor,
//! fmax, fmin, fmod, hypot, log, log10, log1p, log2, pow, round, sin, sinh,
//! sqrt, tan, tanh and trunc — are already exported under exactly those C names
//! by `compiler_builtins`, which every `no_std` target links and which is where
//! Rust now keeps the pure-Rust `libm` port. They are weak symbols, so anything
//! defined here would override them rather than collide, but there is no reason
//! to define them: a second copy of libm would be dead weight in a 32 MiB
//! machine, and the copy already present is the one the Rust test suite is run
//! against. Depending on it also keeps this build free of a registry fetch,
//! which taking `libm` as a direct dependency would not.
//!
//! `modf`, `lrint` and `atanh` are the gaps. The first two are short enough to
//! write against the bit pattern directly, which is how musl and the `libm`
//! crate write them too; the third is a two-line identity over `log1p`.
//!
//! Two things to keep an eye on, both of which `third_party/check-symbols.sh`
//! turns into an automatic failure rather than a surprise:
//!
//!  * `compiler_builtins` only exports the maths symbols for targets whose `os`
//!    is `none` (or another OS with no libm of its own), so a custom target JSON
//!    must keep `"os": "none"`.
//!  * The `mem` group — memcpy, memmove, memset, memcmp, strlen — is behind a
//!    Cargo feature, so a `-Z build-std` build must pass
//!    `-Zbuild-std-features=compiler-builtins-mem`.
//!
//! `atanh` going missing from `compiler_builtins` while its neighbours `asinh`
//! and `acosh` are present is exactly the kind of gap that motivates that
//! script.

extern "C" {
    /// From `compiler_builtins`. Used by `atanh` below because reaching for the
    /// accurate log1p already in the link beats carrying a second polynomial.
    fn log1p(x: f64) -> f64;
}

/// Truncate towards zero without going through libm.
///
/// Used by `modf` below rather than calling the weak `trunc`, so that this
/// module has no link-order dependency on the symbol it is meant to complement.
fn trunc_toward_zero(x: f64) -> f64 {
    let bits = x.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023;

    // 52 or more bits of exponent means every mantissa bit is already integral;
    // that also catches infinities and NaNs, which must come back unchanged.
    if exponent >= 52 {
        return x;
    }
    // Below 1.0 the whole value is fraction, and the sign has to survive so
    // that modf(-0.5) stores -0.0 rather than +0.0.
    if exponent < 0 {
        return f64::from_bits(bits & (1 << 63));
    }
    let fraction_mask = (1u64 << (52 - exponent)) - 1;
    f64::from_bits(bits & !fraction_mask)
}

/// Split `x` into its fractional and integral parts, storing the latter through
/// `iptr`. QuickJS calls this from `Atomics.pause` to reject a non-integral
/// argument.
///
/// # Safety
/// `iptr` must be a valid, aligned, writable `f64`.
#[no_mangle]
pub unsafe extern "C" fn modf(x: f64, iptr: *mut f64) -> f64 {
    let integral = trunc_toward_zero(x);
    unsafe { core::ptr::write(iptr, integral) };

    if x.is_nan() {
        return x;
    }
    // POSIX: an infinity has no fractional part, and the zero returned keeps the
    // sign of the argument.
    if x.is_infinite() {
        return f64::from_bits(x.to_bits() & (1 << 63));
    }
    x - integral
}

/// Round to the nearest integer, ties to even, and return it as a C `long`.
///
/// The add-and-subtract trick is musl's: adding 2^52 forces every fractional bit
/// out of the mantissa using the hardware's current rounding mode, which is
/// round-to-nearest-even unless something has deliberately changed it, and
/// subtracting it again leaves the rounded value. Rust never reassociates
/// floating-point arithmetic, so the pair does not cancel out under
/// optimisation.
#[no_mangle]
pub extern "C" fn lrint(x: f64) -> i64 {
    const TWO_POW_52: f64 = 4503599627370496.0;

    let exponent = ((x.to_bits() >> 52) & 0x7ff) as i32 - 1023;
    // Already integral, or too large to have a fractional part at all. The cast
    // saturates in Rust, which is a defined answer where C would leave the
    // result unspecified.
    if exponent >= 52 {
        return x as i64;
    }
    let rounded = if x.is_sign_negative() {
        x - TWO_POW_52 + TWO_POW_52
    } else {
        x + TWO_POW_52 - TWO_POW_52
    };
    rounded as i64
}

/// Inverse hyperbolic tangent, reached from `Math.atanh`.
///
/// This is musl's formulation. The split at |x| < 0.5 is not an optimisation:
/// `2x/(1-x)` loses most of its significance for a small argument, and going
/// through `log1p` of the rearranged expression is what keeps `atanh(1e-17)`
/// from coming back as zero.
#[no_mangle]
pub extern "C" fn atanh(x: f64) -> f64 {
    let negative = x.is_sign_negative();
    let magnitude = if negative { -x } else { x };

    let result = if magnitude < 0.5 {
        0.5 * unsafe {
            log1p(2.0 * magnitude + 2.0 * magnitude * magnitude / (1.0 - magnitude))
        }
    } else {
        // Written as one division so that atanh(1) reaches log1p(inf) and comes
        // back as an infinity rather than as a NaN from inf - inf.
        0.5 * unsafe { log1p(2.0 * (magnitude / (1.0 - magnitude))) }
    };

    if negative {
        -result
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modf_of(x: f64) -> (f64, f64) {
        let mut integral = 0.0f64;
        let fraction = unsafe { modf(x, &mut integral) };
        (fraction, integral)
    }

    #[test]
    fn modf_splits_ordinary_values() {
        assert_eq!(modf_of(3.5), (0.5, 3.0));
        assert_eq!(modf_of(-3.5), (-0.5, -3.0));
        assert_eq!(modf_of(7.0), (0.0, 7.0));
        assert_eq!(modf_of(0.25), (0.25, 0.0));
    }

    #[test]
    fn modf_keeps_the_sign_of_a_negative_zero_result() {
        let (fraction, integral) = modf_of(-0.5);
        assert_eq!(fraction, -0.5);
        assert!(integral == 0.0 && integral.is_sign_negative());
    }

    #[test]
    fn modf_handles_infinities_and_nan() {
        let (fraction, integral) = modf_of(f64::INFINITY);
        assert_eq!(fraction, 0.0);
        assert!(fraction.is_sign_positive());
        assert_eq!(integral, f64::INFINITY);

        let (fraction, integral) = modf_of(f64::NEG_INFINITY);
        assert_eq!(fraction, 0.0);
        assert!(fraction.is_sign_negative());
        assert_eq!(integral, f64::NEG_INFINITY);

        let (fraction, integral) = modf_of(f64::NAN);
        assert!(fraction.is_nan());
        assert!(integral.is_nan());
    }

    #[test]
    fn modf_agrees_with_trunc_on_large_values() {
        for x in [1e15, -1e15, 4503599627370495.5, 1e300] {
            let (fraction, integral) = modf_of(x);
            assert_eq!(integral + fraction, x, "disagreed at {x}");
        }
    }

    #[test]
    fn lrint_rounds_halves_to_even() {
        assert_eq!(lrint(0.5), 0);
        assert_eq!(lrint(1.5), 2);
        assert_eq!(lrint(2.5), 2);
        assert_eq!(lrint(3.5), 4);
        assert_eq!(lrint(-0.5), 0);
        assert_eq!(lrint(-1.5), -2);
        assert_eq!(lrint(-2.5), -2);
    }

    #[test]
    fn atanh_matches_the_reference_values() {
        // Reference values from CPython's math.atanh, which is the platform's
        // libm. Compared to within a couple of units in the last place.
        for (x, want) in [
            (0.0, 0.0),
            (0.5, 0.549_306_144_334_054_9),
            (-0.5, -0.549_306_144_334_054_9),
            (0.25, 0.255_412_811_882_995_3),
            (0.9, 1.472_219_489_583_220_4),
            (0.999, 3.800_201_167_250_199_4),
        ] {
            let got = atanh(x);
            assert!(
                (got - want).abs() <= 1e-15 * want.abs().max(1.0),
                "atanh({x}) gave {got}, wanted {want}"
            );
        }
    }

    #[test]
    fn atanh_keeps_its_accuracy_near_zero() {
        // The naive 0.5*ln((1+x)/(1-x)) collapses to zero here; atanh(x) ~ x.
        let tiny = 1e-17;
        assert_eq!(atanh(tiny), tiny);
        assert!(atanh(-0.0).is_sign_negative());
    }

    #[test]
    fn atanh_diverges_at_the_ends_of_its_domain() {
        assert_eq!(atanh(1.0), f64::INFINITY);
        assert_eq!(atanh(-1.0), f64::NEG_INFINITY);
        assert!(atanh(2.0).is_nan());
    }

    #[test]
    fn lrint_rounds_ordinary_values_to_nearest() {
        assert_eq!(lrint(0.4), 0);
        assert_eq!(lrint(0.6), 1);
        assert_eq!(lrint(-0.6), -1);
        assert_eq!(lrint(1e15), 1_000_000_000_000_000);
        assert_eq!(lrint(-1e15), -1_000_000_000_000_000);
    }
}
