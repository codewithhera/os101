#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <os101.h>

typedef struct Hdr {
    size_t size;
    struct Hdr *next;
    int free;
} Hdr;

static Hdr *free_list;

void *malloc(size_t n)
{
    Hdr *h, **p;
    size_t need;
    if (n == 0)
        n = 1;
    need = (n + 15) & ~(size_t)15;
    for (p = &free_list; (h = *p); p = &h->next) {
        if (h->free && h->size >= need) {
            h->free = 0;
            return (char *)h + sizeof(Hdr);
        }
    }
    h = (Hdr *)os101_sbrk((long)(sizeof(Hdr) + need));
    if (h == (Hdr *)(intptr_t)-1)
        return 0;
    h->size = need;
    h->next = free_list;
    h->free = 0;
    free_list = h;
    return (char *)h + sizeof(Hdr);
}

void free(void *p)
{
    Hdr *h;
    if (!p)
        return;
    h = (Hdr *)((char *)p - sizeof(Hdr));
    h->free = 1;
}

void *calloc(size_t n, size_t sz)
{
    size_t t;
    void *p;
    if (n && sz > (size_t)-1 / n)
        return 0;
    t = n * sz;
    p = malloc(t);
    if (p)
        memset(p, 0, t);
    return p;
}

void *realloc(void *p, size_t n)
{
    Hdr *h;
    void *q;
    if (!p)
        return malloc(n);
    h = (Hdr *)((char *)p - sizeof(Hdr));
    if (h->size >= n)
        return p;
    q = malloc(n);
    if (!q)
        return 0;
    memcpy(q, p, h->size);
    free(p);
    return q;
}
