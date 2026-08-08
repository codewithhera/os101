/*
 * printf/snprintf/vsnprintf for the OS101 QuickJS build.
 *
 * Written here rather than vendored from one of the embedded printf projects
 * because the contract QuickJS needs is much smaller than it first looks. The
 * engine never formats a JavaScript number through printf — dtoa.c does every
 * double-to-string conversion itself, with integer arithmetic and its own
 * correct rounding — so what reaches this file is only exception-message text
 * and the JS_DumpMemoryUsage diagnostic table. Grepping the vendored sources
 * for conversion specifiers turns up exactly d, i, u, x, X, c, s, p, f and g,
 * with the `-`, `0` and `+` flags, `*` widths and precisions, and the `l`
 * length modifier that PRId64 expands to. That is small enough to audit in one
 * sitting, and it keeps a second upstream (with its own release cadence and its
 * own configuration header) out of the tree.
 *
 * The float paths are deliberately the simple textbook ones. Their only caller
 * is a memory-usage dump printing ratios like "%0.1f", so a last-digit
 * disagreement with glibc costs nothing; the moment a JavaScript-visible string
 * depends on this code, that would stop being true, and the comment above is
 * the reason it does not.
 */
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>

/*
 * Where a stream write ends up is the Rust half's business: in the kernel it is
 * the 16550 serial port, in the host test harness it is fwrite. Either way the
 * C side only knows a stream number.
 */
void os101_shim_write_bytes(int stream, const char *buf, size_t len);

#include "shim_file.h"

/* stdout/stderr/stdin live in fileio.c */

/*
 * One sink type serves both the bounded-buffer and the stream case. `total` is
 * the count the caller gets back, and it keeps counting past the end of a full
 * buffer because that is what snprintf is specified to return.
 */
#define STAGE_SIZE 128

typedef struct {
    char *buf;
    size_t cap;
    size_t total;
    int stream;
    char stage[STAGE_SIZE];
    size_t staged;
} Sink;

static void sink_flush(Sink *s)
{
    if (s->staged > 0) {
        os101_shim_write_bytes(s->stream, s->stage, s->staged);
        s->staged = 0;
    }
}

static void sink_put(Sink *s, char c)
{
    if (s->buf != NULL) {
        if (s->total + 1 < s->cap)
            s->buf[s->total] = c;
    } else {
        s->stage[s->staged++] = c;
        if (s->staged == STAGE_SIZE)
            sink_flush(s);
    }
    s->total++;
}

static void sink_pad(Sink *s, char c, int n)
{
    while (n-- > 0)
        sink_put(s, c);
}

static void sink_str(Sink *s, const char *p, size_t n)
{
    while (n-- > 0)
        sink_put(s, *p++);
}

static void sink_finish(Sink *s)
{
    if (s->buf != NULL) {
        if (s->cap > 0) {
            size_t at = s->total < s->cap - 1 ? s->total : s->cap - 1;
            s->buf[at] = '\0';
        }
    } else {
        sink_flush(s);
    }
}

#define FLAG_LEFT 0x01
#define FLAG_ZERO 0x02
#define FLAG_PLUS 0x04
#define FLAG_SPACE 0x08
#define FLAG_ALT 0x10

typedef struct {
    unsigned flags;
    int width;
    int prec; /* negative when the format gave none */
} Spec;

static const char DIGITS_LOWER[] = "0123456789abcdef";
static const char DIGITS_UPPER[] = "0123456789ABCDEF";

/*
 * `prefix` carries the sign or the 0x that must sit inside the zero padding
 * rather than outside it, which is the one detail that makes integer padding
 * fiddly enough to be worth factoring out.
 */
static void emit_number(Sink *s, const char *digits, int ndigits,
                        const char *prefix, int nprefix, const Spec *sp)
{
    int zeros = 0;
    int width;

    if (sp->prec > ndigits)
        zeros = sp->prec - ndigits;
    else if ((sp->flags & FLAG_ZERO) && sp->prec < 0
             && !(sp->flags & FLAG_LEFT))
        zeros = sp->width - nprefix - ndigits;
    if (zeros < 0)
        zeros = 0;

    width = sp->width - nprefix - zeros - ndigits;
    if (!(sp->flags & FLAG_LEFT))
        sink_pad(s, ' ', width);
    sink_str(s, prefix, (size_t)nprefix);
    sink_pad(s, '0', zeros);
    sink_str(s, digits, (size_t)ndigits);
    if (sp->flags & FLAG_LEFT)
        sink_pad(s, ' ', width);
}

