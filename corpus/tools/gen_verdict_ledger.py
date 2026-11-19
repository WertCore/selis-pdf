#!/usr/bin/env python3
"""Generate the TEXT divergence-cohort verdict ledger (SL-3.CONF / TEXT.14-25).

The ledger records what the conformance sweep *actually* established. It is
deliberately explicit about provenance: a verdict derived from a local
read-back test, and a verdict inherited from the cluster analysis in
`pdf-plan/13-PHASE-3-text.md` but never re-measured locally, are NOT the same
claim. Every record says which it is.

Why there are no fresh per-file sweep verdicts in this tree:

* the harness (`xtask text-sweep`) reads PDFs from `corpus/pdfs`; that directory
  is fetch-only and is NOT present in any worktree (SL-0.LEGAL.04),
* the text oracles it compares against (mutool locally, PDFium/pdf.js in the
  pinned CI images) are not installed on the dev host and Docker is absent,
* therefore neither the similarity numbers nor the *membership of the
  divergence-tail subset* of a cohort can be re-derived here. The expect tree
  holds the whole corpus (e.g. 429 PDF_UA-1/2 files), the tail is a much
  smaller documented subset (98 files for TEXT.17). Inventing per-file tail
  verdicts from the full set would be fabrication, so the ledger does not.

Run from the repository root:

    python corpus/tools/gen_verdict_ledger.py

Outputs `corpus/expect/<cohort>/verdicts.jsonl` (one JSON object per line).
"""

from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EXPECT = ROOT / "corpus" / "expect"

