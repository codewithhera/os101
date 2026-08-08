/*
 * Exact conversion between doubles and decimal digit strings.
 *
 * printf's %f, %e and %g and stdlib's strtod both need arithmetic that a
 * double cannot do on itself. The usual shortcut — scale by a power of ten and
 * peel digits off with repeated multiplication — is wrong in the last place
 * often enough to be visible: it disagrees with a hosted libc, a printed value
 * no longer reads back as itself, and the disagreement is worst exactly where
 * someone is debugging something else.
 *
 * So both directions are done exactly here, with a fixed-capacity big integer.
 * A double is m * 2^e with m and e integers, which means:
 *
 *   - Its decimal expansion is finite. For e >= 0 the value is the integer
 *     m << e. For e < 0 it is m * 5^-e with the point -e digits from the
 *     right, because 1/2^k = 5^k/10^k. Either way the digits come out of one
 *     big integer by repeated division, with nothing rounded.
 *
 *   - Comparing a candidate double against a decimal literal is a comparison
 *     of two integers once both sides are multiplied up by the powers they are
 *     missing. That makes the nearest double to a decimal string decidable:
 *     guess, then walk to the neighbour and compare against the midpoint. The
 *     result is correctly rounded, ties to even, like a hosted strtod.
 *
 * The capacity below is what bounds the work. The widest thing asked of it is
 * the comparison in dec_to_double: about 5100 bits, for a 800-digit literal
 * with a 10^-400 exponent against a subnormal candidate. 320 limbs is 10240
 * bits, close to twice that.
 */
#include "internal.h"

#include <math.h>
#include <stdint.h>
#include <string.h>

#define BIG_LIMBS 320

typedef struct {
    int n; /* limbs in use, least significant first; 0 means zero */
    uint32_t d[BIG_LIMBS];
} Big;

static void big_set_u64(Big *a, uint64_t v)
{
    a->n = 0;
    while (v != 0) {
        a->d[a->n++] = (uint32_t)v;
        v >>= 32;
    }
}

/* Multiply by a value that fits a limb. Growth past the capacity cannot
   happen for the inputs this file allows (see the header comment), and
   dropping the carry rather than corrupting the array is the safe way to be
   wrong if it ever does. */
static void big_mul_small(Big *a, uint32_t m)
{
    uint64_t carry = 0;
    int i;

    for (i = 0; i < a->n; i++) {
        uint64_t t = (uint64_t)a->d[i] * m + carry;
        a->d[i] = (uint32_t)t;
        carry = t >> 32;
    }
    while (carry != 0 && a->n < BIG_LIMBS) {
        a->d[a->n++] = (uint32_t)carry;
        carry >>= 32;
    }
}

static void big_add_small(Big *a, uint32_t v)
{
    uint64_t carry = v;
    int i = 0;

    while (carry != 0 && i < BIG_LIMBS) {
        uint64_t t = (i < a->n ? (uint64_t)a->d[i] : 0) + carry;
        a->d[i] = (uint32_t)t;
        carry = t >> 32;
        if (i >= a->n) {
            a->n = i + 1;
        }
        i++;
    }
}

static void big_shl(Big *a, int bits)
{
    int limbs = bits / 32;
    int rest = bits % 32;
    int i;

    if (a->n == 0 || bits <= 0) {
        return;
    }
    if (rest != 0) {
        uint32_t carry = 0;
        for (i = 0; i < a->n; i++) {
            uint32_t v = a->d[i];
            a->d[i] = (v << rest) | carry;
            carry = v >> (32 - rest);
        }
        if (carry != 0 && a->n < BIG_LIMBS) {
            a->d[a->n++] = carry;
        }
    }
    if (limbs != 0) {
        int n = a->n + limbs;
        if (n > BIG_LIMBS) {
            n = BIG_LIMBS;
        }
        for (i = n - 1; i >= limbs; i--) {
            a->d[i] = a->d[i - limbs];
        }
        for (i = 0; i < limbs && i < n; i++) {
            a->d[i] = 0;
        }
        a->n = n;
    }
}

/* 5^13 is the largest power of five that fits a limb, so thirteen at a time
   is the cheapest way to reach the 5^1074 a subnormal needs. */
