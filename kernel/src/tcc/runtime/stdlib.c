#include <stdlib.h>
#include <string.h>
#include <os101.h>

int errno;

void exit(int code)
{
    os101_exit_process(code);
}

void abort(void)
{
    os101_exit_process(127);
}

int abs(int x)
{
    return x < 0 ? -x : x;
}

long strtol(const char *s, char **end, int base)
{
    long acc = 0;
    int neg = 0;
    if (base == 0)
        base = 10;
    while (*s == ' ' || *s == '\t')
        s++;
    if (*s == '-' || *s == '+') {
        neg = (*s == '-');
        s++;
    }
    while (*s) {
        int d;
        if (*s >= '0' && *s <= '9')
            d = *s - '0';
        else
            break;
        if (d >= base)
            break;
        acc = acc * base + d;
        s++;
    }
    if (end)
        *end = (char *)s;
    return neg ? -acc : acc;
}

int atoi(const char *s)
{
    return (int)strtol(s, 0, 10);
}
