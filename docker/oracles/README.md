# Oracle containers (SL-0.ORACLE.01, ADR-P0009)

Pinned, external-process oracles: qpdf, MuPDF, Ghostscript, PDFium (via a
small C driver), and pdf.js (headless). **Nothing in this directory is linked
into any Selis build** — oracles produce reference output only, and the AGPL
tools (MuPDF, Ghostscript) exist exclusively inside their containers
(ADR-P0021).

## Layout

| Path | What |
|---|---|
| `qpdf/Dockerfile` | Structural oracle (`qpdf --json`), built from the pinned release tarball |
| `mupdf/Dockerfile` | `mutool` render/text oracle, built from the pinned source tarball |
| `ghostscript/Dockerfile` | Codec/colour/shading reference, built from the pinned source tarball |
| `pdfium/Dockerfile` + `driver/pdfium_driver.c` | PDFium BSD-3 binaries + our ~250-line C render driver |
| `pdfjs/Dockerfile` + `driver.mjs` | Headless pdf.js render driver over pinned pdfjs-dist + @napi-rs/canvas |
| `fixtures/smoke.pdf` | 5-object, 415-byte single-page PDF used to smoke-test every image |

Every pin is recorded in `xtask/oracles.toml` together with the base-image
manifest digest and the tool artifact sha256. Each digest/sha256 in the file
was resolved or computed on 2026-09-08 (registry API for base images; local
download + hash for source artifacts; npm tarball cross-checked against the
registry's published integrity hash). The five image manifest digests were
recorded from the CI push of 2026-09-09 (all five images built,
smoke-rendered against `fixtures/smoke.pdf`, and pushed to GHCR).

## Dispatch model

`xtask oracle render/compare/triage` are **local-first**: when the tool is
installed on the host (qpdf, mutool, gs; or a locally-built
`pdfium_driver.exe` for the PDFium path), it runs natively — faster for local
development. Otherwise the pinned container is dispatched through Docker and
is pulled **by digest** from GHCR.

## Building and re-recording digests

Re-record a digest only after a Dockerfile change (digests are
content-addressed; `oracle check` compares output against exactly the
recorded digest):

```sh
docker build -t ghcr.io/wertcore/selis-pdf/oracle-qpdf:11.9.0 docker/oracles/qpdf
docker push ghcr.io/wertcore/selis-pdf/oracle-qpdf:11.9.0
docker buildx imagetools inspect ghcr.io/wertcore/selis-pdf/oracle-qpdf:11.9.0
# -> record the manifest digest in xtask/oracles.toml [tool.qpdf] digest
```

The CI `oracle-images` workflow builds all five images on every push that
touches `docker/oracles/**`, smoke-renders `fixtures/smoke.pdf` through each
render oracle (plus a `qpdf --json` structural pass), pushes when the repo
variable `ENABLE_ORACLE_PUSH=1` (currently set), and prints the digests.

## Local verification status (host without Docker, 2026-09-08)

| Oracle | Local verification |
|---|---|
| qpdf 12.4.1 | `qpdf --json` exercised on real corpus files (structural comparisons) |
| mutool 1.23.0 | `mutool draw -r 150` renders PNG/PPM (ORACLE.02 compare path) |
| PDFium chromium/7961 | driver.c compiled with MSVC against the pinned win-x64 tarball; renders 160F-2019.pdf at 150 dpi to a valid 1240×1754 PNG |
| pdf.js 6.2.108 | driver.mjs run with node 24 + pinned deps (`npm ci` from the committed lockfile); renders the same page to a valid PNG |
| Ghostscript 9.56.1 | not installed locally; container built + smoke-rendered in CI (run 34319440352) |

Container builds themselves are validated by CI (`oracle-images`); expect
first-run recipe fixes, which is what that job exists to shake out.
