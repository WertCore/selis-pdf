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

- [ ] **SL-0.LEGAL.01 — Trademark clearance + namespace reservation** · owner: HUMAN
  - **Do:** Clear **Selis** in USPTO/EUIPO classes 9 and 42. Secure `recto.com`/`.io`/`.app`, the
    GitHub org, crates.io `selis-pdf-*`, npm `@recto/*`, PyPI, Maven `io.recto`, a Homebrew tap.
  - **DoD:** Written clearance opinion on file; all namespaces reserved; ADR-P0034 confirmed or
    superseded.
  - **Risk:** "Selis" is a printing term of art — expect a descriptive-mark objection. Have a
    second and third candidate ready rather than discovering the problem at month 6.

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

- [ ] **SL-0.LEGAL.05 — Redaction claim review** · owner: HUMAN · blocks G5
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

- [ ] **SL-0.WS.06 — Six-target build matrix** · deps: WS.01 · owner: AI
  - **Do:** CI builds `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`,
    `x86_64-pc-windows-msvc`, `wasm32-unknown-unknown`, `aarch64-apple-ios`,
    `aarch64-linux-android`. WASM is a **required** target from day 1 (ADR-P0011).
  - **DoD:** All six green on the empty workspace in under 8 minutes with caching.

- [ ] **SL-0.WS.07 — `cargo-deny`, `cargo-vet`, SBOM** · deps: WS.01 · owner: AI
  - **Do:** `deny.toml` per ADR-P0021; initialise `cargo vet`; `xtask sbom` emits CycloneDX.
  - **DoD:** `cargo deny check` clean; SBOM produced; both in CI.

- [ ] **SL-0.WS.08 — Coverage + mutation harness** · deps: WS.06 · owner: AI
  - **Do:** `cargo-llvm-cov` with the per-crate floors of `03-CONVENTIONS.md §6` in
    `xtask/coverage.toml`; `cargo-mutants` scoped to `selis-sandbox`/`selis-pdf-edit`/`selis-pdf-redact`/`selis-pdf-sign`.
  - **DoD:** Floors enforced; a deliberately-uncovered branch fails CI.

- [ ] **SL-0.WS.09 — `size-check` and the WASM budget table** · deps: WS.06 · owner: AI
  - **Do:** Build the WASM target with `wasm-opt`, measure brotli-compressed size per feature
    chunk, compare against `xtask/size-budgets.toml`. Fail on regression beyond 2%.
  - **DoD:** Baseline recorded; a deliberate bloat commit fails CI.
  - **Note:** Start this in week 1, empty. A size budget introduced at month 6 is a size budget
    that gets raised at month 6.

- [ ] **SL-0.WS.10 — pnpm workspace for `apps/*/ui`** · owner: AI
  - **Do:** pnpm workspace, TypeScript strict, Vite, Vitest, Biome or ESLint+Prettier (pick one,
    record it), and a shared `packages/ui-kit` for the design system.
  - **DoD:** `pnpm -r build` and `pnpm -r test` green on empty apps.

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

- [ ] **SL-0.ERR.03 — Panic trampoline at the binding boundary** · deps: ERR.01 · owner: AI+
  - **Do:** `catch_unwind` wrappers in `selis-pdf-wasm`/`selis-pdf-ffi` converting a panic into
    `INTERNAL_PANIC` with the code path but **no document bytes** (ADR-P0017). Set
    `panic = "abort"` off for release builds of the shipped libraries so unwinding works.
  - **DoD:** A test that a deliberate panic in a deep parser returns an error rather than killing
    the host process; a test that the payload contains no document-derived bytes.

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

- [ ] **SL-0.SBX.06 — WASM sandbox host for untrusted codecs** · deps: SBX.01 · owner: AI+
  - **Do:** The Tier-2 mechanism of `01-ARCHITECTURE.md §4`: a `wasmtime` embedding (native) and a
    browser-native path (web) that runs a codec module with a hard linear-memory cap, no WASI
    filesystem, no clock beyond a deadline, and a copy-in/copy-out buffer protocol.
  - **DoD:** A deliberately-malicious test module that tries to allocate unbounded memory, spin
    forever, and access the host is contained in all three cases.
  - **Note:** Build this now, empty. OpenJPEG lands on it in Phase 2 and Tesseract in Phase 5.

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

