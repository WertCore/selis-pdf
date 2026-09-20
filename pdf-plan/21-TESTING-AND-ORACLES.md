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
images — which have measured **0 comparable** since 2026-09-14 on two evidenced counts (runs
34806315351/34946893403): `Unable to find image` for the superseded 2026-09-09 pdfium/pdfjs/
mupdf digests — re-recorded from the 2026-09-14 push (run 34820871908, drivers with `--text`) —
and, on legs that pull, the by-file bind turning into a directory (`cannot write /out.img`,
EISDIR, `Device or resource busy`) plus the text plan's lost `mutool`→`mupdf` alias and relative
`oracle.txt` volume name. SL-3.CONF.02 re-pins and restores the smoke contract's `/out` bind with
tests. Any pre-09-14 CI cell is unconfirmed from retained artifacts; CONF.06 asserts the first
credible run on real comparable counts.

**CONF.06 status (2026-09-20):** the detection half is now code, not prose —
`xtask/src/sweep.rs::check_comparable_or_fail` and its mirror in
`xtask/src/text_sweep.rs` run after the report is written and fail the step when a
leg's comparable count is 0, quoting the first three typed details, so `render-conf`
can no longer stay green while measuring nothing (and the failing step's report still
uploads — the artifact steps carry `if: always()`). The measurement half is still
outstanding: the last scheduled run is 34946893403 (2026-09-15, `03ece8ba`), which
predates the SL-3.CONF.02 bind fix, so no post-fix scheduled `render-conf` exists and
every PDFium/pdf.js cell in this table stays unmeasured. Cron is `0 3 */2 * *` (the
next window after the 2026-09-20 implementation is 2026-09-22 03:00 UTC); dispatch is
push-scoped and the branch does not push, so the first credible run lands post-merge.
Until then the only measured cells here are the local legs.
    Sample: 300-file stride over the general corpus + the full 95-file Ghent suite. Fraction
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

Re-measured by SL-3.CONF.02 on the merged tree (3,848+12 → 3,860 files, same local
mutool-1.23.0 + ΔE76>2.3 metric; SL-3.CONF.05 re-ran it on the post-TEXT.11 tree and the
merged numbers quoted here are that re-run — the pre-TEXT.11 pass differed by ≤0.3pp
everywhere): general ≤0.5% 67.94, ≤1% 74.91, ≤2% 80.90, ≤5% 86.86, ≤10% 92.58,
≤25% 97.19, p50 0.07, p95 13.79 (n=3,774 @150; @72 65.64/75.28/79.59/85.18/90.55/96.72
n=3,778; @300 70.52/77.1/82.73/88.86/94.32/97.48 n=3,769) — the p50 shift and the whole
1–2% excess are the RAST.14 + BT-fix effect; the ghent class is unchanged at ≤0.5% 0 /
≤10% 12.6 / ≤25% 60.0 (n=95), and TEXT.11 is band-neutral for pixels. The bar itself is
unchanged — the envelope standards stay the oracle
pairs above. CI legs vs PDFium/pdf.js remain **unmeasured** on any merged tree: the
sweeps reach the containers fine but die on our own output bind (docker turns a
not-yet-written `-v` source into a directory → `cannot write /out.img` on the render
legs, `Device or resource busy` on the pair legs, `no [tool.mutool] pin` + a relative
`oracle.txt` volume name on the text legs — runs 34806315351 and 34946893403). SL-3.CONF.02
fixes the binds/alias and re-records the run-34820871908 digests (the `--text` driver modes
live there); the first scheduled post-merge run is what finally confirms them.

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
  SL-2.CONF.02. **Correction (SL-3.CONF.02, 2026-09-16):** the CONF.02 fix was incomplete — it
  absolutised the *file* bind, and docker turns an absent host *file* into a *directory* inside
  the container, so the merged post-fix runs (34806315351 on text legs, 34946893403 on render +
  calibration pairs) still return 0 comparable with `cannot write /out.img` / EBUSY; the text-plan
  pin alias was missed entirely. Fixed properly this wave (`/out` parent-directory binds +
  `pin_id`), with tests; the confirmation still needs one post-merge scheduled `render-conf`.

### The measured text-extraction calibration (SL-0.ORACLE.04, 2026-09-14)

