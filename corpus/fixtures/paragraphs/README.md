# Paragraph labelled set (SL-5.TEXTEDIT.01)

A **small, synthetic** labelled set used to exercise paragraph reconstruction
and its confidence, because the DoD's 500-page hand-labelled set is **not
present in this repository**.

* `manifest.toml` — 12 synthetic pages, described at the assembled-line level
  (baseline `y`, em `size`, font, optional MCID, text) with hand-labelled
  `paragraph_ends`. Six pages are tagged, six are untagged; fonts, sizes, line
  counts and gaps vary. Paragraph boundaries are unambiguous by construction
  (within-gaps ≤ 1.5 em, between-gaps = 3.4 em against a 1.6 em grouping bar).
* `two_paragraphs.pdf` — a real minimal PDF with four text lines in two
  paragraphs, used by the engine-level read-back test.
* `generate.py` — regenerates both files.

This set supports the claim "confidence ranks correct groupings above incorrect
ones" on **12 synthetic pages**. It does **not** support the DoD's 500-page
correlation, which remains unmet. Consequently `PROVISIONAL_REFLOW_THRESHOLD`
in `selis-pdf-text::paragraphs` is a provisional midpoint derived from this
small set, not a measured decision boundary. On this 12-page set the harness
measures min(correct) = 0.918, max(incorrect) = 0.681, midpoint = 0.799 — which
is where the provisional 0.80 threshold comes from.

To extend it honestly: add real, hand-labelled pages to `manifest.toml`, re-run
the harness (`cargo test -p selis-pdf-text labelled_set`), and re-derive the
threshold from the measured correct/incorrect separation.
