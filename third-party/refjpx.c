/*
 * Native reference decoder for the vendored OpenJPEG 2.5.3 (SL-1.FILT.08
 * oracle). COMPILED FOR THE HOST ONLY — this is a test oracle, never part
 * of the engine or shipped (the engine decodes JPX exclusively through the
 * Tier-2 sandbox; ADR-P0018 / plan rule "never linked natively" applies to
 * the engine, and oracle tooling here plays the same role as the qpdf /
 * Ghostscript containers).
 *
 * Usage: refjpx <input.jp2|input.j2k> <output.raw>
 * Output: the same payload the wasm module emits through the buffer
 * protocol: [u32 w][u32 h][u8 chans][u8 rsvd][interleaved 8-bit samples].
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "openjpeg.h"

static void verbose(const char *msg, void *data) {
    fprintf(stderr, "[opj] %s", msg);
    (void)data;
}

static const unsigned char *g_buf;
static size_t g_len;
static size_t g_pos;

static OPJ_SIZE_T rread(void *dst, OPJ_SIZE_T n, void *user) {
    (void)user;
    if (g_pos >= g_len) return (OPJ_SIZE_T)-1;
    size_t left = g_len - g_pos;
    if ((size_t)n > left) n = (OPJ_SIZE_T)left;
    memcpy(dst, g_buf + g_pos, (size_t)n);
    g_pos += (size_t)n;
    return n;
}

static OPJ_OFF_T rskip(OPJ_OFF_T n, void *user) {
    (void)user;
    OPJ_OFF_T t = (OPJ_OFF_T)g_pos + n;
    if (t < 0) return -1;
    if ((size_t)t > g_len) { g_pos = g_len; return (OPJ_OFF_T)g_len; }
    g_pos = (size_t)t;
    return n;
}

static OPJ_BOOL rseek(OPJ_OFF_T n, void *user) {
    (void)user;
    if (n < 0 || (size_t)n > g_len) return OPJ_FALSE;
    g_pos = (size_t)n;
    return OPJ_TRUE;
}

static void pack8(const opj_image_t *img, unsigned char *dst) {
    OPJ_UINT32 w = img->comps[0].w, h = img->comps[0].h;
    OPJ_UINT32 chans = img->numcomps == 1 ? 1u : 3u;
    for (OPJ_UINT32 i = 0; i < w * h; i++) {
        for (OPJ_UINT32 c = 0; c < chans; c++) {
            OPJ_INT32 v = img->comps[c].data[i];
            OPJ_UINT32 prec = img->comps[c].prec;
            int s = prec > 8 ? (int)(prec - 8) : 0;
            int t = v < 0 ? 0 : (v >> s) > 255 ? 255 : (v >> s);
            dst[10 + i * chans + c] = (unsigned char)t;
        }
    }
}

static void write_all(FILE *o, const unsigned char *b, size_t n) {
    fwrite(b, 1, n, o);
}

/* `refjpx enc <out.j2k>`: emit a deterministic 64x48 RGB reversible (5/3)
 * codestream of a gradient — the valid-set fixture for the sandbox decode
 * tests. Lossless round-trip means the sandbox decode must reproduce the
 * gradient exactly. */
static int getenv_fixture_mct(void) {
    const char *m = getenv("REFJPX_MCT");
    return (m && m[0] == '0') ? 0 : 1;
}

