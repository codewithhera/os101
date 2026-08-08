#include <math.h>
#include <time.h>
#include <stddef.h>

long double ldexpl(long double x, int n)
{
    return (long double)ldexp((double)x, n);
}

time_t time(time_t *t)
{
    struct timespec ts;
    time_t sec;
    if (clock_gettime(CLOCK_REALTIME, &ts) != 0)
        sec = 0;
    else
        sec = ts.tv_sec;
    if (t)
        *t = sec;
    return sec;
}

struct tm *localtime(const time_t *t)
{
    static struct tm buf;
    return localtime_r(t, &buf);
}
