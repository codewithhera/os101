/*
 * The OS101 system interface: everything in os101.h that is not a raw
 * syscall.
 *
 * Two jobs. The first is the argument packing the GUI calls need — several of
 * them put two 32-bit values in one register, and the packing is defined by
 * kernel/src/syscall.rs, not by anything a caller could guess. The second is
 * to be the one file in this library that knows a syscall exists: malloc,
 * stdio, stdlib and time all reach the kernel through the small hooks here
 * (os101_sbrk, os101_console_write, os101_exit_process, os101_time_ms), which
 * is what lets the host test harness link those files against stubs and check
 * them on the build machine.
 */
#include <os101.h>
#include <string.h>

__attribute__((naked)) void os101_yield(void)
{
    /* Naked: no compiler-generated frame. Resume after yield is the `ret`
       below, which returns to the caller the same way Rust's nostack
       `yield_now` continues after its `syscall`. A framed version resumed
       into `pop %rbp; ret` with a trashed slot and faulted at address 1. */
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
    /* The kernel's write takes (pointer, length) and sends everything to the
       one system console; there are no file descriptors yet, so stdout and
       stderr are the same place and `stream` is recorded only for the day
       there is somewhere else to send it. */
    (void)stream;
    if (len == 0) {
        return 0;
    }
    return (long)os101_syscall2(OS101_SYS_WRITE, (uint64_t)(uintptr_t)buf,
                                (uint64_t)len);
}

void *os101_sbrk(long increment)
{
    uint64_t previous =
        os101_syscall1(OS101_SYS_SBRK, (uint64_t)(int64_t)increment);
    if (previous == OS101_SYS_ERROR) {
        return (void *)-1;
    }
    return (void *)(uintptr_t)previous;
}

int64_t os101_time_ms(void)
{
    return (int64_t)os101_syscall0(OS101_SYS_TIME_MS);
}

void os101_exit_process(int code)
{
    (void)os101_syscall1(OS101_SYS_EXIT, (uint64_t)(unsigned)code);
    /* The kernel never comes back from exit. If it somehow did, stop here
       rather than return into a caller that has already been torn down. */
    for (;;) {
        __asm__ __volatile__("hlt");
    }
}

/* The GUI calls take (pointer, length) pairs rather than NUL-terminated
   strings, because the kernel side turns them straight into a Rust &str. */
static uint64_t text_ptr(const char *s)
{
    return (uint64_t)(uintptr_t)(s == NULL ? "" : s);
}

static uint64_t text_len(const char *s)
{
    return s == NULL ? 0 : (uint64_t)strlen(s);
}

static uint64_t pack32(unsigned high, unsigned low)
{
    return ((uint64_t)high << 32) | (uint64_t)low;
}

uint64_t os101_window_create(const char *title, unsigned w, unsigned h)
{
    return os101_syscall3(OS101_SYS_GUI_CREATE_WINDOW, text_ptr(title),
                          text_len(title), pack32(w, h));
}

uint64_t os101_button_add(uint64_t window, unsigned x, unsigned y, unsigned w,
                          unsigned h, const char *text, uint64_t action_id)
{
    return os101_syscall6(OS101_SYS_GUI_ADD_BUTTON, window, pack32(x, y),
                          pack32(w, h), text_ptr(text), text_len(text),
                          action_id);
}

uint64_t os101_label_add(uint64_t window, unsigned x, unsigned y,
                         const char *text)
{
    return os101_syscall6(OS101_SYS_GUI_ADD_LABEL, window, pack32(x, y),
                          text_ptr(text), text_len(text), 0, 0);
}

int os101_widget_update(uint64_t window, uint64_t widget, const char *text)
{
    uint64_t res = os101_syscall6(OS101_SYS_GUI_UPDATE_WIDGET, window, widget,
                                  text_ptr(text), text_len(text), 0, 0);
    return res == OS101_SYS_ERROR ? -1 : 0;
}

int os101_footer_set(uint64_t window, const char *text)
{
    uint64_t res = os101_syscall3(OS101_SYS_GUI_SET_FOOTER, window,
                                  text_ptr(text), text_len(text));
    return res == OS101_SYS_ERROR ? -1 : 0;
}

os101_event os101_event_poll(uint64_t window)
{
    os101_event ev;
    uint64_t res = os101_syscall1(OS101_SYS_GUI_GET_EVENT, window);

    ev.kind = OS101_EVENT_NONE;
    ev.action_id = 0;
    if (res == OS101_SYS_ERROR) {
        /* The window is not ours any more: the user closed it, so the app
           should stop rather than spin on a dead handle. */
        ev.kind = OS101_EVENT_CLOSED;
        return ev;
    }
    if (res == 0) {
        return ev;
    }
    /* Low byte is the event type, the rest is its payload. */
    if ((res & 0xff) == 1) {
        ev.kind = OS101_EVENT_BUTTON;
        ev.action_id = res >> 8;
    }
    return ev;
}
