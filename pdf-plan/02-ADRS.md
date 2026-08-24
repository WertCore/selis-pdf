# 02 — Architecture Decision Records

Status values: **Accepted** (locked), **Provisional** (revisit at the named gate), **Deferred**.
An AI agent may not contradict an Accepted ADR. To change one, write a superseding ADR.

---

## ADR-P0001 — Rust core; pinned toolchain; MSRV N−2
**Status:** Accepted
**Decision:** All engine, orchestration, CLI and binding code is Rust. C/C++ appears only as
vendored third-party where no credible Rust option exists (currently: OpenJPEG for JPEG 2000,
Tesseract for OCR) and always behind a process or WASM boundary, never linked into the client core.
`rust-toolchain.toml` pins an exact stable; CI also builds `beta` and MSRV.
**Rationale:** A PDF engine is a parser for hostile input. The historical CVE record for PDF
software is dominated by memory-safety bugs in exactly the code we are writing — image codecs,
font parsers, xref recovery. Rust removes the class, and gives `cargo-fuzz`, `proptest`, `miri`,
and `cargo-mutants` for free. It is also the only language that compiles credibly to native,
WASM, and a stable C ABI for mobile from one source.
**Consequences:** JPEG 2000 and JBIG2 need care (ADR-P0018). Some OS integration needs FFI.

## ADR-P0002 — Single Cargo workspace monorepo, automated with `cargo xtask`
**Status:** Accepted
**Decision:** One repo, one workspace, one lockfile, for engine + bindings + CLI + all app shells.
Frontend TypeScript lives in `apps/*/ui` under a single pnpm workspace. All automation is Rust in
`xtask/`.
**Rationale:** The whole thesis is that product #8 is cheaper than product #1. That is only true
if a change to the edit model updates web, desktop, and mobile in one atomic commit with one test
run. Split repos guarantee version skew between the engine and six shells.
**Consequences:** Long clean builds; mitigate with `sccache` and a lockfile-keyed CI cache.
Mobile store builds need a thin satellite repo for signing secrets — the code stays here.
Format-neutral crates still publish to crates.io individually; ADR-P0034 sets the tiering and the
criteria under which one graduates to its own repo.

## ADR-P0003 — Strict, mechanically-enforced layering
**Status:** Accepted
**Decision:** The six-layer model of `01-ARCHITECTURE.md §2`, with an explicit edge allowlist in
`xtask/layers.toml`, gated by `cargo xtask check-layers` in CI.
**Rationale:** Architecture that is not enforced by a build failure decays in weeks.
**Consequences:** Friction when two domains genuinely need to share — resolve by lifting the
shared concept to L0/L1 or composing at L3, never by adding a sideways edge.

## ADR-P0004 — Synchronous engine core; async only at the edges
**Status:** Accepted
**Decision:** L0–L3 are synchronous. `tokio` appears only in `selis-pdf-service`, `cloud/`, and the
desktop helper. Parallelism inside the engine is explicit thread pools (`rayon` for tile
rasterisation) with a caller-supplied `CancelToken`.
**Rationale:** (a) Rendering is CPU-bound; async buys nothing on a rasteriser and costs a runtime
we cannot ship to WASM cheaply. (b) `wasm32-unknown-unknown` has no threads without
SharedArrayBuffer + COOP/COEP; a sync core degrades to single-threaded cleanly, an async core does
not. (c) Fuzzers, `proptest`, and `miri` are far easier against sync code.
**Consequences:** The web app must run the engine in a Worker to keep the main thread free — which
we want anyway. The service tier needs a bounded blocking pool bridging tokio → engine.
**Rejected alternative:** async-everywhere — rejected on the WASM-portability argument alone.

## ADR-P0005 — `DocSource` / `DocSink` traits; all I/O is injected
**Status:** Accepted
**Decision:** No engine code ever opens a file or issues a network request. Documents arrive as
`&dyn DocSource` (random-access, possibly partial, possibly remote) and are written to
`&mut dyn DocSink`. Read-only features are typed such that sink methods are not in scope.
**Rationale:** Makes "the viewer cannot write" a compile-time claim. Makes HTTP-range streaming,
OPFS, Android SAF, iOS security-scoped URLs, and an in-memory fuzz harness all drop-in.
**Consequences:** Plumbing. Worth it — this one decision is what lets the same core serve a browser
fetching byte ranges and a phone reading a `content://` URI.

## ADR-P0006 — Every parse runs under an explicit `Budget`
**Status:** Accepted
**Decision:** `Budget { bytes, wall, depth, objects, pixels }` is a required parameter on every
entry point that consumes document bytes. Exhaustion returns `Error::BudgetExceeded { resource,
limit }` — never a panic, never a partial-but-unmarked result. Allocation inside the engine goes
through a budget-aware allocator wrapper in `selis-sandbox`.
**Rationale:** The dominant real-world failure mode for PDF software is not a wrong pixel; it is a
crafted file that allocates 40 GB, recurses until the stack dies, or renders for six hours. A tab
that hangs is a security bug. This makes resource exhaustion a *typed, testable* outcome.
**Consequences:** Every signature is a little longer. Every fuzz target gets a free oracle:
"terminated within budget" is an assertion. Callers must choose budgets — `selis-policy` supplies
per-surface defaults (thumbnail < viewer < editor < server batch).

