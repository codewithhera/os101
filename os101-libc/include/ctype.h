/*
 * ctype for OS101. Plain functions rather than the usual table-and-macro
 * arrangement: there is one locale, it is "C", and a call per character costs
 * nothing an application here will notice.
 */
#ifndef _OS101_CTYPE_H
#define _OS101_CTYPE_H

#ifdef __cplusplus
extern "C" {
#endif

int isalnum(int c);
int isalpha(int c);
int isascii(int c);
int isblank(int c);
int iscntrl(int c);
int isdigit(int c);
int isgraph(int c);
int islower(int c);
int isprint(int c);
int ispunct(int c);
int isspace(int c);
int isupper(int c);
int isxdigit(int c);
int tolower(int c);
int toupper(int c);

#ifdef __cplusplus
}
#endif

#endif /* _OS101_CTYPE_H */
