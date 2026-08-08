/*
 * Freestanding <assert.h> for the OS101 QuickJS build.
 *
 * QuickJS has around 160 live assertions and Bellard's own release build keeps
 * them enabled, so we do too: in a kernel with one address space an assertion
 * that fires is a much better outcome than the memory corruption it is
 * guarding against. Define NDEBUG in the build recipe to trade them for size.
 *
 * This header is deliberately not guarded against multiple inclusion, because
 * the C standard requires assert() to be redefinable by re-including it after
 * changing NDEBUG.
 */
#include <stddef.h>

#ifndef OS101_SHIM_ASSERT_DECLARED
#define OS101_SHIM_ASSERT_DECLARED
#ifdef __cplusplus
extern "C" {
#endif
void os101_shim_assert_fail(const char *expr, const char *file, int line)
    __attribute__((noreturn));
#ifdef __cplusplus
}
#endif
#endif

#undef assert
#ifdef NDEBUG
#define assert(e) ((void)0)
#else
#define assert(e) \
    ((e) ? (void)0 : os101_shim_assert_fail(#e, __FILE__, __LINE__))
#endif
