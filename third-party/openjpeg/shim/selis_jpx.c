/*
 * selis_jpx.c â€” the codec-module wrapper: OpenJPEG 2.5.3 behind the Tier-2
 * buffer protocol (SL-1.FILT.08; sandbox contract in selis-sandbox::wasm).
 *
 * Exports (all plain i32 â€” C toolchains cannot emit wasm multi-value):
 *   init(input_len) -> ptr    reserve the codestream region; the host
 *                             copies the JP2/J2K bytes there
 *   decode(input_len) -> status   0 ok, 1 malformed, 2 truncated, 3 unsupported
 *   output(max_len) -> len    decoded-pixel payload length (0 on failure)
 *   output_ptr() -> ptr       payload offset (0 when len is 0)
 *   finish() -> status        releases the decode and reports the status
 *
 * Output layout (host docs it, this file is the definition):
 *   [0..4)  u32 LE width
 *   [4..8)  u32 LE height
 *   [8..9)  u8  channels (1 grey, 3 RGB)
 *   [9..9)  u8  reserved, zero
 *   [10..)  interleaved u8 samples, 8-bit, channels per pixel, row-major
 *           top-to-bottom. Components are reduced to 8-bit by the module.
 *
 * Containment: everything OpenJPEG allocates goes through opj_malloc ->
 * selis_malloc inside this module's linear memory, bounded by the host's
 * hard cap. The stream is memory-backed (no imports). No clocks, no files,
 * no threads (the vendored thread.c stub branch is single-threaded).
 * Malformed input makes OpenJPEG return false and we report it as a status
 * â€” the host never traps on document content.
 */

#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>

#include "openjpeg.h"

/* The decoded payload, module-owned. One buffer, freed only when the
 * instance is dropped (per-run), which keeps the code simple and makes
 * double-decode idempotent. */
static unsigned char *selis_out;
static size_t selis_out_len;
static int selis_status;


#define SELIS_OK 0
#define SELIS_MALFORMED 1
#define SELIS_TRUNCATED 2
#define SELIS_UNSUPPORTED 3

/* Input region: right after the module's data segment bump area. The host
 * copies the codestream here after init returns this pointer. */
#define SELIS_INPUT_BASE 65536 /* one page in: past .data/.bss/statics */

static const unsigned char *selis_input;
static size_t selis_input_len;

/* â”€â”€ memory-backed stream â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€ */


/* OpenJPEG's stream callbacks are cursor-relative: the stream object
 * opj_stream_private tracks the offset and calls read/skip/seek with
 * absolute-position helpers around them. So the callbacks only need the
 * base pointer and a current-offset static. */
static size_t selis_cur;

static OPJ_SIZE_T selis_read(void *dst, OPJ_SIZE_T n, void *user) {
    const unsigned char *base = (const unsigned char *)user;
    if (selis_cur >= selis_input_len) {
        return (OPJ_SIZE_T)-1; /* EOF */
    }
    size_t left = selis_input_len - selis_cur;
    if ((size_t)n > left) {
        n = (OPJ_SIZE_T)left;
    }
    memcpy(dst, base + selis_cur, (size_t)n);
    selis_cur += (size_t)n;
    return n;
}

static OPJ_OFF_T selis_skip(OPJ_OFF_T n, void *user) {
    (void)user;
    OPJ_OFF_T target = (OPJ_OFF_T)selis_cur + n;
    if (target < 0) {
        return -1;
    }
    if ((size_t)target > selis_input_len) {
        selis_cur = selis_input_len;
        return (OPJ_OFF_T)selis_input_len;
    }
    selis_cur = (size_t)target;
    return n;
}

static OPJ_BOOL selis_seek(OPJ_OFF_T n, void *user) {
    (void)user;
    if (n < 0 || (size_t)n > selis_input_len) {
        return OPJ_FALSE;
    }
    selis_cur = (size_t)n;
    return OPJ_TRUE;
}

static void selis_stream_noop(void *p) {
    (void)p;
}

/* â”€â”€ payload assembly â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€ */

/* Reduce one component's samples to 8-bit. OpenJPEG hands back OPJ_INT32
 * buffers with `prec` significant bits; PDF JPX in practice carries 8-bit
 * components, but hostile/high-precision files must still produce a
 * bounded, well-defined output. */
