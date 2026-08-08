#include <os101.h>

uint64_t os101_syscall0(uint64_t nr)
{
    uint64_t ret;
    __asm__ __volatile__("syscall" : "=a"(ret) : "a"(nr) : "rcx", "r11", "memory");
    return ret;
}

uint64_t os101_syscall1(uint64_t nr, uint64_t a1)
{
    uint64_t ret;
    __asm__ __volatile__("syscall" : "=a"(ret) : "a"(nr), "D"(a1) : "rcx", "r11", "memory");
    return ret;
}

uint64_t os101_syscall2(uint64_t nr, uint64_t a1, uint64_t a2)
{
    uint64_t ret;
    __asm__ __volatile__("syscall"
                         : "=a"(ret)
                         : "a"(nr), "D"(a1), "S"(a2)
                         : "rcx", "r11", "memory");
    return ret;
}

uint64_t os101_syscall3(uint64_t nr, uint64_t a1, uint64_t a2, uint64_t a3)
{
    uint64_t ret;
    __asm__ __volatile__("syscall"
                         : "=a"(ret)
                         : "a"(nr), "D"(a1), "S"(a2), "d"(a3)
                         : "rcx", "r11", "memory");
    return ret;
}
