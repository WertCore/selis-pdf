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

### The measured calibration (SL-2.CONF.03, 2026-09-10)

`xtask oracle sweep --calibrate` renders page 1 with every named oracle and compares all pairs
under the identical CONF.01 metric (ΔE76 > 2.3, overlap region, page 1 @150 DPI), with a repeated
tool as the self-agreement sanity leg. Local legs: PDFium chromium/7961, pdf.js 6.2.108, MuPDF
1.23.0 (mutool ×2 as sanity); the CI `render-conf` job re-runs the same legs on the pinned GHCR
images. Sample: 300-file stride over the general corpus + the full 95-file Ghent suite. Fraction
of comparable pages within each differing-pixels band:

| Pair | corpus | n | ≤0.5% | ≤1% | ≤2% | ≤5% | ≤10% | ≤25% | p50 | p90 | p99 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| mutool↔mutool | mixed | 388 | 100% | 100% | 100% | 100% | 100% | 100% | 0.0 | 0.0 | 0.0 |
| pdfium↔pdfjs | general | 296 | 70.3% | 75.0% | 81.1% | 86.1% | 91.9% | 97.0% | 0.06 | 7.89 | 39.0 |
| pdfium↔mutool | general | 584 | 68.2% | 73.3% | 80.5% | 86.3% | 94.2% | 97.9% | 0.06 | 7.24 | 29.3 |
| pdfjs↔mutool | general | 586 | 70.6% | 76.5% | 82.3% | 88.1% | 94.5% | 96.9% | 0.05 | 5.84 | 37.7 |
| pdfium↔pdfjs | ghent | 95 | 0% | 0% | 0% | 2.1% | 30.5% | 77.9% | 15.97 | 36.9 | 54.1 |
| pdfium↔mutool | ghent | 190 | 0% | 0% | 0% | 11.6% | 38.9% | 76.8% | 17.91 | 36.9 | 55.0 |
| pdfjs↔mutool | ghent | 190 | 0% | 0% | 0% | 2.1% | 23.2% | 73.7% | 16.93 | 39.2 | 54.7 |

For reference, selis↔mutool over the full comparable corpus (SL-2.CONF.01, same metric):
27.3% / 34.1% / 76.2% / 85.1% / 92.0% / 97.0% at the same bands, p50 = 1.68, p90 = 8.55,
p99 = 68.4 (n = 3,747).

**What this calibrates:**

* The original G2 criterion (≤0.5% differing pixels on ≥95% of the corpus @150) is **below the
  independent-renderer noise floor**: the best oracle pair reaches 70.6% at ≤0.5% on general
  content and 0% on prepress. No renderer pair satisfies it at any band below ≤25%.
* **Recommended calibrated G2 bar** (recorded in `12-PHASE-2-render.md`, consumed by
  SL-2.CONF.02): **Bar A** — ≥95% of comparable pages ≤25% differing pixels @150 (every oracle
  pair passes); **Bar B** — selis's ≤2%/≤5%/≤10% band fractions within 5pp of the best oracle
  pair (5pp = observed pair spread + n≈300 sampling noise). The ≤0.5%/≤1% strict bands and the
  Ghent-class CDFs stay published as tracked fidelity metrics, not gates.
* Selis's current position: inside the oracle envelope from ≤2% upward (76.2/85.1/92.0/97.0 vs
  the pairs' 80.5–82.3/86.1–88.1/91.9–94.5/96.9–97.9), failing Bar B at ≤2% by 1.1pp, with the
  deficit concentrated in a uniform 1–2% band excess (SL-2.RAST.14) and a heavy tail that is the
  filed bug clusters (p99 68.4 vs pairs' 29.3–39.0).
* Re-baselining used calibration data only; per §5, tolerances are never adjusted because a build
  is red.
* **CI confirmation (2026-09-11, SL-2.CONF.02):** run 34567473855 completed green but measured 0
  comparable pages (relative docker `-v` output mounts + a `mutool`-vs-`mupdf` pin lookup miss;
  both fixed on the CONF.02 branch with regression tests). The matrix stands on the local legs;
  the pinned-identity confirmation re-runs post-merge. Full record in `12-PHASE-2-render.md`
  SL-2.CONF.02.

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
   Object counts are compared **live-vs-live** (SL-0.ORACLE.03, verified on
   real files): an object is live iff its *latest* xref entry across all
   revisions is in use; object 0 never counts; qpdf's `obj:` map keys are the
   oracle side (`maxobjectid` counts slots incl. the free head and is never
   the live count). `selis inspect --json` exposes per-revision `free`
   arrays so the comparator can apply qpdf's semantics. This closed the two
   artefact classes the first seeded run surfaced: the phantom free-head
   delta on every healthy file, and the union-vs-live skew on files with
   deleted objects. Damaged-but-recoverable files stay in the comparable pool
   via `qpdf --warning-exit-0` (qpdf otherwise exits 2 and emits no JSON for
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
synthetic incl. 62 seeded mutants) × qpdf 12.4.1, live-vs-live comparator →
**17 clusters**:

| Cluster | Files | Verdict |
|---|---|---|
| `match` | 402 | — (agreement; 86.8% of the sample) |
| `selis_rejects` | 21 | `SpecAmbiguous` — deliberate mutants we refuse by design (typed error, no repair attempt); both readings defensible |
| `qpdf_rejects` | 21 | `SpecAmbiguous` — we open (incl. reconstruction) where qpdf refuses hard: pdf.js-corpus pathologies (page-tree loops, unrecoverable `/Root`), the crypto fixtures qpdf will not open password-less, and mutants qpdf cannot recover |
| `both_reject` | 6 | — (agreement on broken files; typed codes already recorded) |
| `obj_delta=87…2163` (govdocs, 9 files) | 9 | `SpecAmbiguous` — damaged files where the two tools read free markers and recovery sets differently; ours honours the spec's free-list uniformly |
| `obj_delta=1 / 17 / 47` (issue5874/11656/16263) | 3 | `SpecAmbiguous` — qpdf's JSON map counts xref-*stream* free-marked objects as live (it honours free markers in classic tables only); we honour the free marker in both forms |
| `obj_delta=2` (issue15716) | 1 | `SpecAmbiguous` — the xref size is internally inconsistent (qpdf's own warning: "reported number of objects (8) is not one plus the highest object number (13)"); repair readings differ |

The remaining obj_delta clusters are genuine tool divergences on broken
files, each annotated with both readings — not comparator artefacts. If a
future comparator change dissolves a cluster, `oracle triage --clear
<signature>` removes its stale annotations.

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
