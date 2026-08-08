/*
 * time.h over syscall 5, which returns milliseconds since the Unix epoch from
 * the machine's real-time clock (kernel/src/rtc.rs).
 *
 * That is the only clock userspace can see, so `clock` is built on it too:
 * the kernel does not account per-process CPU time, and a wall-clock reading
 * is a more useful lie than a constant zero for the one thing `clock` is used
 * for, which is timing a loop.
 */
#include <os101.h>
#include <time.h>

long os101_millis(void)
{
    return (long)os101_time_ms();
}

time_t time(time_t *out)
{
    time_t seconds = (time_t)(os101_time_ms() / 1000);

    if (out != NULL) {
        *out = seconds;
    }
    return seconds;
}

clock_t clock(void)
{
    /* Measured from the first call, so that a difference between two calls is
       the elapsed time and the absolute value is not mistaken for CPU time
       since process start. In .bss, so it starts at zero. */
    static int started;
    static int64_t origin_ms;
    int64_t now = os101_time_ms();

    if (!started) {
        started = 1;
        origin_ms = now;
    }
    return (clock_t)((now - origin_ms) * (CLOCKS_PER_SEC / 1000));
}

double difftime(time_t end, time_t start)
{
    return (double)(end - start);
}

int gettimeofday(struct timeval *tv, struct timezone *tz)
{
    int64_t ms = os101_time_ms();

    if (tz != NULL) {
        /* No time zone database: the clock is UTC as far as anyone here
           knows. */
        tz->tz_minuteswest = 0;
        tz->tz_dsttime = 0;
    }
    if (tv != NULL) {
        tv->tv_sec = (time_t)(ms / 1000);
        tv->tv_usec = (suseconds_t)((ms % 1000) * 1000);
    }
    return 0;
}
