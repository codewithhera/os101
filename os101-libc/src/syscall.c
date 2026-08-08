/*
 * The syscall instruction, once per argument count.
 *
 * The ABI is the one in kernel/src/syscall.rs and os101-user/src/lib.rs:
 * number in rax, arguments in rdi, rsi, rdx, r10, r8, r9, result in rax. The
 * instruction itself destroys rcx (it puts the return address there) and r11
 * (RFLAGS), which is why both are in every clobber list; the kernel's entry
 * stub saves and restores the six argument registers, so they are not.
 *
 * "memory" is in the clobber list because several of these calls hand the
 * kernel a pointer — a window title, a printf buffer — and the compiler must
 * not keep a pending store to it in a register across the instruction.
 */
#include <os101.h>

uint64_t os101_syscall0(uint64_t nr)
{
    uint64_t ret;
    /* xmm0–xmm15: the kernel may use them inside the syscall (hardware SSE2)
       and does not restore them on the yield path; treat them as clobbered so
       the compiler does not keep a value across the call. */
    __asm__ __volatile__("syscall"
                         : "=a"(ret)
                         : "a"(nr)
                         : "rcx", "r11", "memory",
                           "xmm0", "xmm1", "xmm2", "xmm3", "xmm4", "xmm5",
                           "xmm6", "xmm7", "xmm8", "xmm9", "xmm10", "xmm11",
                           "xmm12", "xmm13", "xmm14", "xmm15");
    return ret;
}

uint64_t os101_syscall1(uint64_t nr, uint64_t a1)
{
    uint64_t ret;
    __asm__ __volatile__("syscall"
                         : "=a"(ret)
                         : "a"(nr), "D"(a1)
                         : "rcx", "r11", "memory",
                           "xmm0", "xmm1", "xmm2", "xmm3", "xmm4", "xmm5",
                           "xmm6", "xmm7", "xmm8", "xmm9", "xmm10", "xmm11",
                           "xmm12", "xmm13", "xmm14", "xmm15");
    return ret;
}

uint64_t os101_syscall2(uint64_t nr, uint64_t a1, uint64_t a2)
{
    uint64_t ret;
    __asm__ __volatile__("syscall"
                         : "=a"(ret)
                         : "a"(nr), "D"(a1), "S"(a2)
                         : "rcx", "r11", "memory",
                           "xmm0", "xmm1", "xmm2", "xmm3", "xmm4", "xmm5",
                           "xmm6", "xmm7", "xmm8", "xmm9", "xmm10", "xmm11",
                           "xmm12", "xmm13", "xmm14", "xmm15");
    return ret;
}

uint64_t os101_syscall3(uint64_t nr, uint64_t a1, uint64_t a2, uint64_t a3)
{
    uint64_t ret;
    __asm__ __volatile__("syscall"
                         : "=a"(ret)
                         : "a"(nr), "D"(a1), "S"(a2), "d"(a3)
                         : "rcx", "r11", "memory",
                           "xmm0", "xmm1", "xmm2", "xmm3", "xmm4", "xmm5",
                           "xmm6", "xmm7", "xmm8", "xmm9", "xmm10", "xmm11",
                           "xmm12", "xmm13", "xmm14", "xmm15");
    return ret;
}

uint64_t os101_syscall6(uint64_t nr, uint64_t a1, uint64_t a2, uint64_t a3,
                        uint64_t a4, uint64_t a5, uint64_t a6)
{
    uint64_t ret;
    /* r10, r8 and r9 have no constraint letter of their own, so they are
       named explicitly and passed as ordinary "r" operands. */
    register uint64_t r10 __asm__("r10") = a4;
    register uint64_t r8 __asm__("r8") = a5;
    register uint64_t r9 __asm__("r9") = a6;
    __asm__ __volatile__("syscall"
                         : "=a"(ret)
                         : "a"(nr), "D"(a1), "S"(a2), "d"(a3), "r"(r10),
                           "r"(r8), "r"(r9)
                         : "rcx", "r11", "memory",
                           "xmm0", "xmm1", "xmm2", "xmm3", "xmm4", "xmm5",
                           "xmm6", "xmm7", "xmm8", "xmm9", "xmm10", "xmm11",
                           "xmm12", "xmm13", "xmm14", "xmm15");
    return ret;
}