## ADR-P0007 — Original bytes immutable; incremental append is the default save
**Status:** Accepted
**Decision:** The loaded byte range is treated as immutable. `selis-pdf-edit` records mutations in a
journal; `save()` emits an **incremental update** — new/changed objects plus a new xref section
appended after the original bytes. Full rewrite (`save_rewritten()`) is a separate, explicitly
chosen operation that must render-verify its output before any replacement occurs.
**Rationale:** This is the document analogue of the Wertcore safety kernel. From one mechanism we
get: crash safety (a truncated append recovers to the previous revision), signature preservation
(existing signature byte ranges are untouched), cross-session undo, and a cheap revision history.
**Consequences:** Files grow on repeated save — mitigate with an explicit "optimise" action, never
an implicit one. Incremental save through an encrypted document needs care (ADR-P0019).

## ADR-P0008 — Own the PDF semantics; adopt best-in-class Rust primitives for generic problems
**Status:** Accepted
**Decision:** "Own engine" means we own everything *PDF-specific*: the COS parser, xref and
recovery, the content-stream interpreter, the graphics-state machine, the resource model, text
layout reconstruction, the edit and incremental-save model, annotations, forms, redaction,
signatures, and tagging. It does **not** mean re-deriving general-purpose graphics. We adopt
permissive Rust crates for problems that are not our moat: `tiny-skia` (path rasterisation, BSD-3),
`rustybuzz` (shaping, MIT), `ttf-parser` (font tables), `zune-*`/`image` (JPEG/PNG), `miniz_oxide`
(Flate), `qcms`/`moxcms` (ICC), `unicode-bidi` + `icu4x` (bidi/segmentation).
**Rationale:** Adobe and Foxit do not win on scanline coverage arithmetic. They win on being right
about twenty thousand pages of spec and thirty years of malformed real-world files. Rebuilding a
rasteriser costs a year and buys nothing defensible; every hour spent on PDF semantics compounds.
**Consequences:** We depend on upstreams. Mitigate: all permissively licensed and vendorable, each
behind a thin internal trait (`selis-raster::Backend`, `selis-shape::Shaper`) so a fork or replacement
is contained, each pinned and `cargo vet`ed.
**Rejected alternative:** rasteriser from scratch — reconsider only if `tiny-skia` blocks a
PDF-specific blend/soft-mask requirement we cannot express. Tracked as R6.

## ADR-P0009 — PDFium, pdf.js, MuPDF, Ghostscript and qpdf are CI oracles, never shipped
**Status:** Accepted
**Decision:** These run as separate processes in CI to produce reference output for differential
testing. No shipped artefact links, bundles, or invokes any of them. `xtask oracle` manages them in
pinned containers.
**Rationale:** The direct analogue of the Wertcore G1 rule that `wert disk list` must match
`lsblk`/`diskutil`. An oracle turns "does our renderer work?" from a subjective judgement into a CI
gate over 100k files. Licence-wise it is also the only safe posture: MuPDF and Ghostscript are
AGPL, veraPDF is GPL/MPL — usable as external CI tools, not as dependencies.
**Consequences:** CI needs containers and disk. An oracle disagreeing is not automatically our bug —
`21-TESTING-AND-ORACLES.md §5` defines triage, including "the oracle is wrong" as a legitimate
documented outcome with a corpus annotation.

## ADR-P0010 — The conformance ladder gates every feature area
**Status:** Accepted
**Decision:** No feature area advances a capability level (`Identify → Parse → Render → Extract →
Edit → Author`) until the gate criteria in `20-CONFORMANCE-PROGRAM.md §3` are met, measured on the
corpus. Marketing may not claim a level the ladder has not reached.
**Rationale:** The failure mode of every PDF product is claiming support for a feature that works
on the happy path and silently mangles the rest. The ladder makes "supported" a measurement.
**Consequences:** Some areas ship at `Render` for a long time (XFA will likely never leave
`Parse`/`Render`). That is a correct outcome, not a gap.

## ADR-P0011 — WASM is a first-class target from week 0, not a port
**Status:** Accepted
**Decision:** `wasm32-unknown-unknown` is in the CI matrix from `SL-0.WS.06`. No engine crate may
use `std::fs`, `std::net`, `std::time::Instant` (use an injected clock), threads-by-default, or any
dependency that fails to build for WASM. `xtask size-check` enforces a per-crate WASM budget.
**Rationale:** The beachhead is the web. Retrofitting WASM after twelve months of native-only work
means discovering `mmap`, thread-locals, and 64-bit assumptions everywhere at once.
**Consequences:** Native loses a couple of micro-optimisations (no `mmap` fast path in the core —
it lives behind `DocSource` in the native shell instead). Accept it.

## ADR-P0012 — Deterministic, reproducible rasterisation
**Status:** Accepted
**Decision:** No floating-point non-determinism in the raster path: fixed rounding, no `fast-math`,
no platform-specific SIMD that changes results (SIMD allowed only where proven bit-identical, gated
by a differential test), no hash-order iteration affecting draw order. `render(doc, page, params)`
is a pure function of its inputs.
**Rationale:** Rule 3. Visual regression testing, cross-platform bug reproduction, and golden-PNG
corpus assertions all collapse without it.
**Consequences:** Occasionally slower. A dedicated CI job renders the corpus on Linux x86-64,
macOS arm64, and WASM and asserts hash equality.

