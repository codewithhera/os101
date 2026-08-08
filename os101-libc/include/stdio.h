/*
 * stdio for OS101.
 *
 * There is no filesystem syscall yet, so a FILE here is not a file: it is a
 * console stream and nothing more. stdout and stderr both reach the system
 * console through syscall 1; stdin is always at end of file. The functions
 * that would need a real file (`fopen`, `fread`) are present so that ordinary
 * code compiles, and fail with ENOSYS so that the reason is obvious the first
 * time it runs rather than the tenth time it is read.
 */
#ifndef _OS101_STDIO_H
#define _OS101_STDIO_H

#include <stdarg.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define EOF (-1)
#define BUFSIZ 1024

/* Small enough to be worth exposing: an application that wants to write its
   own diagnostic sink can look at the stream number. */
typedef struct _OS101_FILE {
    int stream; /* 0 stdin, 1 stdout, 2 stderr */
} FILE;

extern FILE *const stdin;
extern FILE *const stdout;
extern FILE *const stderr;

int printf(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
int fprintf(FILE *stream, const char *fmt, ...)
    __attribute__((format(printf, 2, 3)));
int sprintf(char *buf, const char *fmt, ...)
    __attribute__((format(printf, 2, 3)));
int snprintf(char *buf, size_t size, const char *fmt, ...)
    __attribute__((format(printf, 3, 4)));

int vprintf(const char *fmt, va_list ap);
int vfprintf(FILE *stream, const char *fmt, va_list ap);
int vsprintf(char *buf, const char *fmt, va_list ap);
int vsnprintf(char *buf, size_t size, const char *fmt, va_list ap);

int puts(const char *s);
int putchar(int c);
int fputs(const char *s, FILE *stream);
int fputc(int c, FILE *stream);
int putc(int c, FILE *stream);
size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *stream);
int fflush(FILE *stream);
void perror(const char *s);

/* Input: there is nothing to read from yet. These set errno to ENOSYS and
   report end of file. */
int fgetc(FILE *stream);
int getc(FILE *stream);
int getchar(void);
char *fgets(char *buf, int size, FILE *stream);
size_t fread(void *ptr, size_t size, size_t nmemb, FILE *stream);

/* No filesystem: always NULL / EOF with errno == ENOSYS. */
FILE *fopen(const char *path, const char *mode);
int fclose(FILE *stream);

#ifdef __cplusplus
}
#endif

#endif /* _OS101_STDIO_H */
