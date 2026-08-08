/*
 * Freestanding <time.h> for the OS101 QuickJS build.
 *
 * `struct tm` is here only because quickjs.c's getTimezoneOffset() declares one
 * and reads a single field out of it, tm_gmtoff. See src/time.c for why our
 * localtime_r fills in nothing else.
 */
#ifndef OS101_SHIM_TIME_H
#define OS101_SHIM_TIME_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef long time_t;

#define CLOCK_REALTIME 0
#define CLOCK_MONOTONIC 1

struct timespec {
    time_t tv_sec;
    long tv_nsec;
};

struct tm {
    int tm_sec;
    int tm_min;
    int tm_hour;
    int tm_mday;
    int tm_mon;
    int tm_year;
    int tm_wday;
    int tm_yday;
    int tm_isdst;
    long tm_gmtoff;
    const char *tm_zone;
};

int clock_gettime(int clk_id, struct timespec *ts);
time_t time(time_t *t);
struct tm *localtime_r(const time_t *t, struct tm *out);
struct tm *localtime(const time_t *t);

#ifdef __cplusplus
}
#endif

#endif
