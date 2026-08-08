/*
 * math.h for OS101.
 *
 * Two rules decided the shape of this file.
 *
 * The first is which hardware to use. sqrt is one instruction, sqrtsd, and it
 * is part of SSE2, which every x86-64 has by definition — so sqrt here is
 * exact, correctly rounded, and three lines long. The rounding instructions
 * (roundsd and its relatives) are SSE4.1, which is *not* baseline: run.sh
 * starts QEMU with no -cpu argument, so the guest is the default qemu64 model
 * and does not have them. An SSE4.1 instruction there is an invalid opcode and
 * the process dies. floor, ceil, trunc and round are therefore done by masking
 * the fraction out of the exponent field, which is exact everywhere and needs
 * no feature the CPU might not have.
 *
 * The second is accuracy. Everything transcendental is argument-reduced and
 * then evaluated by a truncated Taylor series long enough that its own error is
 * below a tenth of a unit in the last place, so what is left is the reduction
 * and the final rounding. Measured against the host's libm over the ranges in
 * os101-libc/tests/test_math.c, that is under 1 ULP for exp, log, sin, cos and
 * atan, and a few ULP for pow, tan near its poles, and the inverse
 * trigonometric functions near ±1. The comments below say where each one is
 * weak; there is no claim of correct rounding anywhere except sqrt.
 */
#include <math.h>
#include <stdint.h>
#include <string.h>

/*
 * No contraction anywhere in this file.
 *
 * two_product below is Dekker's exact multiplication, and it works by relying on
 * a*b being rounded — the whole point is to recover the bits that the rounding
 * dropped. A compiler that is allowed to contract a*b+c into a fused
 * multiply-add computes the difference without ever rounding the product, the
 * recovered "low half" comes out as zero, and everything built on it (pow's
 * exponent, the argument reduction in sin and cos) silently loses the extra
 * precision it was written to carry. x86-64's baseline SSE2 has no FMA so the
 * target build is safe either way, but the host test build on an arm64 machine
 * is not, and an accuracy bug that appears only on the build machine is the
 * worst kind to chase.
 */
#pragma STDC FP_CONTRACT OFF

/*
 * The constants are hex float literals because that is the only way to write
 * a double and be certain which one: a decimal literal is a request to the
 * compiler to round, and for the two-part splits below the whole point is that
 * the pieces are the exact doubles they are meant to be. Each pair is the
 * nearest double to the value and then the nearest double to what is left
 * over, so hi + lo is the value to about 105 bits.
 */

/* ln 2, split so that k*LN2_HI is exact for the k that exp's reduction
   produces (|k| < 2048 needs eleven bits, LN2_HI has thirty-three). */
#define LN2_HI 0x1.62e42fef00000p-1
#define LN2_LO 0x1.473de6af278edp-34
/* ln 2 as a double-double, for where an exact product is used instead. */
#define LN2_DD_HI 0x1.62e42fefa39efp-1
#define LN2_DD_LO 0x1.abc9e3b39803fp-56

#define INV_LN2 0x1.71547652b82fep+0
#define INV_LN10 0x1.bcb7b1526e50dp-2 /* 1/ln 10 */
#define LOG10_2 0x1.34413509f79ffp-2  /* log10(2) */

#define PI 0x1.921fb54442d18p+1
#define PI_HALF 0x1.921fb54442d18p+0
#define PI_QUARTER 0x1.921fb54442d18p-1
#define TWO_OVER_PI 0x1.45f306dc9c883p-1
/* pi/2 as a double-double. The reduction below multiplies both halves out
   exactly, so two pieces reach further than the three pieces of a Cody-Waite
   chain would: the residual is below 1e-33, and that is what bounds how large
   an argument can still be reduced usefully. */
#define PIO2_HI 0x1.921fb54442d18p+0
#define PIO2_LO 0x1.1a62633145c07p-54

#define SQRT_HALF 0x1.6a09e667f3bcdp-1
#define TAN_PI_8 0x1.a827999fcef33p-2  /* tan(pi/8)  = 0.4142135623730951 */
#define TAN_3PI_8 0x1.3504f333f9de5p+1 /* tan(3pi/8) = 2.414213562373095  */

static uint64_t bits_of(double x)
{
    uint64_t u;

    memcpy(&u, &x, sizeof(u));
    return u;
}

static double from_bits(uint64_t u)
{
    double x;

    memcpy(&x, &u, sizeof(x));
    return x;
}

/* ---- classification --------------------------------------------------- */

int isnan(double x)
{
    uint64_t u = bits_of(x) & 0x7fffffffffffffffULL;

    return u > 0x7ff0000000000000ULL;
}

int isinf(double x)
{
    return (bits_of(x) & 0x7fffffffffffffffULL) == 0x7ff0000000000000ULL;
}

int isfinite(double x)
{
    return (bits_of(x) & 0x7fffffffffffffffULL) < 0x7ff0000000000000ULL;
}

int signbit(double x)
{
    return (int)(bits_of(x) >> 63);
}

