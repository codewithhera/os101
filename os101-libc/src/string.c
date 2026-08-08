/*
 * string.h for OS101.
 *
 * The byte-at-a-time versions, except in memcpy, memmove and memset, which
 * move a word at a time once the pointers line up. Those three are worth it
 * because the compiler emits calls to them for structure assignment and array
 * initialisation, so they carry traffic no application ever asked for.
 *
 * This file has to be built with -fno-builtin (the driver in tools/os101-cc
 * passes it): otherwise a copy loop here is recognised as a memcpy and turned
 * into a call to itself.
 */
#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define WORD_SIZE ((size_t)sizeof(unsigned long))

void *memcpy(void *dst, const void *src, size_t n)
{
    unsigned char *d = (unsigned char *)dst;
    const unsigned char *s = (const unsigned char *)src;

    if ((((uintptr_t)d | (uintptr_t)s) & (WORD_SIZE - 1)) == 0) {
        while (n >= WORD_SIZE) {
            *(unsigned long *)d = *(const unsigned long *)s;
            d += WORD_SIZE;
            s += WORD_SIZE;
            n -= WORD_SIZE;
        }
    }
    while (n-- > 0) {
        *d++ = *s++;
    }
    return dst;
}

void *memmove(void *dst, const void *src, size_t n)
{
    unsigned char *d = (unsigned char *)dst;
    const unsigned char *s = (const unsigned char *)src;

    if (d == s || n == 0) {
        return dst;
    }
    if (d < s) {
        return memcpy(dst, src, n);
    }
    /* Overlapping the other way round: copy from the top down. */
    d += n;
    s += n;
    if ((((uintptr_t)d | (uintptr_t)s) & (WORD_SIZE - 1)) == 0) {
        while (n >= WORD_SIZE) {
            d -= WORD_SIZE;
            s -= WORD_SIZE;
            *(unsigned long *)d = *(const unsigned long *)s;
            n -= WORD_SIZE;
        }
    }
    while (n-- > 0) {
        *--d = *--s;
    }
    return dst;
}

void *memset(void *dst, int c, size_t n)
{
    unsigned char *d = (unsigned char *)dst;
    unsigned char byte = (unsigned char)c;

    if (((uintptr_t)d & (WORD_SIZE - 1)) == 0) {
        unsigned long pattern = 0;
        size_t i;
        for (i = 0; i < WORD_SIZE; i++) {
            pattern = (pattern << 8) | byte;
        }
        while (n >= WORD_SIZE) {
            *(unsigned long *)d = pattern;
            d += WORD_SIZE;
            n -= WORD_SIZE;
        }
    }
    while (n-- > 0) {
        *d++ = byte;
    }
    return dst;
}

int memcmp(const void *a, const void *b, size_t n)
{
    const unsigned char *pa = (const unsigned char *)a;
    const unsigned char *pb = (const unsigned char *)b;

    while (n-- > 0) {
        if (*pa != *pb) {
            return (int)*pa - (int)*pb;
        }
        pa++;
        pb++;
    }
    return 0;
}

void *memchr(const void *s, int c, size_t n)
{
    const unsigned char *p = (const unsigned char *)s;
    unsigned char needle = (unsigned char)c;

    while (n-- > 0) {
        if (*p == needle) {
            return (void *)p;
        }
        p++;
    }
    return NULL;
}

size_t strlen(const char *s)
{
    const char *p = s;

    while (*p != '\0') {
        p++;
    }
    return (size_t)(p - s);
}

size_t strnlen(const char *s, size_t max)
{
    size_t n = 0;

    while (n < max && s[n] != '\0') {
        n++;
    }
    return n;
}

char *strcpy(char *dst, const char *src)
{
    char *d = dst;

    while ((*d++ = *src++) != '\0') {
    }
    return dst;
}

char *strncpy(char *dst, const char *src, size_t n)
{
    size_t i = 0;

    while (i < n && src[i] != '\0') {
        dst[i] = src[i];
        i++;
    }
    /* The standard's odd corner: pad with NULs, and do not terminate at all
       if the source filled the buffer. */
    while (i < n) {
        dst[i++] = '\0';
    }
    return dst;
}

char *strcat(char *dst, const char *src)
{
    strcpy(dst + strlen(dst), src);
    return dst;
}

char *strncat(char *dst, const char *src, size_t n)
{
    char *end = dst + strlen(dst);
    size_t i = 0;

    while (i < n && src[i] != '\0') {
        end[i] = src[i];
        i++;
    }
    end[i] = '\0';
    return dst;
}