## ADR-P0013 — Documented failure contracts on untrusted-input functions
**Status:** Accepted
**Decision:** Every public function consuming document bytes carries rustdoc sections `# Budget`
(what it consumes, what it does on exhaustion) and `# Malformed Input` (what it does with each
class of corruption). `cargo xtask check-contracts` enforces their presence.
**Rationale:** The behaviour on bad input *is* the specification for a PDF parser. Leaving it
undocumented means every caller invents a different recovery policy.

## ADR-P0014 — One binary; entitlement sets are the products
**Status:** Accepted
**Decision:** Reader, Editor, Sign, Convert, and Scan are not separate builds. They are entitlement
sets evaluated by `selis-policy`. A feature check is `policy.allows(Feature::EditText)`, never `#[cfg]`.
**Rationale:** Same reasoning as Wertcore ADR-0032. Five builds means five test matrices and five
release trains for one codebase.
**Consequences:** The free Reader ships editor code it cannot run. Fine — the code is not a secret,
and a stripped build fragments the test matrix. Bypassing the check yields an unlicensed local
editor; an acceptable loss (see ADR-P0015).

## ADR-P0015 — Licensing degrades gracefully and never destroys work
**Status:** Accepted
**Decision:** A licence failure — expiry, revocation, clock skew, offline grace exhausted — takes
effect at the **next document-open boundary**, never mid-edit and never mid-save. An unlicensed
session can always save what is already open, and can always read. Offline grace is 30 days.
**Rationale:** The one thing worse than piracy is a paying customer losing a contract redline
because our licence server had an outage.
**Consequences:** A determined user can extend usage by not closing the app. Accepted.

## ADR-P0016 — Local-first processing; the cloud boundary is enforced in code
**Status:** Accepted
**Decision:** All parse, render, edit, extract, and save operations run on-device (native or WASM).
No document bytes, page images, or extracted text cross the network unless the user has enabled a
named cloud feature for that document. The boundary is a single chokepoint —
`selis-policy::CloudConsent` — and every network egress of document-derived data must present a
consent token. A CI test asserts that no `selis-pdf-*` crate below L4 can name a network API.
**Rationale:** This is the product's sharpest differentiator against Adobe's cloud-default posture,
and it is worth nothing if it is a promise rather than a mechanism. It also removes per-document
infrastructure cost and lets legal/healthcare/government buy without a DPA.
**Consequences:** Heavy OCR and Office conversion are harder on-device — we do them on-device
anyway (ADR-P0018) and offer cloud only as an explicit accelerator.

## ADR-P0017 — Telemetry is opt-in, aggregate, and never contains document content
**Status:** Accepted
**Decision:** Off by default. No filenames, no URLs, no page content, no extracted text, no
document hashes. Crash reports strip document buffers and sit behind a separate consent. A crash in
a parser reports the *code path*, not the bytes — unless the user explicitly attaches the file to a
support ticket.
**Rationale:** Consistent with ADR-P0016. A crash-report pipeline that ships PDF fragments is a
data-exfiltration channel and a compliance liability.
**Consequences:** Field crashes are harder to debug. Mitigate with `selis diagnose`, which produces
a *user-reviewable* redacted repro bundle the user chooses to send.

## ADR-P0018 — Codec and OCR sourcing policy
**Status:** Accepted
**Decision:** Flate/LZW/RunLength/ASCII85/ASCIIHex — own or `miniz_oxide`. DCT (JPEG) —
`zune-jpeg`. CCITT G3/G4 — own (small, and the crates are thin). **JBIG2 — own implementation**,
because the mature C implementation (`jbig2dec`) is AGPL and therefore unshippable. **JPEG 2000 —
OpenJPEG (BSD-2) behind a WASM or subprocess sandbox** in year 1, with an own-implementation task
deferred to Phase 9: JPX is rare enough in the wild not to justify a from-scratch decoder early,
and dangerous enough to justify the sandbox. OCR — Tesseract (Apache-2.0) compiled to WASM for
on-device use, wrapped by `selis-ocr`; evaluate `ocrs` (Rust) as a replacement at Phase 7.
**Rationale:** Codecs are the historical CVE epicentre. Own the small ones, sandbox the big hostile
one, refuse the copyleft one.
**Consequences:** JBIG2 is real work (Phase 2). JPX pages are slower behind the sandbox.