int fpclassify(double x)
{
    uint64_t u = bits_of(x) & 0x7fffffffffffffffULL;
    int e = (int)(u >> 52);

    if (e == 0x7ff) {
        return (u & 0xfffffffffffffULL) != 0 ? FP_NAN : FP_INFINITE;
    }
    if (e == 0) {
        return (u & 0xfffffffffffffULL) != 0 ? FP_SUBNORMAL : FP_ZERO;
    }
    return FP_NORMAL;
}

double nan(const char *tag)
{
    (void)tag; /* no payload: one quiet NaN is enough */
    return from_bits(0x7ff8000000000000ULL);
}

/* ---- sign and magnitude ------------------------------------------------ */

double fabs(double x)
{
    return from_bits(bits_of(x) & 0x7fffffffffffffffULL);
}

double copysign(double x, double y)
{
    return from_bits((bits_of(x) & 0x7fffffffffffffffULL)
                     | (bits_of(y) & 0x8000000000000000ULL));
}

double fmin(double x, double y)
{
    if (isnan(x)) {
        return y;
    }
    if (isnan(y)) {
        return x;
    }
    if (x == 0.0 && y == 0.0) {
        return signbit(x) ? x : y;
    }
    return x < y ? x : y;
}

double fmax(double x, double y)
{
    if (isnan(x)) {
        return y;
    }
    if (isnan(y)) {
        return x;
    }
    if (x == 0.0 && y == 0.0) {
        return signbit(x) ? y : x;
    }
    return x > y ? x : y;
}

/* ---- square root ------------------------------------------------------- */

double sqrt(double x)
{
#if defined(__x86_64__)
    double r;

    /* SSE2, so it is always there, and it is correctly rounded by the
       hardware — the one function in this file that is exact. */
    __asm__("sqrtsd %1, %0" : "=x"(r) : "x"(x));
    return r;
#else
    /* The host test build, on whatever the build machine is. */
    return __builtin_sqrt(x);
#endif
}

/* ---- exponent surgery -------------------------------------------------- */

double frexp(double x, int *e)
{
    uint64_t u = bits_of(x);
    int ex = (int)((u >> 52) & 0x7ff);
    int extra = 0;

    if (ex == 0x7ff || x == 0.0) {
        *e = 0;
        return x;
    }
    if (ex == 0) {
        /* Subnormal: scale into the normal range and pay it back. */
        x *= 18446744073709551616.0; /* 2^64 */
        u = bits_of(x);
        ex = (int)((u >> 52) & 0x7ff);
        extra = -64;
    }
    *e = ex - 1022 + extra;
    return from_bits((u & 0x800fffffffffffffULL) | ((uint64_t)1022 << 52));
}

/* 2^n as an exact double, for |n| <= 1000. */
static double scale2(int n)
{
    return from_bits((uint64_t)(n + 1023) << 52);
}

double ldexp(double x, int n)
{
    if (x == 0.0 || !isfinite(x)) {
        return x;
    }
    if (n > 2200) {
        return copysign(HUGE_VAL, x);
    }
    if (n < -2200) {
        return copysign(0.0, x);
    }
    while (n > 1000) {
        x *= scale2(1000);
        n -= 1000;
    }
    while (n < -1000) {
        x *= scale2(-1000);
        n += 1000;
    }
    /* One multiply by an exact power of two: one rounding, which is what makes
       a result that lands in the subnormal range still correct. */
    return x * scale2(n);
}

/* ---- rounding ---------------------------------------------------------- */

double trunc(double x)
{
    uint64_t u = bits_of(x);
    int e = (int)((u >> 52) & 0x7ff) - 1023;

    if (e >= 52) {
        /* No fraction to lose — includes the infinities and NaNs. */
        return x;
    }
    if (e < 0) {
        return copysign(0.0, x);
    }
    /* Clear the bits of the significand below the binary point. */
    return from_bits(u & ~((1ULL << (52 - e)) - 1));
}

double floor(double x)
{
    double t = trunc(x);

    /* trunc rounds towards zero, so it is above x exactly when x is negative
       and has a fraction — the only case where floor differs from it. */
    return t > x ? t - 1.0 : t;
}

double ceil(double x)
{
    double t = trunc(x);

    return t < x ? t + 1.0 : t;
}

double round(double x)
{
    double t = trunc(x);
    double frac;

    if (!isfinite(x) || t == x) {
        return t;
    }
    /* x - trunc(x) is exact, so the comparison against a half is too. Halves
       go away from zero, which is what round() is specified to do — unlike the
       ties-to-even the hardware would give. */
    frac = fabs(x - t);
    if (frac >= 0.5) {
        return t + copysign(1.0, x);
    }
    return t;
}

double modf(double x, double *ipart)
{
    double t;

    if (isinf(x)) {
        *ipart = x;
        return copysign(0.0, x);
    }
    t = trunc(x);
    *ipart = t;
    if (isnan(x)) {
        return x;
    }
    return copysign(x - t, x);
}