static int encode_fixture(const char *out_path) {
    enum { W = 64, H = 48, C = 3 };
    opj_cparameters_t p;
    opj_set_default_encoder_parameters(&p);
    p.tcp_numlayers = 1;
    p.cp_disto_alloc = 0;
    p.cp_fixed_alloc = 0;
    p.tcp_rates[0] = 0;               /* lossless */
    p.irreversible = 0;               /* 5/3 reversible DWT */
    p.numresolution = 5;
    p.tcp_mct = getenv_fixture_mct();
    p.cod_format = 0;                 /* J2K codestream */

    opj_image_cmptparm_t comps[3];
    memset(comps, 0, sizeof comps);
    for (int i = 0; i < C; i++) {
        comps[i].dx = 1;
        comps[i].dy = 1;
        comps[i].w = W;
        comps[i].h = H;
        comps[i].prec = 8;
        comps[i].bpp = 8;
        comps[i].sgnd = 0;
    }
    opj_image_t *img = opj_image_create(C, comps, OPJ_CLRSPC_SRGB);
    if (!img) return 3;
    img->x0 = 0; img->y0 = 0; img->x1 = W; img->y1 = H;
    for (int c = 0; c < C; c++) {
        for (int y = 0; y < H; y++) {
            for (int x = 0; x < W; x++) {
                int idx = y * W + x;
                int v;
                switch (c) {
                case 0: v = x * 255 / (W - 1); break;
                case 1: v = y * 255 / (H - 1); break;
                default: v = (x ^ y) & 0xFF; break;
                }
                img->comps[c].data[idx] = v;
            }
        }
    }
    opj_codec_t *codec = opj_create_compress(OPJ_CODEC_J2K);
    if (!codec) return 3;
    opj_set_error_handler(codec, verbose, 0);
    opj_set_warning_handler(codec, verbose, 0);
    if (!opj_setup_encoder(codec, &p, img)) return 3;
    opj_stream_t *stream = opj_stream_create_default_file_stream(out_path, 0);
    if (!stream) return 3;
    if (!opj_start_compress(codec, img, stream)) return 3;
    if (!opj_encode(codec, stream)) return 3;
    if (!opj_end_compress(codec, stream)) return 3;
    opj_stream_destroy(stream);
    opj_destroy_codec(codec);
    opj_image_destroy(img);
    return 0;
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "enc") == 0) {
        return encode_fixture(argv[2]);
    }
    if (argc != 3) {
        fprintf(stderr, "usage: refjpx enc <out.j2k> | refjpx <in> <out>\n");
        return 2;
    }
    FILE *f = fopen(argv[1], "rb");
    if (!f) { perror("open"); return 2; }
    fseek(f, 0, SEEK_END);
    long len = ftell(f);
    fseek(f, 0, SEEK_SET);
    unsigned char *buf = malloc((size_t)len);
    if (fread(buf, 1, (size_t)len, f) != (size_t)len) { return 2; }
    fclose(f);
    g_buf = buf;
    g_len = (size_t)len;

    int is_jp2 = g_len > 16 && g_buf[0] == 0 && g_buf[1] == 0 && g_buf[2] == 0 &&
                 g_buf[3] == 0x0C && g_buf[4] == 0x6A && g_buf[5] == 0x50 &&
                 g_buf[6] == 0x20 && g_buf[7] == 0x20;
    opj_dparameters_t params;
    memset(&params, 0, sizeof params);
    opj_codec_t *codec = opj_create_decompress(is_jp2 ? OPJ_CODEC_JP2 : OPJ_CODEC_J2K);
    if (!codec) return 3;
    opj_set_error_handler(codec, verbose, 0);
    opj_set_warning_handler(codec, verbose, 0);
    opj_set_info_handler(codec, verbose, 0);
    if (!opj_setup_decoder(codec, &params)) return 3;
    opj_stream_t *stream = opj_stream_create(1u << 16, 1);
    opj_stream_set_read_function(stream, rread);
    opj_stream_set_skip_function(stream, rskip);
    opj_stream_set_seek_function(stream, rseek);
    opj_stream_set_user_data(stream, (void *)g_buf, 0);
    opj_stream_set_user_data_length(stream, (OPJ_UINT64)g_len);
    opj_image_t *img = 0;
    if (!opj_read_header(stream, codec, &img) || !img) return 3;
    if (!opj_decode(codec, stream, img)) return 3;

    OPJ_UINT32 w = img->comps[0].w, h = img->comps[0].h;
    OPJ_UINT32 chans = img->numcomps == 1 ? 1u : 3u;
    size_t payload = 10u + (size_t)w * h * chans;
    unsigned char *out = malloc(payload);
    out[0] = (unsigned char)(w & 0xFF);
    out[1] = (unsigned char)((w >> 8) & 0xFF);
    out[2] = (unsigned char)((w >> 16) & 0xFF);
    out[3] = (unsigned char)((w >> 24) & 0xFF);
    out[4] = (unsigned char)(h & 0xFF);
    out[5] = (unsigned char)((h >> 8) & 0xFF);
    out[6] = (unsigned char)((h >> 16) & 0xFF);
    out[7] = (unsigned char)((h >> 24) & 0xFF);
    out[8] = (unsigned char)chans;
    out[9] = 0;
    pack8(img, out);

    FILE *o = fopen(argv[2], "wb");
    fwrite(out, 1, payload, o);
    fclose(o);
    fprintf(stderr, "decoded %ux%u x%u ch\n", w, h, chans);
    return 0;
}
