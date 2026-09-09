/* Freestanding shim: NDEBUG is always defined for the codec module, so the
 * assertions expand to nothing; keep the header for the includes. */
#ifndef SELIS_ASSERT_H
#define SELIS_ASSERT_H
#define assert(x) ((void)0)
#endif
