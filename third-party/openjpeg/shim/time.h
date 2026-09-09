/* Freestanding shim: opj_includes.h includes <time.h> unconditionally; the
 * decode path never reads a clock (the sandbox forbids one). */
#ifndef SELIS_TIME_H
#define SELIS_TIME_H
typedef long clock_t;
typedef long time_t;
#define CLOCKS_PER_SEC 1000000L
time_t time(time_t *t);
clock_t clock(void);
#endif
