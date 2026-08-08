#ifndef OS101_SHIM_SYS_MMAN_H
#define OS101_SHIM_SYS_MMAN_H

#include <stddef.h>
#include <sys/types.h>

#define PROT_READ  1
#define PROT_WRITE 2
#define PROT_EXEC  4
#define PROT_NONE  0

#define MAP_SHARED    1
#define MAP_PRIVATE   2
#define MAP_FIXED     0x10
#define MAP_ANONYMOUS 0x20
#define MAP_FAILED    ((void *)-1)

void *mmap(void *addr, size_t length, int prot, int flags, int fd, off_t offset);
int munmap(void *addr, size_t length);
int mprotect(void *addr, size_t length, int prot);

#endif
