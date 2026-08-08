#ifndef OS101_SHIM_UNISTD_H
#define OS101_SHIM_UNISTD_H

#include <sys/types.h>
#include <stddef.h>

#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

#define STDIN_FILENO  0
#define STDOUT_FILENO 1
#define STDERR_FILENO 2

ssize_t read(int fd, void *buf, size_t count);
ssize_t write(int fd, const void *buf, size_t count);
int close(int fd);
off_t lseek(int fd, off_t offset, int whence);
int unlink(const char *path);
int access(const char *path, int mode);
char *getcwd(char *buf, size_t size);
int chdir(const char *path);
unsigned sleep(unsigned seconds);

#define F_OK 0
#define R_OK 4
#define W_OK 2
#define X_OK 1

#ifndef _SC_PAGESIZE
#define _SC_PAGESIZE 30
#endif
long sysconf(int name);
int getpagesize(void);

extern char **environ;

#endif
