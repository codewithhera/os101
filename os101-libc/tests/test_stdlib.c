/*
 * The conversions, the sorting, the generator, and the exit path.
 *
 * strtol and strtod are compared against the host's, including where the end
 * pointer lands and what errno becomes, because those are the parts that get
 * written by guesswork and then quietly disagree. strtod in particular is
 * checked for being correctly rounded: the host's is, so a byte comparison of
 * the two results as %.17g strings is a comparison of the whole 53-bit answer.
 */
#include <errno.h>
#include <limits.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "harness.h"
#include "host_stubs.h"
#include "os101_api.h"

static void integer_parsing(void)
{
    static const char *const INPUTS[] = {
        "0",         "1",          "-1",        "+42",       "  12345",
        "\t\n 7",    "0x1f",       "0X1F",      "0xg",       "017",
        "099",       "abc",        "",          "  ",        "-",
        "+",         "12abc",      "2147483647", "2147483648",
        "-2147483648", "-2147483649", "9223372036854775807",
        "9223372036854775808", "-9223372036854775808",
        "18446744073709551615", "18446744073709551616",
        "-18446744073709551615", "0x7fffffffffffffff",
        "0xffffffffffffffff", "0xfffffffffffffffff", "1e5", "  -0x10zz",
        "777777777777777777777777"
    };
    static const int BASES[] = {0, 2, 8, 10, 16, 36};
    size_t i;
    size_t b;

    for (i = 0; i < sizeof(INPUTS) / sizeof(INPUTS[0]); i++) {
        for (b = 0; b < sizeof(BASES) / sizeof(BASES[0]); b++) {
            const char *s = INPUTS[i];
            int base = BASES[b];
            char *mine_end;
            char *theirs_end;
            long mine;
            long theirs;
            unsigned long mine_u;
            unsigned long theirs_u;
            int mine_errno;
            int theirs_errno;

            os101_errno = 0;
            errno = 0;
            mine = os101_strtol(s, &mine_end, base);
            mine_errno = os101_errno;
            theirs = strtol(s, &theirs_end, base);
            theirs_errno = errno;
            CHECK(mine == theirs, "strtol(\"%s\", %d): got %ld, host %ld", s,
                  base, mine, theirs);
            CHECK(mine_end - s == theirs_end - s,
                  "strtol(\"%s\", %d): end at %ld, host %ld", s, base,
                  (long)(mine_end - s), (long)(theirs_end - s));
            CHECK((mine_errno == ERANGE) == (theirs_errno == ERANGE),
                  "strtol(\"%s\", %d): errno %d, host %d", s, base, mine_errno,
                  theirs_errno);

            os101_errno = 0;
            errno = 0;
            mine_u = os101_strtoul(s, &mine_end, base);
            mine_errno = os101_errno;
            theirs_u = strtoul(s, &theirs_end, base);
            theirs_errno = errno;
            CHECK(mine_u == theirs_u, "strtoul(\"%s\", %d): got %lu, host %lu",
                  s, base, mine_u, theirs_u);
            CHECK(mine_end - s == theirs_end - s,
                  "strtoul(\"%s\", %d): end at %ld, host %ld", s, base,
                  (long)(mine_end - s), (long)(theirs_end - s));
            CHECK((mine_errno == ERANGE) == (theirs_errno == ERANGE),
                  "strtoul(\"%s\", %d): errno %d, host %d", s, base, mine_errno,
                  theirs_errno);
        }
    }

    CHECK(os101_atoi("  -42xyz") == -42, "atoi");
    CHECK(os101_atoi("") == 0, "atoi of an empty string");
    CHECK(os101_abs(-7) == 7 && os101_abs(7) == 7, "abs");
    CHECK(os101_labs(-7L) == 7L, "labs");
}

/* Compared as %.17g, which is enough digits to name a double exactly: equal
   strings mean equal doubles. */
static void check_strtod(const char *s)
{
    char *mine_end;
    char *theirs_end;
    double mine;
    double theirs;
    char mine_text[64];
    char theirs_text[64];

    os101_errno = 0;
    errno = 0;
    mine = os101_strtod(s, &mine_end);
    theirs = strtod(s, &theirs_end);

    snprintf(mine_text, sizeof(mine_text), "%.17g", mine);
    snprintf(theirs_text, sizeof(theirs_text), "%.17g", theirs);
    CHECK(strcmp(mine_text, theirs_text) == 0,
          "strtod(\"%s\"): got %s, host %s", s, mine_text, theirs_text);
    CHECK(mine_end - s == theirs_end - s,
          "strtod(\"%s\"): end at %ld, host %ld", s, (long)(mine_end - s),
          (long)(theirs_end - s));
    if (mine == 0.0 && theirs == 0.0) {
        CHECK(os101_signbit(mine) == !!signbit(theirs),
              "strtod(\"%s\"): sign of zero", s);
    }
}

