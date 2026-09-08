# 21 — Testing, Corpus, and Oracles

The corpus is to this project what the hardware lab is to Wertcore — except ours costs almost
nothing and can run a hundred thousand cases a night. That advantage only materialises if the
harness is built first (Phase 0) and used ruthlessly.

---

## 1. The testing pyramid, inverted for parsers

For most software the unit tests carry the load. For a hostile-input parser the *corpus* carries
the load, and unit tests exist mostly to make corpus failures debuggable.

| Layer | Count | Runs | Catches |
|---|---|---|---|
| Unit | thousands | every PR | logic errors, edge cases you thought of |
| Property | ~150 | every PR | invariant violations across generated input |
| Corpus smoke | 2 000 files | every PR | regressions on the constructs you already fixed |
| Corpus full | 100 000+ files | nightly | the long tail |
| Differential | full corpus × 3 oracles | nightly | *being wrong in a way you cannot imagine* |
| Fuzz | continuous | nightly + OSS-Fuzz | memory safety, hangs, budget escapes |
| Mutation | 4 crates | weekly | tests that assert nothing |

---

## 2. Corpus sources

| Source | Size | Value | Licensing |
|---|---|---|---|
| Synthetic generator (`SL-0.CORP.04`) | ~2 000 | **Highest** — expectations are derived, not guessed | ours |
| Synthetic mutator | unbounded | Damaged-file robustness | ours |
| pdf.js test corpus | ~1 000 | Curated real-world pathologies, already triaged by another team | mixed; fetch, do not vendor |
| veraPDF corpus | ~1 500 | PDF/A conformance, positive and negative | check per file |
| Isartor test suite | ~200 | PDF/A-1 negative tests — every rule violated deliberately | check |
| Ghent Workgroup output suite | ~50 | Prepress, transparency, colour — brutal rendering tests | check |
| PDF Association test suite | varies | Spec-corner cases, PDF 2.0 features | membership |
| Wild corpus (`SL-0.CORP.05`) | 100 000 | The only thing that predicts real-world behaviour | no redistribution, encrypted at rest |
| User-submitted bug files | grows | The highest-value slice over time | explicit consent, documented handling |
| Budget-exhaustion suite (`SL-1.ROB.03`) | ~50 | Sandbox regression | ours |
| Redaction adversarial suite (`SL-5.REDACT.06`) | ~100 | The redaction claim | ours |

**The synthetic generator is the most under-rated item in this table.** A downloaded file tells you
what *some* producer emitted; a generated file tells you what the spec says *and* what the correct
output is. Invest in it early and keep extending it — every time a spec section is implemented, the
generator should learn to emit that construct.

---

## 3. Expectation records

`corpus/expect/<id>.toml`:

```toml
id      = "wild-0a3f21"
tags    = ["render", "transparency", "wild", "cjk"]
open    = "Ok"                       # or a specific error code
deviations = ["BadXrefOffset"]       # exactly what we expect to report

[render.150]
hash      = "blake3:9f2c…"           # our golden output
oracle_pdfium   = { differing_pixels = 0.0021, verdict = "Agree" }
oracle_pdfjs    = { differing_pixels = 0.0104, verdict = "Agree" }

[extract]
hash      = "blake3:44ab…"
oracle_pdfium = { edit_distance = 0.004, verdict = "Agree" }

[annotation]
note = "pdf.js renders the soft mask backdrop differently; ISO 32000-2 §11.6.5.2 supports our
        reading. Reported upstream as pdf.js#XXXXX."
verdict = "OracleBug"
```

The `annotation` field is what keeps the harness honest over time. Without it, every triaged
disagreement gets re-triaged every time someone new looks at the failure list.

---

## 4. Oracles

| Tool | Used for | Licence posture |
|---|---|---|
| **PDFium** | Primary render + extract oracle | BSD-3; shippable, but we still only use it in CI (ADR-P0009) |
| **pdf.js** | Second render + extract oracle — genuinely independent implementation | Apache-2.0 |
| **qpdf** | Structural oracle: objects, xref, page tree, round-trip | Apache-2.0 |
| **MuPDF** | Third render opinion on hard cases | AGPL — **external process only, never linked** |
| **Ghostscript** | Codec + colour + shading reference | AGPL — external process only |
| **veraPDF** | PDF/A and PDF/UA validation | GPL/MPL — external process only |
| **Acrobat** | Manual spot-check on `Edit`/`Author` gates | commercial; manual, not CI |

Two oracles minimum for a `Render` promotion, and they must be *independently implemented* — PDFium
and pdf.js qualify; PDFium and a PDFium-derived tool do not.