## ADR-P0019 — Encryption: read everything, write only modern
**Status:** Accepted
**Decision:** Read: RC4 40/128, AESV2 (128), AESV3 (256, R6), and the public-key (PKCS#7) handler.
Write: **AESV3/R6 only**. Legacy handlers are decrypt-only; a document opened as RC4 and saved is
either kept byte-identical incrementally (no re-encryption of untouched objects) or upgraded with
an explicit user prompt.
**Rationale:** Refusing to *read* legacy files makes the product useless. Continuing to *write* RC4
in 2026 makes us complicit.
**Consequences:** Incremental save into an RC4 document must encrypt new objects with the
document's existing handler to stay loadable — an explicit, tested exception, owned by HUMAN.

## ADR-P0020 — Active content is off by default and sandboxed when on
**Status:** Accepted
**Decision:** Document JavaScript, `/Launch`, `/GoToR`, `/SubmitForm`, `/ImportData`, embedded
files, and any external stream reference (`/F` on a stream) are disabled by default. JavaScript,
when enabled, runs in a memory- and time-budgeted interpreter with no filesystem, no network, and a
whitelisted subset of the Acrobat JS API (field access, calculation, formatting — not
`app.launchURL`, not `this.exportDataObject`). Form calculation scripts, the overwhelmingly common
legitimate use, work in that subset.
**Rationale:** Rule 4. Embedded JS is the single most abused PDF feature.
**Consequences:** Some enterprise forms will not fully work. Provide a clear per-document "enable
scripting" affordance and an admin policy to pre-allow signed origins.

## ADR-P0021 — Dependency licence policy
**Status:** Accepted
**Decision:** `deny.toml` allows MIT, Apache-2.0, BSD-2/3, ISC, Zlib, Unicode-3.0, MPL-2.0
(file-level, case by case), SIL-OFL-1.1 (fonts only). Denies GPL, LGPL, AGPL, SSPL, CDDL in
anything shipped. `unmaintained = "deny"`, `wildcards = "deny"`, crates.io only.
**Rationale:** We ship a proprietary product on five platforms including two app stores. LGPL
dynamic-linking arguments do not survive an iOS static-link requirement.
**Consequences:** Rules out `jbig2dec`, MuPDF, Ghostscript, veraPDF as libraries. All remain usable
as CI oracles (ADR-P0009).

## ADR-P0022 — Web and desktop share one TypeScript UI; mobile is native
**Status:** Accepted
**Decision:** `apps/web/ui` is the single React/TypeScript application. The desktop shell (Tauri v2)
renders the same UI behind a different platform adapter. iOS (SwiftUI) and Android (Compose) are
**native**, sharing only the engine via the C ABI.
**Rationale:** Web and desktop have the same interaction model, screen sizes, and users — one UI is
a genuine 2× saving. Mobile does not: a webview cannot deliver 120 Hz pinch-zoom over a rendered
page, and the integrations that matter on mobile (share sheet, Files/SAF, camera, stylus pressure,
widgets) are native surfaces.
**Consequences:** Two mobile UIs to build and maintain. Mitigate by keeping mobile scope narrow
(view, annotate, fill, sign, scan) and pushing all logic below the FFI.

## ADR-P0023 — Redaction is a removal operation with a proof obligation
**Status:** Accepted
**Decision:** `selis-pdf-redact` rewrites content streams to delete covered glyph-show operators and
re-encodes covered image regions; strips covered annotations, form fields, structure-tree nodes,
optional-content groups, embedded files, and metadata; then **re-opens the produced bytes and runs
an independent extraction + render pass** asserting nothing survives in the redacted region. If
verification fails, `save()` returns an error and produces no file.
**Rationale:** Rule 5, and SL-0.LEAD.12. Overlay-only "redaction" is the most consequential
correctness bug in this product category.
**Consequences:** Redaction is slower and forces a full rewrite — it is the one operation whose
original bytes must *not* survive. This exception is explicitly carved out of ADR-P0007 and is the
reason `save_rewritten()` exists.

## ADR-P0024 — Text editing reconstructs paragraphs; it never edits raw operators in place
**Status:** Accepted
**Decision:** "Edit text" runs a reconstruction pipeline (glyphs → runs → lines → paragraphs → a
styled text model), edits the model, then **re-lays out and re-emits** the affected content region.
Reconstruction confidence is computed and surfaced; below a threshold the UI offers box-level
editing instead of paragraph reflow, rather than silently producing a mangled page. See
`23-EDIT-MODEL-SPEC.md §4`.
**Rationale:** This is the marquee feature of Acrobat Pro and Foxit PDF Editor, and the one where
everyone else's product visibly breaks. Being honest about confidence is a feature.
**Consequences:** A hard problem, correctly framed. Requires the font engine to subset and embed a
*modified* font (Phase 3), because newly typed characters may not exist in the original subset.

## ADR-P0025 — Page cache and tile rendering are engine concerns, not UI concerns
**Status:** Accepted
**Decision:** `selis-pdf-engine` owns a memory-budgeted cache of parsed pages, display lists, and
rendered tiles, keyed by (page, matrix, params) with LRU eviction against a configured budget. All
shells ask the engine for tiles; no shell implements its own caching.
**Rationale:** Otherwise all six shells reinvent it, badly, and mobile OOMs.

## ADR-P0026 — Annotations are standard PDF annotations, not a private sidecar
**Status:** Accepted
**Decision:** All markup is written as spec-conformant annotations with generated appearance
streams, readable by Acrobat and every other viewer. Collaboration state (threads, resolution,
presence) may live in a sidecar, but the annotation itself is always in the PDF.
**Rationale:** Lock-in via proprietary annotation storage is the thing users hate about incumbents.
Interoperability is the wedge.
**Consequences:** Some collaboration metadata round-trips imperfectly through other viewers.
Accepted — the markup itself never does.

## ADR-P0027 — Collaboration uses CRDTs over an annotation-op log
**Status:** Provisional (revisit at G8)
**Decision:** Real-time co-annotation replicates a CRDT of annotation operations, materialised into
PDF annotations on save. The document body is *not* collaboratively edited in v1 — concurrent
content editing is last-writer-wins at page granularity with an explicit conflict UI.
**Rationale:** Co-annotation is the demanded feature and is naturally commutative. Concurrent
content editing of a page description has no good merge semantics and is not worth the complexity
before there is demand.

## ADR-P0028 — MV3 extension: bundled viewer page + offscreen document; no remote code
**Status:** Accepted
**Decision:** The extension registers a `declarativeNetRequest` redirect from `application/pdf`
navigations to a bundled viewer page, runs the WASM engine in an offscreen document / worker, and
ships **all** code in the package. No remote script, no CDN WASM fetch, no `eval`.
**Rationale:** Chrome Web Store policy forbids remotely-hosted code in MV3, and review punishes
anything that resembles it. Bundling also makes the extension work offline and keeps ADR-P0016's
privacy claim trivially true.
**Consequences:** The package carries the WASM payload, so `size-check` budgets are tighter there
than on the web app. `file://` PDFs require the user to grant file access — handle that permission
flow explicitly (`SL-4.EXT.09`).

## ADR-P0029 — The Wertcore platform substrate is shared, not reimplemented
**Status:** Provisional (revisit at G6)
**Decision:** Licensing/entitlement service, account, update (TUF), crash reporting, telemetry
consent, the code-signing pipeline, and installer/notarisation tooling are **the same systems**
`wertcore-plan/` builds, consumed by Selis rather than rebuilt. Selis owns its document-domain
code; it does not own a second updater.
**Rationale:** These are the undifferentiated 30% of any desktop software company, already planned
and budgeted once. Two implementations means two security surfaces to patch.
**Consequences:** A coupling between two product lines — manage it as a versioned internal platform
API, not a shared branch. If the two are legally separate entities, this becomes a licence
agreement and this ADR is superseded.
**Revisit trigger:** if Selis reaches G6 before Wertcore reaches its G4, Selis builds a minimal
updater itself rather than block.

## ADR-P0030 — Open-source the parser crates
**Status:** Accepted
**Decision:** `selis-pdf-cos`, `selis-pdf-filter`, and `selis-font` are published under Apache-2.0 as standalone
crates. The renderer, edit model, redaction, signing, and all product code remain proprietary.
**Rationale:** Three concrete returns: free OSS-Fuzz capacity on precisely the crates that need it
most (SL-0.LEAD.09), external eyes on the highest-CVE-risk code, and credibility with the developer
audience the SDK product must sell to. Nothing defensible leaks — the moat is the renderer's
correctness and the edit model, not the ability to read an xref table.
**Consequences:** A public issue tracker and API-stability obligations on those three crates.
Budget maintenance time from Phase 1, not as an afterthought.

## ADR-P0031 — Accessibility is built in, not a compliance bolt-on
**Status:** Accepted
**Decision:** Tagged-PDF structure is first-class in the document model from Phase 1 (`selis-pdf-doc`
carries the structure tree), the viewer exposes it to assistive technology on every platform, and
every authoring path (convert, OCR, edit) emits tags. PDF/UA validation runs in CI against veraPDF.
**Rationale:** PDF/UA compliance is a purchasing requirement for government and education, it is
the fastest-growing regulatory obligation in this category, and retrofitting a structure tree into
a renderer built without one is a rewrite.
**Consequences:** More work per feature. Also a differentiator that is nearly impossible for a
competitor to add late.

## ADR-P0032 — No feature ships without a corpus entry
**Status:** Accepted
**Decision:** Every task that changes engine behaviour adds at least one corpus file exercising it,
with an expected-output record (golden hash, extraction text, or oracle-comparison tolerance). The
corpus is the regression suite; `xtask corpus verify` is a required CI stage.
**Rationale:** Wertcore's equivalent is the hardware lab. Ours is far cheaper and must therefore be
used ruthlessly.

## ADR-P0033 — Server-side rendering exists only as a product, never as a fallback
**Status:** Accepted
**Decision:** `selis-pdf-service` (the Server/API SKU) is a deliberate, separately-sold product. No client
ever falls back to server rendering because the local engine failed. A local failure is a bug to
fix, surfaced as a bug.
**Rationale:** A silent server fallback would make ADR-P0016's privacy claim false in exactly the
cases users care about (weird documents), and would hide engine defects behind a network call.

## ADR-P0034 — Naming and identifier set
**Status:** Accepted — name Selis; monorepo settled. Trademark clearance still open at SL-0.LEAD.01.
**Decision:** **Selis.** Identifier set in the "Adopted identifiers" section below.
Status is Provisional only because `SL-0.LEAD.01` (trademark clearance) has not returned.

### Names ruled out (prior use found, August 2026)

| Candidate | Why rejected |
|---|---|
| **Syndex** | Crowded and contested **in software**: SYNDEX registered by BriefCam Ltd (video-surveillance software) and by Real Liquidity Inc (computer software); also SYNDEX by Medline Industries (medical devices), SynDEx the INRIA system-level CAD tool, Syndex Australia (investor management), and Groupe Syndex (EU labour consultancy). Registered marks in class 9 make this the worst option on the list. |
| **Lipi** (Sanskrit, "script") | Lipi Data Systems Ltd — an Indian company making **printers**, i.e. directly adjacent. Plus Shree-Lipi, Indian multilingual typing software shipping since 1993. Two collisions in the same market and region. |
| **Codex** | OpenAI Codex. |
| **Tabula** | An existing PDF table-extraction tool — same product space. |
| **Papyrus** | A widely-shipped font, plus Eclipse Papyrus (UML). |
| **Vellum** | Established book-production software. |
| **Kanon / Canon** | Canon Inc., adjacent classes (imaging, printing). |

### Shortlist

| Name | Language | Meaning | Say it | Notes |
|---|---|---|---|---|
| **Selis** | Greek (σελίς) | *page; column of writing* | SEH-lis | No software collision found. Short, spells itself, works in every language. `selis` / `sel-` / `selis_`. **Recommended.** |
| **Sthira** | Sanskrit (स्थिर) | *fixed, stable, permanent* | STHEE-ra | No collision found. Best meaning on the list — a fixed-layout format built for preservation. Cost: the `sth-` cluster is awkward for English speakers. |
| **Pliego** | Spanish | *a printed sheet folded into pages* — the bookbinding term | plee-EH-go | Strongest metaphor: the folded sheet *is* the page. Three syllables; non-Spanish speakers will mispronounce it. |
| **Satzwerk** | German | *typesetting works* (`Satz` = composition, the trade term) | ZATS-verk | Most distinctive, near-zero collision risk, obviously brandable. Cost: long, and unreadable outside German-speaking markets. |
| **Stele** | Greek (στήλη) | *an inscribed slab, made to outlast its author* | STEE-lee | Beautiful archival resonance; pronunciation is ambiguous in English ("steel"?). |
| **Akshara** | Sanskrit (अक्षर) | *imperishable; a letter of the alphabet* | UK-sha-ra | Excellent meaning. Fairly common in Indian company naming — clearance risk is higher. |

**Recommended:** **Selis** for the engine. Short, unambiguous to pronounce and spell anywhere,
semantically exact (it means *page*), and nothing was found in software using it.
**Sthira** is the strong second if the deeper meaning is worth the harder pronunciation.

### Naming architecture — never put "PDF" in the master brand

**Decision:** the master brand is **format-neutral**. "PDF" is the name of a *product line* under
it, never part of the company, engine, or trademark.

```
<Brand>                     ← company · engine · trademark · domain
├── <Brand> PDF             ← today            (crates parna-*, npm @parna/pdf)
├── <Brand> Words           ← .docx, later     (@parna/words)
├── <Brand> Sheets          ← .xlsx, later     (@parna/sheets)
└── <Brand> Slides          ← .pptx, later     (@parna/slides)
```

This is the Aspose structure (`Aspose.PDF`, `Aspose.Words`, `Aspose.Cells`, `Aspose.Slides`) —
a neutral brand with format-named product lines — and it is what makes multi-format expansion a
product launch instead of a rebrand.

**Evidence this matters:** PDFTron Systems rebranded to **Apryse** in February 2023 precisely
because the name had become limiting as the company expanded past PDF into general document
processing. A well-funded competitor paid for a full corporate rebrand to escape exactly the
mistake `<Brand>PDF` would bake in on day one. Do not repeat it.

**Corollary:** reject every hybrid of the form `<Root>PDF` (e.g. `SandhiPDF`). The suffix belongs
on the product, not the mark.

### Corollary — name the domain, not the operation

Reject candidates that name a single *feature*. Merging, unlocking, and creating are three of
roughly forty operations; a brand meaning "joining" or "liberation" cannot stretch over the other
thirty-seven, and stretches even worse to a spreadsheet engine.

| Rejected | Meaning | Names the operation |
|---|---|---|
| Sandhi, Yuta, Yojan, Samyoga | union / joining | merge only |
| Moksha, Mukti | liberation / release | unlock only |
| Kriti | creation | authoring only |

Additional problems found on those: **Yojan** reads as *yojana*, the standard Hindi word for a
government scheme — a poor association in the target market. **Moksha** and **Mukti** are
religiously and politically loaded and heavily used by wellness and NGO brands. **Kriti** and
**Yuta** are extremely common given names (Indian and Japanese respectively) — unusable signal.

### Corollary — avoid `-ex` / `-ix` suffixes

`Patrex`, `Parnex`, `Selix` all read as pharmaceutical. This is not a hunch: the only registered
**SELIX** mark found was Forest Laboratories' antidepressant (US serial 76280088, now
dead/abandoned since 2003 — so the mark is technically free, but the phonetic association is not).
`-Flow` and `-kit` suffixes (`PatraFlow`, `Patrakit`) are generic and date quickly.

