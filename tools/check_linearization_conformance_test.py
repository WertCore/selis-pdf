#!/usr/bin/env python3
"""Test driver for the independent Annex F conformance checker.

Generates linearized outputs from four fixture *classes* (multi-page with a
part-8 shared object, outline, AcroForm, optional hint tables), runs the
conformance checker, its negative controls and the separately labelled interop
checks, and prints the exact verdict for each.

Inputs
------
* multi-page: generated on the fly by make_lin_conformance_fixtures.py (three
  pages, distinct ink, plus a Form XObject shared by pages 2-3 only so the F.5
  table has a real first-shared object).
* outline / form / hints: existing corpus/fixtures PDFs (outlines_for_editor,
  form_two_pages, lin_optional_hints). verification_features is an extra
  generic multi-page input.

Linearization
-------------
By default the driver runs `target/debug/examples/linearize.exe` (build once
with `cargo build -p selis-pdf-tools --example linearize`). Pass --no-build to
check the committed snapshot vectors in corpus/fixtures/lin_conformance/
instead; pass --binary PATH to use a different executable.

Exit 0 when conformance, every negative control and interop pass for every
fixture; 1 otherwise; 2 on usage/setup error.
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS))

import check_linearization_conformance as CC  # noqa: E402
import make_lin_conformance_fixtures as MF  # noqa: E402

REPO = TOOLS.parent
FIX = REPO / "corpus" / "fixtures"
VECTORS = FIX / "lin_conformance"
DEFAULT_BINARY = REPO / "target" / "debug" / "examples" / "linearize.exe"

# (label, input filename | None for generated, committed vector name, pages, ink)
CASES = [
    ("multi-page (shared object)", None, "lin_multipage.pdf", 3, True),
    ("outline", "outlines_for_editor.pdf", "lin_outline.pdf", 5, False),
    ("form (AcroForm)", "form_two_pages.pdf", "lin_form.pdf", 2, True),
    ("hint tables (labels+thumb)", "lin_optional_hints.pdf", "lin_hints.pdf", 2, True),
    ("verification features", "verification_features.pdf", "lin_verification.pdf", 2, True),
]


def run_binary(binary: Path, src: Path, dst: Path) -> None:
    proc = subprocess.run(
        [str(binary), str(src), str(dst)], capture_output=True, text=True
    )
    if proc.returncode != 0:
        raise RuntimeError(f"linearize failed for {src.name}: {proc.stderr.strip()}")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    ap.add_argument(
        "--no-build",
        action="store_true",
        help="check committed snapshot vectors instead of regenerating",
    )
    ap.add_argument("--keep", type=Path, help="keep generated outputs in this directory")
    args = ap.parse_args(argv)

    work = args.keep if args.keep else Path(tempfile.mkdtemp(prefix="lin-conformance-"))
    work.mkdir(parents=True, exist_ok=True)

    if not args.no_build and not args.binary.is_file():
        print(f"error: linearize binary not found at {args.binary}; build it or pass --no-build")
        return 2

    all_ok = True
    for label, source, vector, pages, ink in CASES:
        out = work / vector
        if args.no_build:
            src = VECTORS / vector
            if not src.is_file():
                print(f"[{label}] missing committed vector {src}")
                all_ok = False
                continue
            shutil.copyfile(src, out)
        else:
            if source is None:
                src = work / "multipage_input.pdf"
                MF.make_multipage(src)
            else:
                src = FIX / source
            run_binary(args.binary, src, out)

        data = out.read_bytes()
        conformance = CC.conformance_check(data, f"{label}: {out.name}")
        negative = CC.negative_controls(data, f"{label}: {out.name}")
        interop = CC.interop_check(str(out), pages, require_ink=ink)

        print(f"\n### {label}  ({out.name}, {len(data)} bytes, /N={pages})")
        for res in (conformance, negative, interop):
            summary = res.by_category()
            counts = ", ".join(f"{c}:{p}/{t}" for c, (p, t) in sorted(summary.items()))
            print(f"  {'PASS' if res.ok else 'FAIL'} {counts}")
            for failure in res.failed():
                print(f"      - {failure}")
        all_ok &= conformance.ok and negative.ok and interop.ok

    print(f"\nOVERALL: {'PASS' if all_ok else 'FAIL'}")
    return 0 if all_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