static void emit_integer(Sink *s, uint64_t mag, int negative, unsigned base,
                         int upper, const Spec *sp)
{
    const char *table = upper ? DIGITS_UPPER : DIGITS_LOWER;
    char digits[24];
    char prefix[2];
    int ndigits = 0;
    int nprefix = 0;

    /* A precision of zero prints nothing at all for a zero value. */
    if (mag == 0 && sp->prec == 0) {
        ndigits = 0;
    } else {
        do {
            digits[sizeof(digits) - 1 - ndigits] = table[mag % base];
            ndigits++;
            mag /= base;
        } while (mag != 0);
    }

    if (negative)
        prefix[nprefix++] = '-';
    else if (sp->flags & FLAG_PLUS)
        prefix[nprefix++] = '+';
    else if (sp->flags & FLAG_SPACE)
        prefix[nprefix++] = ' ';
    else if ((sp->flags & FLAG_ALT) && base == 16 && ndigits > 0) {
        prefix[nprefix++] = '0';
        prefix[nprefix++] = upper ? 'X' : 'x';
    }

    emit_number(s, digits + sizeof(digits) - ndigits, ndigits, prefix, nprefix,
                sp);
}

static void emit_pointer(Sink *s, const void *p, const Spec *sp)
{
    Spec local = *sp;
    local.flags |= FLAG_ALT;
    if (p == NULL) {
        sink_str(s, "(nil)", 5);
        return;
    }
    emit_integer(s, (uint64_t)(uintptr_t)p, 0, 16, 0, &local);
}

/*
 * Decimal powers as exact doubles up to 1e22, which is the largest power of ten
 * that is exactly representable; past that the scaling below stops being exact
 * and the caller has already switched to the exponential form.
 */
static double pow10_exact(int n)
{
    static const double table[] = {
        1e0,  1e1,  1e2,  1e3,  1e4,  1e5,  1e6,  1e7,  1e8,  1e9,  1e10,
        1e11, 1e12, 1e13, 1e14, 1e15, 1e16, 1e17, 1e18, 1e19, 1e20, 1e21,
        1e22,
    };
    if (n < 0)
        return 1.0 / pow10_exact(-n);
    if (n > 22)
        return 1e22 * pow10_exact(n - 22);
    return table[n];
}

static void emit_nonfinite(Sink *s, double v, int upper, const Spec *sp)
{
    const char *text;
    Spec local = *sp;

    if (isnan(v))
        text = upper ? "NAN" : "nan";
    else if (v < 0)
        text = upper ? "-INF" : "-inf";
    else
        text = upper ? "INF" : "inf";

    /* Zero padding never applies to inf or nan, only blank padding. */
    local.flags &= ~FLAG_ZERO;
    local.prec = -1;
    emit_number(s, text, (int)strlen(text), "", 0, &local);
}

#define MAX_FRACTION_DIGITS 320

/*
 * Peel `count` decimal digits off a fraction in [0, 1), leaving what is left of
 * it in *frac so the caller can decide how to round.
 */
static void take_fraction_digits(double *frac, char *out, int count)
{
    int i;

    for (i = 0; i < count; i++) {
        int d;
        *frac *= 10.0;
        d = (int)*frac;
        if (d < 0)
            d = 0;
        else if (d > 9)
            d = 9;
        out[i] = (char)('0' + d);
        *frac -= (double)d;
    }
}

/*
 * True when `v` sits exactly halfway between the two numbers representable with
 * `prec` digits after the decimal point.
 *
 * C rounds to nearest with ties to even, and a tie is not a rare curiosity:
 * printf("%.0f", 2.5) must be "2" and printf("%.1f", 2.25) must be "2.2". But
 * the test cannot be done by multiplying, which is the trap. 0.05 is not a
 * double — the nearest one is very slightly larger — and multiplying it by ten
 * rounds to exactly 0.5, so a remainder-based test would call it a tie and round
 * "%.1f" of it down to 0.0 where the answer is 0.1.
 *
 * Done on the bit pattern it is exact. A finite double is m * 2^e; halfway means
 * 2 * 10^prec * v is an odd integer, and since 10^prec only contributes factors
 * of two and five, that can only happen when the reduced exponent is exactly
 * -(prec + 1). Reducing m to odd first is what makes the exponent comparable.
 */
