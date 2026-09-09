/* Freestanding shim: see selis_libc.c (SL-1.FILT.08). The subset OpenJPEG
 * references; sqrt/pow are exact for the call shapes in the vendored code. */
#ifndef SELIS_MATH_H
#define SELIS_MATH_H
double fabs(double x);
double floor(double x);
double ceil(double x);
double sqrt(double x);
double pow(double x, double y);
double ldexp(double x, int e);
int lrintf(float f);
#endif