### Calibration comes first
`SL-0.ORACLE.02` measures oracle-vs-oracle agreement on the clean corpus **before** we measure
ourselves against them. If PDFium and pdf.js only agree to 1.2% on shadings, demanding 0.5% of
ourselves on shadings is measuring noise. Every tolerance in `20-CONFORMANCE-PROGRAM.md §3` is
subject to revision by that calibration, and the calibration numbers are published alongside them.

---

## 5. Triage workflow

A full-corpus differential run produces thousands of disagreements. Untriaged, that is noise and
the team learns to ignore it — which is the real failure mode.

1. **Seed.** The corpus files are fetch-only (gitignored, refetchable from
   `~/.cache/selis-corpus`): re-extract the archives into `corpus/pdfs/`
   (`xtask corpus synthetic-generate` for the deterministic synthetic set),
   then sample. Sampling is a deterministic stride over the sorted file list
   (`--sample N`), so the sample spans every source instead of whichever
   directory sorts first.
2. **Cluster.** `xtask oracle triage --sample N` runs the structural comparison
   against qpdf over the sample and groups by disagreement signature, not by
   file. 4 000 failures typically collapse to 20–40 clusters. Signatures, as
   implemented in `xtask/src/oracle.rs`:
   - `match` — structural agreement after normalisation (see below);
   - `obj_delta=N` — live-object count differs by N;
   - `selis_rejects` — we refuse with a typed error, qpdf opens (repair-policy
     gap; for deliberately-damaged mutants the refusal is the designed
     posture);
   - `qpdf_rejects` — we open, qpdf refuses hard (oracle over-strictness or a
     repair-policy gap in the other direction);
   - `both_reject` — agreement: the file is broken and both tools say so.
   Two comparator normalisations are applied before counting (SL-0.ORACLE.03
   known differences, verified on real files): object 0 — the free-list head —
   is excluded from our union, and qpdf's `maxobjectid` is *not* used (it
   counts the free-head slot); the comparison uses qpdf's `obj:N 0 R` map
   keys. Without this, every healthy file shows a phantom `obj_delta=1`.
   Damaged-but-recoverable files stay in the comparable pool via
   `qpdf --warning-exit-0` (qpdf otherwise exits 2 and emits no JSON for
   files it repairs, which manufactured a 107-file false `open_failed`
   cluster on the first seeded run).
3. **Rank** by (files affected × corpus weight), where wild-corpus files weigh more than synthetic.
   Implemented as the `[w=…]` column in the triage output (govdocs/wild ×3).
4. **Verdict** per cluster, recorded in the expectation files:
   - `OurBug` → file a task, link the cluster, add a golden corpus entry.
   - `OracleBug` → annotate with the spec citation; report upstream; keep our output.
   - `SpecAmbiguous` → annotate with both readings and the reasoning for ours; consider asking the
     PDF Association (this is what membership is for).
   - `ToleranceTooTight` → adjust the tolerance *with the calibration data as justification*, never
     because a build is red.
   Mechanically: `xtask oracle triage --sample N --verdict "<signature>=<Verdict>" --note "…"`
   re-computes the clusters, validates the verdict against the four-value set,
   and writes an `[annotation]` table (`triage`, `verdict`, `note`) into
   `corpus/expect/<id>.toml` for every file in that cluster. Notes are bounded
   authored prose (≤160 chars, one line) and `check-wild-hygiene` still
   enforces the metadata-only record bounds. Regenerating expectations
   (`corpus expect-generate`) preserves existing annotations.
5. **Never** mark a cluster `WontFix` without an annotation. A silent suppression is a bug that
   will be rediscovered in a year at ten times the cost.

### Baseline: the seeded run (2026-09-08)

463 files (216 pdf.js corpus incl. the crypto fixtures, 40 govdocs1, 203
synthetic incl. 62 seeded mutants) × qpdf 12.4.1 → **15 clusters**:

| Cluster | Files | Verdict |
|---|---|---|
| `match` | 392 | — (agreement; 84.7% of the sample) |
| `selis_rejects` | 21 | `SpecAmbiguous` — deliberate mutants we refuse by design (typed error, no repair attempt); qpdf agrees to disagree; both readings defensible |
| `qpdf_rejects` | 21 | `SpecAmbiguous` — we open (incl. reconstruction) where qpdf refuses hard: pdf.js-corpus pathologies (page-tree loops, unrecoverable `/Root`), the crypto fixtures qpdf will not open password-less, and mutants qpdf cannot recover |
| `obj_delta=1` | 7 | `SpecAmbiguous` — multi-revision files: our union spans all revisions, qpdf's map lists the final revision's live objects; the spec does not define "the" object count |
| `obj_delta=2` | 5 | `ToleranceTooTight` — same union-vs-live divergence at N=2; comparator artefact, not an engine bug |
| `obj_delta=3…21` | 6 | `SpecAmbiguous` — same divergence on files with incrementally deleted objects (incl. the two byte-exact recovery fixtures) |
| `obj_delta=15/27/103` | 3 | `SpecAmbiguous` — govdocs damaged files where our reconstruction recovers the full object set; note & keep |
| `both_reject` | 6 | — (agreement on broken files; typed codes already recorded) |

