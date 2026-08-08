/*
 * math.c against the host's libm, measured in units in the last place.
 *
 * The point is not to pass a threshold but to know the number: the report says
 * what this library's accuracy is, and this is where the figure comes from. Each
 * function is sampled over a range that includes its argument reduction
 * boundaries, the worst error is recorded, and the run prints a table.
 *
 * The thresholds below are deliberately just above what is achieved, so that a
 * change which makes something less accurate fails here rather than being
 * discovered by an application.
 */
#include <float.h>
#include <math.h>
#include <stdio.h>
#include <string.h>

#include "harness.h"
#include "os101_api.h"

/* How many representable doubles apart two values are. Both must be finite and
   have the same sign, which is true everywhere it is used below. */
static double ulp_distance(double a, double b)
{
    long long ia;
    long long ib;
    long long diff;

    if (a == b) {
        return 0.0;
    }
    if (isnan(a) || isnan(b)) {
        return isnan(a) && isnan(b) ? 0.0 : 1e300;
    }
    if (isinf(a) || isinf(b)) {
        return a == b ? 0.0 : 1e300;
    }
    memcpy(&ia, &a, sizeof(ia));
    memcpy(&ib, &b, sizeof(ib));
    /* Flip to a monotonic ordering across zero. */
    if (ia < 0) {
        ia = (long long)0x8000000000000000ULL - ia;
    }
    if (ib < 0) {
        ib = (long long)0x8000000000000000ULL - ib;
    }
    diff = ia > ib ? ia - ib : ib - ia;
    return (double)diff;
}

typedef double (*Unary)(double);
typedef double (*Binary)(double, double);

static void report(const char *name, double worst, double limit, double at)
{
    printf("   %-8s worst %6.2f ulp (limit %.2f)%s\n", name, worst, limit,
           worst > limit ? "  <-- OVER" : "");
    CHECK(worst <= limit, "%s is %.2f ulp out at %.17g, limit %.2f", name,
          worst, at, limit);
}

static void sweep_unary(const char *name, Unary mine, Unary theirs, double lo,
                        double hi, int steps, double limit)
{
    double worst = 0.0;
    double worst_at = 0.0;
    int i;

    for (i = 0; i <= steps; i++) {
        double x = lo + (hi - lo) * ((double)i / (double)steps);
        double a = mine(x);
        double b = theirs(x);
        double d = ulp_distance(a, b);

        if (d > worst) {
            worst = d;
            worst_at = x;
        }
    }
    report(name, worst, limit, worst_at);
}

/* Logarithmic sweep, for the functions whose interesting range spans decades. */
static void sweep_unary_log(const char *name, Unary mine, Unary theirs,
                            double from, double to, int steps, double limit)
{
    double worst = 0.0;
    double worst_at = 0.0;
    double ratio = pow(to / from, 1.0 / (double)steps);
    double x = from;
    int i;

    for (i = 0; i <= steps; i++) {
        double a = mine(x);
        double b = theirs(x);
        double d = ulp_distance(a, b);

        if (d > worst) {
            worst = d;
            worst_at = x;
        }
        x *= ratio;
    }
    report(name, worst, limit, worst_at);
}

