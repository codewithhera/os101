/*
 * Freestanding <inttypes.h> for the OS101 QuickJS build.
 *
 * Clang ships an inttypes.h, but it is a wrapper that #include_next's the
 * platform's real one, so on a target with no platform it is useless. This
 * replacement is found first because the shim's include directory precedes the
 * compiler's resource directory on the search path.
 *
 * The prefixes are taken from clang's own __INT64_FMTd__ family rather than
 * hard-coded to "l". Hard-coding is right for the kernel's LP64 target, where
 * int64_t is `long`, and wrong for the macOS host the test harness runs on,
 * where it is `long long` — and getting it wrong there means a wall of -Wformat
 * warnings from the vendored sources, which is exactly the kind of noise that
 * trains people to ignore warnings.
 */
#ifndef OS101_SHIM_INTTYPES_H
#define OS101_SHIM_INTTYPES_H

#include <stdint.h>

#define PRId8 __INT8_FMTd__
#define PRId16 __INT16_FMTd__
#define PRId32 __INT32_FMTd__
#define PRId64 __INT64_FMTd__

#define PRIi8 __INT8_FMTi__
#define PRIi16 __INT16_FMTi__
#define PRIi32 __INT32_FMTi__
#define PRIi64 __INT64_FMTi__

#define PRIu8 __UINT8_FMTu__
#define PRIu16 __UINT16_FMTu__
#define PRIu32 __UINT32_FMTu__
#define PRIu64 __UINT64_FMTu__

#define PRIo8 __UINT8_FMTo__
#define PRIo16 __UINT16_FMTo__
#define PRIo32 __UINT32_FMTo__
#define PRIo64 __UINT64_FMTo__

#define PRIx8 __UINT8_FMTx__
#define PRIx16 __UINT16_FMTx__
#define PRIx32 __UINT32_FMTx__
#define PRIx64 __UINT64_FMTx__

#define PRIX8 __UINT8_FMTX__
#define PRIX16 __UINT16_FMTX__
#define PRIX32 __UINT32_FMTX__
#define PRIX64 __UINT64_FMTX__

#define PRIdPTR __INTPTR_FMTd__
#define PRIiPTR __INTPTR_FMTi__
#define PRIuPTR __UINTPTR_FMTu__
#define PRIxPTR __UINTPTR_FMTx__
#define PRIXPTR __UINTPTR_FMTX__

#define PRIdMAX __INTMAX_FMTd__
#define PRIiMAX __INTMAX_FMTi__
#define PRIuMAX __UINTMAX_FMTu__
#define PRIxMAX __UINTMAX_FMTx__
#define PRIXMAX __UINTMAX_FMTX__

#endif