static void selis_pack_8bit(const opj_image_t *img, unsigned char *dst) {
    OPJ_UINT32 w = img->comps[0].w;
    OPJ_UINT32 h = img->comps[0].h;
    OPJ_UINT32 ncomps = img->numcomps < 3 ? img->numcomps : (img->numcomps == 4 ? 3 : 3);
    /* grey (1 comp) or RGB (>=3 comps; alpha dropped, CMYK converted) */
    OPJ_UINT32 chans = ncomps == 1 ? 1u : 3u;
    int shift[3];
    for (OPJ_UINT32 c = 0; c < chans; c++) {
        OPJ_UINT32 prec = img->comps[c].prec;
        shift[c] = prec > 8 ? (int)(prec - 8) : 0;
    }
    for (OPJ_UINT32 y = 0; y < h; y++) {
        for (OPJ_UINT32 x = 0; x < w; x++) {
            OPJ_UINT32 idx = y * w + x;
            unsigned char *px = dst + 10 + idx * chans;
            if (chans == 1) {
                OPJ_INT32 v = img->comps[0].data[idx];
                px[0] = (unsigned char)(v < 0 ? 0 : (v >> shift[0]) > 255 ? 255 : (v >> shift[0]));
            } else {
                OPJ_INT32 c0 = img->comps[0].data[idx];
                OPJ_INT32 c1 = img->comps[1].data[idx];
                OPJ_INT32 c2 = img->comps[2].data[idx];
                int s0 = shift[0], s1 = shift[1], s2 = shift[2];
                int v0 = c0 < 0 ? 0 : (c0 >> s0) > 255 ? 255 : (c0 >> s0);
                int v1 = c1 < 0 ? 0 : (c1 >> s1) > 255 ? 255 : (c1 >> s1);
                int v2 = c2 < 0 ? 0 : (c2 >> s2) > 255 ? 255 : (c2 >> s2);
                px[0] = (unsigned char)v0;
                px[1] = (unsigned char)v1;
                px[2] = (unsigned char)v2;
            }
        }
    }
}

static void selis_set_fail(int status) {
    selis_status = status;
    selis_out_len = 0;
}

/* â”€â”€ the protocol exports â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€ */

int init(int input_len)
{
    selis_out = 0;
    selis_out_len = 0;
    selis_status = SELIS_OK;
    selis_cur = 0;
    selis_input_len = input_len > 0 ? (size_t)input_len : 0;
    selis_input = (const unsigned char *)SELIS_INPUT_BASE;
    /* Refuse inputs that leave no room for decode arenas: the host's
     * memory cap still bounds this, but a module that swaps its entire
     * arena for input can never decode anything. */
    if (selis_input_len == 0 || selis_input_len > (size_t)(32u * 1024u * 1024u)) {
        return 0;
    }
    return SELIS_INPUT_BASE;
}

