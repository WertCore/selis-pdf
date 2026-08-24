# 20 — The Conformance Program

The analogue of Wertcore's filesystem capability ladder. This is the moat: not that we render PDFs,
but that we can *prove, per feature area, how well* — and that we say so publicly.

---

## 1. The governing rule

> **A feature area's capability level is a measurement, not an opinion. Marketing may not claim a
> level the corpus has not demonstrated, and a level is revoked the moment the corpus stops
> supporting it.**

Levels only ever change through `xtask conformance promote|demote`, which requires the gate
evidence to be present in CI output. A human cannot edit `conformance/areas.toml` by hand — the
file has a checksum over the evidence records.

---

## 2. The ladder

| Level | Meaning | What the product may do |
|---|---|---|
| `None` | Not implemented. | Report the construct as unsupported, explicitly. |
| `Identify` | We recognise the construct and can report it. | Tell the user it exists; do not act on it. |
| `Parse` | We build a faithful in-memory model and round-trip it losslessly. | Preserve it across a save. Do not render or interpret it. |
| `Render` | Visually correct within tolerance against two oracles across the corpus. | Display it. Print it. |
| `Extract` | Semantic content out with correct order and encoding. | Search, copy, extract, index, feed to conversion. |
| `Edit` | Mutate and re-save with everything else preserved and verified. | Offer editing UI for it. |
| `Author` | Create from scratch, conformant to spec. | Offer creation UI. Claim authoring support. |

A level implies all levels below it. **`Parse` requires lossless round-trip** — the most common
silent data-loss bug in PDF tools is a save that drops constructs the tool did not understand, and
`Parse` is precisely the promise that we do not do that.

---

## 3. Gate criteria per level

### `Identify`
- The construct is detected on 100% of corpus files known to contain it.
- Detection never produces a false positive on the negative corpus.
- Reported through `inspect --json` and the document health panel.

### `Parse`
- Model built for 100% of the area's corpus files.
- **Round-trip:** load → save (incremental, no edits) → reload produces a semantically identical
  model, and the original bytes are a byte-identical prefix.
- **Preservation under unrelated edits:** an unrelated mutation elsewhere in the document leaves
  this area's constructs byte-identical in the output.
- Fuzz target for the area's parser, 8 h clean.
- Every malformation the corpus contains is either handled or produces a typed error.

### `Render`
- ≤0.5% differing pixels vs **two independent oracles** at 72/150/300 DPI on ≥95% of the area's
  corpus, with the residual triaged and each case annotated.
- Deterministic across Linux/macOS/WASM (hash equality).
- Renders within the area's slice of the perf budget.
- Degrades correctly under budget exhaustion (partial render with a reported deviation, not a
  blank page and not a hang).

### `Extract`
- ≥98% normalised-edit-distance agreement with two oracles on the area's extraction corpus.
- Reading order matches the structure tree where one exists; a published accuracy figure where
  none does.
- Encoding correct for all scripts in the corpus; confidence reported and calibrated.

### `Edit` — the first level that can damage a user's document
- Every mutation type for the area has a property test over ≥10 000 random sequences asserting:
  the incremental-save invariant (original bytes are a prefix), semantic correctness, prior
  revisions readable, and the output opens in PDFium *and* Acrobat.
- Crash-injection at 200 random points during save leaves a valid document.
- Round-trip through Acrobat and one other major tool without loss or complaint.
- Undo restores the exact prior state, verified by model equality, not by re-rendering.
- The area's edits preserve: signatures (or correctly invalidate and say so), tagging, optional
  content, form field integrity, and named destinations.