`xtask oracle text-sweep --only-pair pdfium+pdfjs --only-pair pdfium+mutool --only-pair
pdfjs+mutool` extracts page 1 with every named text oracle and scores all pairs *before* selis
is compared against them — the extract analogue of the matrix above, under the one normaliser
(`xtask/src/text_norm.rs`, N1–N6) and the four-decimal normalised edit-distance similarity the
CONF.01 verdicts carry. Sample: the 644-file `smoke` corpus (pdf.js test suite + PDF Association
examples). `n` counts comparable files (both sides produced text; blank-vs-text and rejections
are signed, not scored). Fraction of comparable files within each similarity band:

| Pair | comparable of 644 | ≥0.99 | ≥0.98 | ≥0.95 | ≥0.75 | mean(1−sim)% | p50 | p75 | p90 |
|---|---|---|---|---|---|---|---|---|---|
| mutool↔pdfium | 480 | 72.1% | 72.7% | 74.2% | 78.8% | 19.63 | 0.0 | 7.81 | 100.0 |
| mutool↔pdfjs | 466 | 71.9% | 72.7% | 74.2% | 79.8% | 19.11 | 0.0 | 5.73 | 100.0 |
| **pdfium↔pdfjs** | 472 | **78.4%** | **79.0%** | **80.5%** | **85.6%** | **13.07** | 0.0 | 0.0 | 83.11 |

Run identities: PDFium `chromium/7961` (bblanchon win-x64 binary, our driver, same pin as the
container artifact's revision), pdf.js `pdfjs-dist 6.2.108` (`npm ci` over the committed lockfile
— the pinned identity), MuPDF local `mutool 1.23.0` (pin is 1.23.9; drift recorded, the headline
pair is the two that matched their pins). Artifacts: `C:\selis-build\oracle04-text-smoke`.
**CI re-pin + bind status (SL-3.CONF.02, 2026-09-14/16):** the `oracle-images` job
rebuilt and pushed all five images (run 34820871908) — the pdfium/pdfjs manifests now
carry the `--text` driver modes, and `xtask/oracles.toml` re-pins to them. That fixes the
half that killed *pulls*: the superseded 2026-09-09 digests no longer resolve
(`Unable to find image` — pdfium `@162ef39f`, pdfjs `@9105570`, mupdf `@89be099d`). The
scheduled `render-conf` legs then died a second way, in the harness: the **by-file output
bind** — docker turns a host file that does not exist yet into a *directory*, so render
legs report `pdfium_driver: cannot write /out.img` / `node:fs` EISDIR, pair legs
`cannot remove '/out.img': Device or resource busy`, and the text sweep adds
`mutool: no [tool.mutool] pin recorded` (the `mutool`→`mupdf` alias was fixed for
render legs but missed in the text plan) and a relative `sweep-text/tmp-w0/oracle.txt`
`-v` rejected as a volume name (runs 34806315351, 34746691823, and the 2026-09-15
schedule on `03ece8ba`, 34946893403 — every leg typed, 0 comparable). Both binds now
match the smoke contract (`out_dir_bind()`: parent directory, created and absolute;
`pin_id()` in the text plan) with regression tests in `xtask/src/oracle.rs`.
Dispatches are push-scoped, so the next scheduled run after that merge prints the first
credible PDFium/pdf.js cells; *any §4 cell quoted from CI before 2026-09-14 is
unconfirmed from retained artifacts*, and SL-3.CONF.06 is what asserts on the counts
instead of green-by-default. The mutool legs stay measured either way (local 1.23.0,
container 1.23.9, drift recorded).

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
  SL-2.CONF.02. **Correction (SL-3.CONF.02, 2026-09-16):** the CONF.02 fix was incomplete — it
  absolutised the *file* bind, and docker turns an absent host *file* into a *directory* inside
  the container, so the merged post-fix runs (34806315351 on text legs, 34946893403 on render +
  calibration pairs) still return 0 comparable with `cannot write /out.img` / EBUSY; the text-plan
  pin alias was missed entirely. Fixed properly this wave (`/out` parent-directory binds +
  `pin_id`), with tests; the confirmation still needs one post-merge scheduled `render-conf`.

### The measured text-extraction calibration (SL-0.ORACLE.04, 2026-09-14)

`xtask oracle text-sweep --only-pair pdfium+pdfjs --only-pair pdfium+mutool --only-pair
pdfjs+mutool` extracts page 1 with every named text oracle and scores all pairs *before* selis
is compared against them — the extract analogue of the matrix above, under the one normaliser
(`xtask/src/text_norm.rs`, N1–N6) and the four-decimal normalised edit-distance similarity the
CONF.01 verdicts carry. Sample: the 644-file `smoke` corpus (pdf.js test suite + PDF Association
examples). `n` counts comparable files (both sides produced text; blank-vs-text and rejections
are signed, not scored). Fraction of comparable files within each similarity band:

