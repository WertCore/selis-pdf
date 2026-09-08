# Oracle container: MuPDF / mutool (SL-0.ORACLE.01, ADR-P0009).
#
# Pinning: base image digest (registry-verified 2026-09-08) + source tarball
# sha256 (computed from a local download of the official archive).
#
# MuPDF is the third render opinion and the text-extraction oracle for local
# development. Licence: AGPL-3.0 — external process only, NEVER linked or
# distributed with Selis (ADR-P0009, ADR-P0021).
# syntax=docker/dockerfile:1
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

ARG MUPDF_VERSION=1.23.9
ARG MUPDF_SHA256=d4a5d66801d85f97d76da978b155f66a5659d1131070c0400f076883f604e297

RUN printf 'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/20260901T000000Z bookworm main\n' \
        > /etc/apt/sources.list \
 && printf 'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian-security/20260901T000000Z bookworm-security main\n' \
        > /etc/apt/sources.list.d/security.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl pkg-config \
        libx11-dev libxext-dev libfreetype-dev libharfbuzz-dev \
        libjbig2dec0-dev libjpeg-dev libopenjp2-7-dev zlib1g-dev \
 && rm -rf /var/lib/apt/lists/*

ADD --checksum=sha256:${MUPDF_SHA256} \
    https://mupdf.com/downloads/archive/mupdf-${MUPDF_VERSION}-source.tar.gz /tmp/mupdf.tar.gz

RUN cd /tmp \
 && tar xzf mupdf.tar.gz \
 && make -C mupdf-${MUPDF_VERSION}-source HAVE_X11=no HAVE_GLUT=no \
       shared=no prefix=/usr/local install \
 && rm -rf /tmp/mupdf* \
 && mutool -v 2>&1 | head -1

# Render oracle interface: `docker run <image> draw -r 150 /in.pdf /out.png`
ENTRYPOINT ["/usr/local/bin/mutool"]
