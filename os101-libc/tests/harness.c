/*
 * The test runner.
 *
 * Everything portable in os101-libc is compiled for the build machine and
 * checked against the build machine's own C library: printf against snprintf,
 * the conversions against strtol and strtod, the maths against libm, and malloc
 * against a stub sbrk over a static array. That is the only way to test this
 * code without booting the OS, and for the parts that are pure computation it
 * is a better test than booting would be, because the host is the reference.
 *
 * What it cannot cover is in the report: crt0, the syscall wrappers, the GUI
 * calls and the C++ runtime need the kernel and are verified by inspecting the
 * ELF and by the two example applications.
 */
#include <stdarg.h>
#include <stdio.h>

#include "harness.h"

long test_checks;
long test_failures;

static const char *current_section = "";
static long section_failures;

#define MAX_REPORTED 25

void test_section(const char *name)
{
    current_section = name;
    section_failures = 0;
    printf("== %s\n", name);
}

void test_fail(const char *file, int line, const char *fmt, ...)
{
    va_list ap;

    test_failures++;
    section_failures++;
    if (section_failures > MAX_REPORTED) {
        if (section_failures == MAX_REPORTED + 1) {
            printf("   ... further failures in %s not shown\n",
                   current_section);
        }
        return;
    }
    printf("   FAIL %s:%d: ", file, line);
    va_start(ap, fmt);
    vprintf(fmt, ap);
    va_end(ap);
    printf("\n");
}

int main(void)
{
    run_printf_tests();
    run_string_tests();
    run_stdlib_tests();
    run_malloc_tests();
    run_math_tests();

    printf("\n%ld checks, %ld failures\n", test_checks, test_failures);
    return test_failures == 0 ? 0 : 1;
}