double fmod(double x, double y)
{
    double ax = fabs(x);
    double ay = fabs(y);
    double mx;
    double my;
    int ex;
    int ey;
    int i;

    if (isnan(x) || isnan(y) || isinf(x) || y == 0.0) {
        return nan("");
    }
    if (isinf(y) || ax < ay) {
        return x;
    }
    if (ax == ay) {
        return copysign(0.0, x);
    }

    /* Long division in the significands. Both values are scaled into [0.5, 1),
       where every subtraction below is exact — the remainder never needs a bit
       finer than the inputs already have — so the answer is exact too, which is
       what fmod is required to be. */
    mx = frexp(ax, &ex);
    my = frexp(ay, &ey);
    for (i = ex - ey; i > 0; i--) {
        if (mx >= my) {
            mx -= my;
        }
        mx *= 2.0;
    }
    if (mx >= my) {
        mx -= my;
    }
    return copysign(ldexp(mx, ey), x);
}

/* ---- two-part arithmetic ----------------------------------------------- */

/*
 * pow has to multiply a logarithm by an exponent and then exponentiate the
 * product, and a single rounding of that product costs about y ULP in the
 * answer. These two are the classical way to carry the extra bits without a
 * wider type: Knuth's exact sum, and Dekker's exact product, which needs no
 * fused multiply-add (there is none in baseline SSE2).
 */
static void two_sum(double a, double b, double *hi, double *lo)
{
    double s = a + b;
    double bb = s - a;

    *hi = s;
    *lo = (a - (s - bb)) + (b - bb);
}

static void two_product(double a, double b, double *hi, double *lo)
{
    const double split = 134217729.0; /* 2^27 + 1 */
    double c = split * a;
    double ahi = c - (c - a);
    double alo = a - ahi;
    double d = split * b;
    double bhi = d - (d - b);
    double blo = b - bhi;
    double p = a * b;

    *hi = p;
    *lo = ((ahi * bhi - p) + ahi * blo + alo * bhi) + alo * blo;
}

/* (hi, lo) * (hi, lo), with both halves kept. */
static void dd_multiply(double a_hi, double a_lo, double b_hi, double b_lo,
                        double *hi, double *lo)
{
    double p;
    double e;

    two_product(a_hi, b_hi, &p, &e);
    e += a_hi * b_lo + a_lo * b_hi; /* the a_lo*b_lo term is below the last bit */
    two_sum(p, e, hi, lo);
}

/* 1/(hi + lo), to a double's precision with the division's own rounding taken
   back out: q is the first approximation and the residual corrects it. */
static double dd_reciprocal(double hi, double lo)
{
    double q = 1.0 / hi;
    double prod_hi;
    double prod_lo;
    double residual;

    two_product(q, hi, &prod_hi, &prod_lo);
    residual = ((1.0 - prod_hi) - prod_lo) - q * lo;
    return q + residual * q;
}

/* ---- exp and log ------------------------------------------------------- */

/* exp(r) for |r| <= 0.35, as 1 + r + r^2*P(r). Taylor to 1/13!, whose next
   term is below 1e-19 relative — far under the final rounding. */
static double exp_small(double r)
{
    double p = 1.0 / 6227020800.0; /* 1/13! */

    p = 1.0 / 479001600.0 + r * p;
    p = 1.0 / 39916800.0 + r * p;
    p = 1.0 / 3628800.0 + r * p;
    p = 1.0 / 362880.0 + r * p;
    p = 1.0 / 40320.0 + r * p;
    p = 1.0 / 5040.0 + r * p;
    p = 1.0 / 720.0 + r * p;
    p = 1.0 / 120.0 + r * p;
    p = 1.0 / 24.0 + r * p;
    p = 1.0 / 6.0 + r * p;
    p = 0.5 + r * p;
    return 1.0 + r + r * r * p;
}

double exp(double x)
{
    double kd;
    double r;
    int k;

    if (isnan(x)) {
        return x;
    }
    if (x > 709.782712893384) {
        return HUGE_VAL;
    }
    if (x < -745.1332191019411) {
        return 0.0;
    }

    /* x = k*ln2 + r with |r| <= ln2/2. k*LN2_HI is exact (k needs 11 bits,
       LN2_HI has 33), so the reduction loses nothing. */
    kd = x * INV_LN2;
    k = (int)(kd + copysign(0.5, kd));
    r = (x - (double)k * LN2_HI) - (double)k * LN2_LO;
    return ldexp(exp_small(r), k);
}

double exp2(double x)
{
    double kd = round(x);
    double r;
    int k;

    if (isnan(x)) {
        return x;
    }
    if (x > 1024.0) {
        return HUGE_VAL;
    }
    if (x < -1075.0) {
        return 0.0;
    }
    k = (int)kd;
    /* (x-k)*ln2 in two pieces, same reason as exp's reduction. */
    r = x - kd;
    return ldexp(exp_small(r * LN2_HI + r * LN2_LO), k);
}

