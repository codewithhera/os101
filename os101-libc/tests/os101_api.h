/*
 * What the tests call.
 *
 * The test files include the *host's* headers, so they can compare against the
 * host's snprintf, strtod and libm; they cannot also include os101-libc's
 * headers, because the two would collide. So the library's entry points are
 * declared here under the os101_ names that host_names.h gives them.
 */
#ifndef OS101_LIBC_TESTS_API_H
#define OS101_LIBC_TESTS_API_H

#include <stdarg.h>
#include <stddef.h>

/* stdio */
int os101_snprintf(char *buf, size_t size, const char *fmt, ...);
int os101_vsnprintf(char *buf, size_t size, const char *fmt, va_list ap);
int os101_sprintf(char *buf, const char *fmt, ...);
int os101_printf(const char *fmt, ...);

/* string */
void *os101_memcpy(void *dst, const void *src, size_t n);
void *os101_memmove(void *dst, const void *src, size_t n);
void *os101_memset(void *dst, int c, size_t n);
int os101_memcmp(const void *a, const void *b, size_t n);
void *os101_memchr(const void *s, int c, size_t n);
size_t os101_strlen(const char *s);
size_t os101_strnlen(const char *s, size_t max);
char *os101_strcpy(char *dst, const char *src);
char *os101_strncpy(char *dst, const char *src, size_t n);
char *os101_strcat(char *dst, const char *src);
char *os101_strncat(char *dst, const char *src, size_t n);
int os101_strcmp(const char *a, const char *b);
int os101_strncmp(const char *a, const char *b, size_t n);
char *os101_strchr(const char *s, int c);
char *os101_strrchr(const char *s, int c);
char *os101_strstr(const char *haystack, const char *needle);
size_t os101_strspn(const char *s, const char *accept);
size_t os101_strcspn(const char *s, const char *reject);
char *os101_strpbrk(const char *s, const char *accept);
char *os101_strtok(char *s, const char *delim);
char *os101_strdup(const char *s);
char *os101_strerror(int err);

/* ctype */
int os101_isalnum(int c);
int os101_isalpha(int c);
int os101_isblank(int c);
int os101_iscntrl(int c);
int os101_isdigit(int c);
int os101_isgraph(int c);
int os101_islower(int c);
int os101_isprint(int c);
int os101_ispunct(int c);
int os101_isspace(int c);
int os101_isupper(int c);
int os101_isxdigit(int c);
int os101_tolower(int c);
int os101_toupper(int c);

/* stdlib */
void *os101_malloc(size_t size);
void *os101_calloc(size_t nmemb, size_t size);
void *os101_realloc(void *ptr, size_t size);
void os101_free(void *ptr);
void *os101_aligned_alloc(size_t alignment, size_t size);
int os101_atoi(const char *s);
long os101_strtol(const char *s, char **end, int base);
unsigned long os101_strtoul(const char *s, char **end, int base);
double os101_strtod(const char *s, char **end);
int os101_abs(int v);
long os101_labs(long v);
void os101_qsort(void *base, size_t nmemb, size_t size,
                 int (*cmp)(const void *, const void *));
void *os101_bsearch(const void *key, const void *base, size_t nmemb,
                    size_t size, int (*cmp)(const void *, const void *));
int os101_rand(void);
void os101_srand(unsigned seed);
char *os101_getenv(const char *name);
int os101_atexit(void (*fn)(void));
void os101_exit(int code);
extern int os101_errno;

/* math */
double os101_fabs(double x);
double os101_sqrt(double x);
double os101_cbrt(double x);
double os101_floor(double x);
double os101_ceil(double x);
double os101_round(double x);
double os101_trunc(double x);
double os101_fmod(double x, double y);
double os101_modf(double x, double *ipart);
double os101_frexp(double x, int *e);
double os101_ldexp(double x, int e);
double os101_copysign(double x, double y);
double os101_hypot(double x, double y);
double os101_exp(double x);
double os101_exp2(double x);
double os101_expm1(double x);
double os101_log(double x);
double os101_log2(double x);
double os101_log10(double x);
double os101_log1p(double x);
double os101_pow(double x, double y);
double os101_sin(double x);
double os101_cos(double x);
double os101_tan(double x);
double os101_asin(double x);
double os101_acos(double x);
double os101_atan(double x);
double os101_atan2(double y, double x);
double os101_sinh(double x);
double os101_cosh(double x);
double os101_tanh(double x);
int os101_isnan(double x);
int os101_isinf(double x);
int os101_isfinite(double x);
int os101_signbit(double x);

/* The hooks that stand in for the kernel's syscalls live in host_stubs.h. */

#endif /* OS101_LIBC_TESTS_API_H */