- [ ] **SL-0.IO.03 — `FileSource` (native)** · deps: IO.01 · owner: AI+
  - **Do:** `pread`-based with an optional `mmap` fast path; a size+mtime+inode fingerprint
    revalidated on read so a file replaced under us is an error, not silent corruption.
  - **DoD:** A test that mutating the file mid-read produces `SOURCE_CHANGED`, not garbage.
  - **Risk:** `mmap` + a truncating writer = SIGBUS. Guard it or do not use `mmap`. Decide and
    document; the safe default is `pread`.

- [ ] **SL-0.IO.04 — `HttpRangeSource`** · deps: IO.01 · owner: AI+
  - **Do:** Range-request coalescing, configurable read-ahead, `Accept-Ranges` detection, graceful
    degradation to a full sequential download, and `Pending` semantics wired to a caller-supplied
    fetch callback (the crate itself performs no network I/O — ADR-P0005/`check-purity`).
  - **API:** `HttpRangeSource::new(len_hint, fetch: Arc<dyn Fn(RangeSet)>)`
  - **DoD:** Simulated-network tests for: server without range support, server that lies about
    length, mid-transfer disconnection, out-of-order arrival.

- [ ] **SL-0.IO.05 — `AppendSink` + atomic commit** · deps: IO.01 · owner: AI+
  - **Do:** The append-only sink with a `finish()` that fsyncs. Native variant supports
    write-to-temp-then-rename for the rewrite path and true in-place append for the incremental
    path.
  - **DoD:** A crash-injection test at 200 random offsets proving the file is always either the
    original or a valid extended document.

---

## 0.CORP — Corpus harness

- [ ] **SL-0.CORP.01 — Corpus manifest format + fetcher** · owner: AI
  - **Do:** `corpus/manifests/*.toml` with `{id, source_url, sha256, licence, tags[], notes}`.
    `xtask corpus fetch` downloads to a local cache, verifies hashes, and never commits the files
    (SL-0.LEGAL.04). `tags` drive selection: `smoke`, `render`, `text`, `forms`, `damaged`,
    `encrypted`, `cjk`, `rtl`, `tagged`, `huge`, `malicious`.
  - **DoD:** Fetch is reproducible and offline-cacheable; a hash mismatch fails loudly.

- [ ] **SL-0.CORP.02 — Seed the corpus from public sources** · deps: CORP.01 · owner: AI
  - **Do:** Manifest entries for: the pdf.js test corpus, the veraPDF corpus, Isartor (PDF/A-1
    negative tests), the Ghent Workgroup output suite, the PDF Association test suite, and a
    govdocs1 sample for fuzz seeds. Respect each licence.
  - **DoD:** ≥5 corpora fetching; total file count and tag distribution reported by
    `xtask corpus stats`.

- [ ] **SL-0.CORP.03 — Expectation records** · deps: CORP.01 · owner: AI+
  - **Do:** `corpus/expect/<id>.toml` holding, per file: expected open outcome (`Ok` / a specific
    error code), golden render hashes per DPI, expected extracted text hash, oracle-comparison
    tolerance, and an `annotation` field for "the oracle is wrong here, and why".
  - **DoD:** `xtask corpus verify` compares actual against expected and reports a typed diff.

- [ ] **SL-0.CORP.04 — Synthetic corpus generator** · deps: CORP.01 · owner: AI+
  - **Do:** A generator that emits PDFs exercising specific constructs (each xref flavour, each
    filter, each colour space, each shading type, nested transparency groups, deeply nested form
    XObjects, every encryption revision), plus a *mutator* that damages a valid file in defined
    ways (truncate, corrupt xref offsets, break stream lengths, cyclic references).
  - **DoD:** ≥200 generated files with known-correct expectations; the mutator is seeded and
    reproducible.
  - **Note:** This is worth more than any downloaded corpus, because the expectation is derived,
    not guessed. Invest here.

- [ ] **SL-0.CORP.05 — Wild-corpus acquisition plan** · owner: HUMAN
  - **Do:** Decide how to legally obtain ~100k real-world PDFs for the G1 robustness gate
    (Common Crawl extraction is the usual route; check terms and PII posture). Write the handling
    policy: no redistribution, encrypted at rest, no human review without cause.
  - **DoD:** Written plan + the first 10k fetched.

---

## 0.ORACLE — Differential-testing infrastructure

- [ ] **SL-0.ORACLE.01 — Oracle containers** · owner: AI
  - **Do:** Pinned container images for PDFium (via `pdfium-render` CLI or a small C++ driver),
    pdf.js (headless), qpdf, MuPDF, and Ghostscript. `xtask/oracles.toml` records image digests.
    Nothing is linked into our build (ADR-P0009).
  - **DoD:** `xtask oracle render --tool pdfium --dpi 150 <file>` produces a PNG for each tool.

