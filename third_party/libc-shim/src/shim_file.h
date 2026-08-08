/* Shared FILE layout for printf.c and fileio.c. Not a public header. */
#ifndef OS101_SHIM_FILE_IMPL_H
#define OS101_SHIM_FILE_IMPL_H

struct _OS101_FILE {
    int stream; /* 0=stdin, 1=stdout, 2=stderr, -1=fd-backed */
    int fd;
    int ungot;
    int has_ungot;
    int error;
    int eof;
};

#endif
