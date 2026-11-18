#!/usr/bin/env python3
"""Negative controls for the xref-stream structure in check_linearization.py.

The happy path is exercised by `check_linearization.py corpus/fixtures/lin_xref_streams.pdf`.
This script takes that file, asserts it passes, then corrupts one structural
token at a time and asserts the checker FAILS (never silently accepts):

  * `/Prev` on the first-page xref stream -> a broken /Prev chain;
  * `/W` on the main xref stream -> an undecodable entry layout;
  * `/Type /XRef` -> the section is no longer labelled as an xref stream;
  * the hint stream's `/S` -> an unreadable shared-object table.

Each mutation is byte-preserving except for the token itself, so a failure is
attributable to that token. Usage:

    python tools/test_check_linearization_xref.py [FILE.pdf]

Exit 0 when every control behaves as expected, 1 otherwise. No third-party
modules; it imports the checker directly.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_linearization as checker  # noqa: E402

DEFAULT = Path(__file__).resolve().parent.parent / "corpus/fixtures/lin_xref_streams.pdf"


def mutate_once(data: bytes, pattern: bytes, replacement: bytes, occurrence: int = 0) -> bytes:
    matches = list(re.finditer(re.escape(pattern), data))
    if len(matches) <= occurrence:
        raise AssertionError(f"pattern {pattern!r} not found {occurrence + 1} times")
    m = matches[occurrence]
    return data[: m.start()] + replacement + data[m.end() :]


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    path = Path(args[0]) if args else DEFAULT
    original = path.read_bytes()
    ok = True

    baseline = checker.check(str(path))
    print(f"[{'ok' if baseline.ok else 'FAIL'}] unmodified fixture passes the checker")
    ok &= baseline.ok

    controls: list[tuple[str, bytes]] = [
        # Break the first-page xref stream's /Prev -> main xref offset.
        ("broken /Prev chain", b"/Prev "),
        # Break the main xref stream's /W width array (second occurrence).
        ("broken /W", b"/W [1 4 3]"),
        # Relabel an xref stream so it is no longer /Type /XRef.
        ("missing /Type /XRef", b"/Type /XRef"),
        # Break the hint stream's /S shared-table offset.
        ("broken hint /S", b"/S "),
    ]
    if baseline.ok:
        for label, pattern in controls:
            occurrence = 1 if pattern == b"/W [1 4 3]" else 0
            if pattern == b"/Prev ":
                damaged = mutate_once(original, pattern, b"/Prev 99999999 ", occurrence)
            elif pattern == b"/W [1 4 3]":
                damaged = mutate_once(original, pattern, b"/W [9 9 9]", occurrence)
            elif pattern == b"/Type /XRef":
                damaged = mutate_once(original, pattern, b"/Type /BadXRef", occurrence)
            else:
                damaged = mutate_once(original, pattern, b"/S 99999 ", occurrence)
            tmp = path.with_name(path.stem + "-control.pdf")
            try:
                tmp.write_bytes(damaged)
                result = checker.check(str(tmp))
                passed = not result.ok
                print(f"[{'ok' if passed else 'FAIL'}] {label} is refused by the checker")
                ok &= passed
            finally:
                tmp.unlink(missing_ok=True)
    else:
        print("[FAIL] negative controls skipped: baseline did not pass")

    print(f"xref-stream negative controls: {'PASS' if ok else 'FAIL'}")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