int strcmp(const char *a, const char *b)
{
    /* On unsigned bytes, as the standard requires: it is what makes a sort of
       UTF-8 strings come out in code-point order. */
    const unsigned char *pa = (const unsigned char *)a;
    const unsigned char *pb = (const unsigned char *)b;

    while (*pa != '\0' && *pa == *pb) {
        pa++;
        pb++;
    }
    return (int)*pa - (int)*pb;
}

int strncmp(const char *a, const char *b, size_t n)
{
    const unsigned char *pa = (const unsigned char *)a;
    const unsigned char *pb = (const unsigned char *)b;

    while (n > 0) {
        if (*pa != *pb) {
            return (int)*pa - (int)*pb;
        }
        if (*pa == '\0') {
            return 0;
        }
        pa++;
        pb++;
        n--;
    }
    return 0;
}

char *strchr(const char *s, int c)
{
    char needle = (char)c;

    /* A search for '\0' finds the terminator, not nothing. */
    for (;; s++) {
        if (*s == needle) {
            return (char *)s;
        }
        if (*s == '\0') {
            return NULL;
        }
    }
}

char *strrchr(const char *s, int c)
{
    char needle = (char)c;
    const char *found = NULL;

    for (;; s++) {
        if (*s == needle) {
            found = s;
        }
        if (*s == '\0') {
            break;
        }
    }
    return (char *)found;
}

char *strstr(const char *haystack, const char *needle)
{
    size_t n = strlen(needle);

    if (n == 0) {
        return (char *)haystack;
    }
    for (; *haystack != '\0'; haystack++) {
        if (*haystack == *needle && strncmp(haystack, needle, n) == 0) {
            return (char *)haystack;
        }
    }
    return NULL;
}

size_t strspn(const char *s, const char *accept)
{
    size_t n = 0;

    while (s[n] != '\0' && strchr(accept, s[n]) != NULL) {
        n++;
    }
    return n;
}

size_t strcspn(const char *s, const char *reject)
{
    size_t n = 0;

    while (s[n] != '\0' && strchr(reject, s[n]) == NULL) {
        n++;
    }
    return n;
}

char *strpbrk(const char *s, const char *accept)
{
    for (; *s != '\0'; s++) {
        if (strchr(accept, *s) != NULL) {
            return (char *)s;
        }
    }
    return NULL;
}

char *strtok(char *s, const char *delim)
{
    /* One saved position, as the standard describes it. There is one thread. */
    static char *saved;
    char *start;

    if (s == NULL) {
        s = saved;
    }
    if (s == NULL) {
        return NULL;
    }
    s += strspn(s, delim);
    if (*s == '\0') {
        saved = NULL;
        return NULL;
    }
    start = s;
    s = strpbrk(start, delim);
    if (s == NULL) {
        saved = NULL;
    } else {
        *s = '\0';
        saved = s + 1;
    }
    return start;
}

char *strdup(const char *s)
{
    size_t n = strlen(s) + 1;
    char *copy = (char *)malloc(n);

    if (copy == NULL) {
        return NULL;
    }
    memcpy(copy, s, n);
    return copy;
}

char *strndup(const char *s, size_t n)
{
    size_t len = strnlen(s, n);
    char *copy = (char *)malloc(len + 1);

    if (copy == NULL) {
        return NULL;
    }
    memcpy(copy, s, len);
    copy[len] = '\0';
    return copy;
}

char *strerror(int err)
{
    switch (err) {
    case 0:
        return (char *)"Success";
    case EPERM:
        return (char *)"Operation not permitted";
    case ENOENT:
        return (char *)"No such file or directory";
    case EIO:
        return (char *)"Input/output error";
    case EBADF:
        return (char *)"Bad file descriptor";
    case ENOMEM:
        return (char *)"Cannot allocate memory";
    case EACCES:
        return (char *)"Permission denied";
    case EFAULT:
        return (char *)"Bad address";
    case EEXIST:
        return (char *)"File exists";
    case ENODEV:
        return (char *)"No such device";
    case ENOTDIR:
        return (char *)"Not a directory";
    case EISDIR:
        return (char *)"Is a directory";
    case EINVAL:
        return (char *)"Invalid argument";
    case ENOSPC:
        return (char *)"No space left on device";
    case EROFS:
        return (char *)"Read-only file system";
    case EDOM:
        return (char *)"Numerical argument out of domain";
    case ERANGE:
        return (char *)"Numerical result out of range";
    case ENOSYS:
        return (char *)"Function not implemented";
    default:
        return (char *)"Unknown error";
    }
}