static void big_mul_pow5(Big *a, int k)
{
    static const uint32_t pow5[14] = {
        1u,        5u,        25u,        125u,        625u,       3125u,
        15625u,    78125u,    390625u,    1953125u,    9765625u,   48828125u,
        244140625u, 1220703125u
    };

    while (k >= 13) {
        big_mul_small(a, pow5[13]);
        k -= 13;
    }
    if (k > 0) {
        big_mul_small(a, pow5[k]);
    }
}

static void big_mul_pow10(Big *a, int k)
{
    if (k <= 0) {
        return;
    }
    big_mul_pow5(a, k);
    big_shl(a, k);
}

static uint32_t big_divmod_small(Big *a, uint32_t m)
{
    uint64_t rem = 0;
    int i;

    for (i = a->n - 1; i >= 0; i--) {
        uint64_t cur = (rem << 32) | a->d[i];
        a->d[i] = (uint32_t)(cur / m);
        rem = cur % m;
    }
    while (a->n > 0 && a->d[a->n - 1] == 0) {
        a->n--;
    }
    return (uint32_t)rem;
}

static int big_cmp(const Big *a, const Big *b)
{
    int i;

    if (a->n != b->n) {
        return a->n < b->n ? -1 : 1;
    }
    for (i = a->n - 1; i >= 0; i--) {
        if (a->d[i] != b->d[i]) {
            return a->d[i] < b->d[i] ? -1 : 1;
        }
    }
    return 0;
}

static void big_from_digits(Big *a, const char *digits, int ndigits)
{
    int i = 0;

    a->n = 0;
    while (i < ndigits) {
        uint32_t chunk = 0;
        int take = ndigits - i < 9 ? ndigits - i : 9;
        int scale = 1;
        int j;

        for (j = 0; j < take; j++) {
            chunk = chunk * 10u + (uint32_t)(digits[i + j] - '0');
            scale *= 10;
        }
        big_mul_small(a, (uint32_t)scale);
        big_add_small(a, chunk);
        i += take;
    }
}

/* Decimal digits of `a`, most significant first, consuming it. */
static int big_to_digits(Big *a, char *out, int cap)
{
    char tmp[OS101_DEC_DIGITS + 16];
    int n = 0;
    int i;

    if (a->n == 0) {
        out[0] = '0';
        return 1;
    }
    while (a->n > 0 && n + 9 <= (int)sizeof(tmp)) {
        uint32_t r = big_divmod_small(a, 1000000000u);
        for (i = 0; i < 9; i++) {
            tmp[n++] = (char)('0' + (int)(r % 10u));
            r /= 10u;
        }
    }
    while (n > 1 && tmp[n - 1] == '0') {
        n--;
    }
    /* tmp holds the digits least significant first. Keeping the most
       significant `cap` of them is the only truncation that could ever make
       sense; the capacities here are chosen so it does not happen. */
    {
        int total = n;
        if (n > cap) {
            n = cap;
        }
        for (i = 0; i < n; i++) {
            out[i] = tmp[total - 1 - i];
        }
    }
    return n;
}

/* ---- doubles as (mantissa, exponent) ---------------------------------- */

static uint64_t double_bits(double v)
{
    uint64_t u;
    memcpy(&u, &v, sizeof(u));
    return u;
}

static double bits_double(uint64_t u)
{
    double v;
    memcpy(&v, &u, sizeof(v));
    return v;
}

/* Split the magnitude of a finite double into mant * 2^exp2 with mant an
   integer. Subnormals share the smallest exponent, which is what makes this
   two cases rather than one. */
static void split_double(double v, uint64_t *mant, int *exp2)
{
    uint64_t u = double_bits(v) & 0x7fffffffffffffffULL;
    int e = (int)(u >> 52);
    uint64_t frac = u & 0xfffffffffffffULL;

    if (e == 0) {
        *mant = frac;
        *exp2 = -1074;
    } else {
        *mant = frac | (1ULL << 52);
        *exp2 = e - 1075;
    }
}

