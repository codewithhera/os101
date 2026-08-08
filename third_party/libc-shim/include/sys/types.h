#ifndef OS101_SHIM_SYS_TYPES_H
#define OS101_SHIM_SYS_TYPES_H

#include <stddef.h>
#include <stdint.h>

typedef int64_t off_t;
typedef int64_t ssize_t;
typedef uint32_t mode_t;
typedef int32_t pid_t;
typedef uint32_t uid_t;
typedef uint32_t gid_t;
/* time_t comes from <time.h> — do not redefine it here. */
typedef uint64_t ino_t;
typedef uint64_t dev_t;
typedef uint32_t nlink_t;
typedef int64_t blksize_t;
typedef int64_t blkcnt_t;

#endif