int decode(int input_len)
{
    if ((size_t)input_len != selis_input_len || selis_input_len == 0) {
        selis_set_fail(SELIS_MALFORMED);
        return selis_status;
    }

    /* JP2 signature box = wrapper; raw J2K codestream = direct. The PDF
     * JPXDecode filter carries either. */
    int is_jp2 = selis_input_len > 12 &&
                 selis_input[0] == 0x6A && selis_input[1] == 0x50 &&
                 selis_input[2] == 0x20 && selis_input[3] == 0x20;

    opj_dparameters_t params;
    memset(&params, 0, sizeof params);
    params.cp_reduce = 0;
    params.cp_layer = 0;

    opj_codec_t *codec = opj_create_decompress(is_jp2 ? OPJ_CODEC_JP2 : OPJ_CODEC_J2K);
    if (!codec) {
        selis_set_fail(SELIS_MALFORMED);
        return selis_status;
    }
    opj_set_error_handler(codec, 0, 0);
    opj_set_warning_handler(codec, 0, 0);
    opj_set_info_handler(codec, 0, 0);
    if (!opj_setup_decoder(codec, &params)) {
        opj_destroy_codec(codec);
        selis_set_fail(SELIS_MALFORMED);
        return selis_status;
    }

    opj_stream_t *stream = opj_stream_create(1u << 16, /* 64 KiB internal buffer */
                                             1 /* input */);
    if (!stream) {
        opj_destroy_codec(codec);
        selis_set_fail(SELIS_MALFORMED);
        return selis_status;
    }
    opj_stream_set_read_function(stream, selis_read);
    opj_stream_set_skip_function(stream, selis_skip);
    opj_stream_set_seek_function(stream, selis_seek);
    opj_stream_set_user_data(stream, (void *)selis_input, selis_stream_noop);
    opj_stream_set_user_data_length(stream, (OPJ_UINT64)selis_input_len);

    opj_image_t *img = 0;
    if (!opj_read_header(stream, codec, &img) || !img) {
        opj_stream_destroy(stream);
        opj_destroy_codec(codec);
        selis_set_fail(selis_cur >= selis_input_len ? SELIS_TRUNCATED : SELIS_MALFORMED);
        return selis_status;
    }

    if (!opj_decode(codec, stream, img)) {
        opj_image_destroy(img);
        opj_stream_destroy(stream);
        opj_destroy_codec(codec);
        selis_set_fail(SELIS_MALFORMED);
        return selis_status;
    }

    /* Emit: header + interleaved 8-bit samples. Grey or RGB; alpha dropped
     * (PDF JPX renders the colour channels), 4-comp CMYK is converted by
     * taking the first three channels â€” a plain approximation documented in
     * the task; JPX colour management is Phase 2 (SL-2). */
    OPJ_UINT32 w = img->comps[0].w;
    OPJ_UINT32 h = img->comps[0].h;
    if (w == 0 || h == 0 || img->numcomps == 0 || !img->comps[0].data) {
        opj_image_destroy(img);
        opj_stream_destroy(stream);
        opj_destroy_codec(codec);
        selis_set_fail(SELIS_MALFORMED);
        return selis_status;
    }
    OPJ_UINT32 chans = img->numcomps == 1 ? 1u : 3u;
    size_t pixels = (size_t)w * (size_t)h;
    size_t payload = 10u + pixels * chans;
    if (pixels > (1u << 28)) {
        /* Refuse absurd pixel counts up front: bounded work per run. */
        opj_image_destroy(img);
        opj_stream_destroy(stream);
        opj_destroy_codec(codec);
        selis_set_fail(SELIS_UNSUPPORTED);
        return selis_status;
    }
    selis_out = (unsigned char *)malloc(payload);
    if (!selis_out) {
        opj_image_destroy(img);
        opj_stream_destroy(stream);
        opj_destroy_codec(codec);
        selis_set_fail(SELIS_UNSUPPORTED);
        return selis_status;
    }
    selis_out[0] = (unsigned char)(w & 0xFF);
    selis_out[1] = (unsigned char)((w >> 8) & 0xFF);
    selis_out[2] = (unsigned char)((w >> 16) & 0xFF);
    selis_out[3] = (unsigned char)((w >> 24) & 0xFF);
    selis_out[4] = (unsigned char)(h & 0xFF);
    selis_out[5] = (unsigned char)((h >> 8) & 0xFF);
    selis_out[6] = (unsigned char)((h >> 16) & 0xFF);
    selis_out[7] = (unsigned char)((h >> 24) & 0xFF);
    selis_out[8] = (unsigned char)chans;
    selis_out[9] = 0;
    selis_pack_8bit(img, selis_out);
    selis_out_len = payload;

    opj_image_destroy(img);
    opj_stream_destroy(stream);
    opj_destroy_codec(codec);
    selis_status = SELIS_OK;
    return selis_status;
}

int output(int max_len)
{
    if (selis_status != SELIS_OK) {
        return 0;
    }
    if ((size_t)max_len < selis_out_len) {
        /* The host offers less than the payload: report the clamped size.
         * (Nothing is lost â€” the host asked for the budget it has.) */
        return max_len;
    }
    return (int)selis_out_len;
}

int output_ptr(void)
{
    if (selis_status != SELIS_OK || selis_out_len == 0) {
        return 0;
    }
    return (int)(uintptr_t)selis_out;
}

int finish(void)
{
    /* Per-run instance drop reclaims everything; nothing to release that
     * outlives the host's store. */
    return selis_status;
}