### Prior-use findings, August 2026

| Candidate | Finding | Verdict |
|---|---|---|
| **Parna** (Sanskrit पर्ण, *leaf, sheet*) | No software collision found. | **Clear — recommended** |
| **Selis** (Greek σελίς, *page*) | No software collision found. | Clear |
| **Stele** (Greek στήλη, *inscribed slab*) | No software collision found. | Clear; pronunciation ambiguous in English |
| **Patra** (Sanskrit पत्र, *leaf, page, document*) | **Patra Corp** — established insurance-technology company (founded 2005) shipping cloud software and AI automation. | High risk |
| **Selix** | Dead pharmaceutical mark; phonetically pharma. | Weak |
| **Pustaka** (*book*) | Everyday word for "book" across Indian languages — descriptive, and descriptive marks draw objections. | Weak |
| **Syndex** | Registered software marks (BriefCam, Real Liquidity) + INRIA SynDEx + three more entities. | **Rejected** |
| **Lipi** | Lipi Data Systems (Indian **printer** manufacturer) + Shree-Lipi (Indian typing software since 1993). | **Rejected** |

### Adopted identifiers

**Selis** (Greek σελίς, *page*). Brand is format-neutral; the format lives in the package name —
the Aspose structure, and the thing PDFTron's rebrand to Apryse proves you want.