static int is_exact_tie(double v, int prec)
{
    union {
        double d;
        uint64_t bits;
    } u;
    uint64_t mantissa;
    int biased;
    int e;

    u.d = v;
    biased = (int)((u.bits >> 52) & 0x7ff);
    /* Zero, subnormals, infinities and NaNs are never treated as ties. */
    if (biased == 0 || biased == 0x7ff)
        return 0;
    mantissa = (u.bits & 0xfffffffffffffULL) | (1ULL << 52);
    e = biased - 1075;
    while ((mantissa & 1) == 0) {
        mantissa >>= 1;
        e++;
    }
    return e == -(prec + 1);
}

/*
 * Round the digits of a fixed-point rendering. `remainder` is what iterated
 * multiplication by ten left over, which is reliable everywhere except at the
 * tie that `is_exact_tie` answers properly.
 */
static int round_fixed(double v, int prec, double remainder, int last_digit)
{
    if (is_exact_tie(v, prec))
        return last_digit & 1;
    return remainder >= 0.5;
}

/*
 * The same decision for the exponential form, where the scaling is by
 * 10^(exponent - prec) and there is no equally cheap exact tie test. A remainder
 * of exactly 0.5 is taken at face value; the only caller is %e and %g in the
 * diagnostic dumps, where a last-digit disagreement with glibc changes nothing.
 */
static int round_from_remainder(double remainder, int last_digit)
{
    if (remainder > 0.5)
        return 1;
    if (remainder < 0.5)
        return 0;
    return last_digit & 1;
}

/*
 * Add one to a decimal digit string in place. Returns 1 if the carry ran off the
 * front, which means the caller's integer part has to absorb it.
 */
static int carry_into(char *digits, int count)
{
    int i = count - 1;

    while (i >= 0) {
        if (digits[i] != '9') {
            digits[i]++;
            return 0;
        }
        digits[i] = '0';
        i--;
    }
    return 1;
}

/*
 * Drop the trailing zeros of a fractional part, and the decimal point with them
 * if nothing is left after it. Only %g asks for this; %f and %e keep every digit
 * their precision called for.
 */
static int strip_trailing_zeros(char *digits, int n)
{
    int i;
    int has_point = 0;

    for (i = 0; i < n; i++) {
        if (digits[i] == '.') {
            has_point = 1;
            break;
        }
    }
    if (!has_point)
        return n;
    while (n > 0 && digits[n - 1] == '0')
        n--;
    if (n > 0 && digits[n - 1] == '.')
        n--;
    return n;
}

/* Fixed-point form: the digits of `v` rounded to `prec` places after the point. */
static void emit_fixed(Sink *s, double v, int prec, int strip, const Spec *sp)
{
    char digits[MAX_FRACTION_DIGITS + 32];
    char fraction[MAX_FRACTION_DIGITS];
    char prefix[1];
    int nprefix = 0;
    int n = 0;
    int negative = 0;
    uint64_t whole;
    double frac;

    if (signbit(v)) {
        negative = 1;
        v = -v;
    }
    if (prec > MAX_FRACTION_DIGITS)
        prec = MAX_FRACTION_DIGITS;

    whole = (uint64_t)v;
    frac = v - (double)whole;
    take_fraction_digits(&frac, fraction, prec);
    if (round_fixed(v, prec,
                    frac, prec > 0 ? fraction[prec - 1] - '0'
                                   : (int)(whole % 10))) {
        if (carry_into(fraction, prec))
            whole++;
    }

    {
        char tmp[24];
        int m = 0;
        do {
            tmp[m++] = (char)('0' + (int)(whole % 10));
            whole /= 10;
        } while (whole != 0);
        while (m-- > 0)
            digits[n++] = tmp[m];
    }

    if (prec > 0) {
        int i;
        digits[n++] = '.';
        for (i = 0; i < prec; i++)
            digits[n++] = fraction[i];
    } else if (sp->flags & FLAG_ALT) {
        digits[n++] = '.';
    }
    if (strip && !(sp->flags & FLAG_ALT))
        n = strip_trailing_zeros(digits, n);

    if (negative)
        prefix[nprefix++] = '-';
    else if (sp->flags & FLAG_PLUS)
        prefix[nprefix++] = '+';
    else if (sp->flags & FLAG_SPACE)
        prefix[nprefix++] = ' ';

    {
        Spec local = *sp;
        local.prec = -1;
        if (local.flags & FLAG_ZERO) {
            /* Reuse the integer zero-padding path by counting the whole
               rendered number as the digit run. */
            int zeros = local.width - nprefix - n;
            if (zeros > 0 && !(local.flags & FLAG_LEFT)) {
                sink_str(s, prefix, (size_t)nprefix);
                sink_pad(s, '0', zeros);
                sink_str(s, digits, (size_t)n);
                return;
            }
            local.flags &= ~FLAG_ZERO;
        }
        emit_number(s, digits, n, prefix, nprefix, &local);
    }
}