static void double_parsing(void)
{
    static const char *const INPUTS[] = {
        "0",          "0.0",        "-0.0",      "1",          "1.0",
        "-1.0",       "0.5",        "0.1",       "0.2",        "0.3",
        "1e0",        "1e1",        "1e-1",      "1E5",        "1e+5",
        "1e308",      "1e309",      "1e-308",    "1e-323",     "1e-324",
        "1e-400",     "1e400",      "3.14159265358979323846",
        "2.718281828459045235360287471352662497757",
        "123456789012345678901234567890",
        "0.000000000000000000000000001",
        "1.7976931348623157e308",  "1.7976931348623159e308",
        "2.2250738585072014e-308", "4.9406564584124654e-324",
        "2.4703282292062327e-324", "2.4703282292062328e-324",
        /* Halfway cases, where a lazy implementation rounds the wrong way. */
        "9007199254740993",       "9007199254740992.5",
        "1.0000000000000002",     "1.0000000000000001",
        "0.49999999999999994",    "4503599627370497.5",
        "72057594037927935",      "1e23",
        "8.98846567431158e307",   "5e-324",
        /* Long digit strings, where the exact value is only decided far past
           the seventeenth digit. */
        "1.000000000000000000000000000000000000000000000000001",
        "0.500000000000000000000000000000000000000000000000001",
        "2.00000000000000011102230246251565404236316680908203125",
        "1.00000000000000005551115123125782702118158340454101562",
        "1.00000000000000005551115123125782702118158340454101563",
        /* Junk, signs, and where the end pointer should land. */
        "  1.5abc",   "+.5",       ".5",        "5.",        ".",
        "",           "-",         "e5",        "1e",        "1e+",
        "1.5e",       "inf",       "-inf",      "INFINITY",  "nan",
        "-NaN",       "0x10",      "  \t-2.25e-3xyz"
    };
    size_t i;

    for (i = 0; i < sizeof(INPUTS) / sizeof(INPUTS[0]); i++) {
        check_strtod(INPUTS[i]);
    }

    /* A sweep of random-looking decimals with seventeen significant digits,
       which is where correct rounding is hardest and matters most. */
    {
        unsigned long state = 12345;
        int n;

        for (n = 0; n < 500; n++) {
            char text[64];
            int exponent;

            state = state * 6364136223846793005UL + 1442695040888963407UL;
            exponent = (int)((state >> 33) % 60) - 30;
            snprintf(text, sizeof(text), "%llu.%09llue%d",
                     (unsigned long long)((state >> 40) % 100000),
                     (unsigned long long)(state % 1000000000UL), exponent);
            check_strtod(text);
        }
    }

    /* Every printed value has to read back as itself: this library's printf and
       this library's strtod are the two halves of one round trip. */
    {
        unsigned long state = 999;
        int n;

        for (n = 0; n < 500; n++) {
            char text[64];
            double original;
            double back;
            unsigned long long bits;

            state = state * 6364136223846793005UL + 1442695040888963407UL;
            bits = ((unsigned long long)state << 32) ^ (state >> 7);
            memcpy(&original, &bits, sizeof(original));
            if (!os101_isfinite(original)) {
                continue;
            }
            os101_snprintf(text, sizeof(text), "%.17g", original);
            back = os101_strtod(text, NULL);
            CHECK(memcmp(&original, &back, sizeof(original)) == 0,
                  "round trip failed for %s", text);
        }
    }
}

static int compare_int(const void *a, const void *b)
{
    int x = *(const int *)a;
    int y = *(const int *)b;

    return x < y ? -1 : (x > y ? 1 : 0);
}

static int compare_str(const void *a, const void *b)
{
    return strcmp(*(const char *const *)a, *(const char *const *)b);
}

struct wide {
    char pad[24];
    int key;
};

static int compare_wide(const void *a, const void *b)
{
    return ((const struct wide *)a)->key - ((const struct wide *)b)->key;
}

