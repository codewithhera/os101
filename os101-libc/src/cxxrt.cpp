/*
 * The C++ runtime support a freestanding C++ program needs on OS101.
 *
 * Not a standard library: there is no std::vector, no std::string, no
 * std::type_info and no unwinder. What is here is the set of symbols the
 * compiler itself emits references to, so that ordinary C++ — classes,
 * templates, RAII, virtual functions, new and delete, objects with static
 * storage duration — links and runs:
 *
 *   operator new / delete        every form clang can call, including the
 *                                sized and nothrow ones, and the C++17
 *                                over-aligned ones
 *   __cxa_atexit, __dso_handle   destructors of objects at namespace scope
 *   __cxa_guard_*                the thread-safe-once around a function-local
 *                                static with a non-trivial constructor
 *   __cxa_pure_virtual           a call through a base pointer to a method
 *                                that has no override
 *   __cxa_deleted_virtual        the same, for a method that is `= delete`d
 *
 * Applications are built with -fno-exceptions and -fno-rtti, which is what
 * makes this list short enough to be complete: with exceptions on it would
 * need __cxa_throw, __cxa_begin_catch, the personality routine and a DWARF
 * unwinder, and with RTTI on it would need the type_info class hierarchy.
 * tools/os101-c++ passes both flags, so what the compiler asks for and what
 * this file provides cannot drift apart.
 *
 * There is one thread, so the guard functions do no locking. That is not a
 * shortcut to be fixed later so much as a fact of the system: the kernel gives
 * a process one thread of control and nothing to make another with.
 */
#include <new>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>

namespace std {
const nothrow_t nothrow{};
} // namespace std

/*
 * A failed allocation in a throwing operator new has nowhere to go: there is no
 * std::bad_alloc to throw and no handler to call. Stopping with a message is
 * the only honest end, and it is what a program without exceptions would have
 * to do in its own handler anyway. Code that wants to carry on regardless can
 * use the nothrow forms, which return null.
 */
static void *checked_allocate(size_t size)
{
    void *p = malloc(size);

    if (p == nullptr) {
        fputs("operator new: out of memory\n", stderr);
        abort();
    }
    return p;
}

void *operator new(std::size_t size)
{
    return checked_allocate(size);
}

void *operator new[](std::size_t size)
{
    return checked_allocate(size);
}

void *operator new(std::size_t size, const std::nothrow_t &) noexcept
{
    return malloc(size);
}

void *operator new[](std::size_t size, const std::nothrow_t &) noexcept
{
    return malloc(size);
}

void operator delete(void *p) noexcept
{
    free(p);
}

void operator delete[](void *p) noexcept
{
    free(p);
}

/* The sized forms: clang calls these when it knows the static type's size.
   This allocator does not need the size — every block records its own — so
   they forward. */
void operator delete(void *p, std::size_t) noexcept
{
    free(p);
}

void operator delete[](void *p, std::size_t) noexcept
{
    free(p);
}

void operator delete(void *p, const std::nothrow_t &) noexcept
{
    free(p);
}

void operator delete[](void *p, const std::nothrow_t &) noexcept
{
    free(p);
}

#if defined(__cpp_aligned_new)
static void *checked_allocate_aligned(size_t size, std::align_val_t align)
{
    void *p = aligned_alloc(static_cast<size_t>(align), size);

    if (p == nullptr) {
        fputs("operator new: out of memory\n", stderr);
        abort();
    }
    return p;
}

void *operator new(std::size_t size, std::align_val_t align)
{
    return checked_allocate_aligned(size, align);
}

void *operator new[](std::size_t size, std::align_val_t align)
{
    return checked_allocate_aligned(size, align);
}

void *operator new(std::size_t size, std::align_val_t align,
                   const std::nothrow_t &) noexcept
{
    return aligned_alloc(static_cast<std::size_t>(align), size);
}

void *operator new[](std::size_t size, std::align_val_t align,
                     const std::nothrow_t &) noexcept
{
    return aligned_alloc(static_cast<std::size_t>(align), size);
}

void operator delete(void *p, std::align_val_t) noexcept
{
    free(p);
}

void operator delete[](void *p, std::align_val_t) noexcept
{
    free(p);
}

void operator delete(void *p, std::size_t, std::align_val_t) noexcept
{
    free(p);
}

void operator delete[](void *p, std::size_t, std::align_val_t) noexcept
{
    free(p);
}

void operator delete(void *p, std::align_val_t, const std::nothrow_t &) noexcept
{
    free(p);
}

void operator delete[](void *p, std::align_val_t,
                       const std::nothrow_t &) noexcept
{
    free(p);
}
#endif /* __cpp_aligned_new */

extern "C" {

/* Nothing is loaded or unloaded at runtime, so this only has to exist and be
   unique: it is the "which shared object" argument of every __cxa_atexit call
   the compiler emits. */
void *__dso_handle = &__dso_handle;

int os101_add_exit_handler(void (*fn)(void *), void *arg, void *dso);

int __cxa_atexit(void (*destructor)(void *), void *object, void *dso)
{
    return os101_add_exit_handler(destructor, object, dso);
}

/* A call through a base pointer to a method with no definition — which means
   an object was used during its own base class's construction or after its
   destruction. */
void __cxa_pure_virtual(void)
{
    fputs("pure virtual function called\n", stderr);
    abort();
}

void __cxa_deleted_virtual(void)
{
    fputs("deleted virtual function called\n", stderr);
    abort();
}

/*
 * Function-local statics. The Itanium ABI's guard is a 64-bit object whose
 * first byte says "constructed"; the second is free for implementations to use,
 * and it is used here to catch the one thing that can still go wrong without
 * threads: a constructor that, directly or not, reaches its own variable again.
 * With no way to wait for another thread to finish, that recursion would
 * otherwise return an object under construction.
 */
int __cxa_guard_acquire(char *guard)
{
    if (guard[0] != 0) {
        return 0; /* already constructed */
    }
    if (guard[1] != 0) {
        fputs("recursive initialisation of a function-local static\n", stderr);
        abort();
    }
    guard[1] = 1;
    return 1;
}

void __cxa_guard_release(char *guard)
{
    guard[0] = 1;
    guard[1] = 0;
}

void __cxa_guard_abort(char *guard)
{
    guard[1] = 0;
}

} /* extern "C" */
