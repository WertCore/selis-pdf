/* selis-filt08 shim - the freestanding wasm32-unknown-unknown porting layer
 * for the vendored OpenJPEG 2.5.3 (SL-1.FILT.08).
 *
 * The codec module declares NO imports (the Tier-2 host refuses import
 * sections outright, SL-0.SBX.06): everything OpenJPEG needs from libc/libm
 * is provided here, compiled into the module.
 *
 * What the vendored code needs on this target (census over openjp2 C files):
 *   malloc/calloc/realloc/free  via opj_malloc.h, provided by selis_malloc
 *   memset/memcpy/memmove/memcmp/strcmp/etc.   internal buffers only
 *   qsort                       one call (component sorting, small n)
 *   floor/ceil/sqrt/pow/fabs    libm; the decode path barely touches these
 *   assert()                    compiled out (NDEBUG); header kept
 *   vsnprintf (event.c)         event.c is not compiled; opj_event_msg is a
 *                               no-op stub in selis_event.c, so error
 *                               details never leave the module (ADR-P0017)
 *                               and vsnprintf does not exist here.
 *
 * Memory containment: selis_malloc is a first-fit free-list allocator over
 * the module's own linear memory starting at __heap_base, capped at 64 MiB.
 * The host's ResourceLimiter caps the memory section itself, so a hostile
 * codestream can only exhaust the module arena: OpenJPEG's own OOM paths
 * (opj_malloc returning NULL) turn into malformed/truncated statuses. The
 * module cannot grow past the host cap and cannot touch the host.
 */


#include <stddef.h>
#include <stdint.h>
#include <stdarg.h>
#include "stdio.h"
#include "stdlib.h"
#include "string.h"
#include "math.h"


/* ================================================================== */
/* Minimal first-fit free-list malloc over the wasm linear memory.     */
/* Block header: { size_t size; size_t used; } preceded by a           */
/* free-list pointer cell. Alignment 16. No threads (wasm, single).    */
/* ================================================================== */

#define SELIS_ALIGN 16u
#define SELIS_HEAP_MAX (64u * 1024u * 1024u) /* must be < host cap; host still enforces its own */

extern unsigned char __heap_base; /* provided by wasm-ld */

typedef struct selis_block {
    size_t size;   /* payload size */
    int used;      /* 0 = free */
    struct selis_block *next;
} selis_block_t;

static selis_block_t *selis_free_list;
static uintptr_t selis_bump; /* next never-touched byte */
static uintptr_t selis_heap_end = 0; /* 0 until first allocation: lazy bind to host cap */
static int selis_heap_limit_bytes = 64 * 1024 * 1024;

void selis_heap_set_limit(int bytes) {
    if (bytes > 0 && bytes <= (int)SELIS_HEAP_MAX) {
        selis_heap_limit_bytes = bytes;
    }
}

static uintptr_t selis_heap_hi(void) {
    if (selis_heap_end == 0) {
        selis_heap_end = (uintptr_t)&__heap_base + (uintptr_t)selis_heap_limit_bytes;
    }
    return selis_heap_end;
}

/* Grow the linear memory by `pages` when the bump pointer would exceed the
 * current size. memory.grow is a wasm instruction (not an import), so the
 * no-imports contract holds; the HOST limiter still refuses growth past the
 * cap, which makes this return (size_t)-1 and malloc then reports NULL —
 * OpenJPEG's OOM path. */
static int selis_grow_to(uintptr_t target) {
    uintptr_t cur_pages = (uintptr_t)__builtin_wasm_memory_size(0);
    uintptr_t cur_bytes = cur_pages * 0x10000u;
    if (target <= cur_bytes) {
        return 1;
    }
    uintptr_t need_pages = (target - cur_bytes + 0xFFFFu) >> 16;
    uintptr_t got = __builtin_wasm_memory_grow(0, need_pages);
    return got != (uintptr_t)-1;
}

static size_t selis_round(size_t n) {
    return (n + (SELIS_ALIGN - 1)) & ~(size_t)(SELIS_ALIGN - 1);
}