The residual `obj_delta` clusters are the one real finding: the structural
comparator should also compare final-revision-only counts to silence the
union-vs-live skew for files with incremental updates. Recorded as the
follow-up for SL-0.ORACLE.03 (see the plan notes).

---

## 6. Property tests that matter most

These are worth more than any other single category:

1. **Filter round-trip.** `decode(encode(x)) == x` for every filter that encodes, over generated data.
2. **COS writer round-trip.** `parse(write(obj)) == obj` for arbitrary generated object graphs
   including every string byte value and deep nesting.
3. **The incremental-save invariant** (`SL-5.EDIT.03`). For any mutation sequence: the output's
   prefix equals the original bytes, the output parses, the model matches, every prior revision is
   still readable, and PDFium opens it. **This is the single most important test in the codebase.**
4. **Undo/redo convergence.** Any sequence of do/undo/redo reaches the state the operation log says.
5. **Budget monotonicity.** No sequence of charges exceeds the limit; no operation completes having
   consumed more than it charged.
6. **Render determinism.** Same input → same bytes, twice, on three platforms, threaded and not.
7. **Redaction completeness.** For any region and any document, the redacted output contains no
   byte sequence from the region's extracted text (`SL-5.REDACT.04`).
8. **Structure preservation.** For any unrelated mutation, the structure tree, form fields, optional
   content, and named destinations are byte-identical in the output.

---

## 7. Fuzzing

- One `cargo-fuzz` target per parser entry point. **A new parser without a fuzz target does not
  merge** (`03-CONVENTIONS.md §7`).
- Every target asserts, beyond "no crash": terminates within the fuzz budget, allocates no more
  than the budget, and reports a typed error rather than a wrong-but-plausible success.
- Structure-aware fuzzing for COS and content streams — a mutation-only fuzzer spends its life
  failing the lexer. Use the synthetic generator as a grammar.
- Seeds from: the wild corpus (minimised), the synthetic corpus, and every historical bug file.
- OSS-Fuzz for the three public crates (ADR-P0030). Self-hosted soak for the proprietary ones.
- **Regression discipline:** every fuzz finding becomes a permanent corpus entry named for the
  finding, before the fix lands.

---

## 8. Visual regression

- Golden PNG hashes for the golden slice; perceptual comparison for everything else.
- On a diff, CI publishes a three-up artefact (ours / oracle / diff heatmap) plus the display-list
  structural diff, so a reviewer sees *which operator* changed, not just that pixels moved.
- Zoom levels 72/150/300 DPI minimum; the golden slice also at 600 DPI to catch hinting and
  rounding errors that hide at low resolution.

---

## 9. Performance testing

- Criterion benchmarks on the reference machine, per `03-CONVENTIONS.md §12`.
- **Throughput ratio against PDFium** is tracked as a first-class metric, because absolute numbers
  drift with hardware and the ratio does not.
- Mobile perf measured on a fixed, deliberately mediocre device — the newest phone tells you
  nothing about the user with a four-year-old handset.
- Memory: peak RSS and allocation count per operation, both budgeted.
- A nightly "pathological document" suite: 5 000-page reports, 100 MB single pages, 50 000
  annotations, 10 000 form fields, deeply nested transparency.

---

## 10. Release gates

A release does not ship unless:

1. Full corpus run complete, with no new `OurBug` clusters above the severity threshold.
2. Conformance levels unchanged or promoted — never silently demoted.
3. Determinism job green on all three platforms.
4. Fuzz soak clean for 72 h on the release branch.
5. The incremental-save property suite green at full size (100 000 sequences).
6. The redaction adversarial suite at 100%.
7. Perf budgets met; no regression >5% on any tracked path.
8. Accessibility: axe-core clean on web, platform AT smoke tests passing on desktop and mobile.
9. veraPDF validation unchanged for any `Author`-level conformance area.
10. SBOM generated; `cargo deny` and `cargo vet` clean; no new unreviewed dependency.

Gates 5 and 6 have no override. Everything else can be waived by a named human with a written
reason recorded in the release notes.
