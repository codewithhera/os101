/*
 * Freestanding <errno.h> for the OS101 QuickJS build.
 *
 * Nothing in the engine reads or writes `errno`; the only thing it wants from
 * this header is ETIMEDOUT, which it compares against the return value of
 * pthread_cond_timedwait in Atomics.wait. The numbers below are the Linux
 * values, chosen only because they are the ones a reader is most likely to
 * recognise.
 */
#ifndef OS101_SHIM_ERRNO_H
#define OS101_SHIM_ERRNO_H

#define EPERM 1
#define ENOENT 2
#define EINTR 4
#define EIO 5
#define EBADF 9
#define EAGAIN 11
#define ENOMEM 12
#define EACCES 13
#define EEXIST 17
#define ENOSYS 38
#define EINVAL 22
#define ERANGE 34
#define ETIMEDOUT 110

#ifdef __cplusplus
extern "C" {
#endif

extern int errno;

#ifdef __cplusplus
}
#endif

#endif
