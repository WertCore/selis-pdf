# 28 — First Release Scope and the Exposure Model

**Verified: August 2026.** Supersedes a withdrawn earlier revision of this file (see §1).
Read with `27-COST-AND-LICENSING.md`.

---

## 1. Correction: the engine is owned. Recorded so it is not re-proposed.

An earlier revision of this file proposed a "Track A" — shipping the toolkit quickly by gluing
together permissive JavaScript/WASM libraries (`pdf-lib`, `qpdf-wasm`, `pdfcpu`, `pdf.js`) with the
owned Rust engine relegated to a slower parallel track.

**That is rescinded.** It contradicts ADR-P0008, and the decision stands: the engine is written
from scratch, and no third-party PDF implementation ships in any product. PDFium, pdf.js, qpdf,
pdfcpu, MuPDF and Ghostscript remain **CI oracles only** (ADR-P0009) — they exist to tell us when
our output is wrong, never to produce output we hand to a user.

The concern that motivated Track A was real and does not go away: `ihatepdf.cv` already ships 46
client-side tools, and time-to-first-release matters. **The resolution is not to borrow an engine —
it is that the owned engine can ship the toolkit at month 4 anyway.** See §4.

---

## 2. The competitive finding (retained)

`ihatepdf.cv` ships **46 tools, entirely client-side, free, no watermark, no sign-up, no daily
limit, no upload**, with a ~150 MB file cap.

That is the positioning this plan was built on. It is no longer differentiating. Three wedges hold:

| Wedge | Why it holds |
|---|---|
| **Browser extension** | They appear web-only (verify). An extension that replaces Chrome's built-in viewer and puts tools one click from any PDF is a different channel, a stickier install, and the highest-intent moment in the funnel. **Now the primary differentiator.** |
| **Provable correctness** | 46 tools assembled from off-the-shelf libraries will break on malformed files, CJK, tagged PDFs, forms, and encrypted documents. `20-CONFORMANCE-PROGRAM.md` — published per-feature evidence — is something a feature-count competitor structurally cannot match. This is precisely what owning the engine buys. |
| **Documents that break everyone else** | Their ~150 MB cap is a WebAssembly memory ceiling, not a preference. 500 MB documents, 5 000-page reports, damaged files, and streaming over HTTP range requests (`SL-1.COS.07`) are where an owned engine wins visibly. |

---

## 3. The exposure model — what licensing actually attaches to

Your reasoning: *"for the first phase there won't be a requirement for the paid licences, though
the core will have the functionality — it just won't be exposed."*

Substantially correct. Here is the precise version, because the three obligations attach at three
different moments:

| Obligation | Attaches at | Consequence for the first release |
|---|---|---|
| **Specifications** (ISO 19005 / PDF/A, ISO 14289 / PDF/UA) | **Build time.** You need the spec to write the code, not to expose it. | Free. Because those features are **not built** until Phase 9 — not because they are built and hidden. See the refinement below. |
| **Code-signing certificates, store accounts** | **Distribution time.** Attaches to the artefact you hand a user. | $5 (Chrome Web Store). No Apple, no Windows signing, because no desktop or mobile binary ships. |
| **Inbound dependency licences** | **Linking time.** | **Zero.** The engine is entirely your own code. There is no inbound PDF licence at all. |

### The one refinement

"The core will have the functionality, it just won't be exposed" does not save you the spec cost —
it defers it only if the functionality genuinely **is not written yet**. If the engine actually
implements a PDF/A validator, you needed ISO 19005 to write it correctly, exposed or not.

The conclusion is the same (no paid specs before Phase 9), but the reason matters: **build later,
not build-now-and-hide.** Writing a compliance feature from guesswork because the spec costs
CHF 200 produces a feature that fails audit, which is worse than not having it.

### Three real costs of shipping unexposed code

ADR-P0014 says one binary, entitlements are the products — the free tier ships code it cannot run.
That is correct on desktop. On the web it has three consequences worth naming:

1. **WASM payload.** The viewer budget is 3 MB brotli (`03-CONVENTIONS.md §12`). Unexposed code
   still occupies it. This is why `SL-4.WASM.02` mandates code splitting into separate modules —
   "not exposed" must mean **not in the shipped chunk**, not merely hidden behind a UI flag.
   A feature hidden in the UI but present in the bundle costs you first-paint time on every load.
2. **Attack surface.** Unexposed parser code is still reachable by a crafted document if any code
   path touches it. It must still be fuzzed, still budgeted, still in the corpus. Shipping it
   untested because "nobody can click it" is how a CVE happens.
3. **Conformance honesty.** Code that exists but is unproven must sit at its measured ladder level
   (`20-CONFORMANCE-PROGRAM.md`), not at the level you intend it to reach. Half-finished unexposed
   features do not get to claim a level.

**Practical rule:** unexposed functionality belongs in a lazily-loaded chunk that the first release
never downloads, gated by `selis-policy` **and** by the build. Not `if (feature.enabled)`.

---

## 4. The owned-engine fast path — first release at month 4

This is not a compromise; it is what `11A-PHASE-1A-tools.md` already specifies. The key structural
fact bears repeating:

