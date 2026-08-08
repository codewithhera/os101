/*
 * printf, byte for byte against the host's.
 *
 * The format strings are generated rather than written out: every combination
 * of the flags, widths, precisions and length modifiers this library claims to
 * support, crossed with a list of awkward values, which comes to some tens of
 * thousands of comparisons. That is the only way to have any confidence in a
 * printf — the bugs are never in %d, they are in "%+.0d" of zero and "%#.3o"
 * and "%-08.2f" of a negative number.
 *
 * The host's snprintf is the reference. Where the standard leaves something
 * unspecified and libcs disagree, the case is left out and named here rather
 * than fudged:
 *
 *   %p       glibc prints "(nil)" for a null pointer, macOS prints "0x0".
 *            os101-libc follows glibc; the shape of a non-null pointer also
 *            differs. Tested against fixed expectations further down instead.
 *   %s NULL  not required to work at all. Tested here without a precision,
 *            where both libcs happen to print "(null)".
 *   '#' with d, i, u  undefined; not generated.
 *   '0' with s, c     undefined, and the two libcs differ: the BSDs pad a
 *            string with zeros, glibc pads with spaces. os101-libc pads with
 *            spaces, so the flag is not generated for those conversions.
 */
#include <float.h>
#include <limits.h>
#include <math.h>
#include <stdio.h>
#include <string.h>

#include "harness.h"
#include "os101_api.h"

#define BUF 512

/* Compare one formatting against the host's, and check that truncation to a
   few short buffers agrees too — including the return value, which has to be
   the length the format *would* have produced. */
static void compare(const char *fmt, const char *mine, int mine_len,
                    const char *theirs, int theirs_len)
{
    CHECK(strcmp(mine, theirs) == 0, "%s: got \"%s\", host \"%s\"", fmt, mine,
          theirs);
    CHECK(mine_len == theirs_len, "%s: returned %d, host %d", fmt, mine_len,
          theirs_len);
}

static void truncation(const char *fmt, const char *expect, int expect_len,
                       int arg_kind, long long i, double d, const char *s)
{
    static const size_t sizes[] = {0, 1, 2, 5, 9};
    size_t k;

    for (k = 0; k < sizeof(sizes) / sizeof(sizes[0]); k++) {
        char mine[BUF];
        char theirs[BUF];
        size_t size = sizes[k];
        int n;
        int m;

        memset(mine, '#', sizeof(mine));
        memset(theirs, '#', sizeof(theirs));
        switch (arg_kind) {
        case 0:
            n = os101_snprintf(mine, size, fmt, (int)i);
            m = snprintf(theirs, size, fmt, (int)i);
            break;
        case 1:
            n = os101_snprintf(mine, size, fmt, d);
            m = snprintf(theirs, size, fmt, d);
            break;
        default:
            n = os101_snprintf(mine, size, fmt, s);
            m = snprintf(theirs, size, fmt, s);
            break;
        }
        CHECK(n == m && n == expect_len,
              "%s truncated to %zu: returned %d, host %d, full length %d", fmt,
              size, n, m, expect_len);
        CHECK(memcmp(mine, theirs, sizeof(mine)) == 0,
              "%s truncated to %zu: got \"%s\", host \"%s\" (of \"%s\")", fmt,
              size, size ? mine : "", size ? theirs : "", expect);
    }
}

static void check_int(const char *fmt, int v)
{
    char mine[BUF];
    char theirs[BUF];
    int a = os101_snprintf(mine, sizeof(mine), fmt, v);
    int b = snprintf(theirs, sizeof(theirs), fmt, v);

    compare(fmt, mine, a, theirs, b);
}

static void check_uint(const char *fmt, unsigned v)
{
    char mine[BUF];
    char theirs[BUF];
    int a = os101_snprintf(mine, sizeof(mine), fmt, v);
    int b = snprintf(theirs, sizeof(theirs), fmt, v);

    compare(fmt, mine, a, theirs, b);
}

static void check_long(const char *fmt, long v)
{
    char mine[BUF];
    char theirs[BUF];
    int a = os101_snprintf(mine, sizeof(mine), fmt, v);
    int b = snprintf(theirs, sizeof(theirs), fmt, v);

    compare(fmt, mine, a, theirs, b);
}

