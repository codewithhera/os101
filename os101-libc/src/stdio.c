/*
 * printf and the rest of stdio.
 *
 * The formatting engine is written here rather than taken from
 * third_party/libc-shim/src/printf.c. That one is right for what it does — the
 * QuickJS build's exception text and its memory-usage table — and its integer
 * paths are careful, but its %f, %e and %g scale by powers of ten in double
 * arithmetic and peel digits off one multiplication at a time. That is off in
 * the last place or two often enough that it cannot be compared byte for byte
 * against a hosted libc, which is exactly what os101-libc/tests does with
 * several hundred formats. The float paths here go through decimal.c instead,
 * which converts exactly, so the digits agree with the host's snprintf and a
 * printed value reads back as itself.
 *
 * Everything is written through one Sink, whether the destination is a buffer
 * or the console. `total` keeps counting past the end of a full buffer,
 * because that is the number snprintf has to return.
 */
#include <errno.h>
#include <math.h>
#include <os101.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "internal.h"

static FILE stdin_file = {0};
static FILE stdout_file = {1};
static FILE stderr_file = {2};

FILE *const stdin = &stdin_file;
FILE *const stdout = &stdout_file;
FILE *const stderr = &stderr_file;

/* Console writes are batched: one syscall per printf rather than one per
   character. Nothing is held across calls, so fflush has nothing to do. */
#define STAGE_SIZE 128

typedef struct {
    char *buf;
    size_t cap;   /* buffer size, including room for the terminator */
    size_t total; /* characters the format produced, buffer or no buffer */
    /* Which of the two this is has to be its own flag rather than "buf is
       NULL": snprintf(NULL, 0, ...) is a legal way to ask how long the result
       would be, and it must not print anything. */
    int to_console;
    int stream;
    size_t staged;
    char stage[STAGE_SIZE];
} Sink;

static void sink_flush(Sink *s)
{
    if (s->staged > 0) {
        os101_console_write(s->stream, s->stage, s->staged);
        s->staged = 0;
    }
}

static void sink_put(Sink *s, char c)
{
    if (s->to_console) {
        s->stage[s->staged++] = c;
        if (s->staged == STAGE_SIZE) {
            sink_flush(s);
        }
    } else if (s->buf != NULL && s->total + 1 < s->cap) {
        s->buf[s->total] = c;
    }
    s->total++;
}

static void sink_pad(Sink *s, char c, int n)
{
    while (n-- > 0) {
        sink_put(s, c);
    }
}

static void sink_write(Sink *s, const char *p, int n)
{
    while (n-- > 0) {
        sink_put(s, *p++);
    }
}