| | Value |
|---|---|
| Brand / org / trademark | `Selis` |
| CLI binary | `selis` — format is a subcommand group, so `selis merge` today and `selis docx convert` later |
| **Format-neutral crates** | `selis-error` `selis-bytes` `selis-geom` `selis-color` `selis-log` `selis-io` `selis-sandbox` `selis-crypto` `selis-raster` `selis-shape` `selis-font` `selis-image` `selis-ocr` `selis-policy` `selis-job` |
| **PDF-engine crates** | `selis-pdf-cos` `selis-pdf-filter` `selis-pdf-doc` `selis-pdf-content` `selis-pdf-text` `selis-pdf-annot` `selis-pdf-form` `selis-pdf-edit` `selis-pdf-redact` `selis-pdf-sign` `selis-pdf-a11y` `selis-pdf-optimize` `selis-pdf-js` `selis-pdf-convert` `selis-pdf-engine` `selis-pdf-wasm` `selis-pdf-ffi` |
| Future engines | `selis-docx-*`, `selis-xlsx-*`, `selis-pptx-*` — reusing the whole `selis-*` substrate |
| C ABI | `selis_pdf_` (e.g. `selis_pdf_doc_open`), header `selis_pdf.h` |
| npm | `@selis/pdf`, later `@selis/words`, `@selis/sheets` |
| Session bundle | `.selis` |
| Task ID prefix | `SL-<phase>.<area>.<n>` |

