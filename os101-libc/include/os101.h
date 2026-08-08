/*
 * The OS101 system interface, for C and C++ applications.
 *
 * Everything an application can ask the kernel to do goes through the
 * `syscall` instruction: number in rax, arguments in rdi, rsi, rdx, r10, r8,
 * r9, result in rax. That is the same ABI `os101-user/src/lib.rs` uses, and
 * the numbers below are the ones in `kernel/src/syscall.rs`; the two lists
 * have to be changed together.
 *
 * The GUI calls pack pairs of 32-bit values into single registers. Rather
 * than make every caller remember which pair goes where, the wrappers at the
 * bottom of this file do the packing, so a C program says
 * `os101_window_create("Title", 300, 140)` and never sees a register.
 */
#ifndef OS101_H
#define OS101_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define OS101_SYS_WRITE 1
#define OS101_SYS_EXIT 2
#define OS101_SYS_YIELD 3
#define OS101_SYS_SBRK 4
#define OS101_SYS_TIME_MS 5
#define OS101_SYS_GUI_CREATE_WINDOW 10
#define OS101_SYS_GUI_ADD_BUTTON 11
#define OS101_SYS_GUI_ADD_LABEL 12
#define OS101_SYS_GUI_GET_EVENT 13
#define OS101_SYS_GUI_UPDATE_WIDGET 14
#define OS101_SYS_GUI_SET_FOOTER 15

/* The kernel returns this from any call it refuses. */
#define OS101_SYS_ERROR ((uint64_t)-1)

/* Raw syscalls. The trailing arguments of the shorter forms are zeroed. */
uint64_t os101_syscall0(uint64_t nr);
uint64_t os101_syscall1(uint64_t nr, uint64_t a1);
uint64_t os101_syscall2(uint64_t nr, uint64_t a1, uint64_t a2);
uint64_t os101_syscall3(uint64_t nr, uint64_t a1, uint64_t a2, uint64_t a3);
uint64_t os101_syscall6(uint64_t nr, uint64_t a1, uint64_t a2, uint64_t a3,
                        uint64_t a4, uint64_t a5, uint64_t a6);

/* Give the CPU back until the scheduler comes round again. An application
   that polls for events must call this, or it starves every other process. */
void os101_yield(void);

/* Bytes straight to the system console. Returns the number written. */
long os101_console_write(int stream, const char *buf, size_t len);

/* Move the heap break, Unix style: returns where the break was, or
   (void *)-1 if it cannot grow that far. `malloc` is built on this. */
void *os101_sbrk(long increment);

/* Milliseconds since the Unix epoch, from the machine's real-time clock. */
int64_t os101_time_ms(void);

/* Ends the process. Does not return, and does not run atexit handlers —
   that is `exit`'s job, in stdlib.h. */
void os101_exit_process(int code) __attribute__((noreturn));

/* ---- GUI ----------------------------------------------------------------
 *
 * A window is a handle; widgets are handles within a window. Both are
 * OS101_SYS_ERROR if the kernel refused the request (too many windows, a
 * handle that is not yours, a widget limit reached).
 */

/* Create a window `w` by `h` pixels with `title` in its bar. */
uint64_t os101_window_create(const char *title, unsigned w, unsigned h);

/* Add a button. `action_id` comes back in the event when it is clicked. */
uint64_t os101_button_add(uint64_t window, unsigned x, unsigned y, unsigned w,
                          unsigned h, const char *text, uint64_t action_id);

/* Add a static text label at (x, y) inside the window's content area. */
uint64_t os101_label_add(uint64_t window, unsigned x, unsigned y,
                         const char *text);

/* Replace the text of a label, button or text box. */
int os101_widget_update(uint64_t window, uint64_t widget, const char *text);

/* Set the status line along the bottom of the content area. */
int os101_footer_set(uint64_t window, const char *text);

enum {
    OS101_EVENT_NONE = 0,
    OS101_EVENT_BUTTON = 1,
    /* The window is gone, or was never ours: stop polling and exit. */
    OS101_EVENT_CLOSED = 2
};

typedef struct {
    int kind;
    uint64_t action_id; /* meaningful when kind == OS101_EVENT_BUTTON */
} os101_event;

/* Take one event off the window's queue. Never blocks: when it returns
   OS101_EVENT_NONE the caller should call os101_yield() and ask again. */
os101_event os101_event_poll(uint64_t window);

#ifdef __cplusplus
}
#endif

#endif /* OS101_H */