/* Decimal exponent of `v`, i.e. the e in v = d.ddd * 10^e. */
static int decimal_exponent(double v)
{
    int e = 0;

    if (v == 0.0)
        return 0;
    while (v >= 10.0) {
        v /= 10.0;
        e++;
    }
    while (v < 1.0) {
        v *= 10.0;
        e--;
    }
    return e;
}

static void emit_exponential(Sink *s, double v, int prec, int upper, int strip,
                             const Spec *sp)
{
    int negative = signbit(v);
    double mag = negative ? -v : v;
    int e = decimal_exponent(mag);
    char digits[MAX_FRACTION_DIGITS + 32];
    char fraction[MAX_FRACTION_DIGITS];
    char prefix[1];
    int nprefix = 0;
    int n = 0;
    double scaled;
    int lead;
    Spec local = *sp;

    if (prec > MAX_FRACTION_DIGITS)
        prec = MAX_FRACTION_DIGITS;

    scaled = mag == 0.0 ? 0.0 : mag / pow10_exact(e);
    lead = (int)scaled;
    if (lead > 9)
        lead = 9;
    scaled -= (double)lead;
    take_fraction_digits(&scaled, fraction, prec);
    if (round_from_remainder(scaled,
                             prec > 0 ? fraction[prec - 1] - '0' : lead)) {
        if (carry_into(fraction, prec))
            lead++;
        /* Carrying past 9 shifts the exponent: 9.99e2 rounds to 1.00e3. */
        if (lead > 9) {
            int i;
            lead = 1;
            for (i = 0; i < prec; i++)
                fraction[i] = '0';
            e++;
        }
    }

    digits[n++] = (char)('0' + lead);
    if (prec > 0) {
        int i;
        digits[n++] = '.';
        for (i = 0; i < prec; i++)
            digits[n++] = fraction[i];
    } else if (sp->flags & FLAG_ALT) {
        digits[n++] = '.';
    }
    /* Before the exponent is appended, or it would be what got stripped. */
    if (strip && !(sp->flags & FLAG_ALT))
        n = strip_trailing_zeros(digits, n);

    digits[n++] = upper ? 'E' : 'e';
    digits[n++] = e < 0 ? '-' : '+';
    {
        int ae = e < 0 ? -e : e;
        if (ae >= 100) {
            digits[n++] = (char)('0' + ae / 100);
            ae %= 100;
        }
        digits[n++] = (char)('0' + ae / 10);
        digits[n++] = (char)('0' + ae % 10);
    }

    if (negative)
        prefix[nprefix++] = '-';
    else if (sp->flags & FLAG_PLUS)
        prefix[nprefix++] = '+';
    else if (sp->flags & FLAG_SPACE)
        prefix[nprefix++] = ' ';

    local.prec = -1;
    local.flags &= ~FLAG_ZERO;
    emit_number(s, digits, n, prefix, nprefix, &local);
}

static void emit_general(Sink *s, double v, int prec, int upper, const Spec *sp)
{
    double mag = signbit(v) ? -v : v;
    int e;

    if (prec == 0)
        prec = 1;
    e = mag == 0.0 ? 0 : decimal_exponent(mag);

    /* C's rule for choosing between the two forms, verbatim. */
    if (e < -4 || e >= prec) {
        emit_exponential(s, v, prec - 1, upper, 1, sp);
    } else {
        emit_fixed(s, v, prec - 1 - e, 1, sp);
    }
}

