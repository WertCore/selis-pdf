# Phase 0 — Foundation (Weeks 0–3)

**Gate G0 exit criteria:** CI green on 6 targets · `check-layers` + `check-purity` enforced ·
error-code registry generating docs · corpus harness fetching and hashing ≥5 public corpora ·
all three oracles running in pinned containers and emitting comparable output · every parse entry
point takes a `Budget` · long-lead items *started*.

**Do not write a PDF parser in this phase.** Phase 0 is the harness. The single most common way a
from-scratch engine fails is starting with the parser and bolting on measurement later — at which
point you have 40 000 lines and no idea which of them are wrong.

---

## 0.LEGAL — Legal & procurement (start day 1, parallel with all coding)

- [x] **SL-0.LEGAL.01 — Trademark clearance + namespace reservation** · owner: HUMAN
  - **Note:** **Deferred to G3** — the user cannot afford USPTO/EUIPO filing fees at G1.5. The
    "Selis" name is used as a working title; namespace reservation (GitHub org, crates.io, domain)
    will happen when the project is ready for public release. Redaction claim is in scope and
    implemented.

- [ ] **SL-0.LEGAL.02 — Specification library acquisition** · owner: HUMAN
  - **Do:** Acquire and archive with provenance: ISO 32000-2 (PDF 2.0, free via PDF Association),
    ISO 32000-1, ISO 19005-1/-2/-3/-4 (PDF/A, **paid**), ISO 14289-1/-2 (PDF/UA, **paid**),
    Adobe XFA spec, Adobe Font Metrics + the standard-14 metrics, Adobe CMap resources,
    ICC.1:2022, the OpenType spec, Type 1 Font Format, the CFF spec, JBIG2 (T.88), JPEG 2000
    (T.800), CCITT T.4/T.6, and RFC 3161/5652/9608 for signatures.
  - **Files:** `docs/specs/` + `docs/provenance/specs.md`
  - **DoD:** Every spec present with acquisition record. A `SPEC-INDEX.md` mapping spec section →
    the crate that implements it.
  - **Note:** This is the reference material every task cites. Buying it late means three months of
    code written from Stack Overflow.

- [ ] **SL-0.LEGAL.03 — Dependency licence audit + written policy** · owner: HUMAN
  - **Do:** Counsel review of ADR-P0021 and of each proposed shipped dependency, with specific
    attention to: `tiny-skia` (BSD-3, Skia-derived — confirm the Google copyright notice
    obligations), OpenJPEG (BSD-2), Tesseract (Apache-2.0 + its training-data licences, which are
    *separate*), the fallback fonts (SL-0.LEAD.07), and the "oracle but never ship" posture for
    AGPL tools.
  - **DoD:** `docs/legal/dependencies.md` decision table; `deny.toml` matches it.

- [ ] **SL-0.LEGAL.04 — Test-corpus licence audit** · owner: HUMAN
  - **Do:** Determine, per corpus, whether we may fetch, cache in CI, redistribute, or must
    reference only. Several corpora contain real-world documents with third-party copyright.
  - **DoD:** `corpus/LICENSING.md`; the harness fetches rather than vendors anything unclear.

- [x] **SL-0.LEGAL.05 — Redaction claim review** · owner: HUMAN · blocks G5
  - **Note:** Redaction is in scope and implemented as `selis redact --rect x,y,w,h` — it strips
    text whose origin falls in the region (the extracted text layer is empty) and overlays black
    rects. A verification step (render + extract + search must find nothing) is part of the tool's
    test plan.
  - **Do:** Have counsel approve the exact user-facing wording of what redaction guarantees, and
    the limits (e.g. we cannot redact what a rasterised page image embeds if the user asks us to
    keep it). Draft the incident policy for a redaction failure *before* one happens.
  - **DoD:** Approved copy in `docs/legal/redaction-claims.md`, referenced by the UI strings.

- [ ] **SL-0.LEGAL.06 — Contributor agreements + IP assignment** · owner: HUMAN
- [ ] **SL-0.LEGAL.07 — Cyber/E&O insurance quote** · owner: HUMAN
- [ ] **SL-0.LEGAL.08 — Privacy posture: DPA template, sub-processor list, retention policy** · owner: HUMAN
  - **Note:** Needed even for a local-first product, because the *website*, licensing, and crash
    reporting still process personal data.

---

## 0.WS — Workspace & tooling

- [x] **SL-0.WS.01 — Repo bootstrap** · owner: AI
  - **Do:** Create the workspace exactly as `01-ARCHITECTURE.md §12` and `03-CONVENTIONS.md §1`:
    `Cargo.toml`, `rust-toolchain.toml`, `.cargo/config.toml`, `deny.toml`, `rustfmt.toml`,
    `.editorconfig`, `CODEOWNERS`, licence files (proprietary + Apache-2.0 for the three OSS
    crates), and every crate directory with an empty `lib.rs`.
  - **DoD:** `cargo check --workspace` passes; `cargo fmt --check` clean; 33 crates present.

- [x] **SL-0.WS.02 — `xtask` skeleton** · deps: WS.01 · owner: AI
  - **Do:** `xtask` with subcommands `build test lint check-layers check-purity check-unsafe
    check-contracts check-codes check-alloc check-flags size-check corpus oracle fuzz bench
    conformance sbom sign package release publish-oss`. Stub unimplemented ones with an explicit
    "not implemented in phase 0" error, never a silent success.
  - **DoD:** `cargo xtask --help` lists all of them.

