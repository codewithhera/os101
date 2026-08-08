/*
 * The four string functions QuickJS needs that Rust's compiler_builtins does
 * not already provide. memcpy, memmove, memset, memcmp and strlen come from
 * there instead, so they are deliberately absent.
 *
 * These are the obvious byte-at-a-time versions. QuickJS calls strcmp only when
 * matching a handful of short keyword and property-name literals and memchr only
 * when scanning a line for a newline while building a backtrace, so a word-at-a-
 * time version would buy nothing measurable and cost the alignment edge cases
 * that come with it.
 */
#include <stddef.h>
#include <string.h>

void *memchr(const void *s, int c, size_t n)
{
    const unsigned char *p = (const unsigned char *)s;
    unsigned char needle = (unsigned char)c;

    while (n-- > 0) {
        if (*p == needle)
            return (void *)p;
        p++;
    }
    return NULL;
}

int strcmp(const char *a, const char *b)
{
    /*
     * The comparison is on unsigned bytes: the standard says so, and QuickJS
     * relies on it when it sorts atom names that can hold UTF-8 above 0x7f.
     */
    const unsigned char *pa = (const unsigned char *)a;
    const unsigned char *pb = (const unsigned char *)b;

    while (*pa != '\0' && *pa == *pb) {
        pa++;
        pb++;
    }
    return (int)*pa - (int)*pb;
}

char *strchr(const char *s, int c)
{
    char needle = (char)c;

    /* A search for '\0' finds the terminator, not nothing. */
    for (;; s++) {
        if (*s == needle)
            return (char *)s;
        if (*s == '\0')
            return NULL;
    }
}

char *strrchr(const char *s, int c)
{
    char needle = (char)c;
    const char *found = NULL;

    for (;; s++) {
        if (*s == needle)
            found = s;
        if (*s == '\0')
            break;
    }
    return (char *)found;
}
