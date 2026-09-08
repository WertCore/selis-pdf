# Oracle container: PDFium (SL-0.ORACLE.01, ADR-P0009).
#
# Pinning: base image digest (registry-verified 2026-09-08) + the prebuilt
# PDFium binary tarball by sha256 (computed from a local download of the
# bblanchon/pdfium-binaries chromium/7961 release, the same artifact the
# driver was compiled and exercised against on the dev host).
#
# PDFium is the primary render oracle. Licence: BSD-3 (shippable, but still
# used only as an external CI process per ADR-P0009 — nothing links to it).
# syntax=docker/dockerfile:1
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

ARG PDFIUM_TAG=chromium/7961
ARG PDFIUM_SHA256=019665c8877d46fe65f625f80fd714ab07aac68554b0636acf2a2adf9288adb2
ARG PDFIUM_URL=https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F7961/pdfium-linux-x64.tgz

RUN printf 'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/20260901T000000Z bookworm main\n' \
        > /etc/apt/sources.list \
 && printf 'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian-security/20260901T000000Z bookworm-security main\n' \
        > /etc/apt/sources.list.d/security.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

# URL is fixed in ARG (not derived from PDFIUM_TAG) so the checksum can never
# be paired with a different artifact than the one it was computed against.
ADD --checksum=sha256:${PDFIUM_SHA256} ${PDFIUM_URL} /tmp/pdfium.tgz

COPY driver/pdfium_driver.c /usr/src/driver/pdfium_driver.c

RUN mkdir -p /opt/pdfium \
 && tar xzf /tmp/pdfium.tgz -C /opt/pdfium \
 && rm /tmp/pdfium.tgz \
 && cc -O2 -o /usr/local/bin/pdfium_driver /usr/src/driver/pdfium_driver.c \
       -I/opt/pdfium/include -L/opt/pdfium/lib -lpdfium \
       -Wl,-rpath,/opt/pdfium/lib \
 && cp /opt/pdfium/lib/libpdfium.so /usr/local/lib/ \
 && ldconfig

# Render oracle interface: `docker run <image> --page 1 --dpi 150 /in.pdf /out.png`
ENTRYPOINT ["/usr/local/bin/pdfium_driver"]
