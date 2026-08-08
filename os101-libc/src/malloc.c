/*
 * malloc, over the kernel's sbrk (syscall 4).
 *
 * A boundary-tag allocator: every block carries its own size and the size of
 * the block physically before it, so a free can find both neighbours in
 * constant time and merge with them. Free blocks are also on one doubly linked
 * list, threaded through their own payloads, and malloc takes the first that
 * fits and splits the remainder off. That is the textbook design, chosen for
 * the reason the brief gives: what matters is that a program which allocates
 * and frees in a loop reuses its memory rather than asking the kernel for more
 * forever.
 *
 *   block:  [ size | prev_size ][ payload ... ]
 *             16 bytes of header  16-byte aligned
 *
 * The low bit of `size` says the block is in use; sizes are multiples of 16, so
 * the bit is going spare. `prev_size` is zero at the start of the region, which
 * is how the backward walk knows where to stop.
 *
 * The heap is one contiguous region: the kernel's sbrk hands out a single
 * monotonically growing window (kernel/src/process.rs), so each request comes
 * back adjacent to the last. An application that calls os101_sbrk itself and
 * then calls malloc would break that adjacency, so a gap is walled off with a
 * permanently-allocated block rather than trusted.
 */
#include <errno.h>
#include <os101.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define ALIGNMENT 16u
#define HEADER 16u
#define MIN_BLOCK 32u

/* Ask the kernel for memory in big steps: a page at a time would mean a
   syscall, a page-table walk and a frame allocation for every few objects. */
#define GROW_MIN (64u * 1024u)
/* Hand memory back when this much has piled up free at the top of the heap, so
   that a program with one large peak does not hold on to the peak for ever. */
#define TRIM_THRESHOLD (1024u * 1024u)

typedef struct Block {
    size_t size; /* whole block including the header; low bit = in use */
    size_t prev_size;
} Block;

/* Threaded through a free block's payload, which is why MIN_BLOCK is 32. */
typedef struct FreeNode {
    struct FreeNode *next;
    struct FreeNode *prev;
} FreeNode;

static char *heap_lo;
static char *heap_hi;
/* The block whose end is the break. Kept because free() wants to know in
   constant time whether what it just released is at the top. */
static Block *top_block;
static FreeNode *free_head;

static size_t block_size(const Block *b)
{
    return b->size & ~(size_t)1;
}

static int in_use(const Block *b)
{
    return (int)(b->size & 1u);
}

static void set_block(Block *b, size_t size, int used)
{
    b->size = size | (used ? 1u : 0u);
}

static void *payload(Block *b)
{
    return (char *)b + HEADER;
}

static Block *block_of(void *p)
{
    return (Block *)((char *)p - HEADER);
}

static Block *next_block(Block *b)
{
    char *next = (char *)b + block_size(b);

    return next < heap_hi ? (Block *)next : NULL;
}

static Block *prev_block(Block *b)
{
    if (b->prev_size == 0) {
        return NULL;
    }
    return (Block *)((char *)b - b->prev_size);
}

static size_t align_up(size_t v, size_t align)
{
    return (v + align - 1) & ~(align - 1);
}

/* Called after anything that changes the tiling: the top block is exactly the
   one with nothing above it. */
static void note_top(Block *b)
{
    if (next_block(b) == NULL) {
        top_block = b;
    }
}

static void list_insert(Block *b)
{
    FreeNode *node = (FreeNode *)payload(b);

    node->prev = NULL;
    node->next = free_head;
    if (free_head != NULL) {
        free_head->prev = node;
    }
    free_head = node;
}

static void list_remove(Block *b)
{
    FreeNode *node = (FreeNode *)payload(b);

    if (node->prev != NULL) {
        node->prev->next = node->next;
    } else {
        free_head = node->next;
    }
    if (node->next != NULL) {
        node->next->prev = node->prev;
    }
}

/* Record `b`'s size in the block above it, which is what keeps the backward
   walk honest after a split or a merge. */
static void fix_next_prev_size(Block *b)
{
    Block *next = next_block(b);

    if (next != NULL) {
        next->prev_size = block_size(b);
    }
}

/* Put a block on the free list, merged with whichever of its neighbours are
   also free. Returns the block that resulted. */