static int read_int(const char **fmt)
{
    int n = 0;
    while (**fmt >= '0' && **fmt <= '9') {
        n = n * 10 + (**fmt - '0');
        (*fmt)++;
    }
    return n;
}

static void format(Sink *s, const char *fmt, va_list ap)
{
    while (*fmt != '\0') {
        Spec sp;
        int length = 0; /* 0 int, 1 long, 2 long long, -1 short, -2 char */
        int upper = 0;
        char conv;

        if (*fmt != '%') {
            sink_put(s, *fmt++);
            continue;
        }
        fmt++;

        sp.flags = 0;
        sp.width = 0;
        sp.prec = -1;

        for (;;) {
            if (*fmt == '-')
                sp.flags |= FLAG_LEFT;
            else if (*fmt == '0')
                sp.flags |= FLAG_ZERO;
            else if (*fmt == '+')
                sp.flags |= FLAG_PLUS;
            else if (*fmt == ' ')
                sp.flags |= FLAG_SPACE;
            else if (*fmt == '#')
                sp.flags |= FLAG_ALT;
            else
                break;
            fmt++;
        }

        if (*fmt == '*') {
            sp.width = va_arg(ap, int);
            if (sp.width < 0) {
                sp.flags |= FLAG_LEFT;
                sp.width = -sp.width;
            }
            fmt++;
        } else {
            sp.width = read_int(&fmt);
        }

        if (*fmt == '.') {
            fmt++;
            if (*fmt == '*') {
                sp.prec = va_arg(ap, int);
                fmt++;
            } else {
                sp.prec = read_int(&fmt);
            }
            if (sp.prec < 0)
                sp.prec = -1;
        }

        for (;;) {
            if (*fmt == 'l') {
                length = length == 1 ? 2 : 1;
            } else if (*fmt == 'h') {
                length = length == -1 ? -2 : -1;
            } else if (*fmt == 'z' || *fmt == 't' || *fmt == 'j') {
                length = 2;
            } else if (*fmt == 'L') {
                /* long double is not distinguished; va_arg promotes to double */
            } else {
                break;
            }
            fmt++;
        }

        conv = *fmt;
        if (conv == '\0')
            break;
        fmt++;

        switch (conv) {
        case 'd':
        case 'i': {
            int64_t v;
            if (length >= 2)
                v = va_arg(ap, long long);
            else if (length == 1)
                v = va_arg(ap, long);
            else
                v = va_arg(ap, int);
            if (length == -1)
                v = (short)v;
            else if (length == -2)
                v = (signed char)v;
            emit_integer(s, v < 0 ? (uint64_t)(-(v + 1)) + 1 : (uint64_t)v,
                         v < 0, 10, 0, &sp);
            break;
        }
        case 'u':
        case 'o':
        case 'x':
        case 'X': {
            uint64_t v;
            unsigned base = conv == 'o' ? 8u : (conv == 'u' ? 10u : 16u);
            if (length >= 2)
                v = va_arg(ap, unsigned long long);
            else if (length == 1)
                v = va_arg(ap, unsigned long);
            else
                v = va_arg(ap, unsigned int);
            if (length == -1)
                v = (unsigned short)v;
            else if (length == -2)
                v = (unsigned char)v;
            emit_integer(s, v, 0, base, conv == 'X', &sp);
            break;
        }
        case 'c': {
            char c = (char)va_arg(ap, int);
            Spec local = sp;
            local.prec = -1;
            local.flags &= ~FLAG_ZERO;
            emit_number(s, &c, 1, "", 0, &local);
            break;
        }
        case 's': {
            const char *p = va_arg(ap, const char *);
            size_t len;
            Spec local = sp;
            if (p == NULL)
                p = "(null)";
            len = strlen(p);
            if (sp.prec >= 0 && (size_t)sp.prec < len)
                len = (size_t)sp.prec;
            local.prec = -1;
            local.flags &= ~FLAG_ZERO;
            emit_number(s, p, (int)len, "", 0, &local);
            break;
        }
        case 'p':
            emit_pointer(s, va_arg(ap, const void *), &sp);
            break;
        case 'F':
        case 'E':
        case 'G':
            upper = 1;
            /* fall through */
        case 'f':
        case 'e':
        case 'g': {
            double v = va_arg(ap, double);
            char kind = conv | 0x20;
            int prec = sp.prec < 0 ? 6 : sp.prec;
            if (!isfinite(v)) {
                emit_nonfinite(s, v, upper, &sp);
            } else if (kind == 'f') {
                /*
                 * emit_fixed truncates the integer part into a uint64_t, so it
                 * can only be trusted below 2^63. Anything larger would be
                 * unreadable in fixed notation anyway, and no caller in the
                 * vendored sources gets near it.
                 */
                if (v >= 1e18 || v <= -1e18)
                    emit_exponential(s, v, prec, upper, 0, &sp);
                else
                    emit_fixed(s, v, prec, 0, &sp);
            } else if (kind == 'e') {
                emit_exponential(s, v, prec, upper, 0, &sp);
            } else {
                emit_general(s, v, prec, upper, &sp);
            }
            break;
        }
        case '%':
            sink_put(s, '%');
            break;
        default:
            /* An unknown conversion is echoed so the mistake is visible. */
            sink_put(s, '%');
            sink_put(s, conv);
            break;
        }
    }
}

