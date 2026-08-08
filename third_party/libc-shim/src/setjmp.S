/*
 * x86-64 SysV setjmp / longjmp for the OS101 libc shim.
 *
 * TinyCC recovers from parse/compile errors with longjmp out of a setjmp in
 * tcc_compile(). A stub that always returns 0 would leave the compiler in a
 * corrupt state after the first diagnostic; the buffer layout below matches
 * what glibc and musl use on this ABI so jmp_buf in setjmp.h stays portable.
 *
 * Saved: rbx, rbp, r12–r15, rsp (as it will be after setjmp returns), rip.
 * Not saved: MXCSR / x87 CW — TCC does not change them across the jump.
 */

    .section .text
    .globl setjmp
    .globl longjmp
    .type setjmp, @function
    .type longjmp, @function

setjmp:
    movq %rbx,  0(%rdi)
    movq %rbp,  8(%rdi)
    movq %r12, 16(%rdi)
    movq %r13, 24(%rdi)
    movq %r14, 32(%rdi)
    movq %r15, 40(%rdi)
    /* rsp after `ret` — i.e. the caller's stack pointer. */
    leaq 8(%rsp), %rax
    movq %rax, 48(%rdi)
    /* return address */
    movq (%rsp), %rax
    movq %rax, 56(%rdi)
    xorl %eax, %eax
    ret
    .size setjmp, .-setjmp

longjmp:
    movl %esi, %eax
    testl %eax, %eax
    jnz 1f
    movl $1, %eax
1:
    movq  0(%rdi), %rbx
    movq  8(%rdi), %rbp
    movq 16(%rdi), %r12
    movq 24(%rdi), %r13
    movq 32(%rdi), %r14
    movq 40(%rdi), %r15
    movq 48(%rdi), %rsp
    jmpq *56(%rdi)
    .size longjmp, .-longjmp
