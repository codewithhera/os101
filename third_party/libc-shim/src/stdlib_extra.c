/*
 * stdlib extras for TinyCC: conversions, qsort, exit, getenv, realpath.
 */
#include <ctype.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <limits.h>

extern void abort(void);

int errno;

long labs(long x)
{
    return x < 0 ? -x : x;
}

void exit(int status)
{
    (void)status;
    abort();
}

void _Exit(int status)
{
    exit(status);
}

void *calloc(size_t nmemb, size_t size)
{
    size_t n;
    void *p;
    if (nmemb && size > (size_t)-1 / nmemb) {
        errno = ENOMEM;
        return NULL;
    }
    n = nmemb * size;
    p = malloc(n);
    if (p)
        memset(p, 0, n);
    return p;
}

static int digit_val(int c)
{
    if (c >= '0' && c <= '9')
        return c - '0';
    if (c >= 'a' && c <= 'z')
        return c - 'a' + 10;
    if (c >= 'A' && c <= 'Z')
        return c - 'A' + 10;
    return -1;
}

unsigned long long strtoull(const char *nptr, char **endptr, int base)
{
    const char *s = nptr;
    unsigned long long acc = 0;
    int dig, neg = 0;
    while (*s == ' ' || *s == '\t' || *s == '\n' || *s == '\r' || *s == '\f' || *s == '\v')
        s++;
    if (*s == '+' || *s == '-') {
        neg = (*s == '-');
        s++;
    }
    if (base == 0) {
        if (s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) {
            base = 16;
            s += 2;
        } else if (s[0] == '0') {
            base = 8;
            s++;
        } else {
            base = 10;
        }
    } else if (base == 16 && s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) {
        s += 2;
    }
    for (;;) {
        dig = digit_val((unsigned char)*s);
        if (dig < 0 || dig >= base)
            break;
        acc = acc * (unsigned)base + (unsigned)dig;
        s++;
    }
    if (endptr)
        *endptr = (char *)(s == nptr ? nptr : s);
    return neg ? (unsigned long long)-(long long)acc : acc;
}

long long strtoll(const char *nptr, char **endptr, int base)
{
    return (long long)strtoull(nptr, endptr, base);
}

unsigned long strtoul(const char *nptr, char **endptr, int base)
{
    return (unsigned long)strtoull(nptr, endptr, base);
}

long strtol(const char *nptr, char **endptr, int base)
{
    return (long)strtoll(nptr, endptr, base);
}

int atoi(const char *nptr)
{
    return (int)strtol(nptr, NULL, 10);
}

long atol(const char *nptr)
{
    return strtol(nptr, NULL, 10);
}

/* Enough of strtod for TCC's needs (tokenizing floats in source). */
double strtod(const char *nptr, char **endptr)
{
    const char *s = nptr;
    double acc = 0.0, frac = 1.0;
    int neg = 0, exp = 0, eneg = 0, saw = 0;
    while (*s == ' ' || *s == '\t')
        s++;
    if (*s == '+' || *s == '-') {
        neg = (*s == '-');
        s++;
    }
    while (*s >= '0' && *s <= '9') {
        acc = acc * 10.0 + (*s - '0');
        s++;
        saw = 1;
    }
    if (*s == '.') {
        s++;
        while (*s >= '0' && *s <= '9') {
            frac *= 0.1;
            acc += (*s - '0') * frac;
            s++;
            saw = 1;
        }
    }
    if ((*s == 'e' || *s == 'E') && saw) {
        s++;
        if (*s == '+' || *s == '-') {
            eneg = (*s == '-');
            s++;
        }
        while (*s >= '0' && *s <= '9') {
            exp = exp * 10 + (*s - '0');
            s++;
        }
        {
            double p = 1.0;
            int i;
            for (i = 0; i < exp; i++)
                p *= 10.0;
            if (eneg)
                acc /= p;
            else
                acc *= p;
        }
    }
    if (endptr)
        *endptr = (char *)(saw ? s : nptr);
    return neg ? -acc : acc;
}

float strtof(const char *nptr, char **endptr)
{
    return (float)strtod(nptr, endptr);
}

long double strtold(const char *nptr, char **endptr)
{
    return (long double)strtod(nptr, endptr);
}

static void qsort_swap(char *a, char *b, size_t n)
{
    while (n--) {
        char t = *a;
        *a++ = *b;
        *b++ = t;
    }
}

void qsort(void *base, size_t nmemb, size_t size,
           int (*compar)(const void *, const void *))
{
    size_t i, j;
    char *b = (char *)base;
    if (nmemb < 2)
        return;
    /* Simple insertion sort — fine for TCC's small tables. */
    for (i = 1; i < nmemb; i++) {
        for (j = i; j > 0; j--) {
            char *p = b + (j - 1) * size;
            char *q = b + j * size;
            if (compar(p, q) <= 0)
                break;
            qsort_swap(p, q, size);
        }
    }
}

void *bsearch(const void *key, const void *base, size_t nmemb, size_t size,
              int (*compar)(const void *, const void *))
{
    const char *b = (const char *)base;
    size_t lo = 0, hi = nmemb;
    while (lo < hi) {
        size_t mid = lo + (hi - lo) / 2;
        int c = compar(key, b + mid * size);
        if (c < 0)
            hi = mid;
        else if (c > 0)
            lo = mid + 1;
        else
            return (void *)(b + mid * size);
    }
    return NULL;
}

char *getenv(const char *name)
{
    (void)name;
    return NULL;
}

char *realpath(const char *path, char *resolved)
{
    if (!path)
        return NULL;
    if (resolved) {
        strncpy(resolved, path, 4095);
        resolved[4095] = 0;
        return resolved;
    }
    return strdup(path);
}