static void exact_cases(void)
{
    /* The values that have to come out exactly right, not just close. */
    CHECK(os101_sqrt(4.0) == 2.0, "sqrt(4) is not 2");
    CHECK(os101_sqrt(0.0) == 0.0 && os101_signbit(os101_sqrt(-0.0)),
          "sqrt of zero");
    CHECK(os101_isnan(os101_sqrt(-1.0)), "sqrt(-1) is not NaN");
    CHECK(os101_sqrt(1e300) == sqrt(1e300), "sqrt(1e300)");

    CHECK(os101_floor(2.5) == 2.0 && os101_floor(-2.5) == -3.0, "floor");
    CHECK(os101_ceil(2.5) == 3.0 && os101_ceil(-2.5) == -2.0, "ceil");
    CHECK(os101_trunc(2.9) == 2.0 && os101_trunc(-2.9) == -2.0, "trunc");
    CHECK(os101_round(2.5) == 3.0 && os101_round(-2.5) == -3.0,
          "round does not go away from zero on a half");
    CHECK(os101_round(0.49999999999999994) == 0.0,
          "round(0.49999999999999994) is not 0");
    CHECK(os101_signbit(os101_floor(-0.5)) && os101_floor(-0.5) == -1.0,
          "floor(-0.5)");
    CHECK(os101_signbit(os101_ceil(-0.5)) && os101_ceil(-0.5) == 0.0,
          "ceil(-0.5) should be -0.0");
    CHECK(os101_signbit(os101_trunc(-0.5)), "trunc(-0.5) should be -0.0");

    {
        static const double VALUES[] = {0.0,   -0.0,  0.5,   -0.5,  1.0,
                                       -1.0,  2.5,   -2.5,  1e16,  -1e16,
                                       1e300, 4.5,   -4.5,  0.9999999999,
                                       123456789.5, 5e-324};
        size_t i;
        for (i = 0; i < sizeof(VALUES) / sizeof(VALUES[0]); i++) {
            double x = VALUES[i];
            CHECK(memcmp((double[]){os101_floor(x)}, (double[]){floor(x)},
                         sizeof(double)) == 0,
                  "floor(%.17g)", x);
            CHECK(memcmp((double[]){os101_ceil(x)}, (double[]){ceil(x)},
                         sizeof(double)) == 0,
                  "ceil(%.17g)", x);
            CHECK(memcmp((double[]){os101_trunc(x)}, (double[]){trunc(x)},
                         sizeof(double)) == 0,
                  "trunc(%.17g)", x);
            CHECK(memcmp((double[]){os101_round(x)}, (double[]){round(x)},
                         sizeof(double)) == 0,
                  "round(%.17g)", x);
        }
    }

    /* fmod is required to be exact. */
    {
        static const double PAIRS[][2] = {
            {5.0, 3.0},      {-5.0, 3.0},    {5.0, -3.0},   {-5.0, -3.0},
            {1.0, 0.1},      {1e300, 3.0},   {1e-300, 1e-310},
            {123456.789, 1.5}, {0.0, 1.0},   {7.0, 7.0},    {3.0, 5.0},
            {1e16, 3.0},     {2.5, 0.5}
        };
        size_t i;
        for (i = 0; i < sizeof(PAIRS) / sizeof(PAIRS[0]); i++) {
            double a = os101_fmod(PAIRS[i][0], PAIRS[i][1]);
            double b = fmod(PAIRS[i][0], PAIRS[i][1]);
            CHECK(memcmp(&a, &b, sizeof(a)) == 0,
                  "fmod(%.17g, %.17g): got %.17g, host %.17g", PAIRS[i][0],
                  PAIRS[i][1], a, b);
        }
        CHECK(os101_isnan(os101_fmod(1.0, 0.0)), "fmod by zero is not NaN");
    }

    /* Powers of two, and the small integer exponents that must be exact. */
    CHECK(os101_pow(2.0, 10.0) == 1024.0, "pow(2,10)");
    CHECK(os101_pow(10.0, 3.0) == 1000.0, "pow(10,3)");
    CHECK(os101_pow(2.0, -2.0) == 0.25, "pow(2,-2)");
    CHECK(os101_pow(-2.0, 3.0) == -8.0, "pow(-2,3)");
    CHECK(os101_pow(-2.0, 2.0) == 4.0, "pow(-2,2)");
    CHECK(os101_isnan(os101_pow(-2.0, 0.5)), "pow(-2,0.5) is not NaN");
    CHECK(os101_pow(1.0, 0.0) == 1.0 && os101_pow(0.0, 0.0) == 1.0, "pow(x,0)");
    CHECK(os101_pow(0.0, 3.0) == 0.0, "pow(0,3)");
    CHECK(os101_isinf(os101_pow(0.0, -1.0)), "pow(0,-1) is not infinite");
    CHECK(os101_log2(1024.0) == 10.0, "log2(1024) is not exactly 10");
    CHECK(os101_log2(1.0) == 0.0, "log2(1)");
    CHECK(os101_log(1.0) == 0.0, "log(1)");
    CHECK(os101_exp(0.0) == 1.0, "exp(0)");
    CHECK(os101_sin(0.0) == 0.0 && os101_cos(0.0) == 1.0, "sin/cos of 0");
    CHECK(os101_atan(0.0) == 0.0, "atan(0)");
    CHECK(os101_asin(0.0) == 0.0, "asin(0)");
    CHECK(os101_hypot(3.0, 4.0) == 5.0, "hypot(3,4)");
    CHECK(os101_cbrt(27.0) == 3.0, "cbrt(27) is not exactly 3");
    CHECK(os101_cbrt(-8.0) == -2.0, "cbrt(-8)");

    /* Infinities and NaNs where the standard is specific. */
    CHECK(os101_isinf(os101_exp(1000.0)), "exp overflow");
    CHECK(os101_exp(-1000.0) == 0.0, "exp underflow");
    CHECK(os101_isinf(os101_log(INFINITY)), "log(inf)");
    CHECK(os101_isinf(os101_log(0.0)) && os101_log(0.0) < 0, "log(0)");
    CHECK(os101_isnan(os101_log(-1.0)), "log(-1)");
    CHECK(os101_isnan(os101_asin(1.5)), "asin out of domain");
    CHECK(os101_isnan(os101_acos(-1.5)), "acos out of domain");

    /* frexp, ldexp and modf round trips. */
    {
        static const double VALUES[] = {1.0,   0.5,    3.75,  1e300,
                                       1e-300, 5e-324, 0.0,  -12345.678};
        size_t i;
        for (i = 0; i < sizeof(VALUES) / sizeof(VALUES[0]); i++) {
            int e_mine = 0;
            int e_theirs = 0;
            double m_mine = os101_frexp(VALUES[i], &e_mine);
            double m_theirs = frexp(VALUES[i], &e_theirs);
            double ip_mine = 0.0;
            double ip_theirs = 0.0;
            double f_mine;
            double f_theirs;

            CHECK(m_mine == m_theirs && e_mine == e_theirs,
                  "frexp(%.17g): got %.17g,%d host %.17g,%d", VALUES[i], m_mine,
                  e_mine, m_theirs, e_theirs);
            CHECK(os101_ldexp(m_mine, e_mine) == VALUES[i],
                  "ldexp did not undo frexp for %.17g", VALUES[i]);
            f_mine = os101_modf(VALUES[i], &ip_mine);
            f_theirs = modf(VALUES[i], &ip_theirs);
            CHECK(f_mine == f_theirs && ip_mine == ip_theirs, "modf(%.17g)",
                  VALUES[i]);
        }
        CHECK(os101_ldexp(1.0, -1074) == 5e-324,
              "ldexp into the subnormals lost the value");
        CHECK(os101_ldexp(1.0, 2000) == INFINITY, "ldexp overflow");
        CHECK(os101_ldexp(1.0, -2000) == 0.0, "ldexp underflow");
    }
}

