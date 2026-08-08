#ifndef _STDDEF_H
#define _STDDEF_H
typedef unsigned long size_t;
typedef long ptrdiff_t;
#define NULL ((void*)0)
#define offsetof(t,m) __builtin_offsetof(t,m)
#endif