# (dir, task, tail count, verdict, signature, provenance, evidence, note)
COHORTS = {
    "annotation-appearance": {
        "task": "SL-3.TEXT.14",
        "tail_documented": 62,
        "verdict": "Fixed",
        "signature": "empty_selis -> match",
        "provenance": "readback-test",
        "evidence": (
            "SL-3.TEXT.14 landed at a401ff8 (0bb8ff2); "
            "apps/cli/tests/text14_annot_ap.rs 3/3 pass 2026-11-19 "
            "(empty page, page+AP order, search)"
        ),
        "note": (
            "Per-file tail ledger NOT produced: corpus/pdfs and the text "
            "oracles are absent, so the 62-file membership cannot be re-measured."
        ),
        "pattern": None,
    },
    "tagged-reading-order": {
        "task": "SL-3.TEXT.17",
        "tail_documented": 98,
        "verdict": "OurBug",
        "signature": "diff>=25 / verapdf-ua-reading-order",
        "provenance": "documented-cluster",
        "evidence": (
            "pdf-plan/13-PHASE-3-text.md SL-3.TEXT.17: MuPDF recovers a "
            "structure-tree-tagged order; selis uses the geometry fallback"
        ),
        "note": (
            "STILL OPEN. {n} PDF_UA-1/2 expect files exist but only 98 are in "
            "the documented divergence tail; membership is not re-derivable "
            "without the PDFs + oracles, so no per-file rows are claimed."
        ),
        "pattern": ("verapdf/PDF_UA-1/", "verapdf/PDF_UA-2/"),
        "pattern_mode": "any",
    },
    "type0-cmap": {
        "task": "SL-3.TEXT.19",
        "tail_documented": 47,
        "verdict": "OurBug",
        "signature": "diff>=25 / verapdf-cmap-composite (missing chars)",
        "provenance": "documented-cluster+readback-fix",
        "evidence": (
            "SL-3.TEXT.19: ToUnicode bfrange explicit-array form was dropped "
            "wholesale; pre-fix CLI read-back returned raw CIDs, post-fix "
            "extracts the mapped text (apps/cli/tests/text19_bfrange_array.rs)"
        ),
        "note": (
            "One confirmed cause FIXED (bfrange array, selis-font::cmapfile). "
            "18 6-2-11-3* expect files are enumerated below as candidates, NOT "
            "as verified array-form cases; per-file re-measure is pending CI. "
            "Other TEXT.19 causes may remain in the 47-file tail."
        ),
        "pattern": ("verapdf/", "6-2-11-3"),
        "pattern_mode": "all",
    },
    # --- Bucket 1 (2026-11-20) -------------------------------------------------
    # Every row below is a COHORT verdict. No per-file tail membership is
    # claimed: corpus/pdfs and the text oracles are absent, so the divergence
    # tail subset of each cohort cannot be re-derived here. A "measured" flag
    # is true ONLY where a local read-back test verified a concrete mechanism;
    # it never means a corpus file was swept.
    "silent-empty-extraction": {
        "task": "SL-3.TEXT.10",
        "tail_documented": 62,
        "verdict": "OurBug",
        "signature": "empty_selis (text drawn, nothing recovered)",
        "provenance": "readback-fix+documented-cluster",
        "evidence": (
            "SL-3.TEXT.10: BT/ET state-reset fix + LOW_CONFIDENCE_MARKER landed "
            "at base; apps/cli/tests/b1_text_font.rs pins the typed marker. "
            "62-file tail is annotation-AP text (owned by TEXT.14)."
        ),
        "note": (
            "Mechanism verified locally; 62-file membership NOT re-measured "
            "(corpus + oracles absent). Annotation-AP walk closed at base."
        ),
        "pattern": None,
    },
    "divergent-no-recovery": {
        "task": "SL-3.TEXT.15",
        "tail_documented": 0,
        "verdict": "OurBug",
        "signature": "text_err / >5% divergence with zero recovery",
        "provenance": "readback-fix",
        "evidence": (
            "SL-3.TEXT.15: typed ExtractionVerdict + >=5% undecodable signal "
            "implemented in selis-pdf-text::verdict and wired into the CLI JSON "
            "verdict field; apps/cli/tests/b1_text_font.rs pins flagged and "
            "exact-match-neighbour pages."
        ),
        "note": (
            "LOCAL PROXY verified: fraction of drawn glyphs the recovery chain "
            "could not decode. The literal differential >5% vs an oracle needs "
            "the corpus + text-sweep; sweep text_err wiring is not yet measured."
        ),
        "pattern": None,
    },
    "cjk-asian": {
        "task": "SL-3.TEXT.20",
        "tail_documented": 23,
        "verdict": "OurBug",
        "signature": "diff>=25 / flat-cjk + noembed-cjk-external",
        "provenance": "readback-mechanism+documented-cluster",
        "evidence": (
            "SL-3.TEXT.20: Uni...UCS2... codes recovered explicitly in "
            "Session::text_unicode (they are Unicode scalars); "
            "apps/cli/tests/b1_text_font.rs pins UniGB-UCS2-H CJK with and "
            "without ToUnicode."
        ),
        "note": (
            "Mechanism verified locally on a synthetic Type0 page. The 20 "
            "flat-cjk + 3 noembed files (KozMin/Ryumin/GBKp/...) were NOT "
            "re-measured; JIS7/SJIS/EUC-JP predefined CMaps need the corpus."
        ),
        "pattern": None,
    },
    "latex-cm-fonts": {
        "task": "SL-3.TEXT.21",
        "tail_documented": 16,
        "verdict": "OurBug",
        "signature": "diff>=25 / latex-cm-fonts (sim 0.70-0.72)",
        "provenance": "readback-mechanism+documented-cluster",
        "evidence": (
            "SL-3.TEXT.21: CM glyph names via /Encoding Differences -> AGL "
            "recovered; apps/cli/tests/b1_text_font.rs pins alpha/beta/Delta/"
            "summation -> Greek/math Unicode."
        ),
        "note": (
            "Mechanism verified locally; the 16-file shared-signature cohort "
            "(tracemonkey + 15) was NOT re-measured against an oracle."
        ),
        "pattern": None,
    },
    "rtl-reading-order": {
        "task": "SL-3.TEXT.22",
        "tail_documented": 15,
        "verdict": "OurBug",
        "signature": "diff>=25 / rtl-flagged",
        "provenance": "readback-mechanism+documented-cluster",
        "evidence": (
            "SL-3.TEXT.22: logical-order RTL survives extraction "
            "(apps/cli/tests/b1_text_font.rs); selis-shape bidi reorder_text "
            "recovers logical from visual RTL (unit test)."
        ),
        "note": (
            "Visual->logical reorder is NOT wired into default extraction: "
            "whether a producer emits visual order is not locally decidable "
            "and the normaliser folds no bidi. The 15 rtl-flagged tail files "
            "need corpus + oracle verdicts."
        ),
        "pattern": None,
    },
    "identity-cid": {
        "task": "SL-3.TEXT.23",
        "tail_documented": 10,
        "verdict": "OurBug",
        "signature": "diff>=25 / flat-cid-identity (sim 0.0)",
        "provenance": "readback-mechanism+documented-cluster",
        "evidence": (
            "SL-3.TEXT.23: Identity-H + /ToUnicode bfchar recovers mapped text "
            "end-to-end (apps/cli/tests/b1_text_font.rs)."
        ),
        "note": (
            "Identity-H mapping verified locally; Identity-V vertical writes "
            "and the 10-file tail need corpus + oracle re-measure."
        ),
        "pattern": None,
    },
    "type3-text": {
        "task": "SL-3.TEXT.24",
        "tail_documented": 8,
        "verdict": "OurBug",
        "signature": "diff>=25 / type3 (has_type3 10/63 in G3)",
        "provenance": "readback-mechanism+documented-cluster",
        "evidence": (
            "SL-3.TEXT.24: Type3 glyph names via /Encoding recover text "
            "(apps/cli/tests/b1_text_font.rs pins two /CharProcs)."
        ),
        "note": (
            "Mechanism verified locally; the 8-file tail (and glyph-procedure "
            "embedded text) need corpus + oracle re-measure."
        ),
        "pattern": None,
    },
    "cjk-subset-payload": {
        "task": "SL-3.FONT.10",
        "tail_documented": 0,
        "verdict": "OurBug",
        "signature": "FONT.10 web half: production caller + browser harness",
        "provenance": "documented-gap",
        "evidence": (
            "SL-3.FONT.10: engine lazy-CJK payload/protocol measured at base; "
            "typed subset-substitution disclosure added "
            "(selis_font::cjk::CjkSubstitutionNote)."
        ),
        "note": (
            "STILL OPEN: no production caller of LazyPayloadClient, no browser "
            "harness, no Cache Storage adapter. Web half unbuilt; cannot be "
            "closed from a branch that does not push."
        ),
        "pattern": None,
    },
    "font-parser-panic": {
        "task": "SL-1.ROB.06",
        "tail_documented": 0,
        "verdict": "OurBug",
        "signature": "upstream font-parser overflow-panic containment",
        "provenance": "readback-test",
        "evidence": (
            "SL-1.ROB.06: synthetic hostile bfrange/SFNT/Type1 inputs verified "
            "to terminate with typed deviations, no panic "
            "(crates/selis-font/tests/fuzz_regressions.rs, "
            "apps/cli/tests/b1_text_font.rs)."
        ),
        "note": (
            "Containment verified locally under the fuzz profile. DoD's "
            "upgrade-drop-backstops + full instrumented Linux soak still needs "
            "overflow-clean upstream releases and CI; not closable here."
        ),
        "pattern": None,
    },
}


