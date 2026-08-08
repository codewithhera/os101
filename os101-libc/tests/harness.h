#ifndef OS101_LIBC_TESTS_HARNESS_H
#define OS101_LIBC_TESTS_HARNESS_H

extern long test_checks;
extern long test_failures;

void test_fail(const char *file, int line, const char *fmt, ...);
void test_section(const char *name);

#define CHECK(cond, ...)                                  \
    do {                                                  \
        test_checks++;                                     \
        if (!(cond)) {                                     \
            test_fail(__FILE__, __LINE__, __VA_ARGS__);    \
        }                                                  \
    } while (0)

void run_printf_tests(void);
void run_string_tests(void);
void run_stdlib_tests(void);
void run_malloc_tests(void);
void run_math_tests(void);

#endif /* OS101_LIBC_TESTS_HARNESS_H */