double expm1(double x)
{
    /*
     * exp(x) - 1 cancels catastrophically for small x — at x = -0.28 the
     * subtraction alone throws away two bits — so the series is used out to
     * where the cancellation stops mattering, at |x| = 0.7, which is why it
     * runs to 1/20! instead of 1/13!: the term after that is 1e-23 at 0.7.
     */
    if (fabs(x) <= 0.7) {
        double p = 1.0 / 2432902008176640000.0; /* 1/20! */

        p = 1.0 / 121645100408832000.0 + x * p; /* 1/19! */
        p = 1.0 / 6402373705728000.0 + x * p;
        p = 1.0 / 355687428096000.0 + x * p;
        p = 1.0 / 20922789888000.0 + x * p;
        p = 1.0 / 1307674368000.0 + x * p;
        p = 1.0 / 87178291200.0 + x * p;
        p = 1.0 / 6227020800.0 + x * p;
        p = 1.0 / 479001600.0 + x * p;
        p = 1.0 / 39916800.0 + x * p;
        p = 1.0 / 3628800.0 + x * p;
        p = 1.0 / 362880.0 + x * p;
        p = 1.0 / 40320.0 + x * p;
        p = 1.0 / 5040.0 + x * p;
        p = 1.0 / 720.0 + x * p;
        p = 1.0 / 120.0 + x * p;
        p = 1.0 / 24.0 + x * p;
        p = 1.0 / 6.0 + x * p;
        p = 0.5 + x * p;
        return x + x * x * p;
    }
    return exp(x) - 1.0;
}

/* Split |x| into m * 2^k with m in [sqrt(0.5), sqrt(2)). */
static double log_split(double x, int *k)
{
    double m = frexp(x, k);

    if (m < SQRT_HALF) {
        m *= 2.0;
        (*k)--;
    }
    return m;
}

/*
 * log(m) for m in [sqrt(0.5), sqrt(2)), as hi + lo.
 *
 * The series is the one for atanh, which is what converges quickly for an
 * argument near one: log(m) = 2*atanh(s) with s = (m-1)/(m+1), so |s| stays
 * below 0.1716 and the term in s^19 is already past 1e-19.
 *
 * The low half is carried because without it the rounding of s alone is worth
 * about 3 ulp in log near 1 — where log's value is small and every bit of it is
 * the answer — and pow multiplies that error by its exponent. Both roundings in
 * s can be taken back out exactly: m-1 is exact for m in [0.5, 2] by Sterbenz's
 * lemma, m+1 splits exactly with two_sum, and s*(m+1) splits exactly with
 * two_product, which leaves a residual that becomes a correction to s.
 */
static void log_reduced_extended(double m, double *hi, double *lo)
{
    double num = m - 1.0; /* exact */
    double den_hi;
    double den_lo;
    double s;
    double prod_hi;
    double prod_lo;
    double residual;
    double s_lo;
    double s2;
    double series;
    double a;
    double b;

    two_sum(m, 1.0, &den_hi, &den_lo);
    s = num / den_hi;
    two_product(s, den_hi, &prod_hi, &prod_lo);
    residual = ((num - prod_hi) - prod_lo) - s * den_lo;
    s_lo = residual / den_hi;

    s2 = s * s;
    {
        /*
         * The series runs to s^43, far past the nine terms that a double's
         * worth of log needs. The reason is the low half: pow multiplies this
         * function's error by its exponent, and stopping at s^19 leaves a
         * truncation error of 8e-18 — invisible in `hi`, but it is a hundred
         * times larger than everything `lo` is supposed to be carrying, and it
         * showed up as pow being 8 ulp out. At s^43 the truncation is 2e-36.
         */
        double p = 1.0 / 43.0;
        p = 1.0 / 41.0 + s2 * p;
        p = 1.0 / 39.0 + s2 * p;
        p = 1.0 / 37.0 + s2 * p;
        p = 1.0 / 35.0 + s2 * p;
        p = 1.0 / 33.0 + s2 * p;
        p = 1.0 / 31.0 + s2 * p;
        p = 1.0 / 29.0 + s2 * p;
        p = 1.0 / 27.0 + s2 * p;
        p = 1.0 / 25.0 + s2 * p;
        p = 1.0 / 23.0 + s2 * p;
        p = 1.0 / 21.0 + s2 * p;
        p = 1.0 / 19.0 + s2 * p;
        p = 1.0 / 17.0 + s2 * p;
        p = 1.0 / 15.0 + s2 * p;
        p = 1.0 / 13.0 + s2 * p;
        p = 1.0 / 11.0 + s2 * p;
        p = 1.0 / 9.0 + s2 * p;
        p = 1.0 / 7.0 + s2 * p;
        p = 1.0 / 5.0 + s2 * p;
        p = 1.0 / 3.0 + s2 * p;
        series = s * s2 * p; /* atanh(s) - s */
    }
    two_sum(s, series, &a, &b);
    /* log(m) = 2*atanh(s) */
    *hi = 2.0 * a;
    *lo = 2.0 * (b + s_lo);
}

/* log(x) to about a hundred bits, as hi + lo. Only pow needs this. */
static void log_extended(double x, double *hi, double *lo)
{
    int k;
    double m = log_split(x, &k);
    double mh;
    double ml;
    double a;
    double b;
    double c;
    double d;

    log_reduced_extended(m, &mh, &ml);
    two_product((double)k, LN2_DD_HI, &a, &b); /* exact */
    two_sum(a, mh, &c, &d);
    *hi = c;
    *lo = ((d + b) + ml) + (double)k * LN2_DD_LO;
}