| Pair | comparable of 644 | ≥0.99 | ≥0.98 | ≥0.95 | ≥0.75 | mean(1−sim)% | p50 | p75 | p90 |
|---|---|---|---|---|---|---|---|---|---|
| mutool↔pdfium | 480 | 72.1% | 72.7% | 74.2% | 78.8% | 19.63 | 0.0 | 7.81 | 100.0 |
| mutool↔pdfjs | 466 | 71.9% | 72.7% | 74.2% | 79.8% | 19.11 | 0.0 | 5.73 | 100.0 |
| **pdfium↔pdfjs** | 472 | **78.4%** | **79.0%** | **80.5%** | **85.6%** | **13.07** | 0.0 | 0.0 | 83.11 |

Run identities: PDFium `chromium/7961` (bblanchon win-x64 binary, our driver, same pin as the
container artifact's revision), pdf.js `pdfjs-dist 6.2.108` (`npm ci` over the committed lockfile
— the pinned identity), MuPDF local `mutool 1.23.0` (pin is 1.23.9; drift recorded, the headline
pair is the two that matched their pins). Artifacts: `C:\selis-build\oracle04-text-smoke`.
**CI re-pin status (SL-3.CONF.02, 2026-09-14/16):** the `oracle-images` job rebuilt and pushed
all five images (run 34820871908) — the pdfium/pdfjs manifests are now the ones whose drivers
carry the `--text` modes, and `xtask/oracles.toml` records their digests. The re-pin is *not*
what had been stopping the scheduled `render-conf` legs, though: the legs failed on the harness's
own output bind — a `-v` of a host file that does not exist yet becomes a *directory* inside the
container, so the tools reported `pdfium_driver: cannot write /out.img` / `node:fs` EISDIR and the
pair/calibration legs reported `cannot remove '/out.img': Device or resource busy` (runs
34806315351 render sweep, 34946893403 render sweep + calibration, every leg `oracle_rejects`, 0
comparable) — plus, on the text sweep, a `mutool: no [tool.mutool] pin` alias miss and a relative
`sweep-text/tmp-w0/oracle.txt` `-v` source rejected as a volume name (run 34946893403). Both bind
shapes now match the smoke runs (`/out/` directory binds with an absolutised, created parent, plus
`pin_id` aliasing; regression tests in
`xtask/src/oracle.rs`), so the first green PDFium/pdf.js text and pair legs arrive with the next
post-merge scheduled `render-conf`; dispatches are push-scoped and this branch does not push.
The mutool legs — local install 1.23.0 and container 1.23.9 (drift recorded above) — stay
measured either way.

**What this calibrates:**

* The independent extractors agree at ≥0.98 (the G3 bar) on at most **79.0%** of text-bearing
  smoke pages — and the smoke corpus is curated pathology. Demanding ≥95% of a corpus at ≥0.98
  from selis while PDFium and pdf.js themselves reach ~79% on this file class measures noise
  first, exactly as G2 did for pixels. The SL-3.CONF.02 promotion must read the G3 criterion
  against this matrix (and against whatever the *extraction* corpus scores; the smoke corpus is
  the reference point, not the gate corpus).
* The distribution is bimodal, not graded: p50 = p75 = 0.0 (three quarters of pairs are
  byte-identical after normalisation) while the ≥0.75→1.0 tail (68–102 files per pair) is
  near-total disagreement — Type3/anonymous fonts, CID/RTL reading order, and empty-vs-text
  cases. Those clusters are triage fodder (§5), not tolerance knobs: per this section's own
  rule, tolerances move with calibration data, but *never* because a build is red.
* The 100%-similarity floor of p95/p99 for pairs involving MuPDF (and p90 for pdfium↔pdfjs)
  says independent extractors can recover *entirely different* text from one page; a lone
  selis disagreement in this band is not yet an OurBug.

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

Text-extraction triage inherits the same rule from its own baseline: the
oracle-vs-oracle calibration table above (§4, SL-0.ORACLE.04) is the noise
floor every selis-vs-oracle `diff>=25` cluster is judged against, and it was
recorded *before* the G3 bar is read anywhere.

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
