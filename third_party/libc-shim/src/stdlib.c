/*
 * The one <stdlib.h> function the shim implements in C. Everything else this
 * header promises comes from the Rust side, because it needs the kernel's heap
 * or the kernel's panic path.
 */
#include <stdlib.h>

int abs(int x)
{
    /*
     * Negating INT_MIN is undefined, and dtoa.c reaches this with a
     * mul_log2_radix result that is bounded well inside int range, so clamping
     * rather than negating keeps the function total without pretending the
     * clamped answer is meaningful.
     */
    if (x == (-2147483647 - 1))
        return 2147483647;
    return x < 0 ? -x : x;
}