double log(double x)
{
    double hi;
    double lo;

    if (isnan(x)) {
        return x;
    }
    if (x < 0.0) {
        return nan("");
    }
    if (x == 0.0) {
        return -HUGE_VAL;
    }
    if (isinf(x)) {
        return x;
    }
    /* The extended form costs a dozen more operations and is worth it: with the
       plain one the rounding of (m-1)/(m+1) alone puts log about 3 ulp out for
       arguments near 1, where log's own value is small and every bit of it is
       the answer. */
    log_extended(x, &hi, &lo);
    return hi + lo;
}

double log2(double x)
{
    int k;
    double m;

    if (isnan(x) || isinf(x)) {
        return x < 0.0 ? nan("") : x;
    }
    if (x < 0.0) {
        return nan("");
    }
    if (x == 0.0) {
        return -HUGE_VAL;
    }
    /* The integer part comes out exactly, which is what makes log2 of a power
       of two exact. */
    m = log_split(x, &k);
    {
        double hi;
        double lo;
        log_reduced_extended(m, &hi, &lo);
        return (double)k + (hi + lo) * INV_LN2;
    }
}

double log10(double x)
{
    int k;
    double m;

    if (isnan(x) || isinf(x)) {
        return x < 0.0 ? nan("") : x;
    }
    if (x < 0.0) {
        return nan("");
    }
    if (x == 0.0) {
        return -HUGE_VAL;
    }
    m = log_split(x, &k);
    {
        double hi;
        double lo;
        log_reduced_extended(m, &hi, &lo);
        return (double)k * LOG10_2 + (hi + lo) * INV_LN10;
    }
}

double log1p(double x)
{
    if (x < -1.0) {
        return nan("");
    }
    if (x == -1.0) {
        return -HUGE_VAL;
    }
    if (x > -0.5 && x < 0.5) {
        /*
         * The problem with log(1+x) for small x is not log, it is the addition:
         * 1+x throws away the low bits of x before log ever sees them. Adding
         * back what the addition lost costs two operations and recovers all of
         * them. u-1 is exact for u in [0.5, 2] by Sterbenz's lemma, so
         * x - (u-1) is exactly the part of x that did not fit, and dividing it
         * by u turns it into the correction to log(u) — the derivative of log
         * at u is 1/u. For x small enough that u is exactly 1 this returns x
         * itself, which is the right answer to the last bit.
         */
        double u = 1.0 + x;
        double lost = x - (u - 1.0);

        return log(u) + lost / u;
    }
    return log(1.0 + x);
}

double pow(double x, double y)
{
    double ax;
    double lhi;
    double llo;
    double phi;
    double plo;
    double correction;
    double result;
    int sign = 0;
    int y_is_odd_integer = 0;

    if (y == 0.0 || x == 1.0) {
        return 1.0; /* including pow(nan, 0), as the standard says */
    }
    if (isnan(x) || isnan(y)) {
        return nan("");
    }
    if (y == 1.0) {
        return x;
    }

    if (x < 0.0) {
        double ty = trunc(y);
        if (ty != y) {
            return nan(""); /* a negative base to a fractional power */
        }
        y_is_odd_integer = fabs(fmod(ty, 2.0)) == 1.0;
        sign = y_is_odd_integer;
    }
    ax = fabs(x);

    if (isinf(y)) {
        if (ax == 1.0) {
            return 1.0;
        }
        return (ax > 1.0) == (y > 0.0) ? HUGE_VAL : 0.0;
    }
    if (ax == 0.0) {
        if (y < 0.0) {
            return sign ? -HUGE_VAL : HUGE_VAL;
        }
        return sign ? -0.0 : 0.0;
    }
    if (isinf(ax)) {
        if (y < 0.0) {
            return sign ? -0.0 : 0.0;
        }
        return sign ? -HUGE_VAL : HUGE_VAL;
    }

    /*
     * An integer exponent by repeated squaring, carrying the low half of every
     * product.
     *
     * Doing it in plain doubles is the obvious thing and it is wrong: squaring
     * doubles the relative error each time, so pow(6.987, -51) — six squarings
     * — came out 29 ulp off. Keeping each product as a double-double costs
     * about twenty operations per multiplication and brings the same case to
     * half an ulp, which also means the answer is exact whenever it is
     * representable, as anyone writing pow(x, 2) or pow(10, 3) expects.
     */
    if (y == trunc(y) && fabs(y) <= 1024.0) {
        double base_hi = ax;
        double base_lo = 0.0;
        double acc_hi = 1.0;
        double acc_lo = 0.0;
        int n = (int)fabs(y);
        int usable = 1;

        while (n > 0) {
            if (n & 1) {
                dd_multiply(acc_hi, acc_lo, base_hi, base_lo, &acc_hi, &acc_lo);
            }
            n >>= 1;
            if (n == 0) {
                break;
            }
            dd_multiply(base_hi, base_lo, base_hi, base_lo, &base_hi, &base_lo);
            if (!isfinite(base_hi) || base_hi == 0.0) {
                /* The intermediate ran out of range even though the answer may
                   not have: leave it to the logarithm. */
                usable = 0;
                break;
            }
        }
        if (usable && isfinite(acc_hi) && acc_hi != 0.0) {
            double result_int = y < 0.0 ? dd_reciprocal(acc_hi, acc_lo)
                                        : acc_hi + acc_lo;
            if (isfinite(result_int) && result_int != 0.0) {
                return sign ? -result_int : result_int;
            }
        }
    }

    /* y*log(x), with the product's low half kept: exp(a+b) = exp(a)*(1+b) for
       a b this small, which is what turns the extra bits into accuracy. */
    log_extended(ax, &lhi, &llo);
    two_product(y, lhi, &phi, &plo);
    correction = plo + y * llo;

    if (phi > 709.9) {
        return sign ? -HUGE_VAL : HUGE_VAL;
    }
    if (phi < -745.2) {
        return sign ? -0.0 : 0.0;
    }
    result = exp(phi) * (1.0 + correction);
    return sign ? -result : result;
}

