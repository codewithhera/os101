/*
 * Freestanding <pthread.h> for the OS101 QuickJS build.
 *
 * quickjs.c enables CONFIG_ATOMICS on every target except Emscripten, and the
 * only way to turn it off is to pretend to be Emscripten — which would also
 * switch off the computed-goto interpreter dispatch and, worse, the stack
 * limit check we depend on. So the cheaper trade is to satisfy the fifteen
 * pthread calls instead, and since the whole runtime lives on one kernel thread
 * they are all static inline no-ops: a mutex with a single possible holder never
 * blocks, and a condition variable that nobody else can signal never wakes.
 *
 * Being inline means the shim contributes no pthread symbols at all, and it
 * means a reader who greps the archive for "pthread" correctly finds nothing.
 *
 * The one visible consequence is Atomics.wait with an infinite timeout, which
 * would return "ok" immediately instead of blocking forever. That path is
 * unreachable in practice: it first checks JSRuntime::can_block, which
 * JS_NewRuntime leaves false and which nothing in this OS sets.
 */
#ifndef OS101_SHIM_PTHREAD_H
#define OS101_SHIM_PTHREAD_H

#include <errno.h>

typedef struct {
    int unused;
} pthread_mutex_t;

typedef struct {
    int unused;
} pthread_cond_t;

typedef struct {
    int unused;
} pthread_mutexattr_t;

typedef struct {
    int unused;
} pthread_condattr_t;

#define PTHREAD_MUTEX_INITIALIZER {0}
#define PTHREAD_COND_INITIALIZER {0}

static inline int pthread_mutex_init(pthread_mutex_t *m,
                                     const pthread_mutexattr_t *a)
{
    (void)m;
    (void)a;
    return 0;
}
static inline int pthread_mutex_destroy(pthread_mutex_t *m)
{
    (void)m;
    return 0;
}
static inline int pthread_mutex_lock(pthread_mutex_t *m)
{
    (void)m;
    return 0;
}
static inline int pthread_mutex_trylock(pthread_mutex_t *m)
{
    (void)m;
    return 0;
}
static inline int pthread_mutex_unlock(pthread_mutex_t *m)
{
    (void)m;
    return 0;
}

static inline int pthread_cond_init(pthread_cond_t *c,
                                    const pthread_condattr_t *a)
{
    (void)c;
    (void)a;
    return 0;
}
static inline int pthread_cond_destroy(pthread_cond_t *c)
{
    (void)c;
    return 0;
}
static inline int pthread_cond_signal(pthread_cond_t *c)
{
    (void)c;
    return 0;
}
static inline int pthread_cond_broadcast(pthread_cond_t *c)
{
    (void)c;
    return 0;
}
static inline int pthread_cond_wait(pthread_cond_t *c, pthread_mutex_t *m)
{
    (void)c;
    (void)m;
    return 0;
}
static inline int pthread_cond_timedwait(pthread_cond_t *c, pthread_mutex_t *m,
                                         const struct timespec *ts)
{
    (void)c;
    (void)m;
    (void)ts;
    return ETIMEDOUT;
}

#endif
