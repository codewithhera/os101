/*
 * malloc, against a stub sbrk over a static array (host_stubs.c).
 *
 * The torture test is the one that matters. It keeps hundreds of blocks alive
 * at once, allocates and frees in a shuffled order, fills every block with a
 * pattern derived from its own address and checks the pattern again later, and
 * verifies after every allocation that the new block overlaps none of the live
 * ones. Between them, those catch the two failures that a boundary-tag
 * allocator actually has: a split or a merge that gets a size wrong and hands
 * the same memory out twice, and a coalesce that steps into a neighbour it
 * should not have touched.
 *
 * Then there is the growth test, which is the requirement from the brief: a
 * program that allocates and frees in a loop must not grow without bound. It
 * runs a hundred thousand allocate-and-free cycles and asserts the break has
 * not moved since the first few.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "harness.h"
#include "host_stubs.h"
#include "os101_api.h"

#define LIVE_MAX 512

typedef struct {
    unsigned char *ptr;
    size_t size;
    unsigned char seed;
} Live;

static Live live[LIVE_MAX];
static size_t live_count;

static unsigned long rng_state = 20240501;

static unsigned long next_random(void)
{
    rng_state = rng_state * 6364136223846793005UL + 1442695040888963407UL;
    return rng_state >> 33;
}

static void fill(Live *block)
{
    size_t i;

    for (i = 0; i < block->size; i++) {
        block->ptr[i] = (unsigned char)(block->seed + (unsigned char)i);
    }
}

static int verify(const Live *block)
{
    size_t i;

    for (i = 0; i < block->size; i++) {
        if (block->ptr[i] != (unsigned char)(block->seed + (unsigned char)i)) {
            return 0;
        }
    }
    return 1;
}

static int overlaps_live(const unsigned char *ptr, size_t size, size_t skip)
{
    size_t i;

    for (i = 0; i < live_count; i++) {
        if (i == skip) {
            continue;
        }
        if (ptr < live[i].ptr + live[i].size && live[i].ptr < ptr + size) {
            return 1;
        }
    }
    return 0;
}

static void alignment_and_basics(void)
{
    static const size_t SIZES[] = {0,  1,   2,   3,   8,   15,  16,  17,
                                  31, 32,  33,  64,  100, 1000, 4096, 65536,
                                  100000};
    void *blocks[32];
    size_t i;

    for (i = 0; i < sizeof(SIZES) / sizeof(SIZES[0]); i++) {
        void *p = os101_malloc(SIZES[i]);
        CHECK(p != NULL, "malloc(%zu) returned NULL", SIZES[i]);
        CHECK(((size_t)p & 15u) == 0, "malloc(%zu) is not 16-byte aligned",
              SIZES[i]);
        if (p != NULL && SIZES[i] > 0) {
            memset(p, 0xa5, SIZES[i]);
        }
        os101_free(p);
    }

    /* Two live blocks are never the same block. */
    for (i = 0; i < 32; i++) {
        blocks[i] = os101_malloc(64);
        CHECK(blocks[i] != NULL, "malloc(64) failed at %zu", i);
        memset(blocks[i], (int)i, 64);
    }
    for (i = 0; i < 32; i++) {
        size_t j;
        for (j = i + 1; j < 32; j++) {
            CHECK(blocks[i] != blocks[j], "malloc handed out %zu twice", i);
        }
    }
    for (i = 0; i < 32; i++) {
        unsigned char *p = blocks[i];
        size_t k;
        for (k = 0; k < 64; k++) {
            CHECK(p[k] == (unsigned char)i, "block %zu was corrupted at %zu", i,
                  k);
        }
        os101_free(blocks[i]);
    }

    /* free(NULL) is defined to do nothing. */
    os101_free(NULL);

    {
        unsigned char *p = os101_calloc(1000, 7);
        size_t k;
        int zeroed = 1;
        CHECK(p != NULL, "calloc failed");
        for (k = 0; k < 7000; k++) {
            if (p[k] != 0) {
                zeroed = 0;
            }
        }
        CHECK(zeroed, "calloc did not zero its memory");
        os101_free(p);
        /* An overflowing product has to fail rather than allocate too little. */
        CHECK(os101_calloc((size_t)-1 / 2, 4) == NULL,
              "calloc did not notice an overflowing product");
    }

    {
        void *p = os101_aligned_alloc(64, 100);
        CHECK(p != NULL && ((size_t)p & 63u) == 0, "aligned_alloc(64)");
        os101_free(p);
        p = os101_aligned_alloc(4096, 10);
        CHECK(p != NULL && ((size_t)p & 4095u) == 0, "aligned_alloc(4096)");
        os101_free(p);
        p = os101_aligned_alloc(256, 5000);
        CHECK(p != NULL && ((size_t)p & 255u) == 0, "aligned_alloc(256, 5000)");
        if (p != NULL) {
            memset(p, 1, 5000);
        }
        os101_free(p);
    }
}

