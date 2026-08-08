/*
 * Freestanding <string.h> for the OS101 QuickJS build.
 *
 * memcpy, memmove, memset, memcmp and strlen are declared but never defined:
 * Rust's `compiler_builtins` ships all five under the `compiler-builtins-mem`
 * feature, which every no-std target enables, so defining them again would be
 * redundant at best. Only the four functions that compiler_builtins does not
 * cover live in src/string.c.
 *
 * This header lists nothing the vendored sources do not actually call. If a
 * future QuickJS release reaches for strncmp or strstr the build will break
 * loudly, which is the point — silently growing the shim is how a shim stops
 * being one.
 */
#ifndef OS101_SHIM_STRING_H
#define OS101_SHIM_STRING_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

void *memcpy(void *dst, const void *src, size_t n);
void *memmove(void *dst, const void *src, size_t n);
void *memset(void *dst, int c, size_t n);
int memcmp(const void *a, const void *b, size_t n);
void *memchr(const void *s, int c, size_t n);
size_t strlen(const char *s);
int strcmp(const char *a, const char *b);
int strncmp(const char *a, const char *b, size_t n);
size_t strnlen(const char *s, size_t n);
char *strchr(const char *s, int c);
char *strrchr(const char *s, int c);
char *strstr(const char *haystack, const char *needle);
char *strcpy(char *dst, const char *src);
char *strncpy(char *dst, const char *src, size_t n);
char *strcat(char *dst, const char *src);
char *strncat(char *dst, const char *src, size_t n);
char *strdup(const char *s);
char *strndup(const char *s, size_t n);
char *strerror(int errnum);

#ifdef __cplusplus
}
#endif

#endif
