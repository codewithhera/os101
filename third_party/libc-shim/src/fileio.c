/*
 * VFS-backed file descriptors and FILE* for TinyCC.
 *
 * The Rust half registers callbacks that read and write through the kernel VFS
 * (`/disk`, `/data`, …). Until then, every open fails with ENOENT so a host
 * unit test of the rest of the shim still links.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdarg.h>
#include <sys/stat.h>
#include <sys/mman.h>
#include "shim_file.h"

enum { OS101_FD_MAX = 64 };
enum { OS101_PATH_MAX = 512 };

typedef struct {
    int used;
    int writable;
    int append;
    size_t pos;
    size_t len;
    size_t cap;
    char *data;
    char path[OS101_PATH_MAX];
} FdSlot;

static FdSlot g_fds[OS101_FD_MAX];
static struct _OS101_FILE shim_stdin_file  = {0, 0, 0, 0, 0, 0};
static struct _OS101_FILE shim_stdout_file = {1, 1, 0, 0, 0, 0};
static struct _OS101_FILE shim_stderr_file = {2, 2, 0, 0, 0, 0};

FILE *const os101_shim_stdin  = &shim_stdin_file;
FILE *const os101_shim_stdout = &shim_stdout_file;
FILE *const os101_shim_stderr = &shim_stderr_file;

/* Provided by printf.c */
void os101_shim_write_bytes(int stream, const char *buf, size_t len);

/* Rust VFS hooks — return 0 on success. read fills *out_len / *out_data (malloc). */
int os101_vfs_read_file(const char *path, unsigned char **out_data, size_t *out_len);
int os101_vfs_write_file(const char *path, const unsigned char *data, size_t len);
int os101_vfs_remove_file(const char *path);
int os101_vfs_file_exists(const char *path);

extern int errno;

static int fd_alloc(void)
{
    int i;
    for (i = 3; i < OS101_FD_MAX; i++) {
        if (!g_fds[i].used)
            return i;
    }
    return -1;
}

static void fd_release(int fd)
{
    FdSlot *s;
    if (fd < 0 || fd >= OS101_FD_MAX)
        return;
    s = &g_fds[fd];
    free(s->data);
    s->data = NULL;
    s->used = 0;
    s->len = s->cap = s->pos = 0;
    s->path[0] = 0;
}

int open(const char *path, int flags, ...)
{
    int fd;
    unsigned char *data = NULL;
    size_t len = 0;
    FdSlot *s;
    int creat = (flags & O_CREAT) != 0;
    int trunc = (flags & O_TRUNC) != 0;
    int wr = (flags & O_WRONLY) || (flags & O_RDWR);
    int rd = !(flags & O_WRONLY);

    if (!path) {
        errno = EINVAL;
        return -1;
    }

    if (rd || !creat) {
        if (os101_vfs_read_file(path, &data, &len) != 0) {
            if (!creat) {
                errno = ENOENT;
                return -1;
            }
            data = NULL;
            len = 0;
        }
    }

    if (trunc) {
        free(data);
        data = NULL;
        len = 0;
    }

    fd = fd_alloc();
    if (fd < 0) {
        free(data);
        errno = ENOMEM;
        return -1;
    }
    s = &g_fds[fd];
    s->used = 1;
    s->writable = wr || creat;
    s->append = (flags & O_APPEND) != 0;
    s->data = (char *)data;
    s->len = len;
    s->cap = len;
    s->pos = s->append ? len : 0;
    strncpy(s->path, path, OS101_PATH_MAX - 1);
    s->path[OS101_PATH_MAX - 1] = 0;

    if (creat && !s->data) {
        s->data = NULL;
        s->len = s->cap = 0;
    }
    return fd;
}

