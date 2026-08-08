/*
 * errno for OS101. One process, one thread of control, so one int.
 *
 * The numbers are Linux's, because that is the list every C programmer
 * already knows and because a future host-side test that compares against
 * Linux costs nothing this way.
 */
#ifndef _OS101_ERRNO_H
#define _OS101_ERRNO_H

#ifdef __cplusplus
extern "C" {
#endif

extern int errno;

#define EPERM 1
#define ENOENT 2
#define EIO 5
#define EBADF 9
#define ENOMEM 12
#define EACCES 13
#define EFAULT 14
#define EBUSY 16
#define EEXIST 17
#define ENODEV 19
#define ENOTDIR 20
#define EISDIR 21
#define EINVAL 22
#define ENFILE 23
#define EMFILE 24
#define ENOSPC 28
#define ESPIPE 29
#define EROFS 30
#define EPIPE 32
#define EDOM 33
#define ERANGE 34
/* Everything the kernel has no syscall for yet reports this. */
#define ENOSYS 38

#ifdef __cplusplus
}
#endif

#endif /* _OS101_ERRNO_H */