### Why the neutral/PDF crate split matters

This is not cosmetic. Roughly **half the engine is not PDF-specific**: the error registry, the byte
and geometry primitives, the budget kernel, `DocSource`/`DocSink`, the crypto layer, the
rasteriser, the shaper, the font-program parsers, the image codecs, the OCR pipeline, the policy
and job models. A `.docx` engine would reuse every one of them unchanged.

Splitting the namespaces now — rather than after the second format arrives — means the shared
substrate is *forced* to stay format-agnostic, because a `selis-pdf-*` dependency inside a
`selis-*` crate is a layering violation `xtask check-layers` will reject. The multi-format future
becomes an additive product launch instead of an extraction project.

Two boundaries to watch, recorded so they are not blurred later:
- **Fonts.** `selis-font` owns font *programs* (TrueType, CFF, Type1) — neutral. PDF font
  *dictionaries*, encodings, and `ToUnicode` are PDF constructs and live in `selis-pdf-doc`.
- **Images.** `selis-image` owns codecs — neutral. PDF Image XObjects, `/Decode` arrays, SMasks
  and stencil masks live in `selis-pdf-content`.

### Repository structure — publish widely, split narrowly

**Settled: single workspace.** Graduation to separate repos is deferred, and is governed by
the per-crate criteria below rather than by a single "once everything works" milestone —
reviewed at `SL-9.PLAT.06`.

Reusability is delivered by **publishing**, not by repo topology. A crate published to crates.io
from a monorepo is byte-identical in reusability to one published from its own repo — Cargo
resolves `selis-raster = "1.2"` the same either way, and a consumer never learns where it was
built. This is the standard Rust pattern: `tokio` ships ~10 crates, `bevy` ~40, `serde`, `axum`,
and `rust-analyzer` likewise — all from single repos, all freely reused.

So the two goals decouple cleanly:

| Goal | Delivered by |
|---|---|
| Reusable by other products | Publishing to crates.io + semver discipline |
| Independently maintainable | A named owner and its own CI job |
| Independent release cadence | A separate repo — **and only this needs one** |

Only the third genuinely requires a repo split, and it is the one that matters least before 1.0.

### The reuse axis is not the format axis

`selis-error` is format-neutral and would be reused by a `.docx` engine — but no outside project
wants Selis's error-code registry. `selis-font` is equally neutral and has a large external
audience. Format-neutrality predicts *internal* reuse; it does not predict *external* reuse. The
neutral crates therefore split again:

**Tier A — real external audience; eventual graduation candidates**
`selis-font` (vs `ttf-parser`, `read-fonts`) · `selis-shape` (vs `rustybuzz`) ·
`selis-raster` (vs `tiny-skia`) · `selis-image` (vs `image`) · `selis-ocr`

These are recognisable standalone libraries someone would depend on without ever touching Selis.