static double host_exp2(double x) { return exp2(x); }
static double host_expm1(double x) { return expm1(x); }
static double host_log1p(double x) { return log1p(x); }
static double host_cbrt(double x) { return cbrt(x); }

static void accuracy(void)
{
    /* exp and log: the reduction boundaries are at multiples of ln2 and at
       sqrt(2), so a sweep across several decades crosses them all. */
    sweep_unary("exp", os101_exp, exp, -700.0, 700.0, 20000, 1.0);
    sweep_unary("exp/near0", os101_exp, exp, -1.0, 1.0, 20000, 1.0);
    sweep_unary_log("log", os101_log, log, 1e-300, 1e300, 20000, 1.0);
    sweep_unary_log("log/near1", os101_log, log, 0.9, 1.1, 20000, 2.0);
    sweep_unary_log("log2", os101_log2, log2, 1e-300, 1e300, 20000, 1.5);
    sweep_unary_log("log10", os101_log10, log10, 1e-300, 1e300, 20000, 2.0);
    sweep_unary_log("exp2", os101_exp2, host_exp2, 1e-8, 1000.0, 20000, 1.5);
    sweep_unary("expm1", os101_expm1, host_expm1, -0.5, 0.5, 20000, 2.0);
    sweep_unary("log1p", os101_log1p, host_log1p, -0.5, 0.5, 20000, 2.0);
    sweep_unary_log("cbrt", os101_cbrt, host_cbrt, 1e-100, 1e100, 20000, 2.0);

    /* Trigonometry: across the quadrant boundaries, and out to where the
       argument reduction is all that is holding the answer together. */
    sweep_unary("sin", os101_sin, sin, -10.0, 10.0, 20000, 1.0);
    sweep_unary("cos", os101_cos, cos, -10.0, 10.0, 20000, 1.0);
    sweep_unary("sin/big", os101_sin, sin, 1e6, 1e6 + 100.0, 20000, 2.0);
    sweep_unary("cos/big", os101_cos, cos, 1e6, 1e6 + 100.0, 20000, 2.0);
    /*
     * At 1e14 the reduction has subtracted 6e13 multiples of pi/2, so what is
     * left of the argument is only as good as pi/2 is known to — 105 bits here
     * — and the worst case lands where sin is near a zero, where an absolute
     * error of 1e-19 is a large *relative* one. That is what this limit is:
     * ninety ulp of 9e-6, which is 1e-19 in absolute terms, or a thousand times
     * finer than the spacing of the doubles around 1e14 in the first place.
     * Getting further needs Payne-Hanek reduction; see reduce_pi2 in math.c.
     */
    sweep_unary("sin/huge", os101_sin, sin, 1e14, 1e14 + 1000.0, 20000, 128.0);
    /* tan is a quotient of two series, so it carries both their errors, and
       near its poles the quotient magnifies them. */
    sweep_unary("tan", os101_tan, tan, -1.5, 1.5, 20000, 3.0);
    sweep_unary("atan", os101_atan, atan, -100.0, 100.0, 20000, 1.0);
    /* The middle branch adds pi/4, which is not representable, to the series:
       one rounding of the constant and one of the sum. */
    sweep_unary("atan/sml", os101_atan, atan, -1.0, 1.0, 20000, 2.0);
    sweep_unary("asin", os101_asin, asin, -1.0, 1.0, 20000, 2.0);
    sweep_unary("acos", os101_acos, acos, -1.0, 1.0, 20000, 2.0);
    /* expm1's error, then the division and two additions that turn it into a
       hyperbolic sine. */
    sweep_unary("sinh", os101_sinh, sinh, -20.0, 20.0, 20000, 3.0);
    sweep_unary("cosh", os101_cosh, cosh, -20.0, 20.0, 20000, 2.0);
    sweep_unary("tanh", os101_tanh, tanh, -10.0, 10.0, 20000, 2.0);

    /* pow, where the error is the logarithm's multiplied by the exponent. */
    {
        double worst = 0.0;
        double worst_x = 0.0;
        double worst_y = 0.0;
        int i;
        int j;

        for (i = 1; i <= 200; i++) {
            double x = (double)i * 0.137;
            for (j = -60; j <= 60; j++) {
                double y = (double)j * 1.7;
                double a = os101_pow(x, y);
                double b = pow(x, y);
                double d;
                if (!isfinite(b) || b == 0.0) {
                    continue;
                }
                d = ulp_distance(a, b);
                if (d > worst) {
                    worst = d;
                    worst_x = x;
                    worst_y = y;
                }
            }
        }
        printf("   %-8s worst %6.2f ulp (limit %.2f) at pow(%.17g, %.17g)\n",
               "pow", worst, 4.0, worst_x, worst_y);
        CHECK(worst <= 4.0, "pow is %.2f ulp out at pow(%.17g, %.17g)", worst,
              worst_x, worst_y);
    }

    /* atan2, over all four quadrants. */
    {
        double worst = 0.0;
        int i;
        int j;

        for (i = -50; i <= 50; i++) {
            for (j = -50; j <= 50; j++) {
                double y = (double)i * 0.31;
                double x = (double)j * 0.27;
                double d;
                if (x == 0.0 && y == 0.0) {
                    continue;
                }
                d = ulp_distance(os101_atan2(y, x), atan2(y, x));
                if (d > worst) {
                    worst = d;
                }
            }
        }
        report("atan2", worst, 2.0, 0.0);
    }

    /* hypot, which must not overflow on the way to a finite answer. */
    CHECK(os101_hypot(1e300, 1e300) == hypot(1e300, 1e300),
          "hypot overflowed where the host did not");
    CHECK(os101_hypot(1e-320, 1e-320) == hypot(1e-320, 1e-320),
          "hypot underflowed");
}

void run_math_tests(void)
{
    test_section("math.h, against the host's libm");
    exact_cases();
    accuracy();
}
