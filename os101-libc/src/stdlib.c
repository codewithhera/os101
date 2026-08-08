/*
 * stdlib.h for OS101: conversions, sorting, the pseudo-random generator, and
 * the exit path.
 *
 * strtod goes through decimal.c and is correctly rounded — the nearest double
 * to the digits, ties to even — rather than the usual "multiply by a power of
 * ten as you go", which drifts in the last place. That matters more than it
 * sounds: a program that prints a value and reads it back should get the same
 * value, and both halves of that round trip live in this library.
 */
#include <ctype.h>
#include <errno.h>
#include <limits.h>
#include <math.h>
#include <os101.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "internal.h"

/* ---- exit -------------------------------------------------------------- */

/* The standard asks for at least 32. Both atexit and the C++ runtime's
   __cxa_atexit land here, because handlers run in reverse registration order
   regardless of which of the two registered them. */
#define MAX_EXIT_HANDLERS 64

typedef struct {
    void (*fn)(void *);
    void *arg;
    int takes_arg;
} ExitHandler;

static ExitHandler exit_handlers[MAX_EXIT_HANDLERS];
static int exit_handler_count;
static int exiting;

int os101_add_exit_handler(void (*fn)(void *), void *arg, void *dso)
{
    (void)dso; /* nothing is ever unloaded, so the owning object is moot */
    if (fn == NULL || exit_handler_count >= MAX_EXIT_HANDLERS) {
        return -1;
    }
    exit_handlers[exit_handler_count].fn = fn;
    exit_handlers[exit_handler_count].arg = arg;
    exit_handlers[exit_handler_count].takes_arg = 1;
    exit_handler_count++;
    return 0;
}

int atexit(void (*fn)(void))
{
    if (fn == NULL || exit_handler_count >= MAX_EXIT_HANDLERS) {
        return -1;
    }
    exit_handlers[exit_handler_count].fn = (void (*)(void *))fn;
    exit_handlers[exit_handler_count].arg = NULL;
    exit_handlers[exit_handler_count].takes_arg = 0;
    exit_handler_count++;
    return 0;
}

void os101_run_exit_handlers(void)
{
    /* Reverse order, and re-read the count each time round: a handler is
       allowed to register another one, and the standard says that one runs
       too. */
    while (exit_handler_count > 0) {
        ExitHandler h = exit_handlers[--exit_handler_count];
        if (h.takes_arg) {
            h.fn(h.arg);
        } else {
            ((void (*)(void))h.fn)();
        }
    }
}

void exit(int code)
{
    /* A handler that calls exit again would otherwise recurse until the stack
       runs out. */
    if (!exiting) {
        exiting = 1;
        os101_run_exit_handlers();
        os101_run_fini_array();
    }
    os101_exit_process(code);
}

void _Exit(int code)
{
    os101_exit_process(code);
}

void abort(void)
{
    fputs("abort\n", stderr);
    /* 128 + SIGABRT, the status a shell would report, even though there are no
       signals here to raise. */
    os101_exit_process(134);
}

/* ---- integer conversions ----------------------------------------------- */

static int digit_value(int c, int base)
{
    int v;

    if (isdigit(c)) {
        v = c - '0';
    } else if (isalpha(c)) {
        v = tolower(c) - 'a' + 10;
    } else {
        return -1;
    }
    return v < base ? v : -1;
}

/* The common core of strtoul and strtoull: `limit` is the largest value the
   caller's type can hold. */
static unsigned long long parse_unsigned(const char *s, char **end, int base,
                                         unsigned long long limit,
                                         int *negative_out, int *overflow_out)
{
    const char *start = s;
    unsigned long long value = 0;
    int negative = 0;
    int overflow = 0;
    int any = 0;

    while (isspace((unsigned char)*s)) {
        s++;
    }
    if (*s == '+' || *s == '-') {
        negative = *s == '-';
        s++;
    }
    if (base == 0) {
        if (s[0] == '0' && (s[1] == 'x' || s[1] == 'X')
            && digit_value((unsigned char)s[2], 16) >= 0) {
            base = 16;
            s += 2;
        } else if (s[0] == '0') {
            base = 8;
        } else {
            base = 10;
        }
    } else if (base == 16 && s[0] == '0' && (s[1] == 'x' || s[1] == 'X')
               && digit_value((unsigned char)s[2], 16) >= 0) {
        s += 2;
    }
    *negative_out = 0;
    *overflow_out = 0;
    if (base < 2 || base > 36) {
        errno = EINVAL;
        if (end != NULL) {
            *end = (char *)start;
        }
        return 0;
    }

    for (;;) {
        int d = digit_value((unsigned char)*s, base);
        if (d < 0) {
            break;
        }
        any = 1;
        if (value > (limit - (unsigned long long)d) / (unsigned long long)base) {
            overflow = 1;
        } else {
            value = value * (unsigned long long)base + (unsigned long long)d;
        }
        s++;
    }

    if (end != NULL) {
        /* No digits at all means nothing was converted, and the standard says
           the end pointer goes back to where the whole subject began. */
        *end = (char *)(any ? s : start);
    }
    if (!any) {
        return 0;
    }
    if (overflow) {
        errno = ERANGE;
        value = limit;
        *overflow_out = 1;
    }
    *negative_out = negative;
    return value;
}