> **Merge, split, page operations, unlock, protect, compress and images→PDF require no renderer.**
> They are object-graph operations over the COS layer, the document model, and a writer.
> They need Phase 0 + Phase 1 + Phase 1A. They do **not** need Phase 2 (render), Phase 3 (fonts),
> or Phase 4 (viewer).

| Phase | Weeks | What it buys |
|---|---|---|
| **Phase 0** — foundation, budget kernel, corpus, oracles | 0–3 | The harness. Do not skip it; it is what makes the correctness wedge real. |
| **Phase 1** — COS, xref, filters, encryption, document model | 4–9 | Parse anything. Encryption read/write is here, which is what unlock and protect need. |
| **Phase 1A** — writer + the tools + extension surface | 10–16 | **Ship. Month 4. Procurement cost $5.** |

Then Phase 2/3/4 add the viewer (month 7), Phase 5 the editor (month 12) — each release widening
the exposed surface of the same engine.

### What to cut from the 46-tool list

Do not clone all of it. Roughly a third is filler (POS Billing, Collaborative Whiteboard, Audio to
PDF) that dilutes the product and the test matrix. Ship the ones with real search demand that the
Phase 1A engine can do correctly: merge, split, organize pages, rotate, compress, unlock, protect,
images→PDF, metadata, repair. Ten tools done provably right beats forty done approximately.

Add one that nobody does well and that the owned engine makes cheap:

- **Privacy Risk Scanner** (`SL-1A.TOOL.09` extended) — report metadata and author trails, **prior
  incremental revisions still containing "removed" content**, hidden optional-content layers,
  embedded files, embedded JavaScript, external references that would phone home, and text hidden
  under images. Every one of those is a direct read off the Phase 1 document model, so it is nearly
  free to build, and it is a feature a glue-code competitor cannot easily replicate because it
  requires the revision chain (`SL-1.COS.05`) to be first-class.

---

## 5. What is genuinely hard — plan around these, do not promise them early

| Feature | Why | Recommendation |
|---|---|---|
| **PDF → Word / Excel / PowerPoint** | Requires reconstructing paragraphs, tables and styles from positioned glyphs — the ADR-P0024 problem. **Top-3 by search volume and hardest on the list.** | Wait for `selis-pdf-text` to reach `Extract` (Phase 3). Ship text-and-basic-layout then, labelled honestly. Do not claim fidelity you do not have. |
| **Word / PPT → PDF** | Real fidelity means implementing OOXML layout. A large project in its own right. | Phase 5 `SL-5.CONV` at the earliest. Consider never — it is a different product. |
| **Edit PDF text** | The marquee feature; the one everyone's product visibly breaks. | Phase 5, with the confidence banding of `23-EDIT-MODEL-SPEC.md §4`. Ship text boxes and whiteout earlier — useful and honest. |
| **Redact** | Must be removal, not a black rectangle. A drawn rect leaves text extractable underneath. | ADR-P0023's removal + verification, or nothing. **Never ship an overlay and call it redaction** — that is a liability, not a feature, and it is the one place where being slower than a competitor is correct. |
| **Compress, competitively** | Best gains need image recompression and font subsetting; subsetting is `SL-3.FONT.11`. | Ship lossless (object streams, dedup, GC) at Phase 1A; lossy image tier at Phase 2; font subsetting at Phase 3. |
| **>150 MB documents** | Where you can beat the competitor — but only via `SL-1.COS.07` resumable parsing and streaming. | Build `SL-1.COS.07` properly in Phase 1. It is the task most likely to be deferred and most expensive to retrofit. |

---

## 6. AI features conflict with local-first — decide deliberately

"Chat with PDF" and "AI Summarizer" need a language model. Two options, trading directly against
the positioning:

- **Cloud API** — good quality, small payload, but document text leaves the device. That breaks the
  single claim the product is built on. If ever done, it must be explicit per-document consent with
  a visible indicator (`SL-8.BOUND.01/03`), never a default.
- **On-device** (`transformers.js`, WebLLM) — keeps the promise, zero marginal cost, but a
  multi-hundred-MB model download and slow inference on average hardware.

**Recommendation:** on-device, behind an explicit "download the model" step, marketed as the only
private AI PDF tool. Slower is a fair trade for being the only one that can honestly say the
document never left the browser — and if a competitor routes documents to an API while claiming
privacy, that is a checkable comparison worth making loudly.

Note this is the one area where using someone else's work is *not* an ADR-P0008 violation: a
language model is not a PDF engine. Using an open-weights model is analogous to using `tiny-skia`
for rasterisation — a generic component, not the moat.

---

## 7. Consequences for the rest of the plan

| Document | Status |
|---|---|
| `11A-PHASE-1A-tools.md` | **Confirmed as the first release.** Month 4, owned engine, $5 procurement. |
| `10`–`19` | Unchanged, and no longer demoted. The engine plan *is* the plan. |
| `26-PRODUCT-SURFACE.md` | Corrected — local-first is table stakes; extension + provable correctness are the wedges. |
| `25-RISK-REGISTER.md` | R10 rewritten (competitor materialised). R16 rewritten (see below). |
| `27-COST-AND-LICENSING.md` | Reinforced by §3 here. Add the build-time/distribution-time distinction to its §2 framing. |
| ADR-P0008, ADR-P0009 | **Reaffirmed.** No third-party PDF implementation ships. Oracles are for CI. |