int os101_dec_from_double(double v, char *digits, int *exp10)
{
    Big a;
    uint64_t mant;
    int exp2;
    int frac_digits;
    int len;

    split_double(v, &mant, &exp2);
    if (mant == 0) {
        digits[0] = '0';
        *exp10 = 1;
        return 1;
    }

    big_set_u64(&a, mant);
    if (exp2 >= 0) {
        big_shl(&a, exp2);
        frac_digits = 0;
    } else {
        /* v = mant / 2^k = mant * 5^k / 10^k */
        big_mul_pow5(&a, -exp2);
        frac_digits = -exp2;
    }

    len = big_to_digits(&a, digits, OS101_DEC_DIGITS);
    /* The digits are the integer  value * 10^frac_digits, so the point sits
       frac_digits from the right of a len-digit number. */
    *exp10 = len - frac_digits;
    while (len > 1 && digits[len - 1] == '0') {
        len--;
    }
    return len;
}

int os101_dec_round(char *digits, int ndigits, int keep, int *exp10)
{
    int roundup;
    int i;

    if (keep >= ndigits) {
        return ndigits;
    }
    if (keep < 0) {
        keep = 0;
    }

    if (digits[keep] > '5') {
        roundup = 1;
    } else if (digits[keep] < '5') {
        roundup = 0;
    } else {
        roundup = 0;
        for (i = keep + 1; i < ndigits; i++) {
            if (digits[i] != '0') {
                roundup = 1;
                break;
            }
        }
        if (!roundup) {
            /* Exactly half: round to the even digit. A hosted libc does the
               same, because the tie is resolved in the current rounding mode
               and that mode is round-to-nearest-even. */
            int prev = keep == 0 ? 0 : digits[keep - 1] - '0';
            roundup = prev & 1;
        }
    }

    if (!roundup) {
        return keep;
    }

    for (i = keep - 1; i >= 0; i--) {
        if (digits[i] != '9') {
            digits[i]++;
            return keep;
        }
        digits[i] = '0';
    }

    /* Carried out of the leading digit: 0.999 -> 0.100 one decade up. */
    digits[0] = '1';
    for (i = 1; i < keep; i++) {
        digits[i] = '0';
    }
    *exp10 += 1;
    return keep == 0 ? 1 : keep;
}

/* ---- decimal to double ------------------------------------------------- */

/* Compare mant * 2^exp2 against 0.digits * 10^exp10. Both are positive.
   Returns -1, 0 or 1 for less, equal, greater. */
static int cmp_scaled(uint64_t mant, int exp2, const char *digits, int ndigits,
                      int exp10)
{
    Big a;
    Big b;
    int p10 = exp10 - ndigits; /* 0.digits * 10^exp10 == D * 10^p10 */

    big_set_u64(&a, mant);
    big_from_digits(&b, digits, ndigits);

    if (p10 > 0) {
        big_mul_pow10(&b, p10);
    } else if (p10 < 0) {
        big_mul_pow10(&a, -p10);
    }
    if (exp2 > 0) {
        big_shl(&a, exp2);
    } else if (exp2 < 0) {
        big_shl(&b, -exp2);
    }
    return big_cmp(&a, &b);
}

static int cmp_double(double v, const char *digits, int ndigits, int exp10)
{
    uint64_t mant;
    int exp2;

    split_double(v, &mant, &exp2);
    if (mant == 0) {
        /* Zero is below any value with a non-zero leading digit. */
        return ndigits > 0 && digits[0] != '0' ? -1 : 0;
    }
    return cmp_scaled(mant, exp2, digits, ndigits, exp10);
}

static const double POW10[23] = {
    1e0,  1e1,  1e2,  1e3,  1e4,  1e5,  1e6,  1e7,  1e8,  1e9,  1e10, 1e11,
    1e12, 1e13, 1e14, 1e15, 1e16, 1e17, 1e18, 1e19, 1e20, 1e21, 1e22
};

/* A first approximation, good to a handful of units in the last place: the
   leading digits scaled by 10^k in the largest exactly representable steps.
   Each step rounds once, and the sequence approaches the answer from one side,
   so nothing overflows or underflows on the way that would not have anyway. */
static double scale_pow10(double v, int k)
{
    while (k >= 22) {
        v *= POW10[22];
        k -= 22;
    }
    if (k > 0) {
        v *= POW10[k];
    }
    while (k <= -22) {
        v /= POW10[22];
        k += 22;
    }
    if (k < 0) {
        v /= POW10[-k];
    }
    return v;
}

