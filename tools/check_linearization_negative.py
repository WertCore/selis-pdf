#!/usr/bin/env python3
"""Negative control for the optional hint-table checker.

A clean, self-consistency check that PROVES `check_linearization.py` actually
validates the optional §F.6 outline table: it takes a linearized PDF with an
outline hint table, corrupts the hint-stream `/O` pointer (same byte length so
all xref offsets stay valid), and asserts that the checker fails specifically
on the outline cross-checks while the rest of the file still passes.

Usage:  python tools/check_linearization_negative.py LINEARIZED.pdf
Exit 0 when the negative control behaves as expected, 1 otherwise.
"""

from __future__ import annotations

import re
import sys
import tempfile
from pathlib import Path

import check_linearization as CL


def corrupt_outline_pointer(data: bytes) -> tuple[bytes, int]:
    """Shift the hint-stream /O offset by one; keep every length identical."""
    lin = CL.parse_lin_dict(data)
    h = lin.get("H")
    if not isinstance(h, list) or len(h) < 2:
        raise ValueError("no /H in linearization dictionary")
    parsed = CL.read_stream_object(data, h[0])
    if parsed is None:
        raise ValueError("no hint stream at /H")
    dict_bytes = parsed[0]
    m = re.search(rb"/O\s+(\d+)", dict_bytes)
    if not m:
        raise ValueError("no /O outline-table pointer in hint stream")
    old = m.group(0)
    pos = bytes(data).find(old, h[0])
    if pos < 0:
        raise ValueError("hint-stream /O pointer not found in file")
    new = b"/O " + str(int(m.group(1)) - 1).encode()
    if len(new) != len(old):
        raise ValueError("corruption would change byte length")
    return data[:pos] + new + data[pos + len(old) :], pos


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    data = Path(sys.argv[1]).read_bytes()
    try:
        corrupt, pos = corrupt_outline_pointer(data)
    except ValueError as exc:
        print(f"negative control inconclusive: {exc}")
        return 1

    with tempfile.NamedTemporaryFile(suffix=".pdf", delete=False) as fh:
        fh.write(corrupt)
        tmp = Path(fh.name)
    try:
        result = CL.check(str(tmp))
    finally:
        tmp.unlink(missing_ok=True)

    labels = [label for ok, label in result.checks if not ok]
    outline_failed = any("outline table count" in s or "outline item object numbers" in s
                         for s in labels)
    core_ok = not any("page-offset table" in s or "first_page_offset" in s for s in labels)
    print(f"corrupted hint-stream /O at byte {pos}; checker FAIL labels:")
    for s in labels:
        print(f"  - {s}")
    if not result.ok and outline_failed and core_ok:
        print("negative control: PASS (outline hints validated, core tables unaffected)")
        return 0
    print("negative control: FAIL (checker did not react as expected)")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
