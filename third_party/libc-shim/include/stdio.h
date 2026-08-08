/*
 * Freestanding <stdio.h> for the OS101 libc shim.
 *
 * QuickJS only ever writes to stdout/stderr. TinyCC also opens real files
 * through the VFS (sources, headers, ELF output), so FILE* here is a thin
 * wrapper over the same fd table that backs open/read/write/close.
 */
#ifndef OS101_SHIM_STDIO_H
#define OS101_SHIM_STDIO_H

#include <stddef.h>
#include <stdarg.h>
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

#define EOF (-1)
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

#define BUFSIZ 1024

typedef struct _OS101_FILE FILE;

extern FILE *const os101_shim_stdout;
extern FILE *const os101_shim_stderr;
extern FILE *const os101_shim_stdin;

#define stdout os101_shim_stdout
#define stderr os101_shim_stderr
#define stdin  os101_shim_stdin

int printf(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
int fprintf(FILE *stream, const char *fmt, ...)
    __attribute__((format(printf, 2, 3)));
int sprintf(char *buf, const char *fmt, ...)
    __attribute__((format(printf, 2, 3)));
int snprintf(char *buf, size_t size, const char *fmt, ...)
    __attribute__((format(printf, 3, 4)));
int vsnprintf(char *buf, size_t size, const char *fmt, va_list ap)
    __attribute__((format(printf, 3, 0)));
int vfprintf(FILE *stream, const char *fmt, va_list ap)
    __attribute__((format(printf, 2, 0)));
int vsprintf(char *buf, const char *fmt, va_list ap)
    __attribute__((format(printf, 2, 0)));

int putchar(int c);
int fputc(int c, FILE *stream);
int fputs(const char *s, FILE *stream);
int puts(const char *s);
size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *stream);
size_t fread(void *ptr, size_t size, size_t nmemb, FILE *stream);
int fflush(FILE *stream);

FILE *fopen(const char *path, const char *mode);
FILE *fdopen(int fd, const char *mode);
FILE *freopen(const char *path, const char *mode, FILE *stream);
int fclose(FILE *stream);
int fseek(FILE *stream, long offset, int whence);
long ftell(FILE *stream);
void rewind(FILE *stream);
int fgetc(FILE *stream);
int getc(FILE *stream);
int ungetc(int c, FILE *stream);
char *fgets(char *s, int size, FILE *stream);
int remove(const char *path);
int rename(const char *old, const char *newpath);
int fileno(FILE *stream);

#ifdef __cplusplus
}
#endif

#endif
