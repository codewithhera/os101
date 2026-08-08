/*
 * _start — where the kernel drops a C or C++ application.
 *
 * kernel/src/process.rs enters at the ELF entry point through iretq with rsp
 * eight bytes below the top of a fresh 256 KiB stack, which is what a `call`
 * would have left: the SysV ABI's promise is that rsp+8 is a multiple of 16 on
 * entry, so rsp itself is 8 mod 16 here. Before we can call anything we have
 * to be at 0 mod 16 instead, or the first `movaps` in a callee's prologue
 * faults on a misaligned spill slot. Nothing is on the stack yet that we
 * could lose by rounding down.
 *
 * There are no arguments and no environment to unpack: nothing in the kernel
 * passes any, so main gets (0, NULL, NULL).
 */

    .section .text._start, "ax", @progbits
    .globl _start
    .type _start, @function
_start:
    /* A zero frame pointer terminates a backtrace: a walker following the
       saved-rbp chain stops here instead of following whatever the loader
       happened to leave in the register. */
    xorl %ebp, %ebp

    andq $-16, %rsp

    /* Static constructors, then the program. */
    call __libc_init

    xorl %edi, %edi         /* argc = 0    */
    xorl %esi, %esi         /* argv = NULL */
    xorl %edx, %edx         /* envp = NULL */
    call main

    /* main's return value is the exit status. exit runs atexit handlers and
       the fini_array, then asks the kernel to end the process. */
    movl %eax, %edi
    call exit

    /* exit is noreturn; if it ever did return, stop rather than fall into
       whatever follows in .text. */
1:  hlt
    jmp 1b
    .size _start, . - _start
