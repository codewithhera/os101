/*
 * math for OS101.
 *
 * The classification predicates are plain functions rather than the C99
 * type-generic macros: this library has one floating-point type it takes
 * seriously, and a function can be tested against the host's libm on the
 * build machine, which a macro over __builtin_isnan cannot.
 */
#ifndef _OS101_MATH_H
#define _OS101_MATH_H

#ifdef __cplusplus
extern "C" {
#endif

#define HUGE_VAL __builtin_huge_val()
#define HUGE_VALF __builtin_huge_valf()
#define INFINITY __builtin_inff()
#define NAN __builtin_nanf("")

#define M_E 2.7182818284590452354
#define M_LOG2E 1.4426950408889634074
#define M_LOG10E 0.43429448190325182765
#define M_LN2 0.69314718055994530942
#define M_LN10 2.30258509299404568402
#define M_PI 3.14159265358979323846
#define M_PI_2 1.57079632679489661923
#define M_PI_4 0.78539816339744830962
#define M_1_PI 0.31830988618379067154
#define M_2_PI 0.63661977236758134308
#define M_SQRT2 1.41421356237309504880
#define M_SQRT1_2 0.70710678118654752440

#define FP_NAN 0
#define FP_INFINITE 1
#define FP_ZERO 2
#define FP_SUBNORMAL 3
#define FP_NORMAL 4

int isnan(double x);
int isinf(double x);
int isfinite(double x);
int signbit(double x);
int fpclassify(double x);

double fabs(double x);
double sqrt(double x);
double cbrt(double x);
double floor(double x);
double ceil(double x);
double round(double x);
double trunc(double x);
double fmod(double x, double y);
double modf(double x, double *ipart);
double frexp(double x, int *exp);
double ldexp(double x, int exp);
double copysign(double x, double y);
double fmin(double x, double y);
double fmax(double x, double y);
double hypot(double x, double y);
double nan(const char *tag);

double exp(double x);
double exp2(double x);
double expm1(double x);
double log(double x);
double log2(double x);
double log10(double x);
double log1p(double x);
double pow(double x, double y);

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

/* Single-precision forms, computed in double and rounded once. */
float fabsf(float x);
float sqrtf(float x);
float floorf(float x);
float ceilf(float x);
float roundf(float x);
float truncf(float x);
float fmodf(float x, float y);
float powf(float x, float y);
float expf(float x);
float logf(float x);
float sinf(float x);
float cosf(float x);
float tanf(float x);
float atan2f(float y, float x);

#ifdef __cplusplus
}
#endif

#endif /* _OS101_MATH_H */
