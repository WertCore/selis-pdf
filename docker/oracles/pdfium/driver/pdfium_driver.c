/*
 * pdfium render driver for the pinned PDFium oracle container (SL-0.ORACLE.01).
 *
 * CLI contract (must match `xtask oracle render`, which invokes the container
 * with: <driver> --page N --dpi N <in.pdf> <out.png>):
 *     pdfium_driver --page <n> --dpi <n> <in.pdf> <out.png>
 *
 * Renders one page (1-based, default 1) at `--dpi` into a PNG. PNG encoding
 * uses a tiny self-contained writer (no libpng dependency): zlib stored
 * blocks + adler32, which every PNG decoder accepts. Links dynamically
 * against the PDFium shared library — never linked into Selis (ADR-P0009).
 *
 * Build (container): cc -O2 -o pdfium_driver pdfium_driver.c \
 *     -I/opt/pdfium/include -L/opt/pdfium/lib -lpdfium -Wl,-rpath,/opt/pdfium/lib
 * Build (local verify): cl /O2 pdfium_driver.c /I <dir>/include \
 *     /link <dir>/lib/pdfium.dll.lib
 */

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "fpdfview.h"

/* ---- minimal PNG writer (zlib stream with stored (uncompressed) blocks) --- */

static uint32_t crc_table[256];

static void crc_init(void) {
    for (uint32_t n = 0; n < 256; n++) {
        uint32_t c = n;
        for (int k = 0; k < 8; k++)
            c = (c & 1) ? 0xEDB88320u ^ (c >> 1) : c >> 1;
        crc_table[n] = c;
    }
}

static uint32_t crc32_buf(const uint8_t *buf, size_t len) {
    uint32_t c = 0xFFFFFFFFu;
    for (size_t i = 0; i < len; i++)
        c = crc_table[(c ^ buf[i]) & 0xFFu] ^ (c >> 8);
    return c ^ 0xFFFFFFFFu;
}

static void be32(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)(v >> 24);
    p[1] = (uint8_t)(v >> 16);
    p[2] = (uint8_t)(v >> 8);
    p[3] = (uint8_t)v;
}

static void write_chunk(FILE *f, const char *type, const uint8_t *data, uint32_t len) {
    uint8_t header[8];
    be32(header, len);
    memcpy(header + 4, type, 4);
    fwrite(header, 1, 8, f);
    if (len)
        fwrite(data, 1, len, f);
    uint8_t crc_input[4096];
    /* CRC over type + data; compute incrementally to avoid a huge allocation. */
    uint32_t c = 0xFFFFFFFFu;
    for (int i = 0; i < 4; i++)
        c = crc_table[(c ^ (uint8_t)type[i]) & 0xFFu] ^ (c >> 8);
    uint32_t remaining = len;
    uint32_t off = 0;
    while (remaining > 0) {
        uint32_t take = remaining > 4000 ? 4000 : remaining;
        memcpy(crc_input, data + off, take);
        for (uint32_t i = 0; i < take; i++)
            c = crc_table[(c ^ crc_input[i]) & 0xFFu] ^ (c >> 8);
        off += take;
        remaining -= take;
    }
    uint8_t crc_buf[4];
    be32(crc_buf, c ^ 0xFFFFFFFFu);
    fwrite(crc_buf, 1, 4, f);
}

static void adler32_split(uint32_t a, uint8_t *out) {
    be32(out, a);
}

/* Write a BGRA buffer as an RGB8 PNG using stored zlib blocks. */
static int write_png_rgb(const char *path, uint32_t w, uint32_t h, const uint8_t *bgra) {
    FILE *f = fopen(path, "wb");
    if (!f)
        return 0;
    static const uint8_t sig[8] = {0x89, 'P', 'N', 'G', '\r', '\n', 0x1A, '\n'};
    fwrite(sig, 1, 8, f);

    uint8_t ihdr[13];
    be32(ihdr, w);
    be32(ihdr + 4, h);
    ihdr[8] = 8;  /* bit depth */
    ihdr[9] = 2;  /* colour type: truecolour RGB */
    ihdr[10] = 0; /* compression: deflate */
    ihdr[11] = 0; /* filter: adaptive */
    ihdr[12] = 0; /* interlace: none */
    write_chunk(f, "IHDR", ihdr, 13);

    /* Raw scanlines: filter byte 0 + w*3 bytes, per row. */
    size_t stride = (size_t)w * 3;
    size_t raw_len = (stride + 1) * h;
    uint8_t *raw = malloc(raw_len);
    if (!raw) {
        fclose(f);
        return 0;
    }
    for (uint32_t y = 0; y < h; y++) {
        raw[y * (stride + 1)] = 0;
        const uint8_t *src = bgra + (size_t)y * w * 4;
        uint8_t *dst = raw + y * (stride + 1) + 1;
        for (uint32_t x = 0; x < w; x++) {
            dst[x * 3] = src[x * 4 + 2];
            dst[x * 3 + 1] = src[x * 4 + 1];
            dst[x * 3 + 2] = src[x * 4];
        }
    }

    /* zlib stream: 0x78 0x01 header + stored blocks + adler32. */
    uint32_t n_blocks = (uint32_t)((raw_len + 65534) / 65535);
    if (n_blocks == 0)
        n_blocks = 1;
    size_t zlen = 2 + raw_len + n_blocks * 5 + 4;
    uint8_t *z = malloc(zlen);
    if (!z) {
        free(raw);
        fclose(f);
        return 0;
    }
    size_t zi = 0;
    z[zi++] = 0x78;
    z[zi++] = 0x01;
    size_t off = 0;
    while (off < raw_len) {
        size_t take = raw_len - off > 65535 ? 65535 : raw_len - off;
        int last = (off + take >= raw_len);
        z[zi++] = last ? 1 : 0;
        z[zi++] = (uint8_t)(take & 0xFF);
        z[zi++] = (uint8_t)(take >> 8);
        z[zi++] = (uint8_t)(~take & 0xFF);
        z[zi++] = (uint8_t)((~take >> 8) & 0xFF);
        memcpy(z + zi, raw + off, take);
        zi += take;
        off += take;
    }
    /* adler32 over the raw data */
    uint32_t a = 1, b = 0;
    for (size_t i = 0; i < raw_len; i++) {
        a = (a + raw[i]) % 65521;
        b = (b + a) % 65521;
    }
    adler32_split((b << 16) | a, z + zi);
    zi += 4;
    write_chunk(f, "IDAT", z, (uint32_t)zi);
    write_chunk(f, "IEND", NULL, 0);
    free(z);
    free(raw);
    fclose(f);
    return 1;
}