long strtol(const char *s, char **end, int base)
{
    int negative = 0;
    int overflow = 0;
    unsigned long long mag =
        parse_unsigned(s, end, base, (unsigned long long)LONG_MAX + 1u,
                       &negative, &overflow);

    if (negative) {
        if (mag >= (unsigned long long)LONG_MAX + 1u) {
            if (mag > (unsigned long long)LONG_MAX + 1u) {
                errno = ERANGE;
            }
            return LONG_MIN;
        }
        return -(long)mag;
    }
    if (mag > (unsigned long long)LONG_MAX) {
        errno = ERANGE;
        return LONG_MAX;
    }
    return (long)mag;
}

long long strtoll(const char *s, char **end, int base)
{
    /* long and long long are the same width here, so one implementation
       serves both. */
    return (long long)strtol(s, end, base);
}

unsigned long strtoul(const char *s, char **end, int base)
{
    int negative = 0;
    int overflow = 0;
    unsigned long long mag =
        parse_unsigned(s, end, base, ULONG_MAX, &negative, &overflow);

    /* On overflow the answer is ULONG_MAX whatever the sign was — the negation
       below is for values that did fit, where the standard really does specify
       wrap-around, so that strtoul("-1") is ULONG_MAX. */
    if (overflow) {
        return ULONG_MAX;
    }
    return negative ? (unsigned long)(0ull - mag) : (unsigned long)mag;
}

unsigned long long strtoull(const char *s, char **end, int base)
{
    return (unsigned long long)strtoul(s, end, base);
}

int atoi(const char *s)
{
    long v = strtol(s, NULL, 10);

    if (v > INT_MAX) {
        return INT_MAX;
    }
    if (v < INT_MIN) {
        return INT_MIN;
    }
    return (int)v;
}

long atol(const char *s)
{
    return strtol(s, NULL, 10);
}

long long atoll(const char *s)
{
    return strtoll(s, NULL, 10);
}

/* ---- strtod ------------------------------------------------------------ */

/* Enough digits that the ones past the end cannot change which double is
   nearest; anything beyond is folded into a single sticky digit. */
#define STRTOD_MAX_DIGITS 780

static int match_ignoring_case(const char *s, const char *word)
{
    while (*word != '\0') {
        if (tolower((unsigned char)*s) != *word) {
            return 0;
        }
        s++;
        word++;
    }
    return 1;
}

static int hex_value(int c)
{
    if (c >= '0' && c <= '9') {
        return c - '0';
    }
    c = tolower(c);
    if (c >= 'a' && c <= 'f') {
        return c - 'a' + 10;
    }
    return -1;
}

/*
 * C99's hexadecimal form: 0x1.8p3, where the exponent is a power of two.
 *
 * No decimal conversion is involved, so this needs none of decimal.c: the
 * digits are the mantissa's bits four at a time. Sixty of them are kept, which
 * is more than a double's fifty-three, and anything past that is folded into
 * the low bit — a guard bit, so that a value exactly halfway between two
 * doubles is only rounded up if there was really something below the halfway
 * point. The one conversion at the end therefore rounds once, correctly.
 *
 * Returns 0 if what follows the 0x is not a number after all, in which case
 * the caller falls back to reading the leading "0".
 */
static int parse_hex_float(const char *s, const char **end, double *out)
{
    uint64_t mant = 0;
    int exponent = 0;
    int digits = 0;
    int seen_point = 0;
    int sticky = 0;

    while (*s != '\0') {
        int v;
        if (*s == '.' && !seen_point) {
            seen_point = 1;
            s++;
            continue;
        }
        v = hex_value((unsigned char)*s);
        if (v < 0) {
            break;
        }
        digits++;
        if ((mant >> 59) != 0) {
            /* No room for another four bits. */
            if (v != 0) {
                sticky = 1;
            }
            if (!seen_point) {
                exponent += 4;
            }
        } else {
            mant = (mant << 4) | (uint64_t)v;
            if (seen_point) {
                exponent -= 4;
            }
        }
        s++;
    }
    if (digits == 0) {
        return 0;
    }

    if (*s == 'p' || *s == 'P') {
        const char *after = s + 1;
        int sign = 1;
        int value = 0;
        int count = 0;

        if (*after == '+' || *after == '-') {
            sign = *after == '-' ? -1 : 1;
            after++;
        }
        while (isdigit((unsigned char)*after)) {
            count++;
            if (value < 100000) {
                value = value * 10 + (*after - '0');
            }
            after++;
        }
        if (count > 0) {
            exponent += sign * value;
            s = after;
        }
    }

    if (sticky) {
        mant |= 1;
    }
    *out = ldexp((double)mant, exponent);
    *end = s;
    return 1;
}