def expect_ids() -> list[str]:
    return sorted(
        p.relative_to(EXPECT).as_posix()[: -len(".toml")]
        for p in EXPECT.rglob("*.toml")
    )


def matches(ident: str, pattern: tuple[str, ...] | None, mode: str = "all") -> bool:
    if pattern is None:
        return False
    if mode == "any":
        return any(part in ident for part in pattern)
    return all(part in ident for part in pattern)


def check() -> None:
    """Validate the committed ledger: schema + honesty invariants."""
    ids = expect_ids()
    verdicts = {"Fixed", "OurBug", "OracleBug"}
    seen_files = 0
    for cohort, spec in COHORTS.items():
        path = EXPECT / cohort / "verdicts.jsonl"
        rows = [
            json.loads(line)
            for line in path.read_text(encoding="utf-8").splitlines()
            if line.strip()
        ]
        if not rows:
            raise AssertionError(f"{cohort}: empty ledger")
        head = rows[0]
        assert head["scope"] == "cohort", f"{cohort}: first row must be the cohort"
        assert head["verdict"] in verdicts, f"{cohort}: bad verdict"
        assert head["task"] == spec["task"], f"{cohort}: task drift"
        assert head["cohort"] == cohort, f"{cohort}: cohort drift"
        selected = [
            i
            for i in ids
            if matches(i, spec["pattern"], spec.get("pattern_mode", "all"))
        ]
        assert head["expect_files_in_pattern"] == len(selected), (
            f"{cohort}: pattern count drift"
        )
        assert head["tail_files_documented"] == spec["tail_documented"], (
            f"{cohort}: tail count drift"
        )
        # A verdict is "measured" only when it came from a real read-back test.
        if head["measured"]:
            assert head["provenance"].startswith("readback"), (
                f"{cohort}: measured=True without readback provenance"
            )
        for row in rows[1:]:
            assert row["scope"] == "file", f"{cohort}: rows after the first are files"
            assert row["id"] in selected, f"{cohort}: file not in cohort pattern: {row['id']}"
            assert row["expect_file"] == f"corpus/expect/{row['id']}.toml"
            assert row["verdict"] in verdicts
            assert row["measured"] is False, f"{cohort}: unmeasured row claims measured"
            assert row["fixed_mechanism"] and row["evidence"]
            seen_files += 1
    # Only the cohort with a deterministic per-file pattern may carry file rows.
    assert seen_files == 18, f"expected 18 per-file candidate rows, got {seen_files}"
    print(f"ledger check OK ({len(COHORTS)} cohorts, {seen_files} per-file candidate rows)")