- [ ] **SL-0.ORACLE.02 — Normalised comparison harness** · deps: ORACLE.01 · owner: AI+
  - **Do:** Compare our output to an oracle's with a *perceptual* metric, not exact bytes:
    per-pixel ΔE with an anti-aliasing-tolerant neighbourhood, plus a structural score. Report
    "% differing pixels above threshold" and emit a side-by-side diff artefact.
  - **API:** `fn compare(ours: &Image, theirs: &Image, cfg: &CompareCfg) -> Verdict`
  - **DoD:** Calibrated so that two *oracles* compared against each other on the clean corpus score
    within the same tolerance we demand of ourselves. If PDFium and pdf.js cannot agree to 0.5%,
    our 0.5% target is wrong and this task must say so.

- [ ] **SL-0.ORACLE.03 — Structural oracle (qpdf)** · deps: ORACLE.01 · owner: AI
  - **Do:** `selis inspect --json` vs `qpdf --json` normalisation and comparison for object counts,
    page tree shape, xref entries, and stream lengths.
  - **DoD:** Comparator handles the known representational differences and documents each.

- [ ] **SL-0.ORACLE.04 — Text-extraction oracle** · deps: ORACLE.01 · owner: AI
  - **Do:** Compare extracted text against PDFium and pdf.js by normalised edit distance, with
    Unicode normalisation and whitespace policy defined once.
  - **DoD:** Baseline agreement between the two oracles measured and recorded first.

- [ ] **SL-0.ORACLE.05 — Triage workflow** · deps: ORACLE.02 · owner: AI+
  - **Do:** `xtask oracle triage` groups disagreements by signature, so 4 000 failures collapse to
    ~20 root causes. Each gets a verdict: `OurBug | OracleBug | SpecAmbiguous | ToleranceTooTight`,
    recorded in the expectation file.
  - **DoD:** The workflow documented in `21-TESTING-AND-ORACLES.md §5` and exercised on a seeded
    set of deliberate differences.

---

## 0.SEC — Security foundations

- [ ] **SL-0.SEC.01 — Threat model document** · owner: HUMAN
  - **Do:** Write `22-SECURITY-AND-SUPPLY-CHAIN.md §1` for real: adversary is *the document*.
    Enumerate: memory corruption, resource exhaustion, SSRF via external references, data
    exfiltration via JS/forms/embedded files, phishing via annotation appearance vs. action
    mismatch, signature-spoofing (visual signature that is not a cryptographic one), and
    shell-specific surfaces.
  - **DoD:** Reviewed; each threat maps to a control and to a test.

- [ ] **SL-0.SEC.02 — Fuzzing harness skeleton** · deps: SBX.01, IO.02 · owner: AI
  - **Do:** `cargo-fuzz` set up with a shared harness that constructs a `MemSource` + fuzz `Budget`
    and asserts: no panic, no OOM, terminates within the budget. One stub target per planned
    parser entry point.
  - **DoD:** `cargo fuzz run cos_parse` executes; the budget assertion is proven by a deliberately
    slow test input.

- [ ] **SL-0.SEC.03 — OSS-Fuzz application prepared** · deps: SEC.02 · owner: HUMAN
  - **Do:** Prepare the application for the three OSS crates (ADR-P0030). Submit as soon as the
    repos are public.

- [ ] **SL-0.SEC.04 — Security disclosure policy + `SECURITY.md`** · owner: HUMAN
  - **Do:** Publish the reporting channel, the SLA, the CVE process, and the bug-bounty posture
    (even if "not yet"). A PDF vendor without a disclosure channel gets full-disclosure instead.

---

## 0.PERF — Benchmark harness

- [ ] **SL-0.PERF.01 — Criterion harness + reference machine spec** · owner: AI
  - **Do:** `bench/` with criterion, a documented reference machine, and a stable benchmark corpus
    subset. Record baselines for the empty implementations so the first real numbers have context.
  - **DoD:** `xtask bench --compare-baseline` works and fails on a seeded regression.

- [ ] **SL-0.PERF.02 — Perf budget table wired to CI** · deps: PERF.01 · owner: AI
  - **Do:** Encode `03-CONVENTIONS.md §12` in `xtask/perf-budgets.toml`; nightly job compares.
  - **DoD:** Budgets present (most unmeasurable yet — that is fine, they fail as "not implemented",
    not as "passing").

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
