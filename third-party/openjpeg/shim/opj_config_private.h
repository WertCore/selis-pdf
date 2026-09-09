/*
 * opj_config_private.h — hand-written for the freestanding
 * wasm32-unknown-unknown build of OpenJPEG 2.5.3 (SL-1.FILT.08).
 *
 * Deliberately defines NOTHING optional: no aligned allocators (opj_malloc
 * falls back to its generic implementation over malloc), no fseeko, no
 * threads (thread.c compiles its single-threaded stub branch), no largefile
 * flags. The sandbox host refuses import sections, so there is no libc to
 * have features from — the shim (selis_libc.c) provides the floor.
 */

#define OPJ_PACKAGE_VERSION "2.5.3"