/* ---- trigonometry ------------------------------------------------------ */

/* sin(r) for |r| <= pi/4, Taylor to r^17. */
static double sin_small(double r)
{
    double r2 = r * r;
    double p = -1.0 / 355687428096000.0; /* -1/17! */

    p = 1.0 / 1307674368000.0 + r2 * p;  /* 1/15! */
    p = -1.0 / 6227020800.0 + r2 * p;
    p = 1.0 / 39916800.0 + r2 * p;
    p = -1.0 / 362880.0 + r2 * p;
    p = 1.0 / 5040.0 + r2 * p;
    p = -1.0 / 120.0 + r2 * p;
    p = 1.0 / 6.0 + r2 * p;
    return r - r * r2 * p;
}

/* cos(r) for |r| <= pi/4, Taylor to r^16. */
static double cos_small(double r)
{
    double r2 = r * r;
    double p = 1.0 / 20922789888000.0; /* 1/16! */

    p = -1.0 / 87178291200.0 + r2 * p;
    p = 1.0 / 479001600.0 + r2 * p;
    p = -1.0 / 3628800.0 + r2 * p;
    p = 1.0 / 40320.0 + r2 * p;
    p = -1.0 / 720.0 + r2 * p;
    p = 1.0 / 24.0 + r2 * p;
    p = -1.0 / 2.0 + r2 * p;
    return 1.0 + r2 * p;
}

/*
 * Reduce x to r = x - n*(pi/2) with |r| around pi/4 or less, and return n & 3
 * so the caller knows which quadrant it landed in.
 *
 * The subtraction is the whole difficulty. For a large x, n*(pi/2) is nearly
 * all of x and almost everything cancels, so any error in n*(pi/2) is an error
 * in r of the same absolute size — and r is what the series then evaluates. The
 * usual answer is Cody-Waite: split pi/2 into pieces small enough that each
 * product is exact. Here the products are made exact instead, with Dekker's
 * two_product, which is why two pieces suffice where a chain would need three
 * or four: pi/2 to 105 bits, multiplied out exactly, then subtracted with
 * Knuth's two_sum, which is also exact however much cancels.
 *
 * That holds until n itself stops being exactly representable, at |x| about
 * 2^53. Past there this falls back to folding the angle against a 2*pi that is
 * only a double, which returns a number of the right size and not a meaningful
 * sine; a real answer needs Payne-Hanek reduction against a table of the bits
 * of 2/pi, which is not here.
 */
static int reduce_pi2(double x, double *r_hi, double *r_lo)
{
    double n;
    double prod_hi;
    double prod_lo;
    double sum;
    double err;
    double low;
    int64_t k;

    if (fabs(x) <= PI_QUARTER) {
        *r_hi = x;
        *r_lo = 0.0;
        return 0;
    }
    if (fabs(x) > 9.0e15) {
        x = fmod(x, 2.0 * PI);
    }

    n = round(x * TWO_OVER_PI);
    two_product(n, PIO2_HI, &prod_hi, &prod_lo); /* exact */
    two_sum(x, -prod_hi, &sum, &err);            /* exact */
    low = (err - prod_lo) - n * PIO2_LO;
    /* The low half is returned rather than added in, because rounding it into
       the high half here would put an error of half an ulp of pi/4 into the
       argument — invisible for ordinary x, but for x near 1e14 it is the whole
       error, and near a zero of the function it is tens of ulp of the answer. */
    two_sum(sum, low, r_hi, r_lo);
    k = (int64_t)n;
    return (int)(k & 3);
}

/*
 * sin and cos of a reduced argument given as hi + lo.
 *
 * sin(hi+lo) = sin(hi)cos(lo) + cos(hi)sin(lo), and lo is at most an ulp of hi,
 * so cos(lo) is 1 and sin(lo) is lo to well past the last bit: one extra
 * multiply-add each. The second series is the price, and it buys back the
 * accuracy that the reduction of a large argument would otherwise lose.
 */
static double sin_reduced(double hi, double lo)
{
    if (lo == 0.0) {
        return sin_small(hi);
    }
    return sin_small(hi) + lo * cos_small(hi);
}

static double cos_reduced(double hi, double lo)
{
    if (lo == 0.0) {
        return cos_small(hi);
    }
    return cos_small(hi) - lo * sin_small(hi);
}

double sin(double x)
{
    double hi;
    double lo;
    int q;

    if (!isfinite(x)) {
        return isnan(x) ? x : nan("");
    }
    q = reduce_pi2(x, &hi, &lo);
    switch (q) {
    case 0:
        return sin_reduced(hi, lo);
    case 1:
        return cos_reduced(hi, lo);
    case 2:
        return -sin_reduced(hi, lo);
    default:
        return -cos_reduced(hi, lo);
    }
}

