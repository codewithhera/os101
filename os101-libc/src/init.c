/*
 * Static constructors and destructors.
 *
 * A C++ program with a global object, or a C file with
 * __attribute__((constructor)), leaves a pointer to the initialiser in
 * .init_array. There is no dynamic loader here to walk that table, so _start
 * calls __libc_init and this does it: in order, before main, exactly as the
 * SysV ABI says. os101-libc/user.ld defines the symbols around the tables and
 * KEEPs them, because nothing references them and --gc-sections would
 * otherwise be right to throw them away.
 */
#include "internal.h"

typedef void (*init_fn)(void);

extern init_fn __init_array_start[];
extern init_fn __init_array_end[];
extern init_fn __fini_array_start[];
extern init_fn __fini_array_end[];

void __libc_init(void)
{
    init_fn *fn;

    for (fn = __init_array_start; fn != __init_array_end; fn++) {
        if (*fn != (init_fn)0) {
            (*fn)();
        }
    }
}

void os101_run_fini_array(void)
{
    init_fn *fn;

    /* Reverse order: the last thing constructed is the first thing torn
       down. */
    for (fn = __fini_array_end; fn != __fini_array_start;) {
        fn--;
        if (*fn != (init_fn)0) {
            (*fn)();
        }
    }
}
