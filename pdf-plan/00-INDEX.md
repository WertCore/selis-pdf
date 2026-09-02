# Selis — PDF Platform Engineering Execution Plan

> **Planning date:** August 2026.
> Companion to `wertcore-plan/` — same method, different domain. That plan builds a systems-software
> platform; this one builds a **document platform** covering the product surface of Adobe Acrobat,
> Foxit, Nitro, PDF-XChange, and Smallpdf — on one Rust engine exposed through web, extension,
> desktop, mobile, CLI, SDK, and an opt-in cloud.
>
> **Selis is the product family; the shared engine carries its name; every product is an
> entitlement set over one core.** Identifiers (locked by ADR-P0034, pending trademark clearance):
> org `WertCore` · repo `selis-pdf` · crates `selis-*` (format-neutral) and `selis-pdf-*` (PDF engine) ·
> CLI binary `selis` · C ABI prefix `selis_pdf_` · session bundle `.selis` ·
> product family **Selis Reader / Editor / Sign / Convert / Scan / Server / SDK**.

---

> ### Sequencing note — August 2026
> A competitor (`ihatepdf.cv`) already ships 46 client-side tools with this plan's intended
> "local-first" positioning. That wedge is gone; the extension channel and provable correctness
> replace it. **Read `28-FIRST-RELEASE-SCOPE.md` before `10-PHASE-0-foundation.md`.**
> The engine remains fully owned (ADR-P0008 reaffirmed) — the first release is Phase 0 → 1 → 1A,
> the toolkit at **month 4**, because merge/split/unlock/compress need no renderer.

---

## 0. How to use this plan with an AI coding agent

Every task has a stable ID: `RC-<phase>.<area>.<n>` (e.g. `SL-1.COS.04`).

### The agent prompt template

```text
You are implementing task <TASK-ID> from the Selis PDF platform execution plan.

Read, in order:
  1. pdf-plan/03-CONVENTIONS.md         (non-negotiable coding rules)
  2. pdf-plan/01-ARCHITECTURE.md        (layering + crate boundaries)
  3. pdf-plan/02-ADRS.md                (the decisions you may NOT relitigate)
  4. The phase file containing <TASK-ID>
  5. Any spec file the task references (23-EDIT-MODEL-SPEC.md, 24-BINDINGS-SPEC.md, ...)

Rules:
  - Do not add a dependency that is not in workspace [workspace.dependencies].
    If you need one, stop and propose it with a licence + maintenance justification.
    Copyleft (GPL/AGPL/SSPL) is a hard no in shipped crates — see ADR-P0021.
  - Do not cross a layer boundary. `cargo xtask check-layers` must pass.
  - Do not parse untrusted bytes outside a `Budget` (ADR-P0006).
  - Do not mutate the original document buffer. Ever. (ADR-P0007)
  - Every public item gets a rustdoc comment. Every function that consumes untrusted
    input gets `# Budget` and `# Malformed Input` doc sections (ADR-P0013).
  - Produce the tests listed under "DoD" in the same change, including the corpus
    entries. A task is not done without them.
  - If the task is marked `owner: HUMAN`, produce a design note + tests + a reviewable
    draft, but flag it for human sign-off. Do not self-merge.

Output: the code, the tests, and a short note on anything in the task spec that
turned out to be wrong or under-specified.
```

### Ownership legend

| Marker | Meaning |
|---|---|
| `owner: AI` | AI implements end-to-end; normal PR review. |
| `owner: AI+` | AI implements; an engineer must review line-by-line before merge. |
| `owner: HUMAN` | A human designs and owns correctness. AI assists with tests/scaffolding only. Safety-critical: redaction, digital signatures, encryption, the incremental-save path, the JS/embedded-content sandbox, release criteria. |

### Task block format

```md
- [ ] **SL-1.COS.04 — Cross-reference stream parsing** · deps: `SL-1.COS.01` · owner: AI+
  - **Do:** one-paragraph statement of the work.
  - **Files:** exact paths created/modified.
  - **API:** the signature(s) that must exist afterwards.
  - **DoD:** the checkable acceptance criteria, including named tests and corpus IDs.
  - **Risk / notes:** spec ambiguities, real-world deviations, oracle disagreements.