static Block *release(Block *b)
{
    Block *next = next_block(b);
    Block *prev = prev_block(b);

    set_block(b, block_size(b), 0);

    if (next != NULL && !in_use(next)) {
        list_remove(next);
        set_block(b, block_size(b) + block_size(next), 0);
    }
    if (prev != NULL && !in_use(prev)) {
        list_remove(prev);
        set_block(prev, block_size(prev) + block_size(b), 0);
        b = prev;
    }
    fix_next_prev_size(b);
    note_top(b);
    list_insert(b);
    return b;
}

/* Cut `b` down to exactly `want` bytes, freeing the tail if there is enough of
   it to be a block in its own right. `b` must be off the free list and marked
   in use already. */
static void split(Block *b, size_t want)
{
    size_t have = block_size(b);
    Block *tail;

    if (have - want < MIN_BLOCK) {
        return;
    }
    set_block(b, want, 1);
    tail = (Block *)((char *)b + want);
    tail->prev_size = want;
    set_block(tail, have - want, 0);
    fix_next_prev_size(tail);
    note_top(tail);
    list_insert(tail);
}

/* Lower the break when a lot has piled up free at the top. The kernel leaves
   the pages mapped, so growing back into them later costs nothing. */
static void trim(void)
{
    size_t size;
    size_t give;

    if (top_block == NULL || in_use(top_block)) {
        return;
    }
    size = block_size(top_block);
    if (size < TRIM_THRESHOLD) {
        return;
    }
    /* Keep a comfortable amount, hand the rest back a whole page at a time. */
    give = (size - GROW_MIN) & ~(size_t)0xfff;
    if (give == 0) {
        return;
    }
    if (os101_sbrk(-(long)give) == (void *)-1) {
        return;
    }
    heap_hi -= give;
    set_block(top_block, size - give, 0);
}

/* Extend the heap and return a free block of at least `need` bytes, or NULL. */
static Block *grow(size_t need)
{
    size_t want = need < GROW_MIN ? GROW_MIN : need;
    char *base;
    Block *b;

    want = align_up(want, 4096);
    base = (char *)os101_sbrk((long)want);
    if (base == (char *)-1) {
        /* Nearly full: ask for the minimum rather than the comfortable size. */
        want = align_up(need, 4096);
        base = (char *)os101_sbrk((long)want);
        if (base == (char *)-1) {
            return NULL;
        }
    }

    if (heap_lo == NULL) {
        char *aligned = (char *)align_up((size_t)(uintptr_t)base, ALIGNMENT);
        want -= (size_t)(aligned - base);
        heap_lo = aligned;
        heap_hi = aligned;
        base = aligned;
    } else if (base != heap_hi) {
        /* Something else moved the break. Wall the gap off with a block that
           is permanently in use, so the tiling stays contiguous and neither
           walk ever steps into memory this allocator does not own. */
        size_t gap = (size_t)(base - heap_hi);
        Block *wall = (Block *)heap_hi;

        if (gap < MIN_BLOCK) {
            return NULL;
        }
        wall->prev_size = top_block != NULL ? block_size(top_block) : 0;
        set_block(wall, gap, 1);
        heap_hi = base;
        top_block = wall;
    }

    b = (Block *)base;
    b->prev_size = (base == heap_lo || top_block == NULL)
                       ? 0
                       : block_size(top_block);
    heap_hi = base + want;
    /* Marked in use so that release() below merges it with the block underneath
       if that one is free, and does the bookkeeping in one place. */
    set_block(b, want, 1);
    top_block = b;
    return release(b);
}

static Block *find_fit(size_t want)
{
    FreeNode *node;

    for (node = free_head; node != NULL; node = node->next) {
        Block *b = block_of(node);
        if (block_size(b) >= want) {
            return b;
        }
    }
    return NULL;
}

/* The block size needed to hold `size` payload bytes, or 0 on overflow. */
static size_t needed_for(size_t size)
{
    size_t want;

    if (size + HEADER < size) {
        return 0;
    }
    want = align_up(size + HEADER, ALIGNMENT);
    if (want < size) {
        return 0;
    }
    return want < MIN_BLOCK ? MIN_BLOCK : want;
}

