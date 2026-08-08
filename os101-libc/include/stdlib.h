#ifndef _OS101_STDLIB_H
#define _OS101_STDLIB_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1
#define RAND_MAX 0x7fffffff

typedef struct {
    int quot;
    int rem;
} div_t;

typedef struct {
    long quot;
    long rem;
} ldiv_t;

void *malloc(size_t size);
void *calloc(size_t nmemb, size_t size);
void *realloc(void *ptr, size_t size);
void free(void *ptr);
/* Not in C89, but the C++ runtime's aligned operator new needs it and it is
   nearly free once malloc is written. */
void *aligned_alloc(size_t alignment, size_t size);

void exit(int code) __attribute__((noreturn));
void abort(void) __attribute__((noreturn));
/* Ends the process without running atexit handlers. */
void _Exit(int code) __attribute__((noreturn));
int atexit(void (*fn)(void));

int atoi(const char *s);
long atol(const char *s);
long long atoll(const char *s);
double atof(const char *s);
long strtol(const char *s, char **end, int base);
unsigned long strtoul(const char *s, char **end, int base);
long long strtoll(const char *s, char **end, int base);
unsigned long long strtoull(const char *s, char **end, int base);
double strtod(const char *s, char **end);
float strtof(const char *s, char **end);

int abs(int v);
long labs(long v);
long long llabs(long long v);
div_t div(int num, int den);
ldiv_t ldiv(long num, long den);

void qsort(void *base, size_t nmemb, size_t size,
           int (*cmp)(const void *, const void *));
void *bsearch(const void *key, const void *base, size_t nmemb, size_t size,
              int (*cmp)(const void *, const void *));

int rand(void);
void srand(unsigned seed);

/* There is no environment. Always NULL. */
char *getenv(const char *name);

#ifdef __cplusplus
}
#endif

#endif /* _OS101_STDLIB_H */
