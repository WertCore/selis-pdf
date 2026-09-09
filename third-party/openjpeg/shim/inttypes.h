/* Freestanding shim: <inttypes.h> from libc is unavailable; the vendored
 * code only needs the fixed-width types, which <stdint.h> provides. */
#ifndef SELIS_INTTYPES_H
#define SELIS_INTTYPES_H
#include <stdint.h>
#endif
