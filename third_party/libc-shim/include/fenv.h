/*
 * Freestanding <fenv.h> for the OS101 QuickJS build.
 *
 * quickjs.c includes this header but calls nothing from it — the engine's
 * float formatting reaches the rounding mode it needs through dtoa.c's own
 * integer arithmetic instead. The rounding macros are here so that a future
 * QuickJS release that does start using fegetround() fails to compile loudly
 * rather than silently picking up a wrong default.
 */
#ifndef OS101_SHIM_FENV_H
#define OS101_SHIM_FENV_H

#define FE_TONEAREST 0x0000
#define FE_DOWNWARD 0x2000
#define FE_UPWARD 0x4000
#define FE_TOWARDZERO 0x6000

#endif