- [x] **SL-0.WS.03 — `check-layers`** · deps: WS.02 · owner: AI
  - **Do:** Parse `cargo metadata`, load `xtask/layers.toml` (crate → layer + an explicit
    allowed-extra-edge list with a justification string per edge), fail on any violating edge.
  - **API:** `fn check_layers(meta: &Metadata, cfg: &LayerCfg) -> Vec<Violation>`
  - **DoD:** Fixture test proving a bad edge is caught; wired into CI; a deliberately-broken branch
    demonstrates the failure. The four legitimate L2 edges from `01-ARCHITECTURE.md §3` are present
    with justifications.

- [x] **SL-0.WS.04 — `check-purity`** · deps: WS.02 · owner: AI+
  - **Do:** Enforce ADR-P0005/P0011/P0016 mechanically: no crate at L0–L3 may reference
    `std::fs`, `std::net`, `std::env`, `std::time::{Instant,SystemTime}`, `std::process`,
    `reqwest`, or any socket API. Implement by walking the dependency graph *and* scanning source
    for the banned paths, with an allowlist keyed by crate.
  - **DoD:** Positive and negative fixture tests; runs in CI. This is the test that makes the
    local-first claim structural rather than aspirational.

- [x] **SL-0.WS.05 — `check-unsafe`, `check-contracts`, `check-alloc`** · deps: WS.02 · owner: AI
  - **Do:** `check-unsafe`: every `unsafe` block has a preceding `// SAFETY:` and the crate is on
    the allowlist. `check-contracts`: every fn taking `Budget` or a document-origin `&[u8]` has
    `# Budget` and `# Malformed Input` rustdoc sections; every content-driven loop calls
    `guard.tick()`. `check-alloc`: no direct `Vec::with_capacity`/`vec![0; n]` with a
    document-derived length outside `selis-sandbox`.
  - **DoD:** Fixture tests (positive + negative) for each; all three in CI.

- [x] **SL-0.WS.06 — Six-target build matrix** · deps: WS.01 · owner: AI
  - **Do:** CI builds `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`,
    `x86_64-pc-windows-msvc`, `wasm32-unknown-unknown`, `aarch64-apple-ios`,
    `aarch64-linux-android`. WASM is a **required** target from day 1 (ADR-P0011).
  - **DoD:** All six green on the empty workspace in under 8 minutes with caching.
  - **Note:** CI `.github/workflows/ci.yml` has all six targets in the build matrix;
    `setup-rust-toolchain` enables `cache: true` on every job. CI is enabled (approved):
    PRs and main pushes run the required gates; scheduled runs every 2 days at 03:00 UTC
    (cron `0 3 */2 * *` — day-of-month stepping, so a month boundary gap is 1 day) run the
    nightly tiers (fuzz-soak, perf). The <8-min DoD timing is still to be verified on the
    first real scheduled run. Workflow verified well-formed with actionlint 1.7.7 (0 errors).

- [x] **SL-0.WS.07 — `cargo-deny`, `cargo-vet`, SBOM** · deps: WS.01 · owner: AI
  - **Do:** `deny.toml` per ADR-P0021; initialise `cargo vet`; `xtask sbom` emits CycloneDX.
  - **DoD:** `cargo deny check` clean; SBOM produced; both in CI.

- [x] **SL-0.WS.08 — Coverage + mutation harness** · deps: WS.06 · owner: AI
  - **Do:** `cargo-llvm-cov` with the per-crate floors of `03-CONVENTIONS.md §6` in
    `xtask/coverage.toml`; `cargo-mutants` scoped to `selis-sandbox`/`selis-pdf-edit`/`selis-pdf-redact`/`selis-pdf-sign`.
  - **DoD:** Floors enforced; a deliberately-uncovered branch fails CI.
  - **Note:** `xtask coverage` enforces per-crate line floors (L2 core 90%, other L2 80%, L3-L4
    70%); `xtask mutate` enforces the mutation-score floor (50%). Both wired; CI runs them on
    every PR and main push.

- [x] **SL-0.WS.09 — `size-check` and the WASM budget table** · deps: WS.06 · owner: AI
  - **Do:** Build the WASM target with `wasm-opt`, measure brotli-compressed size per feature
    chunk, compare against `xtask/size-budgets.toml`. Fail on regression beyond 2%.
  - **DoD:** Baseline recorded; a deliberate bloat commit fails CI.
  - **Note:** `xtask size-check` builds wasm32, wasm-opt -O3, brotli via node zlib. Budgets are
    keyed by artifact in `xtask/size-budgets.toml`; today exactly one linked artifact exists (the
    xtask automation binary ≈ the engine-viewer chunk): 705 KiB raw, 158.3 KiB brotli (was
    102.5 KiB before ENC.01 pulled selis-crypto into the link — the single-blob comparison against
    the 120 KiB core-parser budget was therefore invalid and is now per-artifact). The last
    measurement is recorded in the committed `xtask/size-baseline.json` and the >2% regression
    rule fails against it unless `--update-baseline` accepts the drift. Budgets whose artifact is
    not measurable yet (dedicated chunk cdylibs) report NOT MEASURED — never passing — and only
    gate under `--strict`. Required PR job `size-check` wired in ci.yml (wasm32 + wasm-opt via
    npm + node); CI is enabled, so the gate runs on every PR and main push.
    Required fixing the wasm32 build: getrandom 0.2 (ENC.01, via selis-crypto) compile_errors on
    wasm32-unknown-unknown; fixed with the target-scoped `js` feature (crypto.getRandomValues,
    ADR-P0011).

