/*
 * The only file in the host harness compiled against the real system headers.
 *
 * Everything else — QuickJS, the C shim, the glue, the driver — is compiled with
 * `-ffreestanding` and the shim's own headers, because the point of the harness
 * is to prove the engine is correct against *this* libc rather than against
 * macOS's. That leaves three things the harness has to borrow from the host, and
 * they live behind these functions so that the system's <stdio.h> and the shim's
 * never meet in one translation unit.
 *
 * The awkward part is that the shim deliberately exports malloc, free, realloc
 * and the whole printf family, and a definition in an object file wins over the
 * one in a dylib — so a plain call to `malloc` or `fwrite` from this file would
 * come straight back into the shim and recurse until the stack ran out. Hence
 * dlsym(RTLD_NEXT) for the allocator, which resolves past the executable into
 * libSystem, and the raw write(2) syscall wrapper and clock_gettime_nsec_np for
 * the other two, neither of which the shim shadows.
 */
#include <dlfcn.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

typedef void *(*alloc_fn)(unsigned long);
typedef void *(*realloc_fn)(void *, unsigned long);
typedef void (*free_fn)(void *);

static alloc_fn system_malloc;
static realloc_fn system_realloc;
static free_fn system_free;

static void resolve_system_allocator(void)
{
    if (system_malloc != NULL)
        return;
    system_malloc = (alloc_fn)dlsym(RTLD_NEXT, "malloc");
    system_realloc = (realloc_fn)dlsym(RTLD_NEXT, "realloc");
    system_free = (free_fn)dlsym(RTLD_NEXT, "free");
    if (system_malloc == NULL || system_realloc == NULL || system_free == NULL) {
        static const char message[] =
            "host harness: could not find the system allocator\n";
        write(2, message, sizeof(message) - 1);
        _exit(1);
    }
}

long os101_host_unix_micros(void)
{
    return (long)(clock_gettime_nsec_np(CLOCK_REALTIME) / 1000);
}

void *os101_host_raw_alloc(unsigned long size)
{
    resolve_system_allocator();
    return system_malloc(size);
}

void *os101_host_raw_realloc(void *ptr, unsigned long size)
{
    resolve_system_allocator();
    return system_realloc(ptr, size);
}

void os101_host_raw_free(void *ptr)
{
    resolve_system_allocator();
    system_free(ptr);
}

void os101_host_write(int stream, const char *buf, unsigned long len)
{
    ssize_t written = 0;

    while ((unsigned long)written < len) {
        ssize_t n = write(stream == 2 ? 2 : 1, buf + written,
                          len - (unsigned long)written);
        if (n <= 0)
            return;
        written += n;
    }
}

void os101_host_die(const char *message)
{
    os101_host_write(2, message, strlen(message));
    os101_host_write(2, "\n", 1);
    abort();
}
