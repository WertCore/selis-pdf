# Oracle container: pdf.js headless (SL-0.ORACLE.01, ADR-P0009).
#
# Pinning: base image digest (registry-verified 2026-09-08) + package-lock.json
# integrity hashes for pdfjs-dist 6.2.108 and @napi-rs/canvas 1.0.8 (npm ci
# refuses any artifact whose sha512 differs). The pdfjs-dist tarball's sha256
# was additionally cross-checked against the npm registry's published
# integrity (see xtask/oracles.toml).
#
# pdf.js is the second, genuinely independent render/extract oracle.
# Licence: Apache-2.0.
# syntax=docker/dockerfile:1
FROM node:22-bookworm-slim@sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5

WORKDIR /opt/selis-oracle-pdfjs
COPY package.json package-lock.json ./
# `npm ci` is the integrity gate: it installs exactly the lockfile tree and
# fails on any hash mismatch (including the platform canvas binary).
RUN npm ci --omit=dev --no-audit --no-fund \
 && npm cache clean --force
COPY driver.mjs ./

# Render oracle interface: `docker run <image> --page 1 --dpi 150 /in.pdf /out.png`
ENTRYPOINT ["node", "driver.mjs"]
