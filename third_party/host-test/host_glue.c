/*
 * Stands in for the Rust half of the shim when the harness runs on the host.
 *
 * The Rust crate cannot be used here: it is a `no_std` library with no global
 * allocator of its own, and exporting `malloc` into a process that already has
 * one would make Rust's allocator call this file, which would call Rust's
 * allocator, forever. So the host harness reimplements just the boundary — and
 * only the boundary. The functions the Rust crate actually computes something in
 * (`modf`, `lrint`, `atanh`, the block header arithmetic) are covered by
 * `cargo test` in libc-shim/rust instead.
 *
 * The block header below mirrors malloc.rs on purpose, sixteen-byte header and
 * exact stored size included. Without that the harness's memory measurements
 * would be measuring macOS's malloc bucket sizes, and the numbers would not
 * transfer to the kernel — which is the one thing they exist to do.
 */
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>

long os101_host_unix_micros(void);
void *os101_host_raw_alloc(unsigned long size);
void *os101_host_raw_realloc(void *ptr, unsigned long size);
void os101_host_raw_free(void *ptr);
void os101_host_write(int stream, const char *buf, unsigned long len);
void os101_host_die(const char *message) __attribute__((noreturn));

#define HEADER 16
#define MAGIC 0x4f533130314a5300ULL

void *malloc(size_t size)
{
    unsigned char *base;
    size_t want = size == 0 ? 1 : size;

    base = os101_host_raw_alloc(HEADER + want);
    if (base == NULL)
        return NULL;
    ((uint64_t *)base)[0] = (uint64_t)want;
    ((uint64_t *)base)[1] = MAGIC;
    return base + HEADER;
}

void free(void *ptr)
{
    unsigned char *base;

    if (ptr == NULL)
        return;
    base = (unsigned char *)ptr - HEADER;
    if (((uint64_t *)base)[1] != MAGIC)
        os101_host_die("libc shim: free of a pointer it did not allocate");
    os101_host_raw_free(base);
}

void *realloc(void *ptr, size_t size)
{
    unsigned char *base;
    unsigned char *grown;
    size_t want = size == 0 ? 1 : size;

    if (ptr == NULL)
        return malloc(size);
    base = (unsigned char *)ptr - HEADER;
    if (((uint64_t *)base)[1] != MAGIC)
        os101_host_die("libc shim: realloc of a pointer it did not allocate");
    grown = os101_host_raw_realloc(base, HEADER + want);
    if (grown == NULL)
        return NULL;
    ((uint64_t *)grown)[0] = (uint64_t)want;
    return grown + HEADER;
}

size_t malloc_usable_size(const void *ptr)
{
    if (ptr == NULL)
        return 0;
    return (size_t)((const uint64_t *)((const unsigned char *)ptr - HEADER))[0];
}

int gettimeofday(struct timeval *tv, void *tz)
{
    long micros;

    (void)tz;
    if (tv == NULL)
        return 0;
    micros = os101_host_unix_micros();
    tv->tv_sec = micros / 1000000;
    tv->tv_usec = micros % 1000000;
    return 0;
}

int clock_gettime(int clock_id, struct timespec *ts)
{
    long micros;

    (void)clock_id;
    if (ts == NULL)
        return 0;
    micros = os101_host_unix_micros();
    ts->tv_sec = micros / 1000000;
    ts->tv_nsec = (micros % 1000000) * 1000;
    return 0;
}

/* UTC, exactly as clock.rs does it, so that getTimezoneOffset() reads zero. */
struct tm *localtime_r(const time_t *t, struct tm *out)
{
    (void)t;
    if (out == NULL)
        return out;
    memset(out, 0, sizeof(*out));
    out->tm_mday = 1;
    out->tm_year = 70;
    out->tm_wday = 4;
    out->tm_gmtoff = 0;
    out->tm_zone = "UTC";
    return out;
}

void os101_shim_write_bytes(int stream, const char *buf, size_t len)
{
    if (buf != NULL && len > 0)
        os101_host_write(stream, buf, len);
}

void os101_shim_assert_fail(const char *expr, const char *file, int line)
    __attribute__((noreturn));

void os101_shim_assert_fail(const char *expr, const char *file, int line)
{
    char message[512];
    snprintf(message, sizeof(message), "quickjs assertion failed: %s at %s:%d",
             expr == NULL ? "<null>" : expr, file == NULL ? "<null>" : file,
             line);
    os101_host_die(message);
}