**Tier B — format-neutral but Selis-shaped; stay in the workspace permanently**
`selis-error` `selis-bytes` `selis-geom` `selis-color` `selis-log` `selis-io` `selis-sandbox`
`selis-policy` `selis-job`

These encode Selis's own conventions — the error registry, the `DocSource` abstraction, the budget
kernel, the job model. They have essentially no external audience, they are coupled to everything
above them, and they churn the most. Splitting them buys nothing and costs a release dance on
every change.

### Graduation criteria

A Tier A crate moves to its own repo when **all four** hold:

1. It is 1.0 with no breaking change for two consecutive quarters.
2. It has at least one production consumer outside the Selis workspace.
3. It has a named maintainer who owns its issue tracker.
4. Its CI runs green with no Selis-private fixture or corpus.

Until then it lives in the workspace and publishes from there.

### Why the default is "start together"

The cost of splitting is highest in exactly the phase Selis is entering. During Phases 0–4 the
error registry, `DocSource`/`DocSink`, the budget kernel, and the display-list IR are all still
moving, and there is exactly **one** consumer of them. One consumer plus high churn is the worst
possible ratio for split repos: every substrate change becomes PR → release → version bump → PR →
release, multiplied by the number of repos, with no reuse benefit yet earned. That ratio inverts —
and the split starts paying — only once churn drops and consumers multiply, which is precisely what
criteria 1 and 2 above measure.

Two capabilities are also lost outright by splitting early:

- **`xtask check-layers`** enforces the ADR-P0003 layering mechanically, and can only see crates
  inside one workspace. Across repos, layering degrades from a compile-time gate to a review habit.
- **The conformance corpus** (`20-CONFORMANCE-PROGRAM.md`) runs the full stack against reference
  files. Split repos need either a fixture-mirroring scheme or a meta-repo to reproduce it.

### The decision is reversible, and the reversal is invisible

This is what makes "start together" the low-risk default rather than a bet.
`git filter-repo --subdirectory-filter crates/selis-font` extracts a crate with its full history
into a new repo, and because the crates.io name never changes, **no downstream consumer observes
the move** — not a version bump, not a re-import. Splitting later is a mechanical afternoon.

The reverse is not true. Starting split and re-merging means reconciling divergent CI, lockfiles,
and release histories, after having already paid the coordination tax through the churn phase.
Asymmetric reversibility decides it: begin in the workspace, graduate on evidence.

### Considered and rejected — git submodules

Submodules work mechanically: a submodule crate can be listed in the parent's
`[workspace] members`, so one lockfile and `xtask check-layers` both survive. They are still the
wrong tool here, because the property they are reached for is the one they do not provide.

**They do not restore atomicity.** Changing `selis-font` and its PDF consumer together is:
commit in the child → push the child → bump the recorded SHA in the parent → commit → push. Two
ordered PRs, and the parent's CI cannot see the child's change until it is already pushed. That is
the same coordination dance as separate versioned repos, with a SHA substituted for a semver range.

**The substitution is a downgrade for the stated goal.** A SHA tells a consumer nothing about
compatibility, and external consumers cannot use a submodule at all — they use crates.io. So
submodules contribute exactly nothing to reusability, which was the reason for splitting.

**They add real cost.** Detached-HEAD child checkouts, `clone` without `--recursive` yielding empty
directories, `pull` not recursing by default, permanent `modified: (new commits)` noise in the
parent's status, painful bisection across the boundary, and more complex CI caching. Plus a
`[workspace]`-inside-`[workspace]` conflict: the child needs its own workspace root for standalone
CI, which then collides when the parent includes it as a member. Workable, but it is friction paid
on every clone by every contributor.

**If the split does happen, the idiomatic mechanism is not submodules.** It is separate repos
publishing to crates.io, with a `[patch.crates-io]` or `paths` override in `.cargo/config.toml` for
local co-development. That yields independent repos *and* a single local build tree, without any
submodule mechanics.

Ranked: monorepo + publish (preferred) > split repos + `[patch]` override > submodules, which are
dominated by the middle option on every axis.

**Where submodules would have fitted — already solved better.** The two natural candidates are the
conformance corpus and the oracle binaries. The corpus is pinned by content hash
(`20-CONFORMANCE-PROGRAM.md §6`), which gives reproducibility without importing licence-encumbered
files into git history; the PDFium / pdf.js / qpdf oracles are pinned as CI container digests,
which pins *built* artefacts rather than source that would still need building. Both are stronger
than a SHA pointer. No submodule use case remains in this plan.

### Engine name vs product name
One name covering both (the current `selis` model) needs one trademark clearance and is cheaper.
Splitting them — technical engine brand plus a consumer product brand, as with PDFium/Chrome or
WebKit/Safari — is worth revisiting **only at Phase 9**, when the SDK becomes a product sold to
developers and a distinct engine brand starts carrying commercial weight. Do not pay for two
clearances in year one.

**Consequences:** Do not publish a public crate before clearance closes — an abandoned crates.io
name is permanent. Every user-visible string goes through i18n keys from day 1 so a rename is a
resource change, not a code change. A shortlist entry is not cleared until `SL-0.LEAD.01` returns
a written opinion; the searches above are prior-use signal, not a clearance.