static void reallocation(void)
{
    unsigned char *p = os101_realloc(NULL, 100);
    size_t i;

    CHECK(p != NULL, "realloc(NULL, n) did not allocate");
    for (i = 0; i < 100; i++) {
        p[i] = (unsigned char)i;
    }

    /* Growing, repeatedly, which is the case that should mostly happen in
       place because the block above is free. */
    for (i = 200; i <= 20000; i += 300) {
        size_t k;
        unsigned char *q = os101_realloc(p, i);
        CHECK(q != NULL, "realloc to %zu failed", i);
        if (q == NULL) {
            return;
        }
        for (k = 0; k < 100; k++) {
            CHECK(q[k] == (unsigned char)k, "realloc to %zu lost byte %zu", i,
                  k);
        }
        p = q;
    }

    /* Shrinking keeps the contents and should give the tail back. */
    p = os101_realloc(p, 50);
    CHECK(p != NULL, "realloc down failed");
    for (i = 0; i < 50; i++) {
        CHECK(p[i] == (unsigned char)i, "realloc down lost byte %zu", i);
    }

    CHECK(os101_realloc(p, 0) == NULL, "realloc(p, 0) should free and return NULL");
}

static void torture(void)
{
    int step;
    int checked = 0;

    for (step = 0; step < 40000; step++) {
        int allocate = live_count == 0
                       || (live_count < LIVE_MAX && (next_random() % 100) < 55);

        if (allocate) {
            size_t size = (next_random() % 100) == 0
                              ? 1 + next_random() % 200000
                              : 1 + next_random() % 2048;
            unsigned char *p = os101_malloc(size);

            if (p == NULL) {
                /* The arena is finite; running out is not a failure, but it
                   must not happen while barely anything is live. */
                CHECK(live_count > 16, "malloc(%zu) failed with %zu live blocks",
                      size, live_count);
                continue;
            }
            CHECK(((size_t)p & 15u) == 0, "block of %zu is misaligned", size);
            CHECK(!overlaps_live(p, size, (size_t)-1),
                  "block of %zu at %p overlaps a live block", size,
                  (void *)p);
            live[live_count].ptr = p;
            live[live_count].size = size;
            live[live_count].seed = (unsigned char)(next_random() & 0xff);
            fill(&live[live_count]);
            live_count++;
        } else {
            size_t victim = next_random() % live_count;
            CHECK(verify(&live[victim]),
                  "block of %zu bytes was corrupted before being freed",
                  live[victim].size);
            os101_free(live[victim].ptr);
            live[victim] = live[live_count - 1];
            live_count--;
        }

        /* Sweep every live block from time to time: a bad merge shows up as
           damage to a block nobody has touched. */
        if ((step % 500) == 0) {
            size_t i;
            for (i = 0; i < live_count; i++) {
                CHECK(verify(&live[i]), "live block %zu (%zu bytes) corrupted",
                      i, live[i].size);
            }
            checked++;
        }
    }

    CHECK(checked > 50, "the sweep did not run often enough");

    while (live_count > 0) {
        live_count--;
        CHECK(verify(&live[live_count]), "block corrupted at the end of the run");
        os101_free(live[live_count].ptr);
    }
}

static void growth_is_bounded(void)
{
    size_t settled;
    size_t after;
    int i;

    /* Everything above has been freed, so the allocator should be reusing one
       block for the whole of this loop. */
    for (i = 0; i < 200; i++) {
        void *p = os101_malloc(4096);
        os101_free(p);
    }
    settled = os101_test_sbrk_total();
    for (i = 0; i < 100000; i++) {
        void *p = os101_malloc(1 + (size_t)(i % 4000));
        memset(p, 0x33, 1 + (size_t)(i % 4000));
        os101_free(p);
    }
    after = os101_test_sbrk_total();
    CHECK(after == settled,
          "the heap grew from %zu to %zu bytes over a loop that frees "
          "everything it allocates",
          settled, after);

    /* The same, with two blocks alive at a time, which needs a merge of the
       two neighbours to keep reusing the same memory. */
    settled = os101_test_sbrk_total();
    for (i = 0; i < 20000; i++) {
        void *a = os101_malloc(300);
        void *b = os101_malloc(700);
        os101_free(a);
        os101_free(b);
    }
    CHECK(os101_test_sbrk_total() == settled,
          "the heap grew from %zu to %zu over an alternating loop", settled,
          os101_test_sbrk_total());

    /* A saw-tooth: allocate a lot, free it, and see the break come back down.
       That one needs the trim path, which only runs above its threshold. */
    {
        void *big[64];
        size_t before = os101_test_sbrk_total();
        size_t peak;
        int k;

        for (k = 0; k < 64; k++) {
            big[k] = os101_malloc(200000);
        }
        peak = os101_test_sbrk_total();
        for (k = 0; k < 64; k++) {
            os101_free(big[k]);
        }
        CHECK(peak > before, "the heap did not grow for 12 MB of allocations");
        CHECK(os101_test_sbrk_total() < peak,
              "the heap stayed at its peak of %zu after everything was freed",
              peak);
        CHECK(os101_test_sbrk_total() <= before + 1024u * 1024u,
              "the heap kept %zu bytes above the %zu it started with",
              os101_test_sbrk_total() - before, before);
    }
}

static void double_free_is_survivable(void)
{
    void *p = os101_malloc(128);
    void *q;

    os101_free(p);
    os101_free(p); /* wrong, but it must not wreck the heap */
    q = os101_malloc(128);
    CHECK(q != NULL, "the heap did not survive a double free");
    memset(q, 7, 128);
    os101_free(q);
}

void run_malloc_tests(void)
{
    test_section("malloc, over a stub sbrk");
    alignment_and_basics();
    reallocation();
    torture();
    growth_is_bounded();
    double_free_is_survivable();
    printf("   arena: %zu bytes in use at the end, %zu at the peak\n",
           os101_test_sbrk_total(), os101_test_sbrk_peak());
}