void *malloc(size_t n) {
    if (n == 0) {
        n = 1;
    }
    size_t need = selis_round(n);
    /* first fit over the free list */
    selis_block_t **prev = &selis_free_list;
    for (selis_block_t *b = selis_free_list; b; prev = &b->next, b = b->next) {
        if (!b->used && b->size >= need) {
            b->used = 1;
            return (void *)(b + 1);
        }
    }
    /* bump a fresh block */
    if (selis_bump == 0) {
        selis_bump = (uintptr_t)&__heap_base;
    }
    uintptr_t want = selis_bump + sizeof(selis_block_t) + need;
    if (want > selis_heap_hi()) {
        return 0; /* arena limit: OpenJPEG OOM path, host cap backstop */
    }
    if (!selis_grow_to(want)) {
        return 0; /* host limiter refused the growth */
    }
    selis_block_t *b = (selis_block_t *)selis_bump;
    selis_bump += sizeof(selis_block_t) + need;
    b->size = need;
    b->used = 1;
    b->next = 0;
    *prev = b;
    return (void *)(b + 1);
}

void *calloc(size_t count, size_t size) {
    if (count != 0 && size > (size_t)-1 / count) {
        return 0;
    }
    size_t total = count * size;
    void *p = malloc(total);
    if (p) {
        memset(p, 0, total);
    }
    return p;
}

void *realloc(void *old, size_t n) {
    if (!old) {
        return malloc(n);
    }
    selis_block_t *b = ((selis_block_t *)old) - 1;
    if (b->size >= selis_round(n)) {
        return old;
    }
    void *p = malloc(n);
    if (p) {
        memcpy(p, old, b->size < n ? b->size : n);
        free(old);
    }
    return p;
}

void free(void *p) {
    if (!p) {
        return;
    }
    selis_block_t *b = ((selis_block_t *)p) - 1;
    b->used = 0;
    /* coalesce with the next free block when adjacent on the list */
    for (selis_block_t *cur = selis_free_list; cur; cur = cur->next) {
        selis_block_t *nxt = cur->next;
        if (nxt && !cur->used && !nxt->used &&
            (uintptr_t)cur + sizeof(*cur) + cur->size == (uintptr_t)nxt) {
            cur->size += sizeof(*nxt) + nxt->size;
            cur->next = nxt->next;
        }
    }
}

/* stdio stubs: unreachable in the codec module (gc-sections drops them) but
 * referenced by j2k.c's dump code, which must still compile. */
static int selis_file_sink;
FILE *const selis_stdout = (FILE *)&selis_file_sink;
FILE *const selis_stderr = (FILE *)&selis_file_sink;

int fprintf(FILE *f, const char *fmt, ...) {
    (void)f; (void)fmt;
    return 0;
}
int printf(const char *fmt, ...) {
    (void)fmt;
    return 0;
}
int vfprintf(FILE *f, const char *fmt, va_list ap) {
    (void)f; (void)fmt; (void)ap;
    return 0;
}
int vsnprintf(char *dst, size_t n, const char *fmt, va_list ap) {
    (void)fmt; (void)ap;
    if (n) *dst = 0;
    return 0;
}
int snprintf(char *dst, size_t n, const char *fmt, ...) {
    (void)fmt;
    if (n) *dst = 0;
    return 0;
}

/* ================================================================== */
/* libm subset (bit-exact for the shapes OpenJPEG uses).               */
/* ================================================================== */

static double selis_abs(double x) {
    union { double d; uint64_t u; } v;
    v.d = x;
    v.u &= ~((uint64_t)1 << 63);
    return v.d;
}

double fabs(double x) { return selis_abs(x); }

double floor(double x) {
    union { double d; uint64_t u; } v;
    v.d = x;
    uint64_t sign = v.u >> 63;
    uint64_t biased = (v.u >> 52) & 0x7FF;
    if (biased == 0x7FF) return x;           /* inf / NaN */
    int exp = (int)biased - 1023;
    if (exp >= 52) return x;                 /* already integral magnitude */
    if (biased == 0) return sign ? -0.0 : 0.0; /* zero or subnormal -> 0 */
    if (exp < 0) {
        /* |x| < 1 */
        return sign ? -1.0 : 0.0;
    }
    uint64_t frac_bits = 52 - exp;
    uint64_t frac_mask = ((uint64_t)1 << frac_bits) - 1;
    if (!(v.u & frac_mask)) {
        return x;
    }
    v.u &= ~frac_mask; /* truncate toward zero */
    if (sign) {
        /* negative: floor is one ULP lower (toward -inf) */
        v.d -= 1.0;
    }
    return v.d;
}

double ceil(double x) {
    return -floor(-x);
}

static double selis_pow2_int(int e) {
    if (e > 1023) return 1.0 / 0.0;
    if (e < -1074) return 0.0;
    union { double d; uint64_t u; } v;
    v.u = (uint64_t)(1023 + e) << 52;
    return v.d;
}