double cos(double x)
{
    double hi;
    double lo;
    int q;

    if (!isfinite(x)) {
        return isnan(x) ? x : nan("");
    }
    q = reduce_pi2(x, &hi, &lo);
    switch (q) {
    case 0:
        return cos_reduced(hi, lo);
    case 1:
        return -sin_reduced(hi, lo);
    case 2:
        return -cos_reduced(hi, lo);
    default:
        return sin_reduced(hi, lo);
    }
}

double tan(double x)
{
    double hi;
    double lo;
    double s;
    double c;
    int q;

    if (!isfinite(x)) {
        return isnan(x) ? x : nan("");
    }
    q = reduce_pi2(x, &hi, &lo);
    s = sin_reduced(hi, lo);
    c = cos_reduced(hi, lo);
    /* Near a pole the quotient is large and its accuracy is limited by how
       well the reduction pinned down r, not by the series. */
    if (q & 1) {
        return -c / s;
    }
    return s / c;
}

/*
 * atan(y) for |y| <= tan(pi/8) = 0.4143, from its Taylor series:
 * atan(y) = y + y^3*(-1/3 + y^2/5 - y^4/7 + ...).
 *
 * The series is the slowest-converging one in this file, because y^2 is 0.172
 * at the end of the range rather than the 0.03 or less that the exponential and
 * trigonometric reductions leave. Twelve terms — which is what a quick reading
 * suggests — leaves an error of 3e-12, twenty thousand ulp, and that was
 * exactly the bug this length fixes: the last term below is y^51, where the
 * next one is 1e-22. A minimax polynomial would reach the same accuracy in half
 * the terms, but it would also be a table of magic numbers that cannot be
 * checked by reading.
 */
static double atan_small(double y)
{
    double y2 = y * y;
    double p = -1.0 / 51.0;

    p = 1.0 / 49.0 + y2 * p;
    p = -1.0 / 47.0 + y2 * p;
    p = 1.0 / 45.0 + y2 * p;
    p = -1.0 / 43.0 + y2 * p;
    p = 1.0 / 41.0 + y2 * p;
    p = -1.0 / 39.0 + y2 * p;
    p = 1.0 / 37.0 + y2 * p;
    p = -1.0 / 35.0 + y2 * p;
    p = 1.0 / 33.0 + y2 * p;
    p = -1.0 / 31.0 + y2 * p;
    p = 1.0 / 29.0 + y2 * p;
    p = -1.0 / 27.0 + y2 * p;
    p = 1.0 / 25.0 + y2 * p;
    p = -1.0 / 23.0 + y2 * p;
    p = 1.0 / 21.0 + y2 * p;
    p = -1.0 / 19.0 + y2 * p;
    p = 1.0 / 17.0 + y2 * p;
    p = -1.0 / 15.0 + y2 * p;
    p = 1.0 / 13.0 + y2 * p;
    p = -1.0 / 11.0 + y2 * p;
    p = 1.0 / 9.0 + y2 * p;
    p = -1.0 / 7.0 + y2 * p;
    p = 1.0 / 5.0 + y2 * p;
    p = -1.0 / 3.0 + y2 * p;
    return y + y * y2 * p;
}

double atan(double x)
{
    double ax = fabs(x);
    double r;

    if (isnan(x)) {
        return x;
    }
    if (isinf(x)) {
        return copysign(PI_HALF, x);
    }
    if (ax <= TAN_PI_8) {
        return atan_small(x);
    }
    if (ax <= TAN_3PI_8) {
        /* atan(a) = pi/4 + atan((a-1)/(a+1)) */
        r = PI_QUARTER + atan_small((ax - 1.0) / (ax + 1.0));
    } else {
        r = PI_HALF - atan_small(1.0 / ax);
    }
    return copysign(r, x);
}

double atan2(double y, double x)
{
    if (isnan(x) || isnan(y)) {
        return nan("");
    }
    if (y == 0.0) {
        if (signbit(y)) {
            return signbit(x) ? -PI : -0.0;
        }
        return signbit(x) ? PI : 0.0;
    }
    if (x == 0.0) {
        return copysign(PI_HALF, y);
    }
    if (isinf(x)) {
        if (isinf(y)) {
            double q = signbit(x) ? 3.0 * PI_QUARTER : PI_QUARTER;
            return copysign(q, y);
        }
        return signbit(x) ? copysign(PI, y) : copysign(0.0, y);
    }
    if (isinf(y)) {
        return copysign(PI_HALF, y);
    }
    if (signbit(x)) {
        /* pi is not exactly representable, so the subtraction here is where
           the second quadrant loses its last bit. */
        return copysign(PI - atan(fabs(y / x)), y);
    }
    return copysign(atan(fabs(y / x)), y);
}

/* asin(a) for 0 <= a <= 0.5, from its Taylor series. The coefficients are
   generated as they go: c_{n} = c_{n-1} * (2n-1)/(2n). */