```

---

## 1. Document map

| File | Contains | Read when |
|---|---|---|
| `00-INDEX.md` | This file. Task ID scheme, gate map, long-lead items, global checklist. | Always first. |
| `01-ARCHITECTURE.md` | Layer model, full crate list + dependency DAG, core traits, every client architecture. | Before any code. |
| `02-ADRS.md` | 34 Architecture Decision Records. Locked decisions. | Before proposing an alternative. |
| `03-CONVENTIONS.md` | Error model, naming, unsafe policy, lint gates, Definition of Done, PR template. | Before any code. |
| `06-CORPUS-POLICY.md` | Handling policy for wild (web-derived) corpora: fetch-only, encrypted at rest, no redistribution, no human review without cause, provenance rules. | Before acquiring any wild corpus. |
| `10-PHASE-0-foundation.md` | Weeks 0–3. Repo, CI, corpus harness, oracle harness, budget/sandbox kernel, error model. | Now. |
| `11-PHASE-1-cos.md` | Weeks 4–9. COS object model, xref, filters, damaged-file recovery, encryption. |  |
| `11A-PHASE-1A-tools.md` | Weeks 10–16. Merge, split, page ops, unlock, protect, compress. **First revenue, month 4.** Runs parallel with Phase 2. |  |
| `12-PHASE-2-render.md` | Weeks 10–17. Content interpreter, graphics state, rasterizer, colour, images, transparency. |  |
| `13-PHASE-3-text.md` | Weeks 18–23. Font engine, shaping, text extraction, reading order, search. |  |
| `14-PHASE-4-web-alpha.md` | Weeks 24–30 (month 7). WASM core, web viewer, MV3 extension. **First ship.** |  |
| `15-PHASE-5-edit.md` | Months 8–12. Edit model, incremental save, annotations, forms, redaction, signing. Paid Beta. |  |
| `16-PHASE-6-desktop.md` | Months 10–15. Tauri desktop, CLI, OS integration, installers, signing/notarisation. |  |
| `17-PHASE-7-mobile.md` | Months 13–18. C ABI, iOS/Android apps, camera scan pipeline, on-device OCR. |  |
| `18-PHASE-8-cloud.md` | Months 15–21. Opt-in cloud: sync, collaboration CRDT, OCR/convert farm, e-sign workflow. |  |
| `19-PHASE-9-platform.md` | Months 18–30. SDK/OEM, server API, enterprise admin, compliance products. |  |
| `20-CONFORMANCE-PROGRAM.md` | The moat. Capability ladder per feature area, gate criteria, per-area task lists. | Before touching any engine crate. |
| `21-TESTING-AND-ORACLES.md` | Corpus, differential testing vs PDFium/pdf.js/qpdf, fuzzing, visual regression, release gates. | Before writing tests. |
| `22-SECURITY-AND-SUPPLY-CHAIN.md` | Threat model (PDF is a malware vector), sandboxing, signing, SBOM, SLSA, CVE process. |  |
| `23-EDIT-MODEL-SPEC.md` | The document mutation / undo / incremental-save model — normative. | Before `selis-pdf-edit`. |
| `24-BINDINGS-SPEC.md` | WASM boundary, C ABI, JNI/Swift, extension messaging — normative. | Before `selis-pdf-wasm` / `selis-pdf-ffi`. |
| `25-RISK-REGISTER.md` | Live risk register with owners, triggers, and kill-criteria. | Monthly review. |
| `26-PRODUCT-SURFACE.md` | Competitor surface → SKU map, entitlement matrix, pricing shape. | Before scoping any product. |
| `28-FIRST-RELEASE-SCOPE.md` | **Read before any code.** Competitive reality, the exposure model (what licensing attaches to build vs. distribute), and the month-4 owned-engine path. |
| `27-COST-AND-LICENSING.md` | What money is actually required and when; the open-source/dual-licence analysis. | Before budgeting, and before changing ADR-P0030. |

---

## 2. Milestone map (what "done" means at each gate)

| Gate | When | Hard exit criteria (all must be true) |
|---|---|---|
| **G0 — Foundation** | End week 3 | CI green; `cargo xtask check-layers` enforced; error-code registry generating docs; corpus harness pulls and hashes ≥5 public corpora; the three oracles (PDFium, pdf.js, qpdf) run in CI containers and emit comparable output; every parse entry point takes a `Budget`. |
| **G1 — Parse truth** | End week 9 | `selis inspect --json` structural output matches `qpdf --json` on 100% of the clean corpus; ≥99% of the 10k-file wild corpus opens or fails with a *typed* error (no panic, no hang, no OOM); 24h fuzz on `selis-pdf-cos` with zero crashes; encrypted-document support (RC4/AESV2/AESV3) round-trips. |
| **G1.5 — Tools Alpha** | Month 4 | Merge/split/page-ops/unlock/protect/compress shipping in the extension + web app · every output opens in Acrobat and PDFium · structural verification on 100% of tool outputs · zero data-loss reports · the writer's property suite green at 50k sequences. |
| **G2 — Render parity** | End week 17 | Perceptual diff vs PDFium ≤ 0.5% differing pixels on ≥95% of the render corpus at 150 DPI; transparency groups + soft masks + shadings 1–7 implemented; deterministic raster (same bytes twice, and across platforms); render throughput ≥ 0.6× PDFium on the benchmark set. |
| **G3 — Text & fonts** | End week 23 | All 14 standard fonts substituted metric-compatibly; embedded Type1/CFF/TrueType/CID/Type3 render at G2 tolerance; text extraction matches PDFium `GetText` ≥ 98% by edit distance on the extraction corpus; reading order matches tagged order on 100% of tagged corpus; CJK + RTL + Indic corpora at G2 tolerance. |
| **G4 — Web Alpha** | Month 7 | Web viewer + MV3 extension published; core WASM ≤ 3 MB brotli; first page painted < 1.2 s p75 on a 5 MB linearised PDF over Fast 3G; 1000 external users; crash-free session rate ≥ 99.5%; zero document-corruption reports (viewer is read-only, so this must be trivially true). |
| **G5 — Editor Beta (paid)** | Month 12 | Annotate, fill, sign, edit-text, page-ops, redact, OCR shipping on web + extension; **incremental-save invariant proven** by a 100k-mutation property suite (original bytes remain a byte-identical prefix); redaction verified by an independent extraction pass on 100% of the redaction corpus; billing + entitlements live. |
| **G6 — Desktop GA** | Month 15 | Signed + notarised installers for Win/mac/Linux (macOS needs `SL-0.LEAD.02`); default-PDF-handler registration; print pipeline; offline licence; crash-free ≥ 99.7% over 5000 sessions; feature parity with web editor. |
| **G7 — Mobile GA** | Month 18 | Android in the Play Store; **iOS shipping as an installable PWA** (ADR-P0022); scan → dewarp → OCR → PDF pipeline on Android; annotate/fill/sign on both; share-target + SAF on Android; cold-open of a 20 MB PDF < 800 ms on a 4-year-old midrange Android device; simulated iOS storage eviction loses no saved work. |
| **G7b — iOS native** | Funding-gated | Deferred by ADR-P0022, specified at `17-PHASE-7-mobile.md §7.IOS-NATIVE`. Unblocked by `SL-0.LEAD.02`, not by engineering. |
| **G8 — Cloud & collaboration** | Month 21 | Opt-in sync + real-time annotation collaboration; SOC 2 Type I; DPA + sub-processor list published; the local/cloud boundary enforced by `selis-policy` and provable by a test that fails if any document byte leaves the device without consent. |
| **G9 — Platform** | Month 30 | C ABI GA + Python/Node/Go/Java wrappers; server API GA; enterprise admin console + MDM/GPO deployment; PDF/A + PDF/UA conformance products validated by veraPDF; ≥1 OEM LOI. |

---

## 3. Long-lead items — START ON DAY 1

These block later gates and cannot be compressed.

- [ ] **SL-0.LEAD.01 — Trademark clearance for "Selis" + namespaces.** USPTO/EUIPO classes 9 and 42;
      `recto.com`/`.io`/`.app`; GitHub org; crates.io `selis-pdf-*`, npm `@recto/*`, PyPI, Maven,
      Homebrew tap. "Selis" is a printing term of art — expect descriptive-mark objections and
      prior use. Blocks every user-visible string, the cert CN, and store listings. See ADR-P0034.
- [ ] **SL-0.LEAD.02 — Apple Developer Program.** Needs a D-U-N-S number for *Organization*
      enrolment (5–14 days alone); *Individual* enrolment carries the same signing rights without
      it. Required for Developer ID (desktop notarisation), the Safari extension, and the App Store.
      Blocks `G6`. **No longer blocks `G7`** — ADR-P0022 ships iOS as a PWA — but it still gates
      macOS notarisation, so deferring it defers signed macOS desktop, not just native iOS.
- [ ] **SL-0.LEAD.03 — Windows code-signing identity.** Azure Trusted Signing preferred (EV-equivalent
      SmartScreen reputation, no HSM to run). 1–6 weeks org validation. Blocks `G6`.
- [ ] **SL-0.LEAD.04 — Chrome Web Store + Edge Add-ons + Google Play developer accounts.**
      CWS now requires verified publisher identity; Play requires D-U-N-S for org accounts and has a
      14-day closed-test requirement before production for new personal accounts. Blocks `G4`, `G7`.
- [ ] **SL-0.LEAD.05a — Free specification library (week 1, blocks engine work).** Every spec
      needed to build the engine through G5 is **free**: ISO 32000-2 (PDF Association, no cost),
      PDF Reference 1.7 (Adobe), ITU-T T.4/T.6/T.88/T.800 (itu.int publishes all in-force
      Recommendations free), the OpenType spec (Microsoft), CFF/Type1/Type2 charstring specs,
      Adobe Font Metrics, the CMap resources (BSD-licensed on GitHub), the Adobe Glyph List,
      XMP, XFA, and ICC.1. Store in `docs/specs/` with provenance. **Cost: zero.**
      See `27-COST-AND-LICENSING.md §2`.
- [ ] **SL-0.LEAD.05b — Paid conformance specs (Phase 9, blocks only the compliance SKU).**
      ISO 19005 (PDF/A) and ISO 14289 (PDF/UA) are paid ISO purchases. Defer them: the free
      **Matterhorn Protocol** enumerates every PDF/UA failure condition, and veraPDF + Isartor
      encode the PDF/A rules as executable tests. Buy when `SL-9.COMPL.01/02` starts.
- [ ] **SL-0.LEAD.06 — PDF Association membership (Phase 5, not day 1).** Deferred. The corpora,
      Matterhorn, and the ISO 32000-2 errata are published publicly. Join when spec ambiguities
      start costing real engineering time and the working groups become worth the fee.
- [x] **SL-0.LEAD.07 — Fallback fonts (week 1, no purchase, no counsel).** Liberation (SIL OFL) or
      TeX Gyre (GUST FL) for metric-compatible Helvetica/Times/Courier, plus Noto Sans/Serif CJK
      (SIL OFL). All three licences explicitly permit embedding and redistribution — read them,
      but this does not need a lawyer. Note the size problem: full Noto CJK is ~100 MB — see
      `SL-4.WASM.07` and `SL-3.FONT.10` for the subsetting and lazy-chunk plan.
- [ ] **SL-0.LEAD.08 — Signature trust chain.** Two separate long leads: (a) a **Timestamp Authority**
      contract (needed for LTV/PAdES-B-LT) and (b) a **CA partnership** if we ever offer
      "sign with a Selis identity". Adobe-trusted signatures require the signer's root to be in the
      AATL or EUTL — we are a *consumer* of that trust, not a member, unless we become a CA.
      For eIDAS qualified signatures, a QTSP partnership is mandatory and takes months. Blocks the
      Sign SKU's marketing claims, not its code.
- [ ] **SL-0.LEAD.09 — OSS-Fuzz application** for the open-sourced parser crates (`selis-pdf-cos`,
      `selis-pdf-filter`, `selis-font`). Free continuous fuzzing at a scale a small team cannot self-fund.
      Application review takes weeks. See ADR-P0030.
- [ ] **SL-0.LEAD.10 — Test-corpus acquisition and licence audit.** pdf.js corpus, veraPDF corpus,
      Isartor (PDF/A-1 negative tests), Ghent Workgroup output suite, PDF Association test suite,
      govdocs1 (fuzzing seeds). Each has its own terms. Some cannot be redistributed — the harness
      must fetch, not vendor. See `21-TESTING-AND-ORACLES.md §2`.
- [ ] **SL-0.LEAD.11 — Cyber/E&O insurance.** A document editor that corrupts a legal filing or
      fails a redaction is a claim. Uninsurable after the first incident. Blocks paid GA.
- [ ] **SL-0.LEAD.12 — Legal review of the redaction claim.** Redaction is the one feature where a
      bug is a headline (see: every "redacted" court filing recovered with copy-paste). Get counsel
      to approve the exact wording of what we promise, before it ships. Blocks `G5`.

---

## 4. Global phase checklist

- [ ] **Phase 0 — Foundation** (`10-PHASE-0-foundation.md`) — weeks 0–3 — 53 tasks
- [ ] **Phase 1 — COS / parse** (`11-PHASE-1-cos.md`) — weeks 4–9 — 40 tasks
- [ ] **Phase 1A — PDF toolkit** (`11A-PHASE-1A-tools.md`) — weeks 10–16, parallel with Phase 2 — 28 tasks
- [ ] **Phase 2 — Render** (`12-PHASE-2-render.md`) — weeks 10–17 — 33 tasks
- [ ] **Phase 3 — Text & fonts** (`13-PHASE-3-text.md`) — weeks 18–23 — 25 tasks
- [ ] **Phase 4 — Web Alpha** (`14-PHASE-4-web-alpha.md`) — weeks 24–30 — 41 tasks
- [ ] **Phase 5 — Edit / Beta** (`15-PHASE-5-edit.md`) — months 8–12 — 67 tasks
- [ ] **Phase 6 — Desktop** (`16-PHASE-6-desktop.md`) — months 10–15 — 27 tasks
- [ ] **Phase 7 — Mobile** (`17-PHASE-7-mobile.md`) — months 13–18 — 30 tasks
- [ ] **Phase 8 — Cloud & collaboration** (`18-PHASE-8-cloud.md`) — months 15–21 — 30 tasks
- [ ] **Phase 9 — Platform / SDK / Enterprise** (`19-PHASE-9-platform.md`) — months 18–30 — 29 tasks

**403 numbered tasks across the ten phases, plus 12 long-lead items (§3) and the three continuous programs.**

- [ ] **Continuous — Conformance program** (`20-CONFORMANCE-PROGRAM.md`) — runs from week 4
- [ ] **Continuous — Testing & oracles** (`21-TESTING-AND-ORACLES.md`) — runs from week 0
- [ ] **Continuous — Security & supply chain** (`22-SECURITY-AND-SUPPLY-CHAIN.md`) — from week 0

---

## 5. The five rules that override everything else

If a task in this plan conflicts with one of these, the rule wins and the task is wrong.

1. **Untrusted bytes are always parsed under a `Budget`.** Every entry point that touches a
   document takes an explicit allocation, wall-clock, recursion-depth, and object-count budget, and
   returns a typed error when it is exhausted. There is no unbounded loop, no unbounded recursion,
   and no unbounded allocation anywhere in `selis-pdf-cos`, `selis-pdf-filter`, `selis-font`, or `selis-pdf-content`.
   (ADR-P0006)
2. **The user's original bytes are immutable.** The source buffer is never mutated in place. Every
   save is an **incremental update appended** to the original byte range by default; a full rewrite
   is a distinct, explicitly-chosen operation that must verify the result before replacing anything.
   A crash at any point leaves either the original file or a valid appended document. (ADR-P0007)
3. **Rendering is deterministic.** The same document, version, and render parameters produce
   byte-identical output on every platform and every run. This is not an aesthetic preference — the
   entire differential-testing and visual-regression apparatus depends on it. (ADR-P0012)
4. **No content embedded in a document executes by default.** JavaScript, launch/URI actions,
   embedded files, external stream references, and remote resource fetches are off unless the user
   turns them on for that document, and they run inside the sandbox when they do. (ADR-P0020)
5. **Redaction removes content, or it fails loudly.** If `selis-pdf-redact` cannot prove that every
   glyph, image sample, annotation, metadata field, and structure node in the redacted region is
   gone from the output bytes, it returns an error. It never draws a black box and calls it done.
   (ADR-P0023)
