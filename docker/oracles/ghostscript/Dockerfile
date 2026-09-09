# Oracle container: Ghostscript (SL-0.ORACLE.01, ADR-P0009).
#
# Pinning: base image digest (registry-verified 2026-09-08) + release tarball
# sha256 (computed from a local download of the official GitHub release).
#
# Ghostscript is the codec + colour + shading reference oracle.
# Licence: AGPL-3.0 — external process only, NEVER linked or distributed
# with Selis (ADR-P0009, ADR-P0021).
# syntax=docker/dockerfile:1
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

ARG GS_VERSION=9.56.1
ARG GS_SHA256=1598b9a38659cce8448d42a73054b2f9cbfcc40a9b97eeec5f22d4d6cd1de8e6

RUN printf 'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/20260901T000000Z bookworm main\n' \
        > /etc/apt/sources.list \
 && printf 'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian-security/20260901T000000Z bookworm-security main\n' \
        > /etc/apt/sources.list.d/security.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl pkg-config \
        libpng-dev libjpeg-dev zlib1g-dev \
 && rm -rf /var/lib/apt/lists/*

ADD --checksum=sha256:${GS_SHA256} \
    https://github.com/ArtifexSoftware/ghostpdl-downloads/releases/download/gs9561/ghostscript-${GS_VERSION}.tar.gz /tmp/gs.tar.gz

RUN cd /tmp \
 && tar xzf gs.tar.gz \
 && cd ghostscript-${GS_VERSION} \
 && ./configure --prefix=/usr/local --without-x --disable-cups \
       --disable-gtk --with-drivers=FILES \
 && make -j"$(nproc)" so \
 && make soinstall \
 && cd / && rm -rf /tmp/gs* /tmp/ghostscript* \
 && gs --version

# Render oracle interface: `docker run <image> -sDEVICE=png16m -r150 -o /out.png /in.pdf`
ENTRYPOINT ["/usr/local/bin/gs"]