double os101_dec_to_double(const char *digits, int ndigits, int exp10,
                           int negative, int *status)
{
    uint64_t bits;
    uint64_t lead = 0;
    double guess;
    double result;
    int used = 0;
    int iter;

    *status = 0;

    /* Normalise: no leading zeros, so 0.digits is in [0.1, 1) and exp10 is
       the value's decimal magnitude. */
    while (ndigits > 0 && digits[0] == '0') {
        digits++;
        ndigits--;
        exp10--;
    }
    while (ndigits > 0 && digits[ndigits - 1] == '0') {
        ndigits--;
    }
    if (ndigits == 0) {
        return negative ? -0.0 : 0.0;
    }

    /* Far outside the range of a double, and far enough outside that the big
       integers below would need more room than they have. */
    if (exp10 > 400) {
        *status = 1;
        return negative ? -HUGE_VAL : HUGE_VAL;
    }
    if (exp10 < -400) {
        *status = -1;
        return negative ? -0.0 : 0.0;
    }

    /* Nineteen digits is as many as fit a uint64_t. Taking all of them matters
       even though a double cannot hold them all: the walk below starts from
       this value and steps one double at a time, so a guess built from fifteen
       digits — a relative error of 1e-15, which is dozens of ulp — would need
       dozens of steps and, at the old limit, sometimes never arrived. */
    while (used < ndigits && used < 19) {
        lead = lead * 10u + (uint64_t)(digits[used] - '0');
        used++;
    }
    /* When the digits fit a double exactly and the power of ten is one of the
       exactly representable ones, a single multiply or divide is correctly
       rounded and there is nothing more to do. */
    if (used == ndigits && lead <= (1ULL << 53)) {
        int k = exp10 - used;
        if (k >= -22 && k <= 22) {
            double exact = k >= 0 ? (double)lead * POW10[k]
                                  : (double)lead / POW10[-k];
            return negative ? -exact : exact;
        }
    }

    guess = scale_pow10((double)lead, exp10 - used);
    if (isinf(guess)) {
        /* Step back to the largest finite double and let the walk below
           decide whether the value really is out of range. */
        guess = 1.7976931348623157e308;
    }
    bits = double_bits(guess) & 0x7fffffffffffffffULL;

    /* Walk to the neighbouring double until the value is bracketed. The guess
       is within a few units in the last place — nineteen digits scaled by at
       most fifteen exact powers of ten — so this is a handful of steps; the
       bound only exists so that a mistake cannot become a hang. */
    for (iter = 0; iter < 300; iter++) {
        int here = cmp_double(bits_double(bits), digits, ndigits, exp10);
        uint64_t other;
        int there;

        if (here == 0) {
            break;
        }
        if (here < 0) {
            if (bits >= 0x7ff0000000000000ULL - 1) {
                *status = 1;
                bits = 0x7ff0000000000000ULL;
                break;
            }
            other = bits + 1;
        } else {
            if (bits == 0) {
                break;
            }
            other = bits - 1;
        }

        there = cmp_double(bits_double(other), digits, ndigits, exp10);
        if (there == 0) {
            bits = other;
            break;
        }
        if ((here < 0 && there > 0) || (here > 0 && there < 0)) {
            /* Bracketed by two adjacent doubles: the answer is whichever of
               them the value is nearer, and the midpoint decides it exactly.
               For adjacent doubles the gap is one unit in the last place of
               the smaller, so the midpoint is (2m+1) * 2^(e-1). */
            uint64_t lo_bits = here < 0 ? bits : other;
            uint64_t hi_bits = here < 0 ? other : bits;
            uint64_t mant;
            int exp2;
            int side;

            split_double(bits_double(lo_bits), &mant, &exp2);
            side = cmp_scaled(2 * mant + 1, exp2 - 1, digits, ndigits, exp10);
            if (side < 0) {
                bits = hi_bits; /* midpoint below the value */
            } else if (side > 0) {
                bits = lo_bits;
            } else {
                /* Exactly halfway: to even, which is the low bit of the
                   significand being clear. */
                bits = (lo_bits & 1) == 0 ? lo_bits : hi_bits;
            }
            break;
        }
        bits = other;
    }

    if (bits >= 0x7ff0000000000000ULL) {
        *status = 1;
        bits = 0x7ff0000000000000ULL;
    } else if (bits == 0) {
        *status = -1;
    }

    result = bits_double(bits);
    return negative ? -result : result;
}
