#include <stdio.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <os101.h>
#include <string.h>

static FILE stdin_file = {0};
static FILE stdout_file = {1};
static FILE stderr_file = {2};
FILE *const stdin = &stdin_file;
FILE *const stdout = &stdout_file;
FILE *const stderr = &stderr_file;

static void out_bytes(FILE *f, const char *p, size_t n)
{
    int stream = f ? f->stream : 1;
    if (stream < 0)
        stream = 1;
    (void)os101_console_write(stream, p, n);
}

int putchar(int c)
{
    char ch = (char)c;
    out_bytes(stdout, &ch, 1);
    return c;
}

int fputc(int c, FILE *f)
{
    char ch = (char)c;
    out_bytes(f, &ch, 1);
    return c;
}

int fputs(const char *s, FILE *f)
{
    out_bytes(f, s, strlen(s));
    return 0;
}

int puts(const char *s)
{
    fputs(s, stdout);
    putchar('\n');
    return 0;
}

int fflush(FILE *f)
{
    (void)f;
    return 0;
}

static void emit_uint(char *buf, size_t *pos, size_t cap, unsigned long long v, int base, int upper)
{
    char tmp[32];
    const char *dig = upper ? "0123456789ABCDEF" : "0123456789abcdef";
    int i = 0;
    if (v == 0)
        tmp[i++] = '0';
    while (v) {
        tmp[i++] = dig[v % (unsigned)base];
        v /= (unsigned)base;
    }
    while (i--) {
        if (*pos + 1 < cap)
            buf[(*pos)++] = tmp[i];
        else
            (*pos)++;
    }
}

int vsnprintf(char *buf, size_t n, const char *fmt, va_list ap)
{
    size_t pos = 0;
    for (; *fmt; fmt++) {
        if (*fmt != '%') {
            if (pos + 1 < n)
                buf[pos] = *fmt;
            pos++;
            continue;
        }
        fmt++;
        if (*fmt == '%') {
            if (pos + 1 < n)
                buf[pos] = '%';
            pos++;
            continue;
        }
        if (*fmt == 's') {
            const char *s = va_arg(ap, const char *);
            if (!s)
                s = "(null)";
            while (*s) {
                if (pos + 1 < n)
                    buf[pos] = *s;
                pos++;
                s++;
            }
        } else if (*fmt == 'c') {
            char c = (char)va_arg(ap, int);
            if (pos + 1 < n)
                buf[pos] = c;
            pos++;
        } else if (*fmt == 'd' || *fmt == 'i') {
            long long v = va_arg(ap, int);
            if (v < 0) {
                if (pos + 1 < n)
                    buf[pos] = '-';
                pos++;
                v = -v;
            }
            emit_uint(buf, &pos, n, (unsigned long long)v, 10, 0);
        } else if (*fmt == 'u') {
            emit_uint(buf, &pos, n, va_arg(ap, unsigned), 10, 0);
        } else if (*fmt == 'x') {
            emit_uint(buf, &pos, n, va_arg(ap, unsigned), 16, 0);
        } else if (*fmt == 'X') {
            emit_uint(buf, &pos, n, va_arg(ap, unsigned), 16, 1);
        } else if (*fmt == 'p') {
            unsigned long long v = (unsigned long long)(uintptr_t)va_arg(ap, void *);
            if (pos + 1 < n)
                buf[pos] = '0';
            pos++;
            if (pos + 1 < n)
                buf[pos] = 'x';
            pos++;
            emit_uint(buf, &pos, n, v, 16, 0);
        } else if (*fmt == 'l') {
            fmt++;
            if (*fmt == 'd' || *fmt == 'i') {
                long long v = va_arg(ap, long);
                if (v < 0) {
                    if (pos + 1 < n)
                        buf[pos] = '-';
                    pos++;
                    v = -v;
                }
                emit_uint(buf, &pos, n, (unsigned long long)v, 10, 0);
            } else if (*fmt == 'u') {
                emit_uint(buf, &pos, n, va_arg(ap, unsigned long), 10, 0);
            } else if (*fmt == 'x') {
                emit_uint(buf, &pos, n, va_arg(ap, unsigned long), 16, 0);
            }
        }
    }
    if (n) {
        size_t at = pos < n ? pos : n - 1;
        buf[at] = 0;
    }
    return (int)pos;
}

int snprintf(char *buf, size_t n, const char *fmt, ...)
{
    va_list ap;
    int r;
    va_start(ap, fmt);
    r = vsnprintf(buf, n, fmt, ap);
    va_end(ap);
    return r;
}

int sprintf(char *buf, const char *fmt, ...)
{
    va_list ap;
    int r;
    va_start(ap, fmt);
    r = vsnprintf(buf, (size_t)-1 / 2, fmt, ap);
    va_end(ap);
    return r;
}

int vfprintf(FILE *f, const char *fmt, va_list ap)
{
    char tmp[512];
    int n = vsnprintf(tmp, sizeof tmp, fmt, ap);
    if (n > 0)
        out_bytes(f, tmp, (size_t)n < sizeof tmp ? (size_t)n : sizeof tmp - 1);
    return n;
}

int fprintf(FILE *f, const char *fmt, ...)
{
    va_list ap;
    int n;
    va_start(ap, fmt);
    n = vfprintf(f, fmt, ap);
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
