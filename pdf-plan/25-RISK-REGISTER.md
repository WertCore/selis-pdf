# 25 — Risk Register

Review monthly. Every risk has an owner, a **trigger** (the observable that says it is
materialising), and a **kill criterion** (what makes us stop or change course). A risk without a
trigger is a worry, not a managed risk.

---

## R1 — The from-scratch engine takes far longer than planned
- **Severity:** existential · **Owner:** founder
- **The honest baseline:** pdf.js is ~15 years of sustained work. PDFium descends from Foxit's
  engine and carries 20+ years. Chromium, Mozilla, and Adobe each fund theirs with teams. This plan
  reaches a credible *viewer* at month 7 and a credible *editor* at month 12 with a small team.
  That is aggressive, and it is only achievable because ADR-P0008 confines "from scratch" to PDF
  semantics rather than 2D graphics, shaping, and codecs.
- **Trigger:** G2 (render parity) slips past week 22, **or** the G2 sweep shows <80% of the corpus
  within tolerance at week 17.
- **Mitigation:** the conformance ladder makes partial progress shippable — a viewer that renders
  95% of documents correctly and *says so honestly* is a real product. Ship the viewer on what
  works; do not wait for completeness.
- **Kill criterion:** if at month 9 the render corpus is below 85% at G2 tolerance, adopt the
  hybrid posture from ADR-P0008's rejected alternatives: keep our parser, edit model, and text
  pipeline; call PDFium for rasterisation of page features we have not reached; retire it feature
  by feature behind the ladder. This preserves the moat and the timeline. Decide this at month 9,
  not month 18 — the cost of the decision rises steeply with time.