int creat(const char *path, mode_t mode)
{
    (void)mode;
    return open(path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
}

int close(int fd)
{
    FdSlot *s;
    int rc = 0;
    if (fd < 3)
        return 0;
    if (fd >= OS101_FD_MAX || !g_fds[fd].used) {
        errno = EBADF;
        return -1;
    }
    s = &g_fds[fd];
    if (s->writable && s->path[0]) {
        if (os101_vfs_write_file(s->path, (unsigned char *)s->data, s->len) != 0) {
            errno = EIO;
            rc = -1;
        }
    }
    fd_release(fd);
    return rc;
}

ssize_t read(int fd, void *buf, size_t count)
{
    FdSlot *s;
    size_t n;
    if (fd < 3) {
        errno = EIO;
        return -1;
    }
    if (fd >= OS101_FD_MAX || !g_fds[fd].used) {
        errno = EBADF;
        return -1;
    }
    s = &g_fds[fd];
    if (s->pos >= s->len)
        return 0;
    n = s->len - s->pos;
    if (n > count)
        n = count;
    memcpy(buf, s->data + s->pos, n);
    s->pos += n;
    return (ssize_t)n;
}

ssize_t write(int fd, const void *buf, size_t count)
{
    FdSlot *s;
    char *nbuf;
    size_t need;
    if (fd == 1 || fd == 2) {
        os101_shim_write_bytes(fd, (const char *)buf, count);
        return (ssize_t)count;
    }
    if (fd < 3) {
        errno = EIO;
        return -1;
    }
    if (fd >= OS101_FD_MAX || !g_fds[fd].used || !g_fds[fd].writable) {
        errno = EBADF;
        return -1;
    }
    s = &g_fds[fd];
    if (s->append)
        s->pos = s->len;
    need = s->pos + count;
    if (need > s->cap) {
        size_t ncap = s->cap ? s->cap * 2 : 256;
        while (ncap < need)
            ncap *= 2;
        nbuf = realloc(s->data, ncap);
        if (!nbuf) {
            errno = ENOMEM;
            return -1;
        }
        s->data = nbuf;
        s->cap = ncap;
    }
    memcpy(s->data + s->pos, buf, count);
    s->pos += count;
    if (s->pos > s->len)
        s->len = s->pos;
    return (ssize_t)count;
}

off_t lseek(int fd, off_t offset, int whence)
{
    FdSlot *s;
    off_t np;
    if (fd < 3 || fd >= OS101_FD_MAX || !g_fds[fd].used) {
        errno = EBADF;
        return -1;
    }
    s = &g_fds[fd];
    if (whence == SEEK_SET)
        np = offset;
    else if (whence == SEEK_CUR)
        np = (off_t)s->pos + offset;
    else if (whence == SEEK_END)
        np = (off_t)s->len + offset;
    else {
        errno = EINVAL;
        return -1;
    }
    if (np < 0) {
        errno = EINVAL;
        return -1;
    }
    s->pos = (size_t)np;
    return np;
}

int unlink(const char *path)
{
    if (os101_vfs_remove_file(path) != 0) {
        errno = ENOENT;
        return -1;
    }
    return 0;
}

int access(const char *path, int mode)
{
    (void)mode;
    if (os101_vfs_file_exists(path) != 0) {
        errno = ENOENT;
        return -1;
    }
    return 0;
}

char *getcwd(char *buf, size_t size)
{
    static const char root[] = "/disk";
    if (!buf || size < sizeof(root)) {
        errno = EINVAL;
        return NULL;
    }
    memcpy(buf, root, sizeof(root));
    return buf;
}

int chdir(const char *path)
{
    (void)path;
    errno = ENOSYS;
    return -1;
}

unsigned sleep(unsigned seconds)
{
    (void)seconds;
    return 0;
}

long sysconf(int name)
{
    if (name == _SC_PAGESIZE)
        return 4096;
    errno = EINVAL;
    return -1;
}

int getpagesize(void)
{
    return 4096;
}

int stat(const char *path, struct stat *st)
{
    unsigned char *data = NULL;
    size_t len = 0;
    if (!st) {
        errno = EINVAL;
        return -1;
    }
    if (os101_vfs_read_file(path, &data, &len) != 0) {
        errno = ENOENT;
        return -1;
    }
    free(data);
    memset(st, 0, sizeof(*st));
    st->st_mode = S_IFREG | 0644;
    st->st_size = (off_t)len;
    st->st_nlink = 1;
    return 0;
}

int fstat(int fd, struct stat *st)
{
    if (fd < 3 || fd >= OS101_FD_MAX || !g_fds[fd].used) {
        errno = EBADF;
        return -1;
    }
    memset(st, 0, sizeof(*st));
    st->st_mode = S_IFREG | 0644;
    st->st_size = (off_t)g_fds[fd].len;
    st->st_nlink = 1;
    return 0;
}

void *mmap(void *addr, size_t length, int prot, int flags, int fd, off_t offset)
{
    void *p;
    (void)addr;
    (void)prot;
    (void)flags;
    (void)fd;
    (void)offset;
    p = malloc(length ? length : 1);
    if (!p) {
        errno = ENOMEM;
        return MAP_FAILED;
    }
    memset(p, 0, length);
    return p;
}

int munmap(void *addr, size_t length)
{
    (void)length;
    free(addr);
    return 0;
}

int mprotect(void *addr, size_t length, int prot)
{
    (void)addr;
    (void)length;
    (void)prot;
    return 0;
}

FILE *fopen(const char *path, const char *mode)
{
    int flags = O_RDONLY;
    int fd;
    FILE *f;
    if (!mode)
        mode = "r";
    if (strchr(mode, 'w')) {
        flags = O_WRONLY | O_CREAT | O_TRUNC;
        if (strchr(mode, '+'))
            flags = O_RDWR | O_CREAT | O_TRUNC;
    } else if (strchr(mode, 'a')) {
        flags = O_WRONLY | O_CREAT | O_APPEND;
        if (strchr(mode, '+'))
            flags = O_RDWR | O_CREAT | O_APPEND;
    } else if (strchr(mode, '+')) {
        flags = O_RDWR;
    }
    fd = open(path, flags, 0666);
    if (fd < 0)
        return NULL;
    f = malloc(sizeof(*f));
    if (!f) {
        close(fd);
        errno = ENOMEM;
        return NULL;
    }
    memset(f, 0, sizeof(*f));
    f->stream = -1;
    f->fd = fd;
    return f;
}

FILE *fdopen(int fd, const char *mode)
{
    FILE *f;
    (void)mode;
    if (fd == 0)
        return stdin;
    if (fd == 1)
        return stdout;
    if (fd == 2)
        return stderr;
    if (fd < 0 || fd >= OS101_FD_MAX || !g_fds[fd].used) {
        errno = EBADF;
        return NULL;
    }
    f = malloc(sizeof(*f));
    if (!f) {
        errno = ENOMEM;
        return NULL;
    }
    memset(f, 0, sizeof(*f));
    f->stream = -1;
    f->fd = fd;
    return f;
}

int fclose(FILE *stream)
{
    int rc;
    if (!stream)
        return EOF;
    if (stream->stream >= 0)
        return 0; /* stdio streams */
    rc = close(stream->fd);
    free(stream);
    return rc == 0 ? 0 : EOF;
}

size_t fread(void *ptr, size_t size, size_t nmemb, FILE *stream)
{
    size_t bytes, got;
    ssize_t n;
    if (!stream || size == 0 || nmemb == 0)
        return 0;
    if (stream->stream >= 0) {
        stream->eof = 1;
        return 0;
    }
    bytes = size * nmemb;
    if (stream->has_ungot && bytes > 0) {
        ((unsigned char *)ptr)[0] = (unsigned char)stream->ungot;
        stream->has_ungot = 0;
        ptr = (unsigned char *)ptr + 1;
        bytes--;
        if (bytes == 0)
            return 1 / size; /* at least one unit if size==1; approx */
    }
    n = read(stream->fd, ptr, bytes);
    if (n < 0) {
        stream->error = 1;
        return 0;
    }
    if ((size_t)n < bytes)
        stream->eof = 1;
    got = (size_t)n;
    /* count partial first-byte ungetc if any — simplify: */
    return got / size;
}

size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *stream)
{
    size_t bytes;
    ssize_t n;
    if (!stream || size == 0 || nmemb == 0)
        return 0;
    bytes = size * nmemb;
    if (stream->stream == 1 || stream->stream == 2) {
        os101_shim_write_bytes(stream->stream, (const char *)ptr, bytes);
        return nmemb;
    }
    if (stream->stream == 0) {
        stream->error = 1;
        return 0;
    }
    n = write(stream->fd, ptr, bytes);
    if (n < 0) {
        stream->error = 1;
        return 0;
    }
    return (size_t)n / size;
}