static void check_ulong(const char *fmt, unsigned long v)
{
    char mine[BUF];
    char theirs[BUF];
    int a = os101_snprintf(mine, sizeof(mine), fmt, v);
    int b = snprintf(theirs, sizeof(theirs), fmt, v);

    compare(fmt, mine, a, theirs, b);
}

static void check_llong(const char *fmt, long long v)
{
    char mine[BUF];
    char theirs[BUF];
    int a = os101_snprintf(mine, sizeof(mine), fmt, v);
    int b = snprintf(theirs, sizeof(theirs), fmt, v);

    compare(fmt, mine, a, theirs, b);
}

static void check_size(const char *fmt, size_t v)
{
    char mine[BUF];
    char theirs[BUF];
    int a = os101_snprintf(mine, sizeof(mine), fmt, v);
    int b = snprintf(theirs, sizeof(theirs), fmt, v);

    compare(fmt, mine, a, theirs, b);
}

static void check_double(const char *fmt, double v)
{
    char mine[BUF];
    char theirs[BUF];
    int a = os101_snprintf(mine, sizeof(mine), fmt, v);
    int b = snprintf(theirs, sizeof(theirs), fmt, v);

    compare(fmt, mine, a, theirs, b);
}

static void check_string(const char *fmt, const char *v)
{
    char mine[BUF];
    char theirs[BUF];
    int a = os101_snprintf(mine, sizeof(mine), fmt, v);
    int b = snprintf(theirs, sizeof(theirs), fmt, v);

    compare(fmt, mine, a, theirs, b);
}

static void check_char(const char *fmt, int v)
{
    char mine[BUF];
    char theirs[BUF];
    int a = os101_snprintf(mine, sizeof(mine), fmt, v);
    int b = snprintf(theirs, sizeof(theirs), fmt, v);

    compare(fmt, mine, a, theirs, b);
}

static const char *const FLAGS[] = {"",   "-",  "+",  " ",  "0",
                                    "-0", "+0", "- ", "+ ", "0+"};
static const char *const WIDTHS[] = {"", "1", "3", "8", "12", "20"};
static const char *const PRECS[] = {"", ".0", ".1", ".3", ".8", ".17"};

static void build(char *out, const char *flags, const char *width,
                  const char *prec, const char *length, const char *conv)
{
    sprintf(out, "%%%s%s%s%s%s", flags, width, prec, length, conv);
}

static void integer_conversions(void)
{
    static const int VALUES[] = {0,  1,   -1,  7,   -7,  42,  -42,
                                 99, 100, 255, 256, 1000000, -1000000};
    static const char *const CONVS[] = {"d", "i", "u", "x", "X", "o"};
    static const char *const HASH_CONVS[] = {"x", "X", "o"};
    size_t f;
    size_t w;
    size_t p;
    size_t c;
    size_t v;

    for (f = 0; f < sizeof(FLAGS) / sizeof(FLAGS[0]); f++) {
        for (w = 0; w < sizeof(WIDTHS) / sizeof(WIDTHS[0]); w++) {
            for (p = 0; p < sizeof(PRECS) / sizeof(PRECS[0]); p++) {
                for (c = 0; c < sizeof(CONVS) / sizeof(CONVS[0]); c++) {
                    char fmt[32];
                    build(fmt, FLAGS[f], WIDTHS[w], PRECS[p], "", CONVS[c]);
                    for (v = 0; v < sizeof(VALUES) / sizeof(VALUES[0]); v++) {
                        if (CONVS[c][0] == 'd' || CONVS[c][0] == 'i') {
                            check_int(fmt, VALUES[v]);
                        } else {
                            check_uint(fmt, (unsigned)VALUES[v]);
                        }
                    }
                }
                /* '#' is only defined for the bases that have a prefix. */
                for (c = 0; c < sizeof(HASH_CONVS) / sizeof(HASH_CONVS[0]);
                     c++) {
                    char fmt[32];
                    char flags[8];
                    sprintf(flags, "#%s", FLAGS[f]);
                    build(fmt, flags, WIDTHS[w], PRECS[p], "", HASH_CONVS[c]);
                    for (v = 0; v < sizeof(VALUES) / sizeof(VALUES[0]); v++) {
                        check_uint(fmt, (unsigned)VALUES[v]);
                    }
                }
            }
        }
    }
}

