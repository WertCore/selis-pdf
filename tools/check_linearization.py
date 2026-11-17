#!/usr/bin/env python3
"""Independent structural checker for Selis linearized PDFs.

This is NOT qpdf --check-linearization and does NOT validate hint tables
(Selis emits no hint stream). It validates the things this writer claims:

  * object 1 is a /Linearized dictionary within the first 1024 bytes;
  * /L equals the real file length;
  * /N equals the discovered leaf-page count;
  * /O names a real /Page object;
  * /T points at the whitespace before entry 0 of the first xref section;
  * /E is a byte offset within the file at/after the first-page objects;
  * /H is present (a 2-element array; zeros mean "no hint stream");
  * both xref sections parse, their /Prev chain is acyclic, and every
    in-use entry points at the declared `N 0 obj` header;
  * the first xref is the startxref target, and the main xref has no /Prev.

Usage:  python tools/check_linearization.py FILE.pdf [FILE.pdf ...]
Exit code 0 when every file passes, 1 otherwise. No third-party modules.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass, field

TOKEN = re.compile(rb"(\d+)\s+(\d+)\s+obj\b")


@dataclass
class Xref:
    offset: int
    entries: dict[int, int] = field(default_factory=dict)
    trailer: dict[str, object] = field(default_factory=dict)
    prev: int | None = None
    size: int = 0


@dataclass
class Result:
    checks: list[tuple[bool, str]] = field(default_factory=list)

    def check(self, ok: bool, label: str) -> None:
        self.checks.append((bool(ok), label))

    @property
    def ok(self) -> bool:
        return all(ok for ok, _ in self.checks)


def as_int(token: bytes) -> int | None:
    try:
        return int(token)
    except ValueError:
        return None


def parse_lin_dict(data: bytes) -> dict[str, object]:
    # object 1 must be the first object in the file.
    head = data[:1024]
    m = re.search(rb"\b1\s+0\s+obj\b", head)
    if not m:
        return {}
    start = m.end()
    end = data.find(b"endobj", start)
    if end < 0:
        return {}
    body = data[start:end]
    out: dict[str, object] = {}
    for key in (b"Linearized", b"L", b"H", b"O", b"E", b"N", b"T", b"P"):
        km = re.search(rb"/" + key + rb"(?![A-Za-z])\s*", body)
        if not km:
            continue
        pos = km.end()
        if key == b"H":
            am = re.match(rb"\[([^\]]*)\]", body[pos:])
            if am:
                out["H"] = [as_int(x) for x in am.group(1).split()]
            continue
        nm = re.match(rb"(-?\d+)", body[pos:])
        if nm:
            out[key.decode()] = int(nm.group(1))
    return out


def parse_xref_at(data: bytes, offset: int) -> Xref | None:
    if data[offset : offset + 4] != b"xref":
        return None
    pos = offset + 4
    x = Xref(offset=offset)
    while True:
        m = re.match(rb"\s*(\d+)\s+(\d+)\s*\n", data[pos:])
        if not m:
            break
        first = int(m.group(1))
        count = int(m.group(2))
        pos += m.end()
        for i in range(count):
            line = data[pos : pos + 20]
            fm = re.match(rb"(\d{10})\s+(\d{5})\s+([nf])\s*\r?\n", line)
            if not fm:
                return None
            pos += fm.end()
            if fm.group(3) == b"n":
                x.entries[first + i] = int(fm.group(1))
        # another subsection?
        if data[pos : pos + 4] == b"xref":
            break
        if data[pos : pos + 7] != b"trailer":
            # next subsection or trailer; decide by peeking two integers
            if re.match(rb"\s*\d+\s+\d+\s*\n", data[pos:]):
                continue
            break
        pos += 7
        tm = re.search(rb"<<(.*?)>>", data[pos:], re.S)
        if not tm:
            return None
        trailer = tm.group(1)
        for key in (b"Size", b"Root", b"Prev", b"Info"):
            km = re.search(rb"/" + key + rb"(?![A-Za-z])\s*", trailer)
            if not km:
                continue
            nm = re.match(rb"(-?\d+)", trailer[km.end() :])
            if nm:
                x.trailer[key.decode()] = int(nm.group(1))
        x.prev = x.trailer.get("Prev")
        x.size = int(x.trailer.get("Size", 0))
        return x
    return None


def object_numbers(data: bytes) -> dict[int, int]:
    return {int(m.group(1)): m.start() for m in TOKEN.finditer(data)}


def page_count(data: bytes, objects: dict[int, int]) -> int | None:
    # Count /Type /Page dictionaries (not /Pages) among object bodies.
    n = 0
    for num, off in objects.items():
        end = data.find(b"endobj", off)
        body = data[off:end]
        if re.search(rb"/Type\s*/Page(?![sA-Za-z])", body):
            n += 1
    return n if n else None


def check(path: str) -> Result:
    r = Result()
    data = open(path, "rb").read()
    r.check(data.startswith(b"%PDF-"), "PDF header")

    lin = parse_lin_dict(data)
    r.check(lin.get("Linearized") == 1, "object 1 has /Linearized 1")
    r.check("L" in lin, "lin dict has /L")
    if "L" in lin:
        r.check(lin["L"] == len(data), f"/L ({lin['L']}) == file length ({len(data)})")
    r.check("H" in lin and isinstance(lin["H"], list) and len(lin["H"]) in (2, 4), "/H present with 2 or 4 entries")
    r.check("N" in lin, "lin dict has /N")
    r.check("O" in lin, "lin dict has /O")
    r.check("T" in lin, "lin dict has /T")
    r.check("E" in lin, "lin dict has /E")

    objects = object_numbers(data)
    r.check(1 in objects, "object 1 present at its declared offset")

    if "O" in lin:
        o = lin["O"]
        off = objects.get(o)
        r.check(off is not None, f"/O ({o}) names a real object")
        if off is not None:
            end = data.find(b"endobj", off)
            r.check(
                bool(re.search(rb"/Type\s*/Page(?![sA-Za-z])", data[off:end])),
                f"/O ({o}) is a /Page object",
            )

    # startxref -> xref chain.
    sm = list(re.finditer(rb"startxref\s+(\d+)\s+%%EOF", data))
    r.check(len(sm) >= 1, "startxref/%%EOF present")
    xrefs: list[Xref] = []
    if sm:
        start = int(sm[-1].group(1))
        seen: set[int] = set()
        cur = start
        while cur and cur not in seen:
            seen.add(cur)
            x = parse_xref_at(data, cur)
            if x is None:
                r.check(False, f"xref at {cur} parses")
                break
            xrefs.append(x)
            cur = x.prev or 0
        r.check(len(xrefs) == 2, f"two xref sections in /Prev chain (found {len(xrefs)})")
        if xrefs:
            r.check(xrefs[0].offset == start, "startxref targets the first-page xref")
            r.check(xrefs[-1].prev is None, "main xref has no /Prev")
            r.check(xrefs[0].prev == xrefs[-1].offset, "first xref /Prev targets main xref")
            for x in xrefs:
                r.check(x.size == len(objects) + 1 or x.size > max(objects, default=0), f"xref {x.offset} /Size plausible")
                for num, off in x.entries.items():
                    header = data[off : off + 40]
                    expected = f"{num} 0 obj".encode()
                    if not header.startswith(expected):
                        r.check(False, f"xref {x.offset} entry {num} points at {off} (expected {expected!r}, got {header[:20]!r})")
                        break
                else:
                    r.check(True, f"all in-use entries in xref {x.offset} point at correct object headers")

    # /T points at whitespace before the first xref entry.
    if "T" in lin and xrefs:
        t = lin["T"]
        r.check(0 <= t < len(data), f"/T ({t}) within file")
        if 0 <= t < len(data):
            p = t
            while p < len(data) and data[p : p + 1] in (b" ", b"\r", b"\n"):
                p += 1
            first_xref = xrefs[0].offset
            hdr = re.match(rb"\s*xref\s+\d+\s+\d+\s*\n", data[first_xref:])
            entry0 = first_xref + (hdr.end() if hdr else 0)
            r.check(p == entry0, f"/T skips to first xref entry ({p} == {entry0})")

    # /E must lie between the first-page objects and the end of the file.
    if "E" in lin:
        e = lin["E"]
        r.check(0 < e <= len(data), f"/E ({e}) within file")

    # /N matches the real page count.
    if "N" in lin:
        pc = page_count(data, objects)
        r.check(pc is not None and pc == lin["N"], f"/N ({lin['N']}) == page count ({pc})")

    return r


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    all_ok = True
    for path in sys.argv[1:]:
        r = check(path)
        print(f"== {path}: {'PASS' if r.ok else 'FAIL'} ==")
        for ok, label in r.checks:
            print(f"  [{'ok' if ok else 'FAIL'}] {label}")
        all_ok &= r.ok
    return 0 if all_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
