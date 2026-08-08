/*
 * Every standard name this library defines, renamed to os101_* for the host
 * test build.
 *
 * The tests compare os101-libc against the build machine's own libc, so both
 * have to be in one program: the tests include the system <stdio.h> and call
 * the host's snprintf, while the library's own sources are compiled with this
 * header forced in front of them (-include) so that everything they define and
 * everything they call inside themselves becomes os101_-prefixed and cannot
 * collide with, or silently replace, the host's version.
 *
 * The list has to be complete. os101-libc/tests/run.sh checks that with nm:
 * every external symbol the library's host objects define has to start with
 * os101_, so a name left out of this file fails the build rather than quietly
 * overriding the libc the answers are being compared against.
 *
 * Names that already start with os101_ are absent on purpose — they need no
 * renaming, and they are how the harness plugs in its own sbrk and console.
 */
#ifndef OS101_LIBC_HOST_NAMES_H
#define OS101_LIBC_HOST_NAMES_H

/* stdio.h */
#define printf os101_printf
#define fprintf os101_fprintf
#define sprintf os101_sprintf
#define snprintf os101_snprintf
#define vprintf os101_vprintf
#define vfprintf os101_vfprintf
#define vsprintf os101_vsprintf
#define vsnprintf os101_vsnprintf
#define puts os101_puts
#define putchar os101_putchar
#define fputs os101_fputs
#define fputc os101_fputc
#define putc os101_putc
#define fwrite os101_fwrite
#define fflush os101_fflush
#define perror os101_perror
#define fgetc os101_fgetc
#define getc os101_getc
#define getchar os101_getchar
#define fgets os101_fgets
#define fread os101_fread
#define fopen os101_fopen
#define fclose os101_fclose
#define stdin os101_stdin
#define stdout os101_stdout
#define stderr os101_stderr

/* stdlib.h */
#define malloc os101_malloc
#define calloc os101_calloc
#define realloc os101_realloc
#define free os101_free
#define aligned_alloc os101_aligned_alloc
#define exit os101_exit
#define _Exit os101_Exit
#define abort os101_abort
#define atexit os101_atexit
#define atoi os101_atoi
#define atol os101_atol
#define atoll os101_atoll
#define atof os101_atof
#define strtol os101_strtol
#define strtoul os101_strtoul
#define strtoll os101_strtoll
#define strtoull os101_strtoull
#define strtod os101_strtod
#define strtof os101_strtof
#define abs os101_abs
#define labs os101_labs
#define llabs os101_llabs
#define div os101_div
#define ldiv os101_ldiv
#define qsort os101_qsort
#define bsearch os101_bsearch
#define rand os101_rand
#define srand os101_srand
#define getenv os101_getenv

/* string.h */
#define memcpy os101_memcpy
#define memmove os101_memmove
#define memset os101_memset
#define memcmp os101_memcmp
#define memchr os101_memchr
#define strlen os101_strlen
#define strnlen os101_strnlen
#define strcpy os101_strcpy
#define strncpy os101_strncpy
#define strcat os101_strcat
#define strncat os101_strncat
#define strcmp os101_strcmp
#define strncmp os101_strncmp
#define strchr os101_strchr
#define strrchr os101_strrchr
#define strstr os101_strstr
#define strspn os101_strspn
#define strcspn os101_strcspn
#define strpbrk os101_strpbrk
#define strtok os101_strtok
#define strdup os101_strdup
#define strndup os101_strndup
#define strerror os101_strerror

/* ctype.h */
#define isalnum os101_isalnum
#define isalpha os101_isalpha
#define isascii os101_isascii
#define isblank os101_isblank
#define iscntrl os101_iscntrl
#define isdigit os101_isdigit
#define isgraph os101_isgraph
#define islower os101_islower
#define isprint os101_isprint
#define ispunct os101_ispunct
#define isspace os101_isspace
#define isupper os101_isupper
#define isxdigit os101_isxdigit
#define tolower os101_tolower
#define toupper os101_toupper

/* errno.h */
#define errno os101_errno

/* math.h */
#define isnan os101_isnan
#define isinf os101_isinf
#define isfinite os101_isfinite
#define signbit os101_signbit
#define fpclassify os101_fpclassify
#define nan os101_nan
#define fabs os101_fabs
#define sqrt os101_sqrt
#define cbrt os101_cbrt
#define floor os101_floor
#define ceil os101_ceil
#define round os101_round
#define trunc os101_trunc
#define fmod os101_fmod
#define modf os101_modf
#define frexp os101_frexp
#define ldexp os101_ldexp
#define copysign os101_copysign
#define fmin os101_fmin
#define fmax os101_fmax
#define hypot os101_hypot
#define exp os101_exp
#define exp2 os101_exp2
#define expm1 os101_expm1
#define log os101_log
#define log2 os101_log2
#define log10 os101_log10
#define log1p os101_log1p
#define pow os101_pow
#define sin os101_sin
#define cos os101_cos
#define tan os101_tan
#define asin os101_asin
#define acos os101_acos
#define atan os101_atan
#define atan2 os101_atan2
#define sinh os101_sinh
#define cosh os101_cosh
#define tanh os101_tanh
#define fabsf os101_fabsf
#define sqrtf os101_sqrtf
#define floorf os101_floorf
#define ceilf os101_ceilf
#define roundf os101_roundf
#define truncf os101_truncf
#define fmodf os101_fmodf
#define powf os101_powf
#define expf os101_expf
#define logf os101_logf
#define sinf os101_sinf
#define cosf os101_cosf
#define tanf os101_tanf
#define atan2f os101_atan2f

/* time.h — not built for the host (it is all syscall), but listed so that
   adding it later does not need a second look at this file. */
#define time os101_time
#define clock os101_clock
#define difftime os101_difftime
#define gettimeofday os101_gettimeofday

#endif /* OS101_LIBC_HOST_NAMES_H */