static double asin_small(double a)
{
    double terms[32];
    double a2 = a * a;
    double coeff = 1.0;
    double power = a;
    double sum = 0.0;
    int n;
    int count = 0;

    for (n = 0; n < 30; n++) {
        if (n > 0) {
            coeff *= (double)(2 * n - 1) / (double)(2 * n);
            power *= a2;
        }
        terms[count++] = coeff * power / (double)(2 * n + 1);
    }
    /* Smallest first: the sum of the tail is then never lost against the
       leading term. */
    while (count-- > 0) {
        sum += terms[count];
    }
    return sum;
}

double asin(double x)
{
    double ax = fabs(x);
    double r;

    if (isnan(x)) {
        return x;
    }
    if (ax > 1.0) {
        return nan("");
    }
    if (ax <= 0.5) {
        return copysign(asin_small(ax), x);
    }
    /* asin(a) = pi/2 - 2*asin(sqrt((1-a)/2)), which is well conditioned right
       up to 1 where the series is not. */
    r = PI_HALF - 2.0 * asin_small(sqrt((1.0 - ax) * 0.5));
    return copysign(r, x);
}

double acos(double x)
{
    if (isnan(x)) {
        return x;
    }
    if (fabs(x) > 1.0) {
        return nan("");
    }
    if (x > 0.5) {
        return 2.0 * asin_small(sqrt((1.0 - x) * 0.5));
    }
    if (x < -0.5) {
        return PI - 2.0 * asin_small(sqrt((1.0 + x) * 0.5));
    }
    return PI_HALF - asin_small(fabs(x)) * (signbit(x) ? -1.0 : 1.0);
}

/* ---- hyperbolic -------------------------------------------------------- */

double sinh(double x)
{
    double ax = fabs(x);
    double t;

    if (!isfinite(x)) {
        return x;
    }
    if (ax > 710.0) {
        return copysign(HUGE_VAL, x);
    }
    /*
     * With t = e^x - 1, sinh(x) = t*(t+2)/(2*(t+1)), which is the same as
     * (e^x - e^-x)/2 rearranged so that nothing cancels: for a small x the
     * difference of two nearly equal exponentials loses most of its digits,
     * and this form loses none, at any x.
     */
    t = expm1(ax);
    return copysign(0.5 * (t + t / (t + 1.0)), x);
}

double cosh(double x)
{
    double ax = fabs(x);
    double t;

    if (isnan(x)) {
        return x;
    }
    if (ax > 710.0) {
        return HUGE_VAL;
    }
    t = exp(ax);
    return 0.5 * (t + 1.0 / t);
}

double tanh(double x)
{
    double ax = fabs(x);
    double t;

    if (isnan(x)) {
        return x;
    }
    if (ax > 22.0) {
        /* Beyond here tanh is one to the last bit. */
        return copysign(1.0, x);
    }
    if (ax < 0.25) {
        t = expm1(2.0 * ax);
        return copysign(t / (t + 2.0), x);
    }
    t = exp(2.0 * ax);
    return copysign((t - 1.0) / (t + 1.0), x);
}

/* ---- odds and ends ----------------------------------------------------- */

double hypot(double x, double y)
{
    double ax = fabs(x);
    double ay = fabs(y);
    double t;

    if (isinf(ax) || isinf(ay)) {
        return HUGE_VAL;
    }
    if (isnan(ax) || isnan(ay)) {
        return nan("");
    }
    if (ay > ax) {
        t = ax;
        ax = ay;
        ay = t;
    }
    if (ax == 0.0) {
        return 0.0;
    }
    /* Scaling by the larger keeps the square out of overflow and out of the
       subnormals. */
    t = ay / ax;
    return ax * sqrt(1.0 + t * t);
}

double cbrt(double x)
{
    double ax = fabs(x);
    double r;
    int e;
    int i;

    if (x == 0.0 || !isfinite(x)) {
        return x;
    }
    /* Start from an exact third of the exponent, then let Newton do the rest;
       each step doubles the correct digits, so three are plenty from a
       one-digit start. */
    (void)frexp(ax, &e);
    r = ldexp(1.0, e / 3);
    for (i = 0; i < 6; i++) {
        r = r - (r - ax / (r * r)) / 3.0;
    }
    return copysign(r, x);
}

/* ---- float forms ------------------------------------------------------- */

float fabsf(float x)
{
    return (float)fabs((double)x);
}

float sqrtf(float x)
{
    return (float)sqrt((double)x);
}

float floorf(float x)
{
    return (float)floor((double)x);
}

float ceilf(float x)
{
    return (float)ceil((double)x);
}

float roundf(float x)
{
    return (float)round((double)x);
}

float truncf(float x)
{
    return (float)trunc((double)x);
}

float fmodf(float x, float y)
{
    return (float)fmod((double)x, (double)y);
}

float powf(float x, float y)
{
    return (float)pow((double)x, (double)y);
}

float expf(float x)
{
    return (float)exp((double)x);
}

float logf(float x)
{
    return (float)log((double)x);
}

float sinf(float x)
{
    return (float)sin((double)x);
}

float cosf(float x)
{
    return (float)cos((double)x);
}

float tanf(float x)
{
    return (float)tan((double)x);
}

float atan2f(float y, float x)
{
    return (float)atan2((double)y, (double)x);
}
