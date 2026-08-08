/*
 * assert for OS101. A failure prints the expression and its location to the
 * console and ends the process; there is no core to dump and no debugger to
 * attach.
 *
 * Deliberately outside the include guard below the declaration: the standard
 * lets a translation unit include this header more than once with NDEBUG
 * changed in between, and expects the macro to follow.
 */
#ifndef _OS101_ASSERT_H
#define _OS101_ASSERT_H

#ifdef __cplusplus
extern "C" {
#endif

void os101_assert_fail(const char *expr, const char *file, int line,
                       const char *func) __attribute__((noreturn));

#ifdef __cplusplus
}
#endif

#endif /* _OS101_ASSERT_H */

#undef assert
#ifdef NDEBUG
#define assert(e) ((void)0)
#else
#define assert(e) \
    ((e) ? (void)0 : os101_assert_fail(#e, __FILE__, __LINE__, __func__))
#endif
