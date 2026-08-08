#ifndef OS101_H
#define OS101_H
#include <stddef.h>
#include <stdint.h>
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
#define OS101_SYS_ERROR ((uint64_t)-1)
uint64_t os101_syscall0(uint64_t nr);
uint64_t os101_syscall1(uint64_t nr, uint64_t a1);
uint64_t os101_syscall2(uint64_t nr, uint64_t a1, uint64_t a2);
uint64_t os101_syscall3(uint64_t nr, uint64_t a1, uint64_t a2, uint64_t a3);
void os101_yield(void);
long os101_console_write(int stream, const char *buf, size_t len);
void *os101_sbrk(long increment);
int64_t os101_time_ms(void);
void os101_exit_process(int code);
uint64_t os101_window_create(const char *title, unsigned w, unsigned h);
uint64_t os101_button_add(uint64_t window, unsigned x, unsigned y, unsigned w,
                          unsigned h, const char *text, uint64_t action_id);
uint64_t os101_label_add(uint64_t window, unsigned x, unsigned y, const char *text);
#endif