double pow(double x, double y) {
    if (x == 2.0) {
        return selis_pow2_int((int)y);
    }
    if (x == 10.0) {
        double r = 1.0;
        int n = (int)y;
        int neg = n < 0;
        if (neg) n = -n;
        for (int i = 0; i < n; i++) r *= 10.0;
        return neg ? 1.0 / r : r;
    }
    return 0.0 / 0.0; /* unsupported shape: loud NaN */
}

double sqrt(double x) {
    if (x < 0.0 || x != x) return 0.0 / 0.0;
    if (x == 0.0) return 0.0;
    union { double d; uint64_t u; } v;
    v.d = x;
    v.u = (v.u >> 1) + ((uint64_t)1 << 61);
    double r = v.d;
    for (int i = 0; i < 6; i++) {
        r = 0.5 * (r + x / r);
    }
    return r;
}

double ldexp(double x, int e) {
    return x * selis_pow2_int(e);
}

/* ================================================================== */
/* libc string/memory (module-internal buffers only).                  */
/* ================================================================== */

void *memset(void *dst, int c, size_t n) {
    unsigned char *d = dst;
    while (n--) *d++ = (unsigned char)c;
    return dst;
}

void *memcpy(void *dst, const void *src, size_t n) {
    unsigned char *d = dst;
    const unsigned char *s = src;
    while (n--) *d++ = *s++;
    return dst;
}

void *memmove(void *dst, const void *src, size_t n) {
    unsigned char *d = dst;
    const unsigned char *s = src;
    if (d < s) {
        while (n--) *d++ = *s++;
    } else if (d > s) {
        d += n;
        s += n;
        while (n--) *--d = *--s;
    }
    return dst;
}

int memcmp(const void *a, const void *b, size_t n) {
    const unsigned char *pa = a;
    const unsigned char *pb = b;
    for (; n--; pa++, pb++) {
        if (*pa != *pb) return *pa - *pb;
    }
    return 0;
}

size_t strlen(const char *s) {
    const char *p = s;
    while (*p) p++;
    return (size_t)(p - s);
}

int strcmp(const char *a, const char *b) {
    while (*a && *a == *b) { a++; b++; }
    return (unsigned char)*a - (unsigned char)*b;
}

int strncmp(const char *a, const char *b, size_t n) {
    for (; n--; a++, b++) {
        if (*a != *b) return (unsigned char)*a - (unsigned char)*b;
        if (!*a) break;
    }
    return 0;
}

char *strcpy(char *dst, const char *src) {
    char *d = dst;
    while ((*d++ = *src++)) {}
    return dst;
}

void qsort(void *base, size_t nmemb, size_t width,
           int (*compar)(const void *, const void *)) {
    unsigned char *p = base;
    unsigned char tmp[64];
    if (width > sizeof tmp) return; /* OpenJPEG sorts small fixed-width items */
    for (size_t i = 1; i < nmemb; i++) {
        for (size_t j = i; j > 0; j--) {
            unsigned char *a = p + (j - 1) * width;
            unsigned char *b = p + j * width;
            if (compar(a, b) > 0) {
                memcpy(tmp, a, width);
                memcpy(a, b, width);
                memcpy(b, tmp, width);
            } else {
                break;
            }
        }
    }
}

int lrintf(float f) {
    /* round-half-to-even; only used by OPJ_FLOAT macros on edge paths */
    if (f != f || f - f != 0.0f) {
        return 0; /* NaN / inf: undefined in C, bounded here */
    }
    int r = (int)(f >= 0.0f ? f + 0.5f : f - 0.5f);
    return r;
}
/* j2k.c thread-config and dump paths reference these; the sandbox has no
 * environment (purity: no getenv anywhere, SL-0.WS.04) and the dump code is
 * unreachable, so the definitions exist only to keep the module linking. */
const char *getenv(const char *name) {
    (void)name;
    return 0;
}

int atoi(const char *s) {
    int v = 0;
    int neg = 0;
    while (*s == ' ') s++;
    if (*s == '-') { neg = 1; s++; }
    for (; *s >= '0' && *s <= '9'; s++) {
        v = v * 10 + (*s - '0');
    }
    return neg ? -v : v;
}
int abs(int v) {
    return v < 0 ? -v : v;
}

/* probe: allocate `n` bytes (16-aligned) and return the pointer or 0 */