static void length_modifiers(void)
{
    static const long long VALUES[] = {0,
                                       1,
                                       -1,
                                       300,
                                       -300,
                                       65535,
                                       65536,
                                       2147483647LL,
                                       -2147483648LL,
                                       4294967295LL,
                                       9223372036854775807LL,
                                       -9223372036854775807LL - 1};
    static const char *const WIDTH_PREC[] = {"", "8", ".5", "12.6", "-8", "08"};
    size_t i;
    size_t v;

    for (i = 0; i < sizeof(WIDTH_PREC) / sizeof(WIDTH_PREC[0]); i++) {
        for (v = 0; v < sizeof(VALUES) / sizeof(VALUES[0]); v++) {
            char fmt[32];
            long long x = VALUES[v];

            sprintf(fmt, "%%%shhd", WIDTH_PREC[i]);
            check_int(fmt, (int)x);
            sprintf(fmt, "%%%shd", WIDTH_PREC[i]);
            check_int(fmt, (int)x);
            sprintf(fmt, "%%%sld", WIDTH_PREC[i]);
            check_long(fmt, (long)x);
            sprintf(fmt, "%%%slu", WIDTH_PREC[i]);
            check_ulong(fmt, (unsigned long)x);
            sprintf(fmt, "%%%slx", WIDTH_PREC[i]);
            check_ulong(fmt, (unsigned long)x);
            sprintf(fmt, "%%%slld", WIDTH_PREC[i]);
            check_llong(fmt, x);
            sprintf(fmt, "%%%szu", WIDTH_PREC[i]);
            check_size(fmt, (size_t)x);
            sprintf(fmt, "%%%szx", WIDTH_PREC[i]);
            check_size(fmt, (size_t)x);
        }
    }
}

static void float_conversions(void)
{
    static const double VALUES[] = {
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        1.5,
        2.5,
        -2.5,
        0.1,
        0.2,
        1.0 / 3.0,
        2.0 / 3.0,
        0.49999999999999994,   /* rounds down, and catches x+0.5 shortcuts */
        4503599627370496.5,    /* not representable: the .5 is a lie */
        123456.789,
        999999.9999,
        9.9999999999,
        1e-5,
        1e-4,
        1e-1,
        1e5,
        1e15,
        1e16,
        1e20,
        1e-20,
        1e100,
        1e-100,
        3.141592653589793,
        2.718281828459045,
        6.02214076e23,
        1.602176634e-19,
        DBL_MAX,
        DBL_MIN,
        5e-324,                /* the smallest subnormal */
        2.2250738585072011e-308,
        1234567890123456789.0,
        -98765.4321
    };
    static const char *const CONVS[] = {"f", "e", "g", "F", "E", "G"};
    size_t f;
    size_t w;
    size_t p;
    size_t c;
    size_t v;

    for (f = 0; f < sizeof(FLAGS) / sizeof(FLAGS[0]); f++) {
        for (w = 0; w < sizeof(WIDTHS) / sizeof(WIDTHS[0]); w++) {
            for (p = 0; p < sizeof(PRECS) / sizeof(PRECS[0]); p++) {
                for (c = 0; c < sizeof(CONVS) / sizeof(CONVS[0]); c++) {
                    char fmt[32];
                    build(fmt, FLAGS[f], WIDTHS[w], PRECS[p], "", CONVS[c]);
                    for (v = 0; v < sizeof(VALUES) / sizeof(VALUES[0]); v++) {
                        check_double(fmt, VALUES[v]);
                    }
                }
            }
        }
    }

    /* The alternative form, which keeps the point and %g's trailing zeros. */
    for (p = 0; p < sizeof(PRECS) / sizeof(PRECS[0]); p++) {
        for (c = 0; c < sizeof(CONVS) / sizeof(CONVS[0]); c++) {
            char fmt[32];
            build(fmt, "#", "", PRECS[p], "", CONVS[c]);
            for (v = 0; v < sizeof(VALUES) / sizeof(VALUES[0]); v++) {
                check_double(fmt, VALUES[v]);
            }
        }
    }

    /* Long precisions, where the exact expansion of a double runs out and
       every further digit has to be a zero. */
    check_double("%.30f", 0.1);
    check_double("%.60f", 1.0 / 3.0);
    check_double("%.40e", 3.141592653589793);
    check_double("%.50g", 0.2);
    check_double("%.60f", 5e-324);
    check_double("%.0f", 0.5);
    check_double("%.0f", 1.5);
    check_double("%.0f", 2.5);
    check_double("%.0f", -0.5);
    check_double("%.0f", 3.5);
    check_double("%.0e", 9.5);
    check_double("%.1f", 0.05);
    check_double("%.1f", 0.15);
    check_double("%.1f", 0.25);
    check_double("%.2f", 0.005);
    check_double("%.17g", 0.1);
    check_double("%.17e", DBL_MAX);
}

