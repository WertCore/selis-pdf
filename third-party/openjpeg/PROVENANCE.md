# Vendored OpenJPEG 2.5.3 (SL-1.FILT.08)

Upstream: https://github.com/uclouvain/openjpeg
Pinned tag: v2.5.3, commit 210a8a5690d0da66f02d49420d7176a21ef409dc
Source tarball sha256: 368fe0468228e767433c9ebdea82ad9d801a3ad1e4234421f352c8b06e7aa707
Licence: BSD-2-Clause (see LICENCE below / upstream LICENSE).

## What changed versus upstream

* Sources are verbatim upstream `src/lib/openjp2/*` with these files
  excluded from the wasm build: `event.c` (replaced by the no-op message
  stub `shim/selis_event.c` — formatted messages embed document-shaped
  data, which the engine keeps out of logs per ADR-P0017), `opj_clock.c`
  (clock reads are sandbox-forbidden), `bench_dwt.c`,
  `t1_generate_luts.c`, `t1_ht_generate_luts.c`, `test_sparse_array.c`
  (dev tools), and the codestream-index managers (`cidx_manager.c`,
  `phix/ppix/thix/tpix_manager.c` — encoder-side PDF-unneeded).
  `event.h` is kept verbatim; `thread.c` compiles its upstream
  single-threaded stub branch (no MUTEX_* defined).
* `shim/` adds: hand-written `opj_config.h` / `opj_config_private.h`
  (replacing the CMake-generated ones, with every optional libc feature
  undefined), minimal freestanding libc/libm headers, `selis_libc.c`
  (first-fit free-list malloc over the module's linear memory, memory.grow
  through the wasm intrinsic so the host's ResourceLimiter stays the hard
  cap; string/memory/qsort; the libm subset OpenJPEG references),
  `selis_jpx.c` (the Tier-2 buffer-protocol wrapper: init/decode/output/
  output_ptr/finish over opj_*), and `selis_event.c`.
* `../build-jpx.ps1` builds `crates/selis-pdf-filter/assets/selis-jpx.wasm`
  (wasm32-unknown-unknown, -O2, --no-entry, zero imports, --stack-first,
  64 MiB module max-memory; the HOST limiter enforces the real per-run
  cap). `../refjpx.c` builds the native reference decoder/encoder used by
  the DoD tests as an oracle — host-build only, never shipped.

## Binary provenance

The committed `assets/selis-jpx.wasm` is reproducible from this tree with
LLVM clang 18.1.8; its sha256 is recorded in `assets/selis-jpx.wasm.sha256`
and re-verified in-crate on every decode (`jpx::verify_module_pinned`).