int fseek(FILE *stream, long offset, int whence)
{
    if (!stream || stream->stream >= 0)
        return -1;
    stream->has_ungot = 0;
    stream->eof = 0;
    return lseek(stream->fd, offset, whence) < 0 ? -1 : 0;
}

long ftell(FILE *stream)
{
    off_t pos;
    if (!stream || stream->stream >= 0)
        return -1L;
    pos = lseek(stream->fd, 0, SEEK_CUR);
    return (long)pos;
}

void rewind(FILE *stream)
{
    (void)fseek(stream, 0, SEEK_SET);
    if (stream) {
        stream->error = 0;
        stream->eof = 0;
    }
}

int fgetc(FILE *stream)
{
    unsigned char c;
    if (!stream)
        return EOF;
    if (stream->has_ungot) {
        stream->has_ungot = 0;
        return stream->ungot;
    }
    if (fread(&c, 1, 1, stream) != 1)
        return EOF;
    return c;
}

int getc(FILE *stream)
{
    return fgetc(stream);
}

int ungetc(int c, FILE *stream)
{
    if (!stream || c == EOF || stream->has_ungot)
        return EOF;
    stream->ungot = c;
    stream->has_ungot = 1;
    stream->eof = 0;
    return c;
}

char *fgets(char *s, int size, FILE *stream)
{
    int i, c;
    if (!s || size <= 0 || !stream)
        return NULL;
    for (i = 0; i < size - 1; i++) {
        c = fgetc(stream);
        if (c == EOF) {
            if (i == 0)
                return NULL;
            break;
        }
        s[i] = (char)c;
        if (c == '\n') {
            i++;
            break;
        }
    }
    s[i] = 0;
    return s;
}

int remove(const char *path)
{
    return unlink(path);
}

int rename(const char *old, const char *newpath)
{
    unsigned char *data = NULL;
    size_t len = 0;
    if (os101_vfs_read_file(old, &data, &len) != 0) {
        errno = ENOENT;
        return -1;
    }
    if (os101_vfs_write_file(newpath, data, len) != 0) {
        free(data);
        errno = EIO;
        return -1;
    }
    free(data);
    (void)os101_vfs_remove_file(old);
    return 0;
}

int fileno(FILE *stream)
{
    if (!stream)
        return -1;
    if (stream->stream >= 0)
        return stream->stream == 0 ? 0 : stream->stream;
    return stream->fd;
}

int puts(const char *s)
{
    if (fputs(s, stdout) < 0)
        return EOF;
    return fputc('\n', stdout) < 0 ? EOF : 0;
}

FILE *freopen(const char *path, const char *mode, FILE *stream)
{
    FILE *n;
    if (stream && stream->stream < 0)
        fclose(stream);
    n = fopen(path, mode);
    return n;
}

/* tcc_run looks at the process environment; we have none. */
char **environ;
