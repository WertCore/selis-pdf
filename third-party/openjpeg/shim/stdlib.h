/* Freestanding shim: minimal C89 declarations the vendored OpenJPEG uses.
 * wasm32-unknown-unknown has no libc; the implementations live in
 * selis_libc.c. Internal to the codec module (SL-1.FILT.08). */
#ifndef SELIS_STDLIB_H
#define SELIS_STDLIB_H
#include <stddef.h>
void *malloc(size_t n);
void *calloc(size_t count, size_t size);
void *realloc(void *p, size_t n);
void free(void *p);
void qsort(void *base, size_t nmemb, size_t width, int (*compar)(const void *, const void *));
void abort(void);
void exit(int code);
const char *getenv(const char *name);
int atoi(const char *s);
int abs(int v);
#endif