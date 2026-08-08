#ifndef OS101_LIBC_TESTS_HOST_STUBS_H
#define OS101_LIBC_TESTS_HOST_STUBS_H

#include <setjmp.h>
#include <stddef.h>

void *os101_sbrk(long increment);
long os101_console_write(int stream, const char *buf, size_t len);
void os101_exit_process(int code);
void os101_run_fini_array(void);

/* How far the stub sbrk's break has moved, and how far it ever moved: the
   malloc tests use the first to show that a loop of allocate-and-free stops
   asking for memory, and the second to bound the peak. */
size_t os101_test_sbrk_total(void);
size_t os101_test_sbrk_peak(void);

/* Arm this around a call that ends in exit(), so that the process-exit hook
   comes back here instead of ending the test run. */
extern jmp_buf os101_test_exit_jmp;
extern int os101_test_exit_armed;
extern int os101_test_exit_code;

#endif /* OS101_LIBC_TESTS_HOST_STUBS_H */
