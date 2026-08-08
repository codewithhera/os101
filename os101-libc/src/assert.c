/*
 * A failed assertion. There is no core to dump and no debugger to attach, so
 * the message on the console is the whole report — print it and stop.
 */
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>

void os101_assert_fail(const char *expr, const char *file, int line,
                       const char *func)
{
    fprintf(stderr, "%s:%d: %s: assertion failed: %s\n",
            file == NULL ? "?" : file, line, func == NULL ? "?" : func,
            expr == NULL ? "?" : expr);
    abort();
}