def main() -> None:
    if "--check" in __import__("sys").argv:
        check()
        return
    ids = expect_ids()
    for cohort, spec in COHORTS.items():
        directory = EXPECT / cohort
        directory.mkdir(parents=True, exist_ok=True)
        selected = [
            i
            for i in ids
            if matches(i, spec["pattern"], spec.get("pattern_mode", "all"))
        ]
        records = [
            {
                "id": f"cohort:{cohort}",
                "scope": "cohort",
                "cohort": cohort,
                "task": spec["task"],
                "verdict": spec["verdict"],
                "signature": spec["signature"],
                "expect_files_in_pattern": len(selected),
                "tail_files_documented": spec["tail_documented"],
                "measured": spec["provenance"].startswith("readback"),
                "provenance": spec["provenance"],
                "evidence": spec["evidence"],
                "note": spec["note"].format(n=len(selected)),
            }
        ]
        # Per-file rows are emitted ONLY where the pattern is an exact,
        # deterministic cohort definition (not a superset of the tail).
        if cohort == "type0-cmap":
            for ident in selected:
                records.append(
                    {
                        "id": ident,
                        "scope": "file",
                        "cohort": cohort,
                        "task": spec["task"],
                        "verdict": "OurBug",
                        "signature": spec["signature"],
                        "expect_file": f"corpus/expect/{ident}.toml",
                        "measured": False,
                        "provenance": "documented-cluster",
                        "fixed_mechanism": "ToUnicode bfrange explicit-array (SL-3.TEXT.19)",
                        "evidence": spec["evidence"],
                        "note": (
                            "Candidate: array-form ToUnicode not confirmed per "
                            "file without the source PDF; re-measure in CI."
                        ),
                    }
                )
        out = directory / "verdicts.jsonl"
        with out.open("w", encoding="utf-8", newline="\n") as fh:
            for rec in records:
                fh.write(json.dumps(rec, ensure_ascii=False, sort_keys=True) + "\n")
        print(f"{cohort}: {len(records)} record(s) -> {out.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