static void nonfinite_conversions(void)
{
    double inf = INFINITY;
    double nan_value = NAN;
    static const char *const CONVS[] = {"f", "e", "g", "F", "E", "G"};
    static const char *const SHAPES[] = {"",    "8",  "-8",  "08",
                                         "+8", " 8", "12.4"};
    /* The sign of a NaN is not specified in the output, and the two libcs use
       that: glibc prints "-nan" and "+nan", the BSDs print "nan" whatever the
       sign bit and the flags say. os101-libc follows glibc, so the NaN cases
       are compared against the host only where the two agree, and the glibc
       shape is pinned down by the fixed expectations below. */
    static const char *const UNSIGNED_SHAPES[] = {"", "8", "-8", "08", "12.4"};
    char buf[BUF];
    size_t c;
    size_t s;

    for (c = 0; c < sizeof(CONVS) / sizeof(CONVS[0]); c++) {
        for (s = 0; s < sizeof(SHAPES) / sizeof(SHAPES[0]); s++) {
            char fmt[32];
            sprintf(fmt, "%%%s%s", SHAPES[s], CONVS[c]);
            check_double(fmt, inf);
            check_double(fmt, -inf);
        }
        for (s = 0; s < sizeof(UNSIGNED_SHAPES) / sizeof(UNSIGNED_SHAPES[0]);
             s++) {
            char fmt[32];
            sprintf(fmt, "%%%s%s", UNSIGNED_SHAPES[s], CONVS[c]);
            check_double(fmt, nan_value);
        }
    }

    os101_snprintf(buf, sizeof(buf), "%f", -nan_value);
    CHECK(strcmp(buf, "-nan") == 0, "%%f of a negative NaN gave \"%s\"", buf);
    os101_snprintf(buf, sizeof(buf), "%+f", nan_value);
    CHECK(strcmp(buf, "+nan") == 0, "%%+f of a NaN gave \"%s\"", buf);
    os101_snprintf(buf, sizeof(buf), "%E", -nan_value);
    CHECK(strcmp(buf, "-NAN") == 0, "%%E of a negative NaN gave \"%s\"", buf);
}

static void string_and_char_conversions(void)
{
    static const char *const VALUES[] = {"",  "a",   "ab",  "hello",
                                         "a longer string than the width"};
    /* No '0': see the note at the top of this file. */
    static const char *const STRING_FLAGS[] = {"", "-", "+", " ", "- ", "+ "};
    size_t f;
    size_t w;
    size_t p;
    size_t v;

    for (f = 0; f < sizeof(STRING_FLAGS) / sizeof(STRING_FLAGS[0]); f++) {
        for (w = 0; w < sizeof(WIDTHS) / sizeof(WIDTHS[0]); w++) {
            for (p = 0; p < sizeof(PRECS) / sizeof(PRECS[0]); p++) {
                char fmt[32];
                build(fmt, STRING_FLAGS[f], WIDTHS[w], PRECS[p], "", "s");
                for (v = 0; v < sizeof(VALUES) / sizeof(VALUES[0]); v++) {
                    check_string(fmt, VALUES[v]);
                }
            }
            {
                char fmt[32];
                build(fmt, STRING_FLAGS[f], WIDTHS[w], "", "", "c");
                check_char(fmt, 'x');
                check_char(fmt, '0');
                check_char(fmt, ' ');
            }
        }
    }

    check_string("%s", NULL);
    check_string("[%s]", NULL);
}