double strtod(const char *s, char **end)
{
    char digits[STRTOD_MAX_DIGITS + 2];
    const char *start = s;
    int ndigits = 0;
    int point_at = -1;
    int negative = 0;
    int seen_digit = 0;
    int sticky = 0;
    int int_digits = 0;
    int exp10;
    int status;
    double value;

    while (isspace((unsigned char)*s)) {
        s++;
    }
    if (*s == '+' || *s == '-') {
        negative = *s == '-';
        s++;
    }

    if (match_ignoring_case(s, "inf")) {
        s += 3;
        if (match_ignoring_case(s, "inity")) {
            s += 5;
        }
        if (end != NULL) {
            *end = (char *)s;
        }
        return negative ? -HUGE_VAL : HUGE_VAL;
    }
    if (match_ignoring_case(s, "nan")) {
        s += 3;
        /* An "nan(chars)" payload is accepted and ignored. */
        if (*s == '(') {
            const char *p = s + 1;
            while (*p != '\0' && *p != ')') {
                p++;
            }
            if (*p == ')') {
                s = p + 1;
            }
        }
        if (end != NULL) {
            *end = (char *)s;
        }
        return negative ? -nan("") : nan("");
    }

    if (s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) {
        const char *after = s;
        double value_hex;

        if (parse_hex_float(s + 2, &after, &value_hex)) {
            if (end != NULL) {
                *end = (char *)after;
            }
            if (isinf(value_hex)) {
                errno = ERANGE;
            }
            return negative ? -value_hex : value_hex;
        }
        /* Not a hexadecimal float after all ("0xz"): fall through, and the
           leading zero is converted on its own. */
    }

    for (;;) {
        if (isdigit((unsigned char)*s)) {
            seen_digit = 1;
            if (ndigits < STRTOD_MAX_DIGITS) {
                digits[ndigits++] = *s;
            } else if (*s != '0') {
                sticky = 1;
            }
            if (point_at < 0) {
                int_digits++;
            }
            s++;
        } else if (*s == '.' && point_at < 0) {
            point_at = ndigits;
            s++;
        } else {
            break;
        }
    }

    if (!seen_digit) {
        if (end != NULL) {
            *end = (char *)start;
        }
        return 0.0;
    }

    /* value = 0.digits * 10^int_digits, before the exponent. */
    exp10 = int_digits;

    if (*s == 'e' || *s == 'E') {
        const char *after = s + 1;
        int esign = 1;
        int evalue = 0;
        int edigits = 0;

        if (*after == '+' || *after == '-') {
            esign = *after == '-' ? -1 : 1;
            after++;
        }
        while (isdigit((unsigned char)*after)) {
            edigits++;
            if (evalue < 100000) {
                evalue = evalue * 10 + (*after - '0');
            }
            after++;
        }
        if (edigits > 0) {
            /* An exponent this far out is decided by the range check in
               decimal.c, so clamping it keeps the arithmetic in range without
               changing the answer. */
            if (evalue > 100000) {
                evalue = 100000;
            }
            exp10 += esign * evalue;
            if (exp10 > 100000) {
                exp10 = 100000;
            } else if (exp10 < -100000) {
                exp10 = -100000;
            }
            s = after;
        }
        /* "1e" with no digits after it: the 'e' is not part of the number. */
    }

    if (sticky) {
        digits[ndigits++] = '1';
    }

    if (end != NULL) {
        *end = (char *)s;
    }

    /* Clamp before decimal.c so its big integers stay inside their capacity;
       either side of this range the answer is only ever zero or infinity. */
    if (exp10 > 400) {
        errno = ERANGE;
        return negative ? -HUGE_VAL : HUGE_VAL;
    }
    if (exp10 < -400) {
        errno = ERANGE;
        return negative ? -0.0 : 0.0;
    }

    value = os101_dec_to_double(digits, ndigits, exp10, negative, &status);
    if (status != 0) {
        errno = ERANGE;
    }
    return value;
}

float strtof(const char *s, char **end)
{
    /* Rounded twice — to double and then to float — which can differ from a
       single rounding in the last bit for values almost exactly halfway
       between two floats. Applications here use doubles; this exists so that
       code which asks for a float compiles and gets a sensible answer. */
    return (float)strtod(s, end);
}