### `Author`
- Generated output validates against every applicable conformance checker we run (veraPDF for
  PDF/A and PDF/UA, our own rule engine, and Acrobat's preflight where available).
- Generated output is byte-stable across runs (determinism).
- Generated output round-trips through the `Parse` and `Render` gates of every other tool in the
  oracle set.

---

## 4. Area map and target levels by gate

| Area | G2 (wk 17) | G3 (wk 23) | G4 (mo 7) | G5 (mo 12) | G6–G7 | G9 (mo 30) |
|---|---|---|---|---|---|---|
| COS / structure / xref | Parse | Parse | Parse | **Edit** | Edit | Author |
| Filters: Flate/LZW/A85/AHx/RL | Render | Render | Render | Author | Author | Author |
| Filter: DCT | Render | Render | Render | Render | Author | Author |
| Filter: CCITT | Render | Render | Render | Render | Author | Author |
| Filter: JBIG2 | Render | Render | Render | Render | Render | Author |
| Filter: JPX | Parse | Render | Render | Render | Render | Render |
| Encryption: RC4/AESV2 | Parse | Parse | Render | Render | Render | Render |
| Encryption: AESV3/R6 | Parse | Parse | Render | **Author** | Author | Author |
| Encryption: public-key | Identify | Parse | Parse | Parse | Render | Edit |
| Page graphics: paths, clip | Render | Render | Render | Edit | Edit | Author |
| Colour: device + CIE + ICC | Render | Render | Render | Render | Render | Author |
| Colour: Separation/DeviceN | Render | Render | Render | Render | Render | Author |
| Functions 0/2/3/4 | Render | Render | Render | Render | Render | Author |
| Transparency + soft masks | Render | Render | Render | Render | Render | Author |
| Shadings 1–7 | Render | Render | Render | Render | Render | Author |
| Patterns | Render | Render | Render | Render | Render | Author |
| Images + masks | Render | Render | Render | Edit | Edit | Author |
| Fonts: TrueType/CFF/Type1 | Parse | Render | Render | **Edit** | Edit | Author |
| Fonts: Type3 | Parse | Render | Render | Render | Render | Render |
| Fonts: CID / CJK / vertical | Parse | Render | Render | Render | Edit | Author |
| Text extraction + order | None | Extract | Extract | Extract | Extract | Extract |
| Complex scripts (RTL, Indic) | None | Render | Render | Extract | Extract | Author |
| Annotations | Identify | Parse | Render | **Author** | Author | Author |
| AcroForm | Identify | Parse | Render | **Author** | Author | Author |
| XFA (static) | None | Identify | Parse | Render | Render | Render |
| XFA (dynamic) | None | None | Identify | Identify | Identify | Identify |
| JavaScript (subset) | None | None | None | Render | Render | Render |
| Digital signatures | None | Identify | Parse | **Author** | Author | Author |
| Redaction | None | None | None | **Author** | Author | Author |
| Tagged PDF / structure | Parse | Parse | Extract | Extract | Extract | **Author** |
| Optional content | Parse | Parse | Render | Render | Edit | Author |
| Embedded files | Identify | Identify | Parse | Parse | Edit | Author |
| Metadata (Info + XMP) | Parse | Parse | Parse | Edit | Edit | Author |
| Linearisation | Parse | Parse | Parse | Parse | Author | Author |
| PDF/A | None | None | Identify | Identify | Parse | **Author** |
| PDF/UA | None | None | Identify | Identify | Parse | **Author** |
| PDF/X + prepress | None | None | None | Identify | Identify | Parse |

Bold marks the gate where an area becomes a product capability rather than infrastructure.

Note what is deliberately *never* promoted: dynamic XFA stays at `Identify` forever (ADR, and
`01-ARCHITECTURE.md §13`). Saying so on the public conformance page is more useful to a prospective
buyer than a checkmark that means nothing.

---

## 4a. Conformance rules are written early and activate incrementally

PDF/A and PDF/UA are not features you build; they are **rules about features you build elsewhere**.
"Every font must be embedded" cannot be checked before the font engine exists. That is why they sit
at `Author` only at G9 — not because the ISO documents cost money.

But a large slice of the rules depends on nothing beyond the Phase 1 document model, and those
should be written in Phase 1, not year 3. Split the rule set by the engine area each rule needs:

| Rule class | Example rules | Needs | Writable from |
|---|---|---|---|
| **Structural** | No encryption; no JavaScript; no `/Launch`; no external stream references; no embedded files (PDF/A-1); XMP present and consistent with the Info dictionary; `pdfaid` claim well-formed | `selis-pdf-cos`, `selis-pdf-doc` | **Phase 1** |
| **Structure tree** | Tag hierarchy valid; reading order present; alt text on figures; table structure; language marked | `selis-pdf-doc` structure tree (ADR-P0031) | **Phase 1** |
| **Font** | All fonts embedded; embedding permitted by the font's licence bits; correct `ToUnicode` | `selis-font` | Phase 3 |
| **Colour** | Output intent present; device-independent colour or an OutputIntent covering it | `selis-color` | Phase 2 |
| **Graphics** | No transparency (PDF/A-1); no unsupported blend modes | `selis-pdf-content` | Phase 2 |

**Implementation:** a single rule registry where each rule declares the engine area and minimum
ladder level it requires. A rule whose dependency is not yet met reports `Unevaluated`, never
`Pass`. The validator therefore grows automatically as the engine advances, and the "what can we
check today" answer is always honest and machine-generated rather than a guess.

This is also the cheapest possible on-ramp to the compliance SKU: by the time Phase 9 arrives, most
rules are already written and tested, and the remaining work is the colour/graphics tail plus
buying the ISO text to confirm the wording.

---

## 5. Every area crate has the same shape

```text
crates/selis-pdf-<area>/
├── src/
│   ├── model.rs        # the in-memory representation
│   ├── parse.rs        # bytes → model, budgeted, resumable
│   ├── write.rs        # model → bytes (only if the area reaches Parse round-trip)
│   ├── render.rs       # model → display-list ops (only at Render)
│   ├── edit.rs         # mutations (only at Edit)
│   └── conform.rs      # the area's self-check: which level do we claim, and the evidence
├── tests/
│   ├── roundtrip.rs    # the Parse gate
│   ├── differential.rs # the Render / Extract gate
│   ├── mutation.rs     # the Edit gate
│   └── conformance.rs  # asserts the claimed level's gate criteria still hold
└── fuzz/
```

`tests/conformance.rs` is the important one: it *fails the build* if `areas.toml` claims a level
the evidence no longer supports. A regression does not silently downgrade quality — it breaks CI.

---

## 6. The corpus, per area

Each area owns tagged corpus slices:

| Slice | Purpose |
|---|---|
| `<area>-clean` | Well-formed, spec-conformant. The baseline. |
| `<area>-wild` | Real-world files from the wild corpus containing the construct. |
| `<area>-damaged` | The construct, malformed. From the synthetic mutator (`SL-0.CORP.04`). |
| `<area>-adversarial` | Deliberately hostile: budget bombs, ambiguity exploits, spec-corner abuse. |
| `<area>-negative` | Files that must *not* be detected as containing the construct. |
| `<area>-golden` | Files with hand-verified expected output. Small, expensive, precious. |

An area cannot reach `Render` without a non-empty `wild` slice. Clean-corpus-only conformance is
how you build something that fails on the first real document a user opens.

---

## 7. Promotion and demotion procedure

**Promotion** — `xtask conformance promote <area> <level>`:
1. Runs the level's full gate suite. Any failure aborts.
2. Records an evidence bundle: corpus hashes, oracle versions, pass rates, the commit.
3. Updates `areas.toml` and the public conformance page in the same commit.
4. Requires a human approver for `Edit` and `Author` promotions.

**Demotion** — automatic. If `tests/conformance.rs` fails on trunk, CI opens a P1, and if it is not
fixed within one release cycle, `xtask conformance demote` runs and the public page updates.
Demoting is not a failure of process; silently keeping a claim we cannot support is.

---

## 8. Why this is the moat

Any competent team can render 90% of PDFs in a year. The remaining 10% is thirty years of
accumulated real-world deviation, and it is what separates a demo from a product. The ladder makes
that 10% legible: it tells the team where to spend the next month, it tells buyers exactly what
they are getting, and it converts "we support PDF" — a claim everyone makes and nobody can
verify — into a published, falsifiable, continuously-tested measurement.

It is also the thing that makes product #8 cheap. When the Server SKU or an OEM asks "do you
support Separation colour in shadings?", the answer is a table lookup with evidence behind it,
not a week of investigation.
