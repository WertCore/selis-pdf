#!/usr/bin/env python3
"""Test driver for the independent Annex F optional-hint decoder.

Runs `check_linearization_hints_independent.py` over the committed linearized
vectors that exercise every optional table (F.6 outline, F.7 thumbnail, F.8
named destination, F.9 page label, F.10 form), with built-in negative controls,
and checks that:

* every linearized vector PASSES decode + negative controls;
* a deliberately non-linearized source fixture FAILS (the decoder is not a
  trivial always-pass);
* the decoder shares no code with the writer or the other two checkers (it
  imports only the Python standard library).

Exit 0 when all assertions hold, 1 otherwise. No third-party modules.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
REPO = TOOLS.parent
CHECKER = TOOLS / "check_linearization_hints_independent.py"
VECTORS = REPO / "corpus" / "fixtures" / "lin_conformance"
FIX = REPO / "corpus" / "fixtures"

# (label, committed vector, tables expected to be decoded)
CASES = [
    ("outlines", "lin_outline.pdf", ("O", "A")),
    ("hints", "lin_hints.pdf", ("T", "E")),
    ("form", "lin_form.pdf", ("O", "F")),
]

FORBIDDEN_IMPORTS = (
    "check_linearization",
    "linearize",
    "selis",
    "pypdf",
    "PIL",
    "fitz",
)


def run(args: list[str]) -> tuple[int, str]:
    proc = subprocess.run(
        [sys.executable, str(CHECKER), *args],
        capture_output=True,
        text=True,
    )
    return proc.returncode, proc.stdout + proc.stderr


def main() -> int:
    ok = True

    # 1. Independence: stdlib-only, no imports of writer / other checkers.
    source = CHECKER.read_text(encoding="utf-8")
    import_lines = [ln for ln in source.splitlines()
                    if ln.startswith("import ") or ln.startswith("from ")]
    bad = [ln for ln in import_lines
           if any(token in ln for token in FORBIDDEN_IMPORTS)]
    print(f"[{'ok' if not bad else 'FAIL'}] independence: stdlib-only imports "
          f"({len(import_lines)} import lines)")
    for ln in bad:
        print(f"      forbidden: {ln}")
    ok &= not bad

    # 2. Every linearized vector passes decode + negative controls.
    for label, vector, tables in CASES:
        path = VECTORS / vector
        if not path.is_file():
            print(f"[FAIL] {label}: missing vector {path}")
            ok = False
            continue
        code, out = run(["--negative-controls", str(path)])
        passed = code == 0 and "OVERALL: PASS" in out
        # Confirm every expected table key actually appeared in a verdict line.
        tables_seen = all(f"tables=[{t}" in out or f",{t}]" in out or f",{t}," in out
                          for t in tables)
        passed &= tables_seen
        print(f"[{'ok' if passed else 'FAIL'}] {label} ({vector}): "
              f"decode + negative controls, tables {','.join(tables)}")
        if not passed:
            ok = False
            print(out)

    # 3. A non-linearized source must be rejected (not a trivial always-pass).
    src = FIX / "lin_optional_hints.pdf"
    code, out = run([str(src)])
    rejected = code != 0 and "hint stream located from /H" in out
    print(f"[{'ok' if rejected else 'FAIL'}] non-linearized source is rejected")
    ok &= rejected

    print(f"\nOVERALL: {'PASS' if ok else 'FAIL'}")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
