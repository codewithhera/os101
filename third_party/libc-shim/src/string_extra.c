/*
 * Extra string routines TinyCC needs beyond the QuickJS set.
 */
#include <stddef.h>
#include <stdlib.h>
#include <string.h>

char *strcpy(char *dst, const char *src)
{
    char *d = dst;
    while ((*d++ = *src++) != 0)
        ;
    return dst;
}

char *strncpy(char *dst, const char *src, size_t n)
{
    size_t i;
    for (i = 0; i < n && src[i]; i++)
        dst[i] = src[i];
    for (; i < n; i++)
        dst[i] = 0;
    return dst;
}

char *strcat(char *dst, const char *src)
{
    char *d = dst + strlen(dst);
    while ((*d++ = *src++) != 0)
        ;
    return dst;
}

char *strncat(char *dst, const char *src, size_t n)
{
    char *d = dst + strlen(dst);
    while (n-- && *src)
        *d++ = *src++;
    *d = 0;
    return dst;
}

int strncmp(const char *a, const char *b, size_t n)
{
    const unsigned char *pa = (const unsigned char *)a;
    const unsigned char *pb = (const unsigned char *)b;
    while (n--) {
        if (*pa != *pb)
            return (int)*pa - (int)*pb;
        if (*pa == 0)
            return 0;
        pa++;
        pb++;
    }
    return 0;
}

size_t strnlen(const char *s, size_t n)
{
    size_t i = 0;
    while (i < n && s[i])
        i++;
    return i;
}

char *strstr(const char *hay, const char *needle)
{
    size_t n;
    if (!*needle)
        return (char *)hay;
    n = strlen(needle);
    for (; *hay; hay++) {
        if (strncmp(hay, needle, n) == 0)
            return (char *)hay;
    }
    return NULL;
}

char *strdup(const char *s)
{
    size_t n = strlen(s) + 1;
    char *p = malloc(n);
    if (p)
        memcpy(p, s, n);
    return p;
}

char *strndup(const char *s, size_t n)
{
    size_t len = strnlen(s, n);
    char *p = malloc(len + 1);
    if (!p)
        return NULL;
    memcpy(p, s, len);
    p[len] = 0;
    return p;
}

void *mempcpy(void *dst, const void *src, size_t n)
{
    memcpy(dst, src, n);
    return (char *)dst + n;
}

char *strerror(int errnum)
{
    switch (errnum) {
    case 0: return "Success";
    case 2: return "No such file or directory";
    case 9: return "Bad file descriptor";
    case 12: return "Cannot allocate memory";
    case 22: return "Invalid argument";
    default: return "Unknown error";
    }
}