## R2 — Text-edit reconstruction never reaches acceptable quality
- **Severity:** high (it gates the Editor SKU's headline feature) · **Owner:** technical lead
- **Trigger:** at month 11, reconstruction confidence ≥0.85 covers less than 60% of paragraphs in
  the labelled set, or user testing shows reflow producing visibly wrong output more than 1 in 20 times.
- **Mitigation:** the confidence banding (ADR-P0024, `23-EDIT-MODEL-SPEC.md §4`) means the product
  degrades to line- and box-level editing rather than failing. Every competitor's text editing is
  also imperfect; being *honest* about when it will not work is a differentiator, not a weakness.
- **Kill criterion:** ship Editor without paragraph reflow, market it on annotate/fill/sign/redact/
  OCR/page-ops — still a complete product — and keep reflow in beta until the numbers support it.

## R3 — A redaction failure becomes public
- **Severity:** existential (reputational) · **Owner:** HUMAN, security lead
- **Trigger:** any adversarial-suite failure, any external report of recoverable content, any
  disagreement between the verification pass and an independent extraction.
- **Mitigation:** ADR-P0023's proof obligation; the adversarial corpus at 100% with no waiver;
  legal review of the claim (`SL-0.LEGAL.05`); the pre-written incident policy.
- **Kill criterion:** two verification-bypass findings in one release cycle → pull the feature and
  ship it as "mark for redaction, export to review" until the model is rebuilt. Pull, do not patch.

## R4 — A signature-validation false-positive
- **Severity:** existential (reputational + legal) · **Owner:** HUMAN
- **Trigger:** any case where we report a signature valid that Acrobat reports invalid, or any
  successful shadow/incremental-update attack against our validator.
- **Mitigation:** the published attack corpus in CI from day one of `SL-5.SIGN.01`; the visual-vs-
  cryptographic UI separation; external pentest focused here at G5.
- **Kill criterion:** one confirmed false-valid in the field → validation switches to a conservative
  mode (report "cannot fully verify" wherever the analysis is not exhaustive) until re-audited.

## R5 — Scope explosion
- **Severity:** high — the most likely cause of failure · **Owner:** founder
- **Trigger:** a gate slips twice, or a product surface gets built that is not in
  `26-PRODUCT-SURFACE.md`, or the crate count grows without an ADR.
- **Mitigation:** the gate structure; ADR-P0014 (entitlements, not builds); the explicit
  "what deliberately does not exist" list in `01-ARCHITECTURE.md §13`.
- **Kill criterion:** if two consecutive gates slip, freeze all new surfaces until the current gate
  closes. New product surfaces are the most attractive way to avoid finishing the hard one.

## R6 — `tiny-skia` cannot express a required PDF construct
- **Severity:** medium · **Owner:** technical lead
- **Trigger:** transparency-group, soft-mask, or blend-mode work in Phase 2 needs a primitive the
  backend does not offer.
- **Mitigation:** the `raster::Backend` trait boundary (ADR-P0008) confines the change; most gaps
  are addressable by compositing at our layer rather than forking.
- **Kill criterion:** if more than two constructs require a fork, fork it deliberately as
  `selis-raster-backend` with a maintenance owner, rather than accumulating patches.

## R7 — WASM size budget cannot be met
- **Severity:** medium-high (it gates the beachhead) · **Owner:** technical lead
- **Trigger:** core chunk exceeds 3.5 MB brotli at any point after week 24.
- **Mitigation:** aggressive code splitting from day one (`SL-4.WASM.02`); `size-check` in CI from
  week 1 so growth is visible daily; fonts and codecs as lazy chunks.
- **Kill criterion:** if the core cannot fit, split the viewer further — a "render only, no text
  layer" first-paint chunk that hydrates. Do not ship a 10 MB first load; the beachhead depends on
  first-paint speed.

## R8 — Chrome Web Store rejection or policy change
- **Severity:** medium-high · **Owner:** HUMAN
- **Trigger:** rejection, a permissions-policy change, or a Manifest revision that breaks the
  interception approach.
- **Mitigation:** minimal permissions with written justifications (`SL-4.EXT.01`); no remote code
  (ADR-P0028); the web app works standalone, so the extension is a funnel and not a dependency.
- **Note:** Google has changed extension policy repeatedly. Treat store distribution as
  rentable, not owned, and keep the web app as the durable surface.

## R9 — Oracle disagreement is unresolvable at scale
- **Severity:** medium · **Owner:** QA lead
- **Trigger:** more than 50 untriaged clusters persist for two consecutive nightly runs.
- **Mitigation:** clustering rather than per-file triage (`SL-0.ORACLE.05`); calibrating oracle-vs-
  oracle agreement *first* so tolerances are grounded; PDF Association membership for spec questions.
- **Kill criterion:** if triage cannot keep pace, reduce the nightly corpus and fix the pipeline
  before growing it again. An ignored failure list is worse than a smaller one.

## R10 — A free local-first competitor already holds the position
- **Severity:** high · **Owner:** founder
- **Status:** **materialised, August 2026.** `ihatepdf.cv` ships 46 client-side tools, free, no
  watermark, no sign-up, no daily limit, no upload. The "we don't upload your file" wedge this plan
  was built on is occupied. See `28-FIRST-RELEASE-SCOPE.md §2`.
- **Trigger (for escalation):** they ship a browser extension; or they publish per-feature
  correctness evidence; or they raise funding.
- **Mitigation:** ship the toolkit at **month 4** on the owned engine — merge/split/unlock/compress
  need no renderer, so Phase 0 → 1 → 1A gets there without waiting for Phase 2–4 (ADR-P0008 stands;
  no third-party engine is borrowed to go faster). Differentiate on the extension channel, provable
  correctness, real redaction, the Privacy Risk Scanner, and large-document handling — not on
  tool count.
- **Kill criterion:** if 8 weeks after the month-4 toolkit release the extension shows no organic install
  growth over 8 weeks, the consumer tools market is not winnable and the plan should pivot to the
  **SDK / Server line** (`19-PHASE-9-platform.md`), where the buyers are developers, the
  competition is Apryse and Nutrient at enterprise prices, and an owned engine is the entire
  product rather than a back end for a commodity tool site.

## R16 — The renderer gets built before the toolkit ships
- **Severity:** high · **Owner:** founder
- **Trigger:** any Phase 2 (render) task starts before `11A-PHASE-1A-tools.md` is feature-complete.
- **Why it is likely:** merge, split and unlock are unglamorous object-graph plumbing; the
  rasteriser and font engine are the interesting work. The pull toward Phase 2 is strong and it
  costs three months of time-to-market against a competitor who already shipped.
- **Mitigation:** Phase 1A depends on nothing in Phase 2 or 3. Treat the month-4 toolkit release as
  a hard gate, not a milestone that can slip for "just finishing the renderer".
- **Kill criterion:** if month 5 arrives with no shipped product, stop all Phase 2 work until
  Phase 1A ships.

## R17 — Unexposed code ships anyway
- **Severity:** medium · **Owner:** technical lead
- **Trigger:** the WASM viewer chunk exceeds its 3 MB brotli budget, or a fuzz finding lands in a
  code path no UI can reach.
- **Why:** ADR-P0014 ships one binary with entitlement-gated features. On the web that means
  unexposed code still costs payload, still carries attack surface, and still needs fuzzing.
- **Mitigation:** `28-FIRST-RELEASE-SCOPE.md §3` — unexposed functionality lives in a lazily-loaded
  chunk the first release never downloads, gated by the **build**, not by a UI flag. `size-check`
  (`SL-0.WS.09`) enforces it from week 1.

## R11 — A memory-safety bug in a vendored C dependency
- **Severity:** medium · **Owner:** security lead
- **Trigger:** any CVE in OpenJPEG or Tesseract, or any fuzz finding that escapes the WASM cap.
- **Mitigation:** both are WASM-sandboxed (ADR-P0018), never linked natively into a client.
- **Kill criterion:** a sandbox escape → disable the feature by remote flag within 24 h while the
  runtime is patched.

## R12 — Key-person dependency on the safety-critical crates
- **Severity:** medium-high · **Owner:** founder
- **Trigger:** `selis-pdf-edit`, `selis-pdf-redact`, or `selis-pdf-sign` has one contributor for two quarters.
- **Mitigation:** `owner: HUMAN` marking means these already require senior review; enforce a
  two-reviewer rule and rotate; the normative specs (23, 24) exist precisely so the knowledge is
  not only in someone's head.

## R13 — Wertcore platform coupling becomes a blocker
- **Severity:** medium · **Owner:** founder
- **Trigger:** Selis needs the shared updater/licence substrate (ADR-P0029) before Wertcore ships it.
- **Mitigation:** the revisit trigger is written into the ADR — build a minimal updater rather than
  block G6.
- **Note:** review this at month 9, when the desktop phase starts, not at month 14.

## R14 — Corpus acquisition stalls on legal review
- **Severity:** medium · **Owner:** HUMAN
- **Trigger:** `SL-0.LEGAL.04` or `SL-0.CORP.05` unresolved at week 6.
- **Mitigation:** the synthetic generator (`SL-0.CORP.04`) is entirely ours and is the highest-value
  corpus anyway. G1's wild-corpus gate can slip a fortnight without moving G2.
- **Kill criterion:** if the wild corpus cannot be obtained at all, substitute a smaller
  consent-based corpus from beta users and lower the G1 threshold explicitly rather than quietly.

## R15 — Accessibility regulation changes faster than the product
- **Severity:** low-medium, rising · **Owner:** product
- **Trigger:** a jurisdiction mandates a conformance level our `Author` ladder has not reached.
- **Mitigation:** ADR-P0031 puts tagging in Phase 1, which is the expensive part; reaching
  PDF/UA `Author` from there is incremental.
- **Note:** this risk is mostly an *opportunity*. Regulation is why the remediation segment
  (`SL-9.COMPL.02`) exists and is underserved.
