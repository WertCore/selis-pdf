# Oracle container: qpdf (SL-0.ORACLE.01, ADR-P0009).
#
# Pinned, in order of strength:
#   1. base image by manifest digest   (verified via registry API 2026-09-08)
#   2. apt package set by snapshot timestamp (reproducible apt state)
#   3. qpdf release tarball by sha256  (computed from a local download)
#
# qpdf is the structural oracle (SL-0.ORACLE.03). Licence: Apache-2.0.
# syntax=docker/dockerfile:1
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

ARG QPDF_VERSION=11.9.0
ARG QPDF_SHA256=9f5d6335bb7292cc24a7194d281fc77be2bbf86873e8807b85aeccfbff66082f

# snapshot.debian.org pins the apt package set to a moment in time so the
# compiler/toolchain cannot drift under the pinned qpdf release.
RUN printf 'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian/20260901T000000Z bookworm main\n' \
        > /etc/apt/sources.list \
 && printf 'deb [check-valid-until=no] http://snapshot.debian.org/archive/debian-security/20260901T000000Z bookworm-security main\n' \
        > /etc/apt/sources.list.d/security.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
        build-essential cmake ca-certificates curl zlib1g-dev libjpeg-dev \
 && rm -rf /var/lib/apt/lists/*

ADD --checksum=sha256:${QPDF_SHA256} \
    https://github.com/qpdf/qpdf/releases/download/v${QPDF_VERSION}/qpdf-${QPDF_VERSION}.tar.gz /tmp/qpdf.tar.gz

RUN cd /tmp \
 && tar xzf qpdf.tar.gz \
 && cmake -S qpdf-${QPDF_VERSION} -B build -DCMAKE_BUILD_TYPE=Release \
       -DBUILD_SHARED_LIBS=OFF -DCMAKE_INSTALL_PREFIX=/usr/local \
 && cmake --build build -j"$(nproc)" \
 && cmake --install build \
 && rm -rf /tmp/qpdf* /tmp/build \
 && qpdf --version

# Structural oracle interface: `docker run <image> --json /in.pdf`
# (rendering is not a qpdf capability — the plan's ORACLE.01 DoD wording
# predates that observation; see pdf-plan/10 §0.ORACLE notes).
ENTRYPOINT ["/usr/local/bin/qpdf"]
