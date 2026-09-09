/* Freestanding shim: thread.c's non-threaded branch includes <unistd.h>
 * under a POSIX guard that never fires on this target; harmless stub. */
#ifndef SELIS_UNISTD_H
#define SELIS_UNISTD_H
#endif