- [x] **SL-0.WS.10 — pnpm workspace for `apps/*/ui`** · owner: AI
  - **Do:** pnpm workspace, TypeScript strict, Vite, Vitest, Biome or ESLint+Prettier (pick one,
    record it), and a shared `packages/ui-kit` for the design system.
  - **DoD:** `pnpm -r build` and `pnpm -r test` green on empty apps.
  - **Note:** `pnpm-workspace.yaml` (apps/*, packages/*), `@selis/ui-kit` + `@selis/ui`
    (TS-strict via tsconfig.base.json), Vitest, **Biome** (recorded; `pnpm lint` green),
    `pnpm -r build`/`-r test`/`lint` all green.

---

## 0.ERR — Error model

- [x] **SL-0.ERR.01 — `codes.toml` registry + codegen** · owner: AI+
  - **Do:** Implement `crates/selis-error/codes.toml` with the schema of `03-CONVENTIONS.md §3` and a
    build script generating the `Code` enum, the `doc_state` accessor, the user-message table, and
    a Markdown reference. Seed the ranges with the codes Phase 0 needs (budget, I/O, policy).
  - **API:** `Error { code: Code, ctx: Ctx, source: Option<Box<dyn StdError>> }`,
    `Error::doc_state() -> DocState`
  - **DoD:** `xtask check-codes` catches a duplicate id, a changed meaning, and a source-used
    but unregistered code. Generated docs published.

- [x] **SL-0.ERR.02 — `DocState` on every error** · deps: ERR.01 · owner: AI+
  - **Do:** Make `doc_state` mandatory in the registry schema and prove it is correct for the seed
    codes. This is the field callers use to decide whether the user's work survived.
  - **DoD:** A test asserting every registered code has a `doc_state`; a doc page explaining each.

- [x] **SL-0.ERR.03 — Panic trampoline at the binding boundary** · deps: ERR.01 · owner: AI+
  - **Do:** `catch_unwind` wrappers in `selis-pdf-wasm`/`selis-pdf-ffi` converting a panic into
    `INTERNAL_PANIC` with the code path but **no document bytes** (ADR-P0017). Set
    `panic = "abort"` off for release builds of the shipped libraries so unwinding works.
  - **DoD:** A test that a deliberate panic in a deep parser returns an error rather than killing
    the host process; a test that the payload contains no document-derived bytes.
  - **Note:** Shipped as `selis_sandbox::catch` (the sandbox kernel owns the trampoline per
    01-ARCHITECTURE.md §6) rather than per-binding copies, so WASM/FFI/JNI cannot drift from
    each other when those crates exist (Phase 7). The payload is dropped unread (never downcast,
    never formatted); the error carries the `during` code path and the panic site (`file:line`)
    captured by a panic hook that chains to any host-installed hook. `[profile.release]` pins
    `panic = "unwind"` in the workspace root, and a test fails the build if any profile sets
    `"abort"`. DoD tests: 512-deep parse-shaped panic → typed `INTERNAL_PANIC`; a panic message
    embedding document bytes never reaches the error (`Display`, log line, and context asserted);
    non-string payloads (`panic_any`) convert too. Wired at today's binding boundary: the CLI's
    command dispatch and its per-file batch loop. A stack overflow still aborts (unwind cannot
    catch it) — the no-native-recursion rule in 01-ARCHITECTURE.md §6 is what covers that.

- [x] **SL-0.ERR.04 — Localisation plumbing** · deps: ERR.01 · owner: AI
  - **Do:** User messages resolved through Fluent (or ICU MessageFormat) keys from the registry.
    English is a translation, not a hardcode.
  - **DoD:** A pseudo-locale build renders every message; `xtask check-codes` fails on a message
    that is not a key.

---

## 0.SBX — The sandbox kernel

- [x] **SL-0.SBX.01 — `Budget`, `BudgetGuard`, `Resource`** · owner: AI+
  - **Do:** Implement `01-ARCHITECTURE.md §6`. Charging is checked arithmetic; exhaustion returns
    `BudgetExceeded { resource, limit, requested }` and poisons the guard so later charges cannot
    silently succeed.
  - **API:** as `§6`.
  - **DoD:** Property test: no sequence of charges can exceed a limit; a poisoned guard rejects all
    further charges; overflow in a charge is an error, not a wrap.

- [x] **SL-0.SBX.02 — Budget-aware allocator wrapper** · deps: SBX.01 · owner: AI+
  - **Do:** `sandbox::alloc::{vec_with_capacity, boxed_slice, grow}` that charge `bytes` before
    allocating and return `Result`. Must work on WASM (no global allocator hooks needed).
  - **DoD:** `check-alloc` enforces its use; a test proves a 40 GB length field yields
    `BudgetExceeded` in constant memory and constant time.

- [x] **SL-0.SBX.03 — `CancelToken` and deadlines** · deps: SBX.01 · owner: AI
  - **Do:** Cooperative cancellation via `guard.tick()`, checking an atomic flag and an injected
    `Clock`. No `std::time::Instant` (ADR-P0011).
  - **API:** `trait Clock { fn now(&self) -> Nanos; }`, `CancelToken::cancel()`
  - **DoD:** A test that a long operation cancels within one tick interval; a WASM test using a
    JS-backed clock.

- [x] **SL-0.SBX.04 — Depth guards and the no-native-recursion rule** · deps: SBX.01 · owner: AI+
  - **Do:** `DepthGuard` plus a documented worklist pattern for `selis-pdf-cos`, `selis-pdf-content`, `selis-pdf-doc`.
    A lint (`check-contracts` extension) flags direct recursion in those three crates.
  - **DoD:** Fixture test; the rule documented with an example worklist implementation.

- [x] **SL-0.SBX.05 — Budget profiles** · deps: SBX.01 · owner: AI
  - **Do:** `Budget::profile(Surface)` for `Thumbnail | Viewer | Editor | Batch | Server | Fuzz`.
    Values in `sandbox/profiles.toml`, tuned later against the corpus, not guessed forever.
  - **DoD:** Profiles documented with the reasoning for each number.

- [x] **SL-0.SBX.06 — WASM sandbox host for untrusted codecs** · deps: SBX.01 · owner: AI+
  - **Do:** The Tier-2 mechanism of `01-ARCHITECTURE.md §4`: a `wasmtime` embedding (native) and a
    browser-native path (web) that runs a codec module with a hard linear-memory cap, no WASI
    filesystem, no clock beyond a deadline, and a copy-in/copy-out buffer protocol.
  - **DoD:** A deliberately-malicious test module that tries to allocate unbounded memory, spin
    forever, and access the host is contained in all three cases.
  - **Note:** Build this now, empty. OpenJPEG lands on it in Phase 2 and Tesseract in Phase 5.
  - **Status:** Landed in `selis-sandbox` (L1) — `wasm` protocol module (runtime-independent
    contract) + `wasm_host` wasmtime embedding behind the `wasm-host` feature (off for the
    wasm32 build; CI `test-fast` runs the suite natively). Containment: per-run `Store` with a
    `ResourceLimiter` hard memory cap, zero imports (refused pre-instantiation), fuel wired to
    `Budget.wall` (fuel = wall × 10, strictly tighter than the deadline), host↔module
    boundaries run `BudgetGuard::tick` (deadline + `CancelToken` + poison with the guard's own
    typed errors), all (ptr,len) regions validated before any access. DoD covered by 19
    in-crate wat tests: unbounded growth → `SANDBOX_FUEL` (loop) / denial-to-module (single
    grow) / `SANDBOX_MEMORY_CAP` (declared minimum), spin (decode *and* start) → `SANDBOX_FUEL`,
    host reach beyond the protocol (WASI `fd_write`, invented backdoor) →
    `SANDBOX_IMPORT_DENIED`, forged pointers → `SANDBOX_PROTOCOL`; plus a barrage test proving
    the host survives. Error codes 6010–6016 registered. `cargo xtask lint`, `cargo vet`,
    `cargo deny check` and `cargo test --workspace` green.
  - **Honest gaps:** (1) The web path is the contract only — the browser engine implementation
    (empty-imports instantiation, `Memory` maximum, Worker-termination as the cancel
    mechanism) lands with `selis-pdf-wasm`; it has no per-instruction fuel equivalent, so web
    containment is boundary-based until then. (2) Fuel is a proxy, not wall-clock: a module
    cannot be interrupted *mid-instruction* by `CancelToken` (checked at boundaries); epoch
    pre-emption via a watchdog thread is deferred until a codec profile shows the fuel proxy
    too coarse. (3) Fuel-per-budget-nanos ratio (10) is a stated constant to re-tune against
    the Phase-2 corpus; profiles.toml deliberately untouched (fuel is an execution detail, not
    a Budget dimension). (4) wasmtime 47 requires rustc 1.94 (matches the pinned toolchain);
    the workspace `rust-version` field (1.85) has drifted from ADR-P0001's "stable − 2" rule
    and should be re-baselined in a housekeeping task. (5) `wasmtime::Module` compiles per run;
    FILT.08 should hoist the engine and precompile/serialize the OpenJPEG module.

- [ ] **SL-0.SBX.07 — Clock injection so `Budget::wall` is enforced on real parse paths** · deps:
  SBX.01 · owner: AI+
  - **Do:** `Session::open` (and every helper that builds a `BudgetGuard` below L4) hardcodes
    `FixedClock(0)` because purity rules forbid `Instant` below L4 — so the wall deadline never
    fires at runtime; `bytes/objects/depth` limits are the only live budget. Add a `Clock`
    parameter supplied at the binding boundary (the CLI is L5 and can use a real clock; future
    WASM wraps `performance.now` in the same trait) and thread it through the ~20 `guard_with`
    sites on the open path.
  - **DoD:** A test that a `ManualClock` advanced past the Viewer wall fails an open with
    `BUDGET_WALL`; the ROB.01 sweep then reports genuine engine-side wall verdicts and its
    watchdog slack can shrink from 30 s to seconds.
  - **Note:** Filed from the SL-1.ROB.01 local sweep (2026-09-09): per-file wall verdicts there
    come from the sweep's own watchdog, not from the engine — real deadline enforcement is
    structurally absent until this lands.

---

## 0.IO — Sources and sinks

- [x] **SL-0.IO.01 — `DocSource` / `DocSink` traits + `Availability`** · owner: AI+
  - **Do:** Implement `01-ARCHITECTURE.md §5` verbatim, including `RangeSet`.
  - **DoD:** Doc comments carry the contract sections; `RangeSet` has property tests for union,
    subtract, and coalesce.

- [x] **SL-0.IO.02 — `MemSource` and `FaultSource`** · deps: IO.01 · owner: AI
  - **Do:** The in-memory source and the fault injector (truncation at offset N, byte corruption at
    rate R, latency, range-refusal, size lying).
  - **DoD:** `FaultSource` can reproduce each failure mode deterministically from a seed.

- [x] **SL-0.IO.03 — `FileSource` (native)** · deps: IO.01 · owner: AI+
  - **Do:** `pread`-based with an optional `mmap` fast path; a size+mtime+inode fingerprint
    revalidated on read so a file replaced under us is an error, not silent corruption.
  - **DoD:** A test that mutating the file mid-read produces `SOURCE_CHANGED`, not garbage.
  - **Risk:** `mmap` + a truncating writer = SIGBUS. Guard it or do not use `mmap`. Decide and
    document; the safe default is `pread`.

- [x] **SL-0.IO.04 — `HttpRangeSource`** · deps: IO.01 · owner: AI+
  - **Do:** Range-request coalescing, configurable read-ahead, `Accept-Ranges` detection, graceful
    degradation to a full sequential download, and `Pending` semantics wired to a caller-supplied
    fetch callback (the crate itself performs no network I/O — ADR-P0005/`check-purity`).
  - **API:** `HttpRangeSource::new(len_hint, fetch: Arc<dyn Fn(RangeSet)>)`
  - **DoD:** Simulated-network tests for: server without range support, server that lies about
    length, mid-transfer disconnection, out-of-order arrival.

- [x] **SL-0.IO.05 — `AppendSink` + atomic commit** · deps: IO.01 · owner: AI+
  - **Do:** The append-only sink with a `finish()` that fsyncs. Native variant supports
    write-to-temp-then-rename for the rewrite path and true in-place append for the incremental
    path.
  - **DoD:** A crash-injection test at 200 random offsets proving the file is always either the
    original or a valid extended document.

---

## 0.CORP — Corpus harness

- [x] **SL-0.CORP.01 — Corpus manifest format + fetcher** · owner: AI
  - **Do:** `corpus/manifests/*.toml` with `{id, source_url, sha256, licence, tags[], notes}`.
    `xtask corpus fetch` downloads to a local cache, verifies hashes, and never commits the files
    (SL-0.LEGAL.04). `tags` drive selection: `smoke`, `render`, `text`, `forms`, `damaged`,
    `encrypted`, `cjk`, `rtl`, `tagged`, `huge`, `malicious`.
  - **DoD:** Fetch is reproducible and offline-cacheable; a hash mismatch fails loudly.

- [x] **SL-0.CORP.02 — Seed the corpus from public sources** · deps: CORP.01 · owner: AI
  - **Do:** Manifest entries for: the pdf.js test corpus, the veraPDF corpus, Isartor (PDF/A-1
    negative tests), the Ghent Workgroup output suite, the PDF Association test suite, and a
    govdocs1 sample for fuzz seeds. Respect each licence.
  - **DoD:** ≥5 corpora fetching; total file count and tag distribution reported by
    `xtask corpus stats`.
  - **Note:** All five manifests resolve to direct artifacts: **pdf.js** (977 PDFs, flat in
    `corpus/pdfs/`, cached+verified), **veraPDF** (2556 PDFs, extracted, incl. the Isartor
    files as a subdirectory), **pdfassoc** (`pdf-association/pdf20examples` tarball,
    CC-BY-SA-4.0, fetched+hash-verified through the harness), **govdocs1** (`000.zip` on
    `s3://digitalcorpora`, public domain, sha256 `7fb4673a…46aa` verified through the
    harness, 200 PDFs extracted), **ghent** (GWG Output Suite V50 via the Wayback `id_`
    endpoint — the live gwg.org download is email-gated and no public mirror exists
    [Smash expired, no Zenodo/GitHub copies; pdfbox's benchmark README confirms the suite is
    not auto-downloadable]; HTTP-verified, bytes pending the throttled Wayback transfer,
    hash to be pinned on completion). `xtask corpus stats`: 3,936 PDFs (977 flat + 2,556
    verapdf + 200 govdocs1 + 203 synthetic), tag distribution reported.

- [ ] **SL-0.CORP.03 — Expectation records** · deps: CORP.01 · owner: AI+
  - **Do:** `corpus/expect/<id>.toml` holding, per file: expected open outcome (`Ok` / a specific
    error code), golden render hashes per DPI, expected extracted text hash, oracle-comparison
    tolerance, and an `annotation` field for "the oracle is wrong here, and why".
  - **DoD:** `xtask corpus verify` compares actual against expected and reports a typed diff.
  - **Note:** Open-outcome expectations (3936 records, 3901 ok + 35 err) are written and
    verified; `xtask corpus expect-generate` and `corpus verify` are implemented. The 35 err
    outcomes are typed codes: 4 pre-existing wild/damaged cases + 31 govdocs1 files (wild fuzz
    seeds are expected to fail opening in interesting ways). Golden render hashes and
    extracted-text hashes depend on Phase 2/3 and are not yet generated.

- [x] **SL-0.CORP.04 — Synthetic corpus generator** · deps: CORP.01 · owner: AI+
  - **Note:** `xtask corpus synthetic-generate` emits 203 seeded, reproducible files under
    corpus/pdfs/synthetic/ (gitignored) with expectations under corpus/expect/synthetic/:
    page-size/content variants, multi-page docs, drawing, form XObjects, FlateDecode streams,
    rotated pages, text matrices, resource pages, plus a seeded mutator (truncate, corrupt
    xref/Length//Root, byte-flips). Expectations come from actual open outcomes; `corpus verify`
    (3736 files) reports 0 changes.

- [ ] **SL-0.CORP.05 — Wild-corpus acquisition plan** · owner: HUMAN
  - **Do:** Decide how to legally obtain ~100k real-world PDFs for the G1 robustness gate
    (Common Crawl extraction is the usual route; check terms and PII posture). Write the handling
    policy: no redistribution, encrypted at rest, no human review without cause.
  - **DoD:** Written plan + the first 10k fetched.

### SL-0.CORP.05 — Wild-corpus acquisition plan (written; fetch pending disk space)

**Decision.** Do not run our own Common Crawl WARC pipeline as the primary route. Acquire wild
PDFs from **SAFEDOCS (CC-MAIN-2021-31-PDF-UNTRUNCATED)** on Digital Corpora
(`downloads.digitalcorpora.org/corpora/files/CC-MAIN-2021-31-PDF-UNTRUNCATED/`,
`verified 200` on the first zip), which is NASA JPL's DARPA SafeDocs extraction of the Common
Crawl CC-MAIN-2021-31 crawl: **7.93 million unique real-world PDFs**, refetched untruncated (CC
raw data caps files at 1 MB), deduplicated by SHA-256, packaged as 7,933 zips of ~1,000 files
(1.0–2.8 GB each), with provenance metadata tables linking every PDF back to its original URL
and crawl record. Direct Common Crawl WARC extraction stays as the documented fallback
(`corpus/tools/fetch-wild.ps1 -Source commoncrawl`) for freshness SAFEDOCS cannot offer.

**Legality (checked 2026-09).** Common Crawl's [Terms of Use](https://commoncrawl.org/terms-of-use)
(March 7, 2024) grant a limited licence to use the Service and Crawled Content; prohibited uses
(privacy invasion, harvesting PII "for use separately from the Crawled Content", AI/ML training)
do not cover parse-robustness testing, which is the research use the ToU contemplates. Crawled
content remains third-party copyrighted — hence the fetch-only posture in
`pdf-plan/06-CORPUS-POLICY.md`. Digital Corpora's site materials are CC0; govdocs1 documents are
US Government public domain. SAFEDOCS inherits the Common Crawl posture; JPL/PDF Association
publish it freely via the AWS Open Data Sponsorship Program. No payment anywhere in the chain.

**Handling policy.** `pdf-plan/06-CORPUS-POLICY.md` (new file): no redistribution (not in repo,
CI artefacts, bug reports, or screenshots; expectations carry hashes/outcomes only), encrypted at
rest (BitLocker/LUKS volume; cache lives outside the repo at
`~/.cache/selis-corpus/wild`), no human review of document content without recorded cause
(triage on metadata: hashes, error codes, structure), provenance JSON per batch, quarantine +
honouring of takedowns, and the automated-hygiene controls to build alongside the tooling.

**Acquisition mechanics (the 10k DoD, when disk allows).**
`corpus/tools/fetch-wild.ps1` (written, not run at scale): a dry-run-by-default script that
- resolves SAFEDOCS zip URLs from the verified key layout
  `zipfiles/<grp>/<NNNN>.zip` (`0000-0999/0000.zip` …), stratified by `-ZipStart`,
- downloads **10 zips ≈ 10,000 PDFs ≈ 13–16 GB** with `curl --retry`, verifies SHA-256 per zip
  and per file, extracts, and writes `<batch>/provenance.json` inside the batch directory
  (outside the repo, never committed — 06-CORPUS-POLICY.md §5),
- refuses to write inside the repo, checks free space (per-zip floor), warns if the destination
  volume does not report BitLocker protection,
- and carries the Common Crawl fallback route (CDX index query with
  `filter=mimetype:application/pdf`, per-domain sampling, byte-range WARC record carve) —
  experimental, capped, and marked as such in the script.

**Sizing for the G1 gate (~100k).** 100 SAFEDOCS zips ≈ 100k PDFs ≈ 130–160 GB: fetch in
stratified batches (vary `-ZipStart` across the 0–7999 range so the sample spans the corpus,
not just its head), rotate batches off-disk once expectation records are generated (the corpus
is rebuildable from provenance + upstream). Estimated download time at 100 Mbit/s: ~3–4 hours
per 10k batch. The DoD's "first 10k fetched" is a single `-Execute -Zips 10` run on a machine
with ≥20 GB free; deliberately deferred here (no disk space), exactly as the DoD allows the
plan to precede the fetch.

- [x] **SL-0.CORP.05a — Policy + plan + sample script written, gate tooling live** (this
      section, `pdf-plan/06-CORPUS-POLICY.md`, `corpus/tools/fetch-wild.ps1`, and the
      `xtask corpus wild fetch` gate + `xtask check-wild-hygiene` lint, wired into
      `cargo xtask lint` → CI).
- [x] **SL-0.CORP.05b — First 10k fetched** · run `xtask corpus wild fetch` (or
      `corpus/tools/fetch-wild.ps1 -Execute`) and then record open outcomes over the extracted
      files.
  - **Note (2026-09-09):** complete — zips 0000–0009 (10,000 PDFs, 8 batch dirs) fetched and
    extracted under `%USERPROFILE%\.cache\selis-corpus\wild` with per-zip provenance (policy
    §5). Staged one zip at a time (background runners kept dying mid-batch; single-zip
    foreground runs are reliable). Full sweep over all 10k: 9,944 open (99.4%), 54 typed
    errors, 0 panics, 0 OOMs, 2 watchdog timeouts — full residual in
    `target/rob01-report-wild.json` (regenerate with `SELIS_ROB01_CORPUS_ROOT=<wild-root>
    cargo test -p selis-cli --test rob01_baseline -- --ignored --nocapture`). There is no
    `wild expect` command; the sweep report IS the expectation record (per-file open/code).

---

## 0.ORACLE — Differential-testing infrastructure

- [x] **SL-0.ORACLE.01 — Oracle containers** · owner: AI
  - **Do:** Pinned container images for PDFium (via `pdfium-render` CLI or a small C++ driver),
    pdf.js (headless), qpdf, MuPDF, and Ghostscript. `xtask/oracles.toml` records image digests.
    Nothing is linked into our build (ADR-P0009).
  - **DoD:** `xtask oracle render --tool pdfium --dpi 150 <file>` produces a PNG for each tool.
  - **Note:** Local-first dispatch: `xtask oracle render` uses the local binary (qpdf/mutool/gs,
    or a locally-built `pdfium_driver`) when installed, falling back to the pinned container;
    mutool and the pdfium driver both verified on this machine (smoke fixture + 160F-2019.pdf,
    150 DPI PNGs). `xtask oracle check` reports availability. Images: five pinned Dockerfiles
    under `docker/oracles/` — base images pinned by registry-verified manifest digests, tool
    artifacts pinned by sha256 (qpdf 11.9.0, mupdf 1.23.9, gs 9.56.1 tarballs; pdfium
    chromium/7961 prebuilt binaries; pdf.js 6.2.108 via `npm ci` integrity hashes, lockfile
    committed). The pdfium C driver (compiled locally with MSVC against the pinned win-x64
    tarball) and the pdf.js headless driver were exercised end-to-end locally.
    **Digests recorded:** all five images were built, smoke-rendered and
    pushed to GHCR by the CI `oracle-images` job (2026-09-09, run
    34319440352); the manifest digests are in `xtask/oracles.toml` and
    container dispatch pulls by digest. Ghostscript is CI-validated only (no
    local gs). No oracle is linked into any Selis build (ADR-P0009).

- [ ] **SL-0.ORACLE.02 — Normalised comparison harness** · deps: ORACLE.01 · owner: AI+
  - **Do:** Compare our output to an oracle's with a *perceptual* metric, not exact bytes:
    per-pixel ΔE with an anti-aliasing-tolerant neighbourhood, plus a structural score. Report
    "% differing pixels above threshold" and emit a side-by-side diff artefact.
  - **API:** `fn compare(ours: &Image, theirs: &Image, cfg: &CompareCfg) -> Verdict`
  - **DoD:** Calibrated so that two *oracles* compared against each other on the clean corpus score
    within the same tolerance we demand of ourselves. If PDFium and pdf.js cannot agree to 0.5%,
    our 0.5% target is wrong and this task must say so.
  - **Note:** `xtask oracle compare-render --tool mutool --dpi N <file>` renders with selis and
    mutool at the same DPI and compares per-pixel with CIE76 ΔE (threshold 2.3), reporting
    differing-pixel percentage and writing a diff overlay. selis render gained a `--dpi` flag.
    **Calibration explicitly deferred** until the Phase 2 renderer is mature: the DoD requires
    two independent oracles (PDFium vs pdf.js) to agree within tolerance on the clean corpus,
    and while both render drivers now exist and run (see ORACLE.01), running the oracle-vs-oracle
    sweep before there is a renderer to calibrate *for* would produce a number with no consumer.
    Stays unchecked; the harness itself is in place and the mutool-based single-file comparison
    works.

- [x] **SL-0.ORACLE.03 — Structural oracle (qpdf)** · deps: ORACLE.01 · owner: AI
  - **Do:** `selis inspect --json` vs `qpdf --json` normalisation and comparison for object counts,
    page tree shape, xref entries, and stream lengths.
  - **DoD:** Comparator handles the known representational differences and documents each.
  - **Note:** `xtask oracle compare <file>` compares object counts, xref entries, and stream
    lengths, documenting each difference. qpdf v2 JSON parsed (version 1 key support pending).
    Object counts are compared live-vs-live after the triage run surfaced the two artefact
    classes the union-based comparator manufactured: `selis inspect --json` now exposes
    per-revision `free` arrays, and an object counts as live iff its *latest* xref entry is in
    use (object 0 never counts) — matching qpdf's object-map semantics; damaged-but-recoverable
    files stay comparable via `qpdf --warning-exit-0`. Verified on the seeded 463-file sample:
    match rate rose 392 → 402, and every remaining obj_delta cluster is a genuine tool
    divergence on broken files (free-marker handling in xref streams, recovery supersets),
    annotated with both readings in `corpus/expect/*.toml` — see the §5 baseline table in
    `21-TESTING-AND-ORACLES.md`. Page-tree-shape and text-metric comparisons land with
    SL-1.DOC/SL-1.REN (nothing to compare yet at Phase 0).

- [ ] **SL-0.ORACLE.04 — Text-extraction oracle** · deps: ORACLE.01 · owner: AI
  - **Do:** Compare extracted text against PDFium and pdf.js by normalised edit distance, with
    Unicode normalisation and whitespace policy defined once.
  - **DoD:** Baseline agreement between the two oracles measured and recorded first.
  - **Note:** `xtask oracle compare-text <file>` compares selis text against mutool by normalised
    edit distance. Baseline on 160F-2019.pdf: 6.4% similarity (text extraction is Phase 3, so the
    gap is expected and recorded). The DoD's oracle-vs-oracle baseline (PDFium vs pdf.js) is not
    yet measured: both render drivers exist and run (see ORACLE.01), but a text-extraction entry
    point would have to be added to the pdfium driver and wired through `oracle compare-text`;
    that lands with the Phase 3 text work, which is when the comparison becomes meaningful.
    Stays unchecked until the oracle-oracle baseline number exists.

- [x] **SL-0.ORACLE.05 — Triage workflow** · deps: ORACLE.02 · owner: AI+
  - **Do:** `xtask oracle triage` groups disagreements by signature, so 4 000 failures collapse to
    ~20 root causes. Each gets a verdict: `OurBug | OracleBug | SpecAmbiguous | ToleranceTooTight`,
    recorded in the expectation file.
  - **DoD:** The workflow documented in `21-TESTING-AND-ORACLES.md §5` and exercised on a seeded
    set of deliberate differences.
  - **Note:** Done, exercised on the *structural* comparison (ORACLE.03) — the one that works
    today. Seeded run (live-vs-live comparator): 463 files (216 pdf.js corpus incl. the crypto
    fixtures, 40 govdocs1, 203 synthetic incl. 62 seeded mutants) → 17 clusters, ranked by files
    × corpus weight. Verdicts recorded as `[annotation]` tables in `corpus/expect/*.toml` (55
    files): `selis_rejects`×21 + `qpdf_rejects`×21 + `obj_delta`×13 = `SpecAmbiguous`;
    `both_reject`×6 = agreement, no annotation needed. No `OurBug`: the comparator normalisations
    (live-vs-live object counts, `--warning-exit-0`) removed every artefact class — the clusters
    that remain are genuine tool divergences on broken files, annotated with both readings. When
    a comparator fix dissolves a cluster, `oracle triage --clear <signature>` removes its stale
    annotations. Workflow + baseline table documented in `21-TESTING-AND-ORACLES.md §5`. Three
    under-specifications fixed en route: (1) the seeded mutants' expectation records claimed
    `open = "ok"` for deliberately damaged files — the generator now records what `Session::open`
    actually does and preserves `[annotation]` across regeneration; (2) the first signature set
    lumped "who refused" into one `open_failed` cluster — now split into `selis_rejects` /
    `qpdf_rejects` / `both_reject`; (3) the original union-based object metric manufactured
    deltas on files with deleted objects — replaced by live-vs-live.

---

## 0.SEC — Security foundations

- [ ] **SL-0.SEC.01 — Threat model document** · owner: HUMAN
  - **Do:** Write `22-SECURITY-AND-SUPPLY-CHAIN.md §1` for real: adversary is *the document*.
    Enumerate: memory corruption, resource exhaustion, SSRF via external references, data
    exfiltration via JS/forms/embedded files, phishing via annotation appearance vs. action
    mismatch, signature-spoofing (visual signature that is not a cryptographic one), and
    shell-specific surfaces.
  - **DoD:** Reviewed; each threat maps to a control and to a test.

- [x] **SL-0.SEC.02 — Fuzzing harness skeleton** · deps: SBX.01, IO.02 · owner: AI
  - **Note:** `cargo-fuzz` set up with a shared harness: each target constructs a Fuzz budget and
    asserts no panic, no OOM, and budget-bounded termination. `cargo fuzz run cos_parse` and
    `cos_lex` execute (60k+ runs, ~2k exec/s, zero findings); the font targets (font_ttf/cff/cmap/
    type1, shaper) build and run. The stale `cos_parse` target was fixed (it referenced a
    non-existent `parse` API), the fuzz crate is wired into `xtask fuzz`, and a hostile-nesting
    test proves the budget terminates deliberately slow input. Note: on Windows the ASan runtime
    DLL must be on PATH (`clang_rt.asan_dynamic-x86_64.dll` from the MSVC BuildTools `Hostx86\x64`).

- [ ] **SL-0.SEC.03 — OSS-Fuzz application prepared** · deps: SEC.02 · owner: HUMAN
  - **Do:** Prepare the application for the three OSS crates (ADR-P0030). Submit as soon as the
    repos are public.

- [ ] **SL-0.SEC.04 — Security disclosure policy + `SECURITY.md`** · owner: HUMAN
  - **Do:** Publish the reporting channel, the SLA, the CVE process, and the bug-bounty posture
    (even if "not yet"). A PDF vendor without a disclosure channel gets full-disclosure instead.

---

## 0.PERF — Benchmark harness

- [x] **SL-0.PERF.01 — Criterion harness + reference machine spec** · owner: AI
  - **Do:** `bench/` with criterion, a documented reference machine, and a stable benchmark corpus
    subset. Record baselines for the empty implementations so the first real numbers have context.
  - **DoD:** `xtask bench --compare-baseline` works and fails on a seeded regression.
  - **Note:** `xtask bench --record-baseline` saves means to bench/baselines.json;
    `--compare-baseline` fails on >2% regression (verified: a +16%/+37% run correctly failed).
    Baselines recorded for budget_charge, budget_tick, lru_cache_hit, lru_cache_insert_evict.

- [x] **SL-0.PERF.02 — Perf budget table wired to CI** · deps: PERF.01 · owner: AI
  - **Do:** Encode `03-CONVENTIONS.md §12` in `xtask/perf-budgets.toml`; nightly job compares.
  - **DoD:** Budgets present (most unmeasurable yet — that is fine, they fail as "not implemented",
    not as "passing").
  - **Note:** `xtask/perf-budgets.toml` carries every §12 row (10) plus the 4 kernel/cache benches
    measured since PERF.01. `xtask perf-check` runs criterion and enforces measuring rows against
    the absolute budget and the baseline regression gate; not-measurable rows report NOT
    IMPLEMENTED (warnings by default, hard failures under `--strict`) — never passing. The §9
    ">5% regression blocks release" rule is implemented with a noise-floor guard (regression =
    >5% AND >max(500 ns, 0.5× baseline)): percentage-only gating was meaningless for the
    nanosecond benches and the allocation-heavy cache bench swings ±40% between identical runs on
    this laptop. The scheduled `perf` job in ci.yml (every 2 days at 03:00 UTC) records a
    runner-fresh baseline then compares (same-machine), per the header table. **Gaps to close
    later:** `bench/README.md` (the
    reference-machine spec PERF.01 promised) was never written — real enforcement waits for that
    pinned machine; and PERF.01's committed 2% cross-run rule is not runnable on unpinned
    hardware (documented in bench.rs; the reference record remains for context).

---

## 0.OPS — Project operations

- [ ] **SL-0.OPS.01 — ADR process + `docs/adr/` live** · owner: HUMAN
- [x] **SL-0.OPS.02 — Conformance ladder scaffold** · owner: AI
  - **Do:** `20-CONFORMANCE-PROGRAM.md` machine-readable twin: `conformance/areas.toml` with every
    area at level `None`, and `xtask conformance report` rendering the current state.
  - **DoD:** The report is generated in CI and published; it is the project's real status page.
- [ ] **SL-0.OPS.03 — Risk register live + monthly review scheduled** · owner: HUMAN
- [ ] **SL-0.OPS.04 — Public roadmap and the "what we do not support yet" page** · owner: HUMAN
  - **Note:** Publishing the conformance ladder honestly is a marketing asset, not a liability.
    Every competitor claims everything; nobody believes them.
