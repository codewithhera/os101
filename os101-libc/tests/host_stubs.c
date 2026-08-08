/*
 * The kernel's side of the ABI, for the host test build.
 *
 * os101-libc reaches the kernel through exactly four hooks — sbrk, a console
 * write, process exit, and the fini_array walk — which is what makes malloc,
 * stdio, stdlib and the maths testable on the build machine. Here they are a
 * static array, stderr, a longjmp, and nothing.
 *
 * The sbrk stub follows kernel/src/process.rs deliberately closely: it returns
 * the *previous* break, it is monotonic in address, it refuses to grow past a
 * limit, and it accepts a negative increment by lowering the break without
 * giving the memory back. An allocator that works against this one is working
 * against the same contract the kernel offers.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "host_stubs.h"

#define ARENA_SIZE (32u * 1024u * 1024u)

/* Aligned like the kernel's heap window, which starts on a page boundary. */
static char arena[ARENA_SIZE] __attribute__((aligned(4096)));
static size_t break_offset;
static size_t peak_offset;

void *os101_sbrk(long increment)
{
    size_t previous = break_offset;
    size_t requested;

    if (increment >= 0) {
        requested = previous + (size_t)increment;
        if (requested < previous || requested > ARENA_SIZE) {
            return (void *)-1;
        }
    } else {
        size_t shrink = (size_t)(-increment);
        if (shrink > previous) {
            return (void *)-1;
        }
        requested = previous - shrink;
    }
    break_offset = requested;
    if (break_offset > peak_offset) {
        peak_offset = break_offset;
    }
    return arena + previous;
}

size_t os101_test_sbrk_total(void)
{
    return break_offset;
}

size_t os101_test_sbrk_peak(void)
{
    return peak_offset;
}

long os101_console_write(int stream, const char *buf, size_t len)
{
    /* Anything the library prints during the tests is a diagnostic, so it goes
       to stderr and stays out of the test output on stdout. */
    (void)stream;
    if (len > 0) {
        fwrite(buf, 1, len, stderr);
    }
    return (long)len;
}

jmp_buf os101_test_exit_jmp;
int os101_test_exit_armed;
int os101_test_exit_code;

void os101_exit_process(int code)
{
    os101_test_exit_code = code;
    if (os101_test_exit_armed) {
        os101_test_exit_armed = 0;
        longjmp(os101_test_exit_jmp, 1);
    }
    fprintf(stderr, "os101_exit_process(%d) with nothing to return to\n", code);
    _Exit(code == 0 ? 1 : code);
}

void os101_run_fini_array(void)
{
    /* There is no .fini_array in a Mach-O test binary linked against the host's
       runtime, and nothing here needs one: init.c, which walks the real tables,
       is part of the target build only. */
}
