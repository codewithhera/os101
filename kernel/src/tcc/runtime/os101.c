#include <os101.h>
#include <stddef.h>

__attribute__((naked)) void os101_yield(void)
{
    __asm__ __volatile__(
        "mov $3, %%eax\n\t"
        "syscall\n\t"
        "ret\n\t"
        :
        :
        : "rax", "rcx", "r11", "memory");
}

long os101_console_write(int stream, const char *buf, size_t len)
{
    (void)stream;
    return (long)os101_syscall2(OS101_SYS_WRITE, (uint64_t)(uintptr_t)buf, (uint64_t)len);
}

void *os101_sbrk(long increment)
{
    uint64_t previous = os101_syscall1(OS101_SYS_SBRK, (uint64_t)(int64_t)increment);
    if (previous == OS101_SYS_ERROR)
        return (void *)(intptr_t)-1;
    return (void *)(uintptr_t)previous;
}

int64_t os101_time_ms(void)
{
    return (int64_t)os101_syscall0(OS101_SYS_TIME_MS);
}

void os101_exit_process(int code)
{
    (void)os101_syscall1(OS101_SYS_EXIT, (uint64_t)(unsigned)code);
    for (;;)
        ;
}

uint64_t os101_window_create(const char *title, unsigned w, unsigned h)
{
    return os101_syscall3(OS101_SYS_GUI_CREATE_WINDOW, (uint64_t)(uintptr_t)title,
                          ((uint64_t)w << 32) | h, 0);
}

uint64_t os101_label_add(uint64_t window, unsigned x, unsigned y, const char *text)
{
    return os101_syscall3(OS101_SYS_GUI_ADD_LABEL, window,
                          ((uint64_t)x << 32) | y, (uint64_t)(uintptr_t)text);
}

uint64_t os101_button_add(uint64_t window, unsigned x, unsigned y, unsigned w,
                          unsigned h, const char *text, uint64_t action_id)
{
    (void)w;
    (void)h;
    return os101_syscall3(OS101_SYS_GUI_ADD_BUTTON, window,
                          ((uint64_t)x << 32) | y, (uint64_t)(uintptr_t)text);
    (void)action_id;
}