static void sink_finish(Sink *s)
{
    if (s->to_console) {
        sink_flush(s);
    } else if (s->buf != NULL && s->cap > 0) {
        size_t at = s->total < s->cap - 1 ? s->total : s->cap - 1;
        s->buf[at] = '\0';
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

/* A run of digits with something in front of it — a sign, or the 0x of %#x —
   that has to sit inside the zero padding rather than outside it. */
static void emit_padded(Sink *s, const Spec *sp, const char *prefix,
                        int nprefix, const char *body, int nbody, int zeros)
{
    int pad = sp->width - nprefix - zeros - nbody;

    if (!(sp->flags & FLAG_LEFT)) {
        sink_pad(s, ' ', pad);
    }
    sink_write(s, prefix, nprefix);
    sink_pad(s, '0', zeros);
    sink_write(s, body, nbody);
    if (sp->flags & FLAG_LEFT) {
        sink_pad(s, ' ', pad);
    }
}

/* `is_signed` says whether the conversion was one of d and i: the '+' and ' '
   flags describe the sign of a signed value and have no meaning for %u, %x or
   %o, where a hosted libc ignores them. */
static void emit_integer(Sink *s, uint64_t mag, int negative, unsigned base,
                         int upper, int is_signed, const Spec *sp)
{
    const char *table = upper ? DIGITS_UPPER : DIGITS_LOWER;
    char digits[24];
    char prefix[2];
    int ndigits = 0;
    int nprefix = 0;
    int zeros = 0;
    int nonzero = mag != 0;

    /* "%.0d" of zero prints nothing at all — the one case where a number has
       no digits. */
    if (mag != 0 || sp->prec != 0) {
        do {
            digits[sizeof(digits) - 1 - ndigits] = table[mag % base];
            ndigits++;
            mag /= base;
        } while (mag != 0);
    }

    if (negative) {
        prefix[nprefix++] = '-';
    } else if (!is_signed) {
        /* no sign at all */
    } else if (sp->flags & FLAG_PLUS) {
        prefix[nprefix++] = '+';
    } else if (sp->flags & FLAG_SPACE) {
        prefix[nprefix++] = ' ';
    }
    /* "%#x" of zero is "0", not "0x0": the alternative form adds the prefix to
       a non-zero value only. */
    if ((sp->flags & FLAG_ALT) && base == 16 && nonzero) {
        prefix[nprefix++] = '0';
        prefix[nprefix++] = upper ? 'X' : 'x';
    }

    if (sp->prec > ndigits) {
        zeros = sp->prec - ndigits;
    } else if ((sp->flags & FLAG_ZERO) && sp->prec < 0
               && !(sp->flags & FLAG_LEFT)) {
        zeros = sp->width - nprefix - ndigits;
    }
    if (zeros < 0) {
        zeros = 0;
    }
    /* %#o's leading zero is a digit, not a prefix: it only appears if the
       digits do not already start with one. */
    if ((sp->flags & FLAG_ALT) && base == 8
        && (ndigits == 0 || digits[sizeof(digits) - ndigits] != '0')
        && zeros == 0) {
        zeros = 1;
    }

    emit_padded(s, sp, prefix, nprefix, digits + sizeof(digits) - ndigits,
                ndigits, zeros);
}

static void emit_text(Sink *s, const Spec *sp, const char *text, int len)
{
    Spec local = *sp;

    /* Strings and characters take blank padding only, never zero padding. */
    local.flags &= ~FLAG_ZERO;
    emit_padded(s, &local, "", 0, text, len, 0);
}

static void emit_nonfinite(Sink *s, double v, int upper, const Spec *sp)
{
    char text[8];
    const char *word = isnan(v) ? (upper ? "NAN" : "nan")
                                : (upper ? "INF" : "inf");
    int n = 0;

    /* The sign rules are the same as for a number, including "-nan" for a NaN
       with its sign bit set, which is what both glibc and the BSDs print. Zero
       padding never applies. */
    if (signbit(v)) {
        text[n++] = '-';
    } else if (sp->flags & FLAG_PLUS) {
        text[n++] = '+';
    } else if (sp->flags & FLAG_SPACE) {
        text[n++] = ' ';
    }
    memcpy(text + n, word, 3);
    n += 3;
    emit_text(s, sp, text, n);
}

/* One digit of a rounded decimal, or '0' outside the digits that exist: past
   the end of a double's expansion every digit really is zero. */
static char digit_at(const char *digits, int ndigits, int index)
{
    if (index < 0 || index >= ndigits) {
        return '0';
    }
    return digits[index];
}

/*
 * Lay out a number that is already rounded.
 *
 * `first` is the index in `digits` of the leading integer digit, so integer
 * digit j is digits[first + j] and fraction digit i is
 * digits[first + int_digits + i]; indices outside the string are zeros. That
 * one convention covers both forms: %f asks for as many integer digits as the
 * exponent says, %e always asks for exactly one.
 */
static void emit_decimal(Sink *s, const Spec *sp, char sign,
                         const char *digits, int ndigits, int first,
                         int int_digits, int frac, int has_point,
                         const char *suffix, int nsuffix)
{
    int body = int_digits + (has_point ? 1 : 0) + frac + nsuffix;
    int pad = sp->width - body - (sign != '\0' ? 1 : 0);
    int zeros = 0;
    int i;

    if ((sp->flags & FLAG_ZERO) && !(sp->flags & FLAG_LEFT) && pad > 0) {
        zeros = pad;
        pad = 0;
    }
    if (!(sp->flags & FLAG_LEFT)) {
        sink_pad(s, ' ', pad);
    }
    if (sign != '\0') {
        sink_put(s, sign);
    }
    sink_pad(s, '0', zeros);
    for (i = 0; i < int_digits; i++) {
        sink_put(s, digit_at(digits, ndigits, first + i));
    }
    if (has_point) {
        sink_put(s, '.');
    }
    for (i = 0; i < frac; i++) {
        sink_put(s, digit_at(digits, ndigits, first + int_digits + i));
    }
    sink_write(s, suffix, nsuffix);
    if (sp->flags & FLAG_LEFT) {
        sink_pad(s, ' ', pad);
    }
}

static int exponent_suffix(char *out, int e, int upper)
{
    int n = 0;
    int mag = e < 0 ? -e : e;

    out[n++] = upper ? 'E' : 'e';
    out[n++] = e < 0 ? '-' : '+';
    if (mag >= 100) {
        out[n++] = (char)('0' + mag / 100);
        mag %= 100;
    }
    /* At least two digits, which is what C requires and what every other
       libc prints. */
    out[n++] = (char)('0' + mag / 10);
    out[n++] = (char)('0' + mag % 10);
    return n;
}

static void emit_float(Sink *s, double v, char kind, int upper, const Spec *sp)
{
    char digits[OS101_DEC_DIGITS + 2];
    char suffix[8];
    char sign = '\0';
    int prec = sp->prec < 0 ? 6 : sp->prec;
    int ndigits;
    int exp10;
    int e;
    int keep;
    int frac;
    int int_digits;
    int first;
    int has_point;
    int nsuffix = 0;
    int strip = 0;

    if (!isfinite(v)) {
        emit_nonfinite(s, v, upper, sp);
        return;
    }

    if (signbit(v)) {
        sign = '-';
    } else if (sp->flags & FLAG_PLUS) {
        sign = '+';
    } else if (sp->flags & FLAG_SPACE) {
        sign = ' ';
    }

    if (v == 0.0) {
        ndigits = 0;
        exp10 = 0;
    } else {
        ndigits = os101_dec_from_double(v, digits, &exp10);
    }

    if (kind == 'g') {
        /* %g's precision counts significant digits, and zero means one. */
        int sig = prec == 0 ? 1 : prec;
        if (ndigits > 0) {
            ndigits = os101_dec_round(digits, ndigits, sig, &exp10);
        }
        e = ndigits == 0 ? 0 : exp10 - 1;
        strip = !(sp->flags & FLAG_ALT);
        if (e < -4 || e >= sig) {
            kind = 'e';
            prec = sig - 1;
        } else {
            kind = 'f';
            prec = sig - 1 - e;
        }
        /* The digits are already rounded to `sig` places, so the rounding
           below finds nothing left to do. */
    }

    if (kind == 'e') {
        keep = prec + 1;
        if (ndigits > 0) {
            ndigits = os101_dec_round(digits, ndigits, keep, &exp10);
        }
        e = ndigits == 0 ? 0 : exp10 - 1;
        int_digits = 1;
        first = 0;
        frac = prec;
        nsuffix = exponent_suffix(suffix, e, upper);
    } else {
        keep = exp10 + prec;
        if (ndigits > 0) {
            if (keep < 0) {
                /* Smaller than half of the last place asked for: every digit
                   printed is a zero. */
                ndigits = 0;
                exp10 = 0;
            } else {
                ndigits = os101_dec_round(digits, ndigits, keep, &exp10);
            }
        }
        int_digits = exp10 > 0 ? exp10 : 1;
        first = exp10 - int_digits;
        frac = prec;
    }

    if (strip) {
        /* %g drops trailing zeros in the fraction, and the point with them. */
        while (frac > 0
               && digit_at(digits, ndigits, first + int_digits + frac - 1)
                      == '0') {
            frac--;
        }
    }
    has_point = frac > 0 || (sp->flags & FLAG_ALT) != 0;

    emit_decimal(s, sp, sign, digits, ndigits, first, int_digits, frac,
                 has_point, suffix, nsuffix);
}

static int read_int(const char **fmt)
{
    int n = 0;

    while (**fmt >= '0' && **fmt <= '9') {
        /* A width or precision beyond this is a mistake in the format, and
           clamping keeps the arithmetic below out of overflow. */
        if (n < 1000000) {
            n = n * 10 + (**fmt - '0');
        }
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
            if (*fmt == '-') {
                sp.flags |= FLAG_LEFT;
            } else if (*fmt == '0') {
                sp.flags |= FLAG_ZERO;
            } else if (*fmt == '+') {
                sp.flags |= FLAG_PLUS;
            } else if (*fmt == ' ') {
                sp.flags |= FLAG_SPACE;
            } else if (*fmt == '#') {
                sp.flags |= FLAG_ALT;
            } else {
                break;
            }
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
            /* A negative precision from `*` is as if none were given. */
            if (sp.prec < 0) {
                sp.prec = -1;
            }
        }

        for (;;) {
            if (*fmt == 'l') {
                length = length == 1 ? 2 : 1;
            } else if (*fmt == 'h') {
                length = length == -1 ? -2 : -1;
            } else if (*fmt == 'z' || *fmt == 't' || *fmt == 'j') {
                length = 2;
            } else if (*fmt == 'L') {
                /* No long double: it is passed as a double and read as one. */
            } else {
                break;
            }
            fmt++;
        }

        conv = *fmt;
        if (conv == '\0') {
            break;
        }
        fmt++;

        switch (conv) {
        case 'd':
        case 'i': {
            int64_t v;
            if (length >= 2) {
                v = va_arg(ap, long long);
            } else if (length == 1) {
                v = va_arg(ap, long);
            } else {
                v = va_arg(ap, int);
            }
            if (length == -1) {
                v = (short)v;
            } else if (length == -2) {
                v = (signed char)v;
            }
            /* Negating the most negative value in signed arithmetic is
               undefined; take the magnitude the long way round. */
            emit_integer(s, v < 0 ? ~(uint64_t)v + 1 : (uint64_t)v, v < 0, 10,
                         0, 1, &sp);
            break;
        }
        case 'u':
        case 'o':
        case 'x':
        case 'X': {
            uint64_t v;
            unsigned base = conv == 'o' ? 8u : (conv == 'u' ? 10u : 16u);
            if (length >= 2) {
                v = va_arg(ap, unsigned long long);
            } else if (length == 1) {
                v = va_arg(ap, unsigned long);
            } else {
                v = va_arg(ap, unsigned int);
            }
            if (length == -1) {
                v = (unsigned short)v;
            } else if (length == -2) {
                v = (unsigned char)v;
            }
            emit_integer(s, v, 0, base, conv == 'X', 0, &sp);
            break;
        }
        case 'c': {
            char c = (char)va_arg(ap, int);
            emit_text(s, &sp, &c, 1);
            break;
        }
        case 's': {
            const char *p = va_arg(ap, const char *);
            int len;
            if (p == NULL) {
                p = "(null)";
            }
            len = (int)(sp.prec >= 0 ? strnlen(p, (size_t)sp.prec)
                                     : strlen(p));
            emit_text(s, &sp, p, len);
            break;
        }
        case 'p': {
            const void *p = va_arg(ap, const void *);
            Spec local = sp;
            if (p == NULL) {
                emit_text(s, &sp, "(nil)", 5);
            } else {
                local.flags |= FLAG_ALT;
                local.prec = -1;
                emit_integer(s, (uint64_t)(uintptr_t)p, 0, 16, 0, 0, &local);
            }
            break;
        }
        case 'F':
        case 'E':
        case 'G':
            upper = 1;
            /* fall through */
        case 'f':
        case 'e':
        case 'g':
            emit_float(s, va_arg(ap, double), (char)(conv | 0x20), upper, &sp);
            break;
        case '%':
            sink_put(s, '%');
            break;
        default:
            /* Echo an unknown conversion rather than swallow it, so the
               mistake shows up in the output. */
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
    s.to_console = 0;
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

int vsprintf(char *buf, const char *fmt, va_list ap)
{
    return vsnprintf(buf, (size_t)-1, fmt, ap);
}

int sprintf(char *buf, const char *fmt, ...)
{
    va_list ap;
    int n;

    va_start(ap, fmt);
    n = vsnprintf(buf, (size_t)-1, fmt, ap);
    va_end(ap);
    return n;
}

int vfprintf(FILE *stream, const char *fmt, va_list ap)
{
    Sink s;

    s.buf = NULL;
    s.cap = 0;
    s.total = 0;
    s.to_console = 1;
    s.stream = stream == NULL ? 1 : stream->stream;
    s.staged = 0;
    format(&s, fmt, ap);
    sink_finish(&s);
    return (int)s.total;
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
    n = vfprintf(stdout, fmt, ap);
    va_end(ap);
    return n;
}

int vprintf(const char *fmt, va_list ap)
{
    return vfprintf(stdout, fmt, ap);
}

size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *stream)
{
    size_t total = size * nmemb;

    if (size != 0 && total / size != nmemb) {
        return 0; /* the count overflowed: write nothing */
    }
    if (total > 0) {
        os101_console_write(stream == NULL ? 1 : stream->stream,
                            (const char *)ptr, total);
    }
    return nmemb;
}

int fputs(const char *s, FILE *stream)
{
    size_t len = strlen(s);

    if (len > 0) {
        os101_console_write(stream == NULL ? 1 : stream->stream, s, len);
    }
    return 0;
}

int fputc(int c, FILE *stream)
{
    char ch = (char)c;

    os101_console_write(stream == NULL ? 1 : stream->stream, &ch, 1);
    return (unsigned char)ch;
}

int putc(int c, FILE *stream)
{
    return fputc(c, stream);
}

int putchar(int c)
{
    return fputc(c, stdout);
}

int puts(const char *s)
{
    fputs(s, stdout);
    return fputc('\n', stdout) == EOF ? EOF : 0;
}

int fflush(FILE *stream)
{
    /* Nothing is held between calls: every write reaches the console before
       the call that made it returns. */
    (void)stream;
    return 0;
}

void perror(const char *s)
{
    if (s != NULL && *s != '\0') {
        fputs(s, stderr);
        fputs(": ", stderr);
    }
    fputs(strerror(errno), stderr);
    fputc('\n', stderr);
}

/* ---- input, and files ---------------------------------------------------
 *
 * The kernel has no read syscall and no open syscall. These are here so that
 * ordinary code compiles and so that the failure says why.
 */

int fgetc(FILE *stream)
{
    (void)stream;
    errno = ENOSYS;
    return EOF;
}

int getc(FILE *stream)
{
    return fgetc(stream);
}

int getchar(void)
{
    return fgetc(stdin);
}

char *fgets(char *buf, int size, FILE *stream)
{
    (void)stream;
    if (size > 0) {
        buf[0] = '\0';
    }
    errno = ENOSYS;
    return NULL;
}

size_t fread(void *ptr, size_t size, size_t nmemb, FILE *stream)
{
    (void)ptr;
    (void)size;
    (void)nmemb;
    (void)stream;
    errno = ENOSYS;
    return 0;
}

FILE *fopen(const char *path, const char *mode)
{
    (void)path;
    (void)mode;
    errno = ENOSYS;
    return NULL;
}

int fclose(FILE *stream)
{
    (void)stream;
    errno = ENOSYS;
    return EOF;
}