static void literals_and_positions(void)
{
    char mine[BUF];
    char theirs[BUF];
    int a;
    int b;

    a = os101_snprintf(mine, sizeof(mine), "plain text");
    b = snprintf(theirs, sizeof(theirs), "plain text");
    compare("plain text", mine, a, theirs, b);

    a = os101_snprintf(mine, sizeof(mine), "100%% sure");
    b = snprintf(theirs, sizeof(theirs), "100%% sure");
    compare("100%% sure", mine, a, theirs, b);

    a = os101_snprintf(mine, sizeof(mine), "%d/%s/%c/%.2f/%#x/%%", 42, "s", 'c',
                       1.005, 255u);
    b = snprintf(theirs, sizeof(theirs), "%d/%s/%c/%.2f/%#x/%%", 42, "s", 'c',
                 1.005, 255u);
    compare("mixed", mine, a, theirs, b);

    /* A width and a precision taken from the argument list. */
    a = os101_snprintf(mine, sizeof(mine), "%*d|%-*d|%.*f|%*.*e", 8, 42, 8, 42,
                       3, 2.0 / 3.0, 14, 4, 1234.5678);
    b = snprintf(theirs, sizeof(theirs), "%*d|%-*d|%.*f|%*.*e", 8, 42, 8, 42, 3,
                 2.0 / 3.0, 14, 4, 1234.5678);
    compare("star width", mine, a, theirs, b);

    /* A negative star width means left alignment. */
    a = os101_snprintf(mine, sizeof(mine), "[%*d]", -8, 42);
    b = snprintf(theirs, sizeof(theirs), "[%*d]", -8, 42);
    compare("negative star width", mine, a, theirs, b);
}

static void pointer_conversion(void)
{
    char buf[BUF];
    int one = 1;
    void *p = &one;
    char expect[BUF];

    /* Fixed expectations rather than a comparison: the two libcs this is built
       and run on disagree about %p, so os101-libc picks glibc's shape. */
    os101_snprintf(buf, sizeof(buf), "%p", (void *)NULL);
    CHECK(strcmp(buf, "(nil)") == 0, "%%p of NULL gave \"%s\"", buf);

    os101_snprintf(buf, sizeof(buf), "%p", p);
    CHECK(buf[0] == '0' && buf[1] == 'x', "%%p gave \"%s\"", buf);
    sprintf(expect, "0x%lx", (unsigned long)(size_t)p);
    CHECK(strcmp(buf, expect) == 0, "%%p gave \"%s\", expected \"%s\"", buf,
          expect);
}

static void truncation_cases(void)
{
    char full[BUF];
    int n;

    n = os101_snprintf(full, sizeof(full), "%d apples", 12345);
    truncation("%d apples", full, n, 0, 12345, 0.0, NULL);

    n = os101_snprintf(full, sizeof(full), "[%8.3f]", 3.14159);
    truncation("[%8.3f]", full, n, 1, 0, 3.14159, NULL);

    n = os101_snprintf(full, sizeof(full), "%-12s|", "abcdef");
    truncation("%-12s|", full, n, 2, 0, 0.0, "abcdef");

    /* A null buffer with a zero size is allowed: it asks how long the result
       would be, and must not write anything anywhere. */
    CHECK(os101_snprintf(NULL, 0, "%d", 100000) == 6,
          "snprintf(NULL, 0) did not return the length");
    CHECK(os101_snprintf(NULL, 0, "%s and %.3f", "text", 1.5) == 14,
          "snprintf(NULL, 0) with several conversions");
}

static void sprintf_and_vsnprintf(void)
{
    char mine[BUF];
    char theirs[BUF];

    os101_sprintf(mine, "%s=%d (%.3e)", "value", -17, 0.000123456);
    sprintf(theirs, "%s=%d (%.3e)", "value", -17, 0.000123456);
    CHECK(strcmp(mine, theirs) == 0, "sprintf: got \"%s\", host \"%s\"", mine,
          theirs);
}

void run_printf_tests(void)
{
    test_section("printf, against the host's snprintf");
    integer_conversions();
    length_modifiers();
    float_conversions();
    nonfinite_conversions();
    string_and_char_conversions();
    literals_and_positions();
    pointer_conversion();
    truncation_cases();
    sprintf_and_vsnprintf();
}