void *malloc(size_t size)
{
    size_t want;
    Block *b;

    /* malloc(0) hands back a block of its own rather than NULL: a caller that
       then frees it, or compares it against NULL to detect failure, is right
       either way. */
    if (size == 0) {
        size = 1;
    }
    want = needed_for(size);
    if (want == 0) {
        errno = ENOMEM;
        return NULL;
    }

    b = find_fit(want);
    if (b == NULL) {
        b = grow(want);
        if (b == NULL) {
            errno = ENOMEM;
            return NULL;
        }
    }
    list_remove(b);
    set_block(b, block_size(b), 1);
    split(b, want);
    return payload(b);
}

void free(void *ptr)
{
    Block *b;

    if (ptr == NULL) {
        return;
    }
    b = block_of(ptr);
    if (!in_use(b)) {
        /* Freed twice. Ignoring it keeps the heap consistent, which is worth
           more to someone whose program is already wrong than a crash. */
        return;
    }
    release(b);
    trim();
}

void *calloc(size_t nmemb, size_t size)
{
    size_t total = nmemb * size;
    void *p;

    if (size != 0 && total / size != nmemb) {
        errno = ENOMEM;
        return NULL;
    }
    p = malloc(total);
    if (p != NULL) {
        memset(p, 0, total);
    }
    return p;
}

void *realloc(void *ptr, size_t size)
{
    Block *b;
    Block *next;
    size_t want;
    size_t have;
    void *fresh;

    if (ptr == NULL) {
        return malloc(size);
    }
    if (size == 0) {
        free(ptr);
        return NULL;
    }

    b = block_of(ptr);
    have = block_size(b);
    want = needed_for(size);
    if (want == 0) {
        errno = ENOMEM;
        return NULL;
    }

    if (want <= have) {
        split(b, want);
        return ptr;
    }

    /* Grow in place by swallowing the block above, if it is free and there is
       enough of it — the ordinary case for a buffer being appended to. */
    next = next_block(b);
    if (next != NULL && !in_use(next) && have + block_size(next) >= want) {
        list_remove(next);
        set_block(b, have + block_size(next), 1);
        fix_next_prev_size(b);
        note_top(b);
        split(b, want);
        return ptr;
    }

    fresh = malloc(size);
    if (fresh == NULL) {
        return NULL;
    }
    /* have - HEADER is the old payload, all of which fits: this path only runs
       when the new block is strictly larger than the old one. */
    memcpy(fresh, ptr, have - HEADER);
    free(ptr);
    return fresh;
}

void *aligned_alloc(size_t alignment, size_t size)
{
    size_t want;
    size_t extra;
    Block *b;
    char *aligned;
    size_t front;

    if (alignment <= ALIGNMENT) {
        return malloc(size);
    }
    if ((alignment & (alignment - 1)) != 0) {
        errno = EINVAL;
        return NULL;
    }
    want = needed_for(size);
    if (want == 0) {
        errno = ENOMEM;
        return NULL;
    }

    /* Take enough that an aligned payload is certain to fit with a block's
       worth of slack in front of it, then give the front back. Everything
       stays an ordinary block, so free() needs to know nothing about
       alignment. Twice the alignment because the aligned point may have to
       move up once to leave room for a header below it. */
    extra = 2 * alignment + MIN_BLOCK;
    if (want + extra < want) {
        errno = ENOMEM;
        return NULL;
    }
    b = find_fit(want + extra);
    if (b == NULL) {
        b = grow(want + extra);
        if (b == NULL) {
            errno = ENOMEM;
            return NULL;
        }
    }
    list_remove(b);
    set_block(b, block_size(b), 1);

    aligned = (char *)align_up((size_t)(uintptr_t)payload(b), alignment);
    front = (size_t)(aligned - HEADER - (char *)b);
    if (front != 0) {
        size_t total = block_size(b);
        Block *head = b;

        while (front < MIN_BLOCK) {
            aligned += alignment;
            front = (size_t)(aligned - HEADER - (char *)b);
        }
        b = (Block *)(aligned - HEADER);
        set_block(head, front, 1);
        b->prev_size = front;
        set_block(b, total - front, 1);
        fix_next_prev_size(b);
        note_top(b);
        release(head);
    }
    split(b, want);
    return payload(b);
}
