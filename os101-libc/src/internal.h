/*
 * Declarations shared between the files of this library and used by nothing
 * outside it. Not installed in include/.
 */
#ifndef OS101_LIBC_INTERNAL_H
#define OS101_LIBC_INTERNAL_H

#include <stddef.h>

/* ---- exit ---------------------------------------------------------------
 *
 * atexit and the C++ runtime's __cxa_atexit share one table, because the
 * standard's rule is that handlers run in the reverse order they were
 * registered regardless of which door they came in by.
 */
int os101_add_exit_handler(void (*fn)(void *), void *arg, void *dso);
void os101_run_exit_handlers(void);

/* Walks __fini_array in reverse. Defined in init.c beside __libc_init. */
void os101_run_fini_array(void);

/* ---- decimal conversion -------------------------------------------------
 *
 * printf's %f/%e/%g and strtod both need to move between binary doubles and
 * decimal digit strings without losing a digit, which needs arithmetic wider
 * than a double. decimal.c does it exactly, with a fixed-capacity big integer,
 * so that a printed value round-trips and so that the digits agree with a
 * hosted libc to the last place.
 */

/* Every finite double has a finite decimal expansion, and the longest has 767
 * significant digits (2^-1074 needs 751). Room for the longest, plus slack for
 * a rounding carry. */
#define OS101_DEC_DIGITS 800

/* The exact significant digits of a finite, non-zero double.
 *
 * `v` may be negative; its sign is ignored. On return `digits` holds
 * `n` characters '0'..'9' with no leading zero and no trailing zero, and
 *
 *     |v| = 0.d[0]d[1]...d[n-1] * 10^(*exp10)
 *
 * The buffer must have room for OS101_DEC_DIGITS characters. Returns n. */
int os101_dec_from_double(double v, char *digits, int *exp10);

/* Round a digit string, produced by the function above or by a parser, to
 * `keep` significant digits, breaking an exact tie towards the even digit —
 * which is what a hosted libc does, and what makes a printed value the
 * shortest one that reads back the same.
 *
 * `keep` may be 0, meaning "is the whole value at least a half". A carry out
 * of the leading digit leaves "1" behind and raises *exp10.
 *
 * Returns the new digit count; trailing zeros are kept, because a caller
 * printing %.6f wants them. */
int os101_dec_round(char *digits, int ndigits, int keep, int *exp10);

/* The nearest double to  (negative ? -1 : 1) * 0.digits * 10^exp10, with ties
 * to even: the correctly-rounded result, not an approximation.
 *
 * Sets *status to 1 on overflow (returns a signed HUGE_VAL) or -1 on
 * underflow to zero, so strtod can raise ERANGE. */
double os101_dec_to_double(const char *digits, int ndigits, int exp10,
                           int negative, int *status);

#endif /* OS101_LIBC_INTERNAL_H */
