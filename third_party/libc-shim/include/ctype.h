/*
 * Freestanding <ctype.h> for the OS101 QuickJS build.
 *
 * These are static inline and ASCII-only on purpose. QuickJS calls them while
 * scanning number literals, where a locale-sensitive answer would be a bug
 * rather than a feature, and inlining them keeps them out of the symbol table.
 */
#ifndef OS101_SHIM_CTYPE_H
#define OS101_SHIM_CTYPE_H

static inline int isdigit(int c) { return c >= '0' && c <= '9'; }
static inline int isupper(int c) { return c >= 'A' && c <= 'Z'; }
static inline int islower(int c) { return c >= 'a' && c <= 'z'; }
static inline int isalpha(int c) { return isupper(c) || islower(c); }
static inline int isalnum(int c) { return isalpha(c) || isdigit(c); }
static inline int isxdigit(int c)
{
    return isdigit(c) || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F');
}
static inline int isspace(int c)
{
    return c == ' ' || c == '\t' || c == '\n' || c == '\v' || c == '\f'
        || c == '\r';
}
static inline int isprint(int c) { return c >= 0x20 && c < 0x7f; }
static inline int isgraph(int c) { return c > 0x20 && c < 0x7f; }
static inline int iscntrl(int c) { return c < 0x20 || c == 0x7f; }
static inline int ispunct(int c) { return isgraph(c) && !isalnum(c); }
static inline int toupper(int c) { return islower(c) ? c - 32 : c; }
static inline int tolower(int c) { return isupper(c) ? c + 32 : c; }

#endif
