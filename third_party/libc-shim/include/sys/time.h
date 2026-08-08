/*
 * Freestanding <sys/time.h> for the OS101 QuickJS build.
 *
 * gettimeofday is the single point at which the engine asks what time it is:
 * Date.now() and the seed for Math.random() both go through it, and nothing
 * else does.
 */
#ifndef OS101_SHIM_SYS_TIME_H
#define OS101_SHIM_SYS_TIME_H

#include <time.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef long suseconds_t;

struct timeval {
    time_t tv_sec;
    suseconds_t tv_usec;
};

struct timezone {
    int tz_minuteswest;
    int tz_dsttime;
};

int gettimeofday(struct timeval *tv, void *tz);

#ifdef __cplusplus
}
#endif

#endif