double atof(const char *s)
{
    return strtod(s, NULL);
}

/* ---- arithmetic -------------------------------------------------------- */

int abs(int v)
{
    return v < 0 ? -v : v;
}

long labs(long v)
{
    return v < 0 ? -v : v;
}

long long llabs(long long v)
{
    return v < 0 ? -v : v;
}

div_t div(int num, int den)
{
    div_t r;

    r.quot = num / den;
    r.rem = num % den;
    return r;
}

ldiv_t ldiv(long num, long den)
{
    ldiv_t r;

    r.quot = num / den;
    r.rem = num % den;
    return r;
}

/* ---- searching and sorting -------------------------------------------- */

void *bsearch(const void *key, const void *base, size_t nmemb, size_t size,
              int (*cmp)(const void *, const void *))
{
    size_t lo = 0;
    size_t hi = nmemb;

    while (lo < hi) {
        size_t mid = lo + (hi - lo) / 2;
        const char *at = (const char *)base + mid * size;
        int c = cmp(key, at);

        if (c == 0) {
            return (void *)at;
        }
        if (c < 0) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    return NULL;
}

static void swap_bytes(char *a, char *b, size_t size)
{
    while (size-- > 0) {
        char t = *a;
        *a++ = *b;
        *b++ = t;
    }
}

/*
 * Quicksort with a median-of-three pivot, insertion sort for short runs, and
 * recursion only into the smaller partition — the larger one is iterated. That
 * last detail is what bounds the stack at log2(n) frames, which matters on a
 * 256 KiB userspace stack.
 */
static void quicksort(char *base, size_t nmemb, size_t size,
                      int (*cmp)(const void *, const void *))
{
    while (nmemb > 8) {
        char *lo = base;
        char *hi = base + (nmemb - 1) * size;
        char *mid = base + (nmemb / 2) * size;
        char *i;
        char *j;
        size_t left;
        size_t right;

        /* Median of three, moved to the front to be the pivot. */
        if (cmp(mid, lo) < 0) {
            swap_bytes(mid, lo, size);
        }
        if (cmp(hi, mid) < 0) {
            swap_bytes(hi, mid, size);
            if (cmp(mid, lo) < 0) {
                swap_bytes(mid, lo, size);
            }
        }
        swap_bytes(base, mid, size);

        i = base;
        j = hi + size;
        for (;;) {
            do {
                i += size;
            } while (i <= hi && cmp(i, base) < 0);
            do {
                j -= size;
            } while (cmp(j, base) > 0);
            if (i > j) {
                break;
            }
            swap_bytes(i, j, size);
        }
        swap_bytes(base, j, size);

        left = (size_t)(j - base) / size;
        right = nmemb - left - 1;
        if (left < right) {
            quicksort(base, left, size, cmp);
            base = j + size;
            nmemb = right;
        } else {
            quicksort(j + size, right, size, cmp);
            nmemb = left;
        }
    }

    /* Short run: insertion sort, which is faster here and finishes the job. */
    {
        size_t k;
        for (k = 1; k < nmemb; k++) {
            char *at = base + k * size;
            while (at > base && cmp(at - size, at) > 0) {
                swap_bytes(at - size, at, size);
                at -= size;
            }
        }
    }
}

void qsort(void *base, size_t nmemb, size_t size,
           int (*cmp)(const void *, const void *))
{
    if (nmemb < 2 || size == 0) {
        return;
    }
    quicksort((char *)base, nmemb, size, cmp);
}

/* ---- pseudo-random numbers -------------------------------------------- */

/*
 * xorshift64*, which passes the tests an LCG of this size fails and is four
 * instructions longer. The initial state is exactly what srand(1) computes,
 * because rand() before any srand() has to behave as if srand(1) had been
 * called; zero is avoided because xorshift is stuck there.
 */
#define RAND_SEED(seed) \
    ((uint64_t)(seed) * 6364136223846793005ULL + 1442695040888963407ULL)

static uint64_t rand_state = RAND_SEED(1);

void srand(unsigned seed)
{
    rand_state = RAND_SEED(seed);
    if (rand_state == 0) {
        rand_state = 0x2545f4914f6cdd1dULL;
    }
}

int rand(void)
{
    uint64_t x = rand_state;

    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    rand_state = x;
    /* The high bits of the multiplied state are the good ones. */
    return (int)((x * 0x2545f4914f6cdd1dULL) >> 33);
}

/* ---- environment ------------------------------------------------------- */

char *getenv(const char *name)
{
    /* There is no environment: the kernel starts a process with argc 0 and no
       strings at all. */
    (void)name;
    return NULL;
}