/* ---- argument parsing ----------------------------------------------------- */

static int arg_int(int argc, char **argv, const char *name, int fallback) {
    for (int i = 1; i < argc - 1; i++) {
        if (strcmp(argv[i], name) == 0)
            return atoi(argv[i + 1]);
    }
    return fallback;
}

static const char *arg_pos(int argc, char **argv, int want) {
    /* Positional arguments are the ones not consumed by --name value pairs. */
    static const char *pos[8];
    int n = 0;
    for (int i = 1; i < argc; i++) {
        if (argv[i][0] == '-' && argv[i][1] == '-') {
            i++; /* skip the value too */
            continue;
        }
        if (n < 8)
            pos[n++] = argv[i];
    }
    if (want >= n)
        return NULL;
    return pos[want];
}

int main(int argc, char **argv) {
    int page_no = arg_int(argc, argv, "--page", 1);
    int dpi = arg_int(argc, argv, "--dpi", 150);
    const char *in_pdf = arg_pos(argc, argv, 0);
    const char *out_png = arg_pos(argc, argv, 1);
    if (!in_pdf || !out_png) {
        fprintf(stderr,
                "usage: pdfium_driver --page <n> --dpi <n> <in.pdf> <out.png>\n");
        return 2;
    }
    if (dpi <= 0 || page_no <= 0) {
        fprintf(stderr, "pdfium_driver: --page and --dpi must be positive\n");
        return 2;
    }

    crc_init();
    FPDF_LIBRARY_CONFIG cfg;
    memset(&cfg, 0, sizeof(cfg));
    cfg.version = 2;
    cfg.m_pUserFontPaths = NULL;
    cfg.m_pIsolate = NULL;
    cfg.m_v8EmbedderSlot = 0;
    FPDF_InitLibraryWithConfig(&cfg);

    FPDF_DOCUMENT doc = FPDF_LoadDocument(in_pdf, NULL);
    if (!doc) {
        fprintf(stderr, "pdfium_driver: cannot load %s (err %lu)\n", in_pdf,
                (unsigned long)FPDF_GetLastError());
        FPDF_DestroyLibrary();
        return 1;
    }
    int page_index = page_no - 1;
    if (page_index >= FPDF_GetPageCount(doc)) {
        fprintf(stderr, "pdfium_driver: page %d out of range (%d pages)\n",
                page_no, FPDF_GetPageCount(doc));
        FPDF_CloseDocument(doc);
        FPDF_DestroyLibrary();
        return 1;
    }
    FPDF_PAGE page = FPDF_LoadPage(doc, page_index);
    if (!page) {
        fprintf(stderr, "pdfium_driver: cannot load page %d\n", page_no);
        FPDF_CloseDocument(doc);
        FPDF_DestroyLibrary();
        return 1;
    }
    double scale = (double)dpi / 72.0;
    int w = (int)(FPDF_GetPageWidthF(page) * scale + 0.5);
    int h = (int)(FPDF_GetPageHeightF(page) * scale + 0.5);
    if (w <= 0 || h <= 0 || (int64_t)w * h > 1LL << 28) {
        fprintf(stderr, "pdfium_driver: unreasonable page size %dx%d\n", w, h);
        FPDF_ClosePage(page);
        FPDF_CloseDocument(doc);
        FPDF_DestroyLibrary();
        return 1;
    }
    FPDF_BITMAP bmp = FPDFBitmap_Create(w, h, 0);
    if (!bmp) {
        fprintf(stderr, "pdfium_driver: bitmap allocation failed\n");
        FPDF_ClosePage(page);
        FPDF_CloseDocument(doc);
        FPDF_DestroyLibrary();
        return 1;
    }
    FPDFBitmap_FillRect(bmp, 0, 0, w, h, 0xFFFFFFFFu);
    FPDF_RenderPageBitmap(bmp, page, 0, 0, w, h, 0, FPDF_ANNOT);
    const uint8_t *pixels =
        (const uint8_t *)FPDFBitmap_GetBuffer(bmp); /* BGRA, top-down */
    int ok = write_png_rgb(out_png, (uint32_t)w, (uint32_t)h, pixels);
    FPDFBitmap_Destroy(bmp);
    FPDF_ClosePage(page);
    FPDF_CloseDocument(doc);
    FPDF_DestroyLibrary();
    if (!ok) {
        fprintf(stderr, "pdfium_driver: cannot write %s\n", out_png);
        return 1;
    }
    printf("pdfium driver: rendered page %d of %s at %d dpi -> %s\n", page_no,
           in_pdf, dpi, out_png);
    return 0;
}
