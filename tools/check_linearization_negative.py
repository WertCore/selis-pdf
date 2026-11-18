#!/usr/bin/env python3
"""Negative control for the optional hint-table checker.

Two clean, self-consistency checks that PROVE `check_linearization.py` actually
validates the optional hint tables:

  * default mode: takes a linearized PDF with an outline hint table, corrupts
    the hint-stream `/O` pointer (same byte length so all xref offsets stay
    valid), and asserts the checker fails specifically on the outline
    cross-checks while the rest of the file still passes;
  * `--form` mode: takes a linearized PDF with an interactive form, corrupts
    one field object number inside the §F.10 table (same byte length), and
    asserts the checker fails specifically on the form cross-checks while the
    core tables still parse.

Usage:  python tools/check_linearization_negative.py LINEARIZED.pdf [--form]
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


def corrupt_form_field(data: bytes) -> tuple[bytes, int]:
    """Change the first form field's object number by one; keep byte length."""
    lin = CL.parse_lin_dict(data)
    h = lin.get("H")
    if not isinstance(h, list) or len(h) < 2:
        raise ValueError("no /H in linearization dictionary")
    parsed = CL.read_stream_object(data, h[0])
    if parsed is None:
        raise ValueError("no hint stream at /H")
    fm = re.search(rb"/F\s+(\d+)", parsed[0])
    if not fm:
        raise ValueError("no /F form-table pointer in hint stream")
    table = int(fm.group(1))
    payload = parsed[1]
    if table + 4 > len(payload):
        raise ValueError("form table count outside hint buffer")
    nfields = int.from_bytes(payload[table : table + 4], "big")
    if nfields < 1:
        raise ValueError("form table has no fields")
    row = table + 4  # first field object number (4-byte big-endian)
    obj = int.from_bytes(payload[row : row + 4], "big")
    if obj < 3:
        raise ValueError("first form field object number too small to corrupt")
    new = (obj - 1).to_bytes(4, "big")
    pos = h[0] + payload_start_offset(data, h[0]) + row
    return data[:pos] + new + data[pos + 4 :], pos


def payload_start_offset(data: bytes, hint_offset: int) -> int:
    """Byte distance from the hint object header to its stream payload."""
    sm = re.search(rb"\bstream\r?\n", data[hint_offset:])
    if not sm:
        raise ValueError("no stream keyword in hint object")
    return sm.end()


def main() -> int:
    args = sys.argv[1:]
    form_mode = "--form" in args
    args = [a for a in args if a != "--form"]
    if len(args) != 1:
        print(__doc__)
        return 2
    data = Path(args[0]).read_bytes()
    try:
        if form_mode:
            corrupt, pos = corrupt_form_field(data)
        else:
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
    core_ok = not any("page-offset table" in s or "first_page_offset" in s for s in labels)
    if form_mode:
        targeted = any("form field object numbers" in s or "form table count" in s
                       for s in labels)
        what = "form field object number inside the /F table"
        name = "form"
    else:
        targeted = any("outline table count" in s or "outline item object numbers" in s
                       for s in labels)
        what = "hint-stream /O pointer"
        name = "outline"
    print(f"corrupted {what} at byte {pos}; checker FAIL labels:")
    for s in labels:
        print(f"  - {s}")
    if not result.ok and targeted and core_ok:
        print(f"negative control: PASS ({name} hints validated, core tables unaffected)")
        return 0
    print("negative control: FAIL (checker did not react as expected)")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
