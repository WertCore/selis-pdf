/* Freestanding shim: opj_includes.h includes <ctype.h>; the decode path
 * does not call ctype functions. Declarations only. */
#ifndef SELIS_CTYPE_H
#define SELIS_CTYPE_H
int isspace(int c);
int isprint(int c);
int tolower(int c);
int toupper(int c);
#endif