static void sorting(void)
{
    static const size_t SIZES[] = {0, 1, 2, 3, 5, 8, 9, 17, 64, 100, 1000};
    size_t s;

    for (s = 0; s < sizeof(SIZES) / sizeof(SIZES[0]); s++) {
        size_t n = SIZES[s];
        int mine[1000];
        int theirs[1000];
        unsigned long state = 4321 + n;
        size_t i;

        for (i = 0; i < n; i++) {
            state = state * 6364136223846793005UL + 1442695040888963407UL;
            mine[i] = (int)((state >> 33) % 1000);
            theirs[i] = mine[i];
        }
        os101_qsort(mine, n, sizeof(int), compare_int);
        qsort(theirs, n, sizeof(int), compare_int);
        CHECK(memcmp(mine, theirs, n * sizeof(int)) == 0,
              "qsort of %zu ints disagreed with the host's", n);

        /* Already sorted, and reversed: the two inputs a bad pivot choice
           turns into quadratic time. */
        for (i = 0; i < n; i++) {
            mine[i] = (int)i;
        }
        os101_qsort(mine, n, sizeof(int), compare_int);
        for (i = 0; i < n; i++) {
            CHECK(mine[i] == (int)i, "qsort of sorted input at %zu", i);
        }
        for (i = 0; i < n; i++) {
            mine[i] = (int)(n - i);
        }
        os101_qsort(mine, n, sizeof(int), compare_int);
        for (i = 1; i < n; i++) {
            CHECK(mine[i - 1] <= mine[i], "qsort of reversed input at %zu", i);
        }
        /* All equal, which is the case that sends a naive partition off the
           end of the array. */
        for (i = 0; i < n; i++) {
            mine[i] = 7;
        }
        os101_qsort(mine, n, sizeof(int), compare_int);
        for (i = 0; i < n; i++) {
            CHECK(mine[i] == 7, "qsort of equal elements at %zu", i);
        }
    }

    {
        const char *words[] = {"pear", "apple", "fig", "banana", "apple"};
        const char *expect[] = {"apple", "apple", "banana", "fig", "pear"};
        size_t i;

        os101_qsort(words, 5, sizeof(words[0]), compare_str);
        for (i = 0; i < 5; i++) {
            CHECK(strcmp(words[i], expect[i]) == 0, "qsort of strings at %zu",
                  i);
        }
    }

    {
        /* An element size that is not a power of two, moved by the byte-wise
           swap. */
        struct wide items[32];
        size_t i;

        for (i = 0; i < 32; i++) {
            memset(items[i].pad, (int)i, sizeof(items[i].pad));
            items[i].key = (int)(31 - i);
        }
        os101_qsort(items, 32, sizeof(items[0]), compare_wide);
        for (i = 0; i < 32; i++) {
            CHECK(items[i].key == (int)i, "qsort of wide elements at %zu", i);
            CHECK(items[i].pad[0] == (char)(31 - i),
                  "qsort moved only part of element %zu", i);
        }
    }
}

static void searching(void)
{
    int values[64];
    size_t n;
    int i;

    for (n = 0; n <= 64; n += 8) {
        for (i = 0; i < (int)n; i++) {
            values[i] = i * 3;
        }
        for (i = -1; i < (int)n * 3 + 2; i++) {
            int key = i;
            void *mine = os101_bsearch(&key, values, n, sizeof(int),
                                       compare_int);
            void *theirs = bsearch(&key, values, n, sizeof(int), compare_int);
            CHECK(mine == theirs, "bsearch(%d) in %zu elements", key, n);
        }
    }
}

static void random_numbers(void)
{
    int first[8];
    int again[8];
    int i;
    int in_range = 1;
    int all_same = 1;

    os101_srand(1);
    for (i = 0; i < 8; i++) {
        first[i] = os101_rand();
    }
    os101_srand(1);
    for (i = 0; i < 8; i++) {
        again[i] = os101_rand();
    }
    for (i = 0; i < 8; i++) {
        CHECK(first[i] == again[i], "rand is not reproducible from a seed");
        if (first[i] < 0 || first[i] > 0x7fffffff) {
            in_range = 0;
        }
        if (i > 0 && first[i] != first[0]) {
            all_same = 0;
        }
    }
    CHECK(in_range, "rand returned a value outside 0..RAND_MAX");
    CHECK(!all_same, "rand returned the same value every time");

    os101_srand(2);
    CHECK(os101_rand() != first[0], "a different seed gave the same first value");

    /* Nothing here is a statistical test of the generator, but a run of
       thousands of values should cover the whole range and not repeat early. */
    {
        int low = 0;
        int high = 0;
        os101_srand(99);
        for (i = 0; i < 10000; i++) {
            int v = os101_rand();
            if (v < 0x40000000) {
                low++;
            } else {
                high++;
            }
        }
        CHECK(low > 4000 && high > 4000, "rand is badly skewed: %d low, %d high",
              low, high);
    }
}

static int exit_order[8];
static int exit_count;

static void handler_a(void) { exit_order[exit_count++] = 1; }
static void handler_b(void) { exit_order[exit_count++] = 2; }
static void handler_c(void) { exit_order[exit_count++] = 3; }

static void exit_handlers(void)
{
    CHECK(os101_atexit(handler_a) == 0, "atexit refused a handler");
    CHECK(os101_atexit(handler_b) == 0, "atexit refused a handler");
    CHECK(os101_atexit(handler_c) == 0, "atexit refused a handler");

    os101_test_exit_code = -1;
    if (setjmp(os101_test_exit_jmp) == 0) {
        os101_test_exit_armed = 1;
        os101_exit(9);
        CHECK(0, "exit returned");
    }
    os101_test_exit_armed = 0;

    CHECK(os101_test_exit_code == 9, "exit passed %d to the kernel, not 9",
          os101_test_exit_code);
    CHECK(exit_count == 3, "%d handlers ran, expected 3", exit_count);
    CHECK(exit_order[0] == 3 && exit_order[1] == 2 && exit_order[2] == 1,
          "handlers ran in the order %d,%d,%d, expected 3,2,1", exit_order[0],
          exit_order[1], exit_order[2]);
}

static void environment(void)
{
    CHECK(os101_getenv("PATH") == NULL, "getenv found an environment");
}

void run_stdlib_tests(void)
{
    test_section("stdlib.h: conversions, sorting, rand, exit");
    integer_parsing();
    double_parsing();
    sorting();
    searching();
    random_numbers();
    exit_handlers();
    environment();
}
