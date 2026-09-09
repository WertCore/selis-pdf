/* Freestanding shim: declarations only; nothing in the decode path calls
 * stdio, but opj_includes.h includes the header unconditionally and j2k.c's
 * dump code plus openjpeg.c's FILE-stream helpers reference the names (all
 * gc-sections-dropped in the codec module). PRId64/PRIi64/PRIu32 are
 * re-provided because <inttypes.h> from libc does not exist on this target. */
#ifndef SELIS_STDIO_H
#define SELIS_STDIO_H

#include <stddef.h>
#include <stdarg.h>

typedef struct SELIS_FILE FILE;
extern FILE *const selis_stdout;
extern FILE *const selis_stderr;
#define stdout (selis_stdout)
#define stderr (selis_stderr)

#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

int fprintf(FILE *f, const char *fmt, ...);
int printf(const char *fmt, ...);
int vfprintf(FILE *f, const char *fmt, va_list ap);
int vsnprintf(char *dst, size_t n, const char *fmt, va_list ap);
int snprintf(char *dst, size_t n, const char *fmt, ...);
size_t fread(void *dst, size_t size, size_t count, FILE *f);
size_t fwrite(const void *src, size_t size, size_t count, FILE *f);
int fseek(FILE *f, long off, int whence);
long ftell(FILE *f);
FILE *fopen(const char *path, const char *mode);
int fclose(FILE *f);
void rewind(FILE *f);

#define PRId32 "d"
#define PRIi32 "i"
#define PRIu32 "u"
#define PRIx32 "x"
#define PRIX32 "X"
#define PRId64 "lld"
#define PRIi64 "lli"
#define PRIu64 "llu"
#define PRIx64 "llx"

#endif