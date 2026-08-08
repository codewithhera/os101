/*
 * time for OS101, over syscall 5 (milliseconds since the Unix epoch).
 *
 * That one number is all the kernel offers, so `time` is it divided by a
 * thousand and `clock` is it scaled to CLOCKS_PER_SEC. There is no calendar
 * arithmetic here: `localtime` and friends need a time zone database and a
 * static struct tm, and no application has asked for one yet.
 */
#ifndef _OS101_TIME_H
#define _OS101_TIME_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef long time_t;
typedef long clock_t;
typedef long suseconds_t;

#define CLOCKS_PER_SEC 1000000L

struct timeval {
    time_t tv_sec;
    suseconds_t tv_usec;
};

struct timezone {
    int tz_minuteswest;
    int tz_dsttime;
};

struct timespec {
    time_t tv_sec;
    long tv_nsec;
};

/* Seconds since the epoch. Stores the same value through `out` when it is
   not NULL, the way Unix does. */
time_t time(time_t *out);

/* Processor time used, in CLOCKS_PER_SEC units. The kernel does not account
   per-process CPU time, so this is wall clock since the first call — enough
   to time a loop, not enough to bill for. */
clock_t clock(void);

double difftime(time_t end, time_t start);

int gettimeofday(struct timeval *tv, struct timezone *tz);

/* Milliseconds since the epoch, straight from the syscall. Not standard C,
   but it is what the clock actually provides and it avoids the rounding that
   `time` has to do. */
long os101_millis(void);

#ifdef __cplusplus
}
#endif

#endif /* _OS101_TIME_H */