int vsnprintf(char *buf, size_t size, const char *fmt, va_list ap)
{
    Sink s;
    s.buf = buf;
    s.cap = size;
    s.total = 0;
    s.stream = 0;
    s.staged = 0;
    format(&s, fmt, ap);
    sink_finish(&s);
    return (int)s.total;
}

int snprintf(char *buf, size_t size, const char *fmt, ...)
{
    va_list ap;
    int n;
    va_start(ap, fmt);
    n = vsnprintf(buf, size, fmt, ap);
    va_end(ap);
    return n;
}

int vfprintf(FILE *stream, const char *fmt, va_list ap)
{
    Sink s;
    if (stream != NULL && stream->stream < 0) {
        char stackbuf[1024];
        va_list ap2;
        int n;
        va_copy(ap2, ap);
        n = vsnprintf(stackbuf, sizeof stackbuf, fmt, ap2);
        va_end(ap2);
        if (n < 0)
            return n;
        if ((size_t)n < sizeof stackbuf) {
            if (fwrite(stackbuf, 1, (size_t)n, stream) != (size_t)n)
                return -1;
            return n;
        } else {
            char *heap = malloc((size_t)n + 1);
            if (!heap)
                return -1;
            vsnprintf(heap, (size_t)n + 1, fmt, ap);
            if (fwrite(heap, 1, (size_t)n, stream) != (size_t)n) {
                free(heap);
                return -1;
            }
            free(heap);
            return n;
        }
    }
    s.buf = NULL;
    s.cap = 0;
    s.total = 0;
    s.stream = stream == NULL ? 1 : stream->stream;
    s.staged = 0;
    format(&s, fmt, ap);
    sink_finish(&s);
    return (int)s.total;
}

int sprintf(char *buf, const char *fmt, ...)
{
    va_list ap;
    int n;
    va_start(ap, fmt);
    n = vsprintf(buf, fmt, ap);
    va_end(ap);
    return n;
}

int vsprintf(char *buf, const char *fmt, va_list ap)
{
    return vsnprintf(buf, (size_t)-1 / 2, fmt, ap);
}

int fprintf(FILE *stream, const char *fmt, ...)
{
    va_list ap;
    int n;
    va_start(ap, fmt);
    n = vfprintf(stream, fmt, ap);
    va_end(ap);
    return n;
}

int printf(const char *fmt, ...)
{
    va_list ap;
    int n;
    va_start(ap, fmt);
    n = vfprintf(os101_shim_stdout, fmt, ap);
    va_end(ap);
    return n;
}

int fputc(int c, FILE *stream)
{
    char ch = (char)c;
    if (stream != NULL && stream->stream < 0)
        return fwrite(&ch, 1, 1, stream) == 1 ? c : EOF;
    os101_shim_write_bytes(stream == NULL ? 1 : stream->stream, &ch, 1);
    return c;
}

int fputs(const char *str, FILE *stream)
{
    size_t n = strlen(str);
    if (stream != NULL && stream->stream < 0)
        return fwrite(str, 1, n, stream) == n ? 0 : EOF;
    os101_shim_write_bytes(stream == NULL ? 1 : stream->stream, str, n);
    return 0;
}

int putchar(int c)
{
    return fputc(c, os101_shim_stdout);
}

int fflush(FILE *stream)
{
    /* Every write leaves this file immediately, so there is nothing to flush. */
    (void)stream;
    return 0;
}
