/*
 * Freestanding <setjmp.h> for the OS101 libc shim.
 *
 * QuickJS's dtoa.c includes this and never uses it; TinyCC uses setjmp/longjmp
 * for error recovery. The buffer is eight 64-bit slots matching setjmp.S.
 */
#ifndef OS101_SHIM_SETJMP_H
#define OS101_SHIM_SETJMP_H

#ifdef __cplusplus
extern "C" {
#endif

typedef unsigned long long __jmp_buf[8];
typedef __jmp_buf jmp_buf;

int setjmp(jmp_buf env);
void longjmp(jmp_buf env, int val) __attribute__((noreturn));

#ifdef __cplusplus
}
#endif

#endif
