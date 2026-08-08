/*
 * Freestanding <math.h> for the OS101 QuickJS build.
 *
 * Every function declared here is implemented by the pure-Rust `libm` crate,
 * re-exported under its C name by the Rust half of the shim. That was a
 * deliberate choice over porting musl's C maths code: `libm` is a direct
 * translation of the same musl routines, it is `no_std` by construction, and it
 * comes with the Rust test suite already run against it — so the accuracy
 * question is somebody else's answered problem rather than ours.
 *
 * The classification macros stay as compiler builtins. They compile to a couple
 * of SSE instructions with no call, which matters because QuickJS calls isnan()
 * on nearly every arithmetic fast path.
 */
#ifndef OS101_SHIM_MATH_H
#define OS101_SHIM_MATH_H

#ifdef __cplusplus
extern "C" {
#endif

#define NAN (__builtin_nanf(""))
#define INFINITY (__builtin_inff())
#define HUGE_VAL (__builtin_huge_val())
#define HUGE_VALF (__builtin_huge_valf())

#define M_PI 3.14159265358979323846
#define M_E 2.7182818284590452354
#define M_LN2 0.69314718055994530942
#define M_LN10 2.30258509299404568402
#define M_SQRT2 1.41421356237309504880

#define FP_NAN 0
#define FP_INFINITE 1
#define FP_ZERO 2
#define FP_SUBNORMAL 3
#define FP_NORMAL 4

#define isnan(x) __builtin_isnan(x)
#define isinf(x) __builtin_isinf(x)
#define isfinite(x) __builtin_isfinite(x)
#define isnormal(x) __builtin_isnormal(x)
#define signbit(x) __builtin_signbit(x)
#define fpclassify(x) \
    __builtin_fpclassify(FP_NAN, FP_INFINITE, FP_NORMAL, FP_SUBNORMAL, \
                         FP_ZERO, x)
#define nan(tag) __builtin_nan(tag)
#define nanf(tag) __builtin_nanf(tag)

double fabs(double x);
double sqrt(double x);
double cbrt(double x);
double floor(double x);
double ceil(double x);
double trunc(double x);
double round(double x);
double rint(double x);
double fmod(double x, double y);
double remainder(double x, double y);
double modf(double x, double *iptr);
double fmin(double x, double y);
double fmax(double x, double y);
double copysign(double x, double y);
double scalbn(double x, int n);
double ldexp(double x, int n);
long double ldexpl(long double x, int n);
double frexp(double x, int *e);
double nextafter(double x, double y);
double hypot(double x, double y);
double pow(double x, double y);
double exp(double x);
double expm1(double x);
double log(double x);
double log1p(double x);
double log2(double x);
double log10(double x);
double sin(double x);
double cos(double x);
double tan(double x);
double asin(double x);
double acos(double x);
double atan(double x);
double atan2(double y, double x);
double sinh(double x);
double cosh(double x);
double tanh(double x);
double asinh(double x);
double acosh(double x);
double atanh(double x);

long lrint(double x);
long long llrint(double x);

float fabsf(float x);
float sqrtf(float x);
float floorf(float x);
float ceilf(float x);
float truncf(float x);
float roundf(float x);
float fmodf(float x, float y);
float powf(float x, float y);

#ifdef __cplusplus
}
#endif

#endif
