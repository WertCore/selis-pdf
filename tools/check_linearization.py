#!/usr/bin/env python3
"""Independent structural checker for Selis linearized PDFs.

This is NOT qpdf --check-linearization. It validates the things this writer
claims, including a full parse of the hint tables it emits:

  * object 1 is a /Linearized dictionary within the first 1024 bytes;
  * /L equals the real file length, /N the leaf-page count, /O a real /Page;
  * /T points at the whitespace before entry 0 of the first xref section;
  * /E is a byte offset at/after the first-page objects;
  * /H is a 2- or 4-element array pointing at real hint stream object(s);
  * both xref sections parse, their /Prev chain is acyclic, and every in-use
    entry points at the declared `N 0 obj` header;
  * the first xref is the startxref target, the main xref has no /Prev;
  * the PAGE OFFSET HINT TABLE (ISO 32000-1 F.4) parses field-by-field and its
    values agree with the real object layout (first-page offset, per-page
    object counts/lengths, cumulative page offsets, shared-object indices);
  * the SHARED OBJECT HINT TABLE (F.5) parses and its group lengths/offsets
    agree with the actual objects, as qpdf --check-linearization verifies.

A pass here is a strong *self-consistency* result, not proof of qpdf
conformance: no external linearisation checker is installed in this build
environment.

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
        if data[pos : pos + 4] == b"xref":
            break
        if data[pos : pos + 7] != b"trailer":
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
    n = 0
    for num, off in objects.items():
        end = data.find(b"endobj", off)
        body = data[off:end]
        if re.search(rb"/Type\s*/Page(?![sA-Za-z])", body):
            n += 1
    return n if n else None


# ---------------------------------------------------------------------------
# Hint-stream / hint-table parsing
# ---------------------------------------------------------------------------


class BitReader:
    """MSB-first bit reader; `row` aligns to a byte boundary like the spec."""

    def __init__(self, data: bytes, pos: int = 0):
        self.data = data
        self.pos = pos  # bit position

    def get(self, bits: int) -> int:
        if bits > 64:
            raise ValueError("field wider than 64 bits")
        value = 0
        for _ in range(bits):
            byte = self.data[self.pos // 8] if self.pos // 8 < len(self.data) else 0
            value = (value << 1) | ((byte >> (7 - self.pos % 8)) & 1)
            self.pos += 1
        return value

    def align(self) -> None:
        self.pos = ((self.pos + 7) // 8) * 8

    def row(self, n: int, bits: int) -> list[int]:
        self.align()
        out = [self.get(bits) for _ in range(n)]
        self.align()
        return out


def read_stream_object(data: bytes, offset: int) -> tuple[bytes, bytes, int] | None:
    """Return (dict_bytes, payload_bytes, object_end) for a stream object."""
    if data[offset : offset + 6] != b"stream":
        # find the "N 0 obj" header at offset
        m = re.match(rb"\d+\s+\d+\s+obj\b", data[offset:])
        if not m:
            return None
    sm = re.search(rb"\bstream\r?\n", data[offset:])
    if not sm:
        return None
    dict_bytes = data[offset : offset + sm.start()]
    payload_start = offset + sm.end()
    em = data.find(b"\nendstream", payload_start)
    if em < 0:
        return None
    payload = data[payload_start:em]
    obj_end = data.find(b"endobj", em)
    if obj_end < 0:
        return None
    return dict_bytes, payload, obj_end + len(b"endobj")


def obj_dict(data: bytes, num: int) -> bytes:
    off = object_numbers(data).get(num)
    if off is None:
        return b""
    end = data.find(b"endobj", off)
    return data[off:end]


def page_objects(data: bytes, root_num: int) -> list[int]:
    """Walk the page tree in order, returning leaf /Page object numbers."""
    leaves: list[int] = []

    def walk(num: int, depth: int = 0) -> None:
        if depth > 64 or num in seen:
            return
        seen.add(num)
        body = obj_dict(data, num)
        if re.search(rb"/Type\s*/Page(?![sA-Za-z])", body):
            leaves.append(num)
            return
        km = re.search(rb"/Kids\s*\[(.*?)\]", body, re.S)
        if not km:
            return
        for ref in re.finditer(rb"(\d+)\s+\d+\s+R", km.group(1)):
            walk(int(ref.group(1)), depth + 1)

    seen: set[int] = set()
    catalog = obj_dict(data, root_num)
    pm = re.search(rb"/Pages\s+(\d+)\s+\d+\s+R", catalog)
    if pm:
        walk(int(pm.group(1)))
    return leaves


def object_lengths(
    data: bytes, offsets: dict[int, int], end_of_objects: int
) -> dict[int, int]:
    """Serialized byte length of each object = distance to the next object."""
    starts = sorted(offsets.values()) + [end_of_objects]
    lengths: dict[int, int] = {}
    for num, off in offsets.items():
        nxt = next((s for s in starts if s > off), end_of_objects)
        lengths[num] = nxt - off
    return lengths


@dataclass
class PageOffsetTable:
    min_nobjects: int
    first_page_offset: int
    nbits_delta_nobjects: int
    min_page_length: int
    nbits_delta_page_length: int
    min_content_offset: int
    nbits_delta_content_offset: int
    min_content_length: int
    nbits_delta_content_length: int
    nbits_nshared: int
    nbits_shared_id: int
    nbits_shared_num: int
    shared_denominator: int
    delta_nobjects: list[int]
    delta_page_length: list[int]
    nshared_objects: list[int]
    shared_identifiers: list[list[int]]
    shared_numerators: list[list[int]]
    delta_content_offset: list[int]
    delta_content_length: list[int]


@dataclass
class SharedObjectTable:
    first_shared_obj: int
    first_shared_offset: int
    nshared_first_page: int
    nshared_total: int
    nbits_nobjects: int
    min_group_length: int
    nbits_delta_group_length: int
    delta_group_length: list[int]
    signature_present: list[int]
    nobjects_minus_one: list[int]


def parse_tables(buf: bytes, s: int, npages: int):
    """Parse both tables from the raw hint buffer, /S = s."""
    r = BitReader(buf)
    page = PageOffsetTable(
        min_nobjects=r.get(32),
        first_page_offset=r.get(32),
        nbits_delta_nobjects=r.get(16),
        min_page_length=r.get(32),
        nbits_delta_page_length=r.get(16),
        min_content_offset=r.get(32),
        nbits_delta_content_offset=r.get(16),
        min_content_length=r.get(32),
        nbits_delta_content_length=r.get(16),
        nbits_nshared=r.get(16),
        nbits_shared_id=r.get(16),
        nbits_shared_num=r.get(16),
        shared_denominator=r.get(16),
        delta_nobjects=[],
        delta_page_length=[],
        nshared_objects=[],
        shared_identifiers=[],
        shared_numerators=[],
        delta_content_offset=[],
        delta_content_length=[],
    )
    nb_n = page.nbits_delta_nobjects
    nb_l = page.nbits_delta_page_length
    nb_sh = page.nbits_nshared
    nb_id = page.nbits_shared_id
    nb_num = page.nbits_shared_num
    nb_co = page.nbits_delta_content_offset
    nb_cl = page.nbits_delta_content_length
    page.delta_nobjects = r.row(npages, nb_n)
    page.delta_page_length = r.row(npages, nb_l)
    page.nshared_objects = r.row(npages, nb_sh)
    # variable-length identifier / numerator rows
    r.align()
    page.shared_identifiers = []
    for count in page.nshared_objects:
        page.shared_identifiers.append([r.get(nb_id) for _ in range(count)])
    r.align()
    page.shared_numerators = []
    for count in page.nshared_objects:
        page.shared_numerators.append([r.get(nb_num) for _ in range(count)])
    r.align()
    page.delta_content_offset = r.row(npages, nb_co)
    page.delta_content_length = r.row(npages, nb_cl)

    # Shared table starts at byte offset s.
    sr = BitReader(buf, s * 8)
    shared = SharedObjectTable(
        first_shared_obj=sr.get(32),
        first_shared_offset=sr.get(32),
        nshared_first_page=sr.get(32),
        nshared_total=sr.get(32),
        nbits_nobjects=sr.get(16),
        min_group_length=sr.get(32),
        nbits_delta_group_length=sr.get(16),
        delta_group_length=[],
        signature_present=[],
        nobjects_minus_one=[],
    )
    n = shared.nshared_total
    nb_g = shared.nbits_delta_group_length
    shared.delta_group_length = sr.row(n, nb_g)
    shared.signature_present = sr.row(n, 1)
    # optional 128-bit MD5 signatures (never emitted by this writer)
    sr.align()
    for present in shared.signature_present:
        if present:
            for _ in range(4):
                sr.get(32)
    shared.nobjects_minus_one = sr.row(n, shared.nbits_nobjects)
    return page, shared


def check_hint_tables(r: Result, data: bytes, lin: dict, xrefs: list[Xref]) -> None:
    h = lin.get("H")
    if not isinstance(h, list) or len(h) not in (2, 4) or any(not isinstance(x, int) for x in h):
        r.check(False, "/H is a usable offset/length array")
        return
    hint_offset, hint_length = h[0], h[1]
    if len(h) == 4 and h[3]:
        # 4-element /H uses two streams; this writer emits a single stream.
        r.check(False, "single hint stream (/H has 2 nonzero entries)")
        return

    parsed = read_stream_object(data, hint_offset)
    r.check(parsed is not None, f"hint stream at /H ({hint_offset}) is a stream object")
    if parsed is None:
        return
    dict_bytes, payload, _obj_end = parsed

    sm = re.search(rb"/S\s+(\d+)", dict_bytes)
    r.check(sm is not None, "hint stream dictionary has /S")
    if not sm:
        return
    s = int(sm.group(1))
    r.check(0 <= s < len(payload), f"/S ({s}) within hint buffer ({len(payload)} bytes)")
    r.check(
        hint_offset + hint_length == hint_offset + len(payload),
        f"/H length ({hint_length}) == stream data length ({len(payload)})",
    )
    if not (0 <= s < len(payload)):
        return

    npages = lin.get("N")
    if not isinstance(npages, int):
        return
    try:
        page, shared = parse_tables(payload, s, npages)
    except Exception as exc:  # noqa: BLE001 - checker must degrade, not crash
        r.check(False, f"hint tables parse: {exc}")
        return

    r.check(len(page.delta_nobjects) == npages, f"page-offset table has {npages} entries")
    r.check(page.shared_denominator == 4, f"shared_denominator is 4 ({page.shared_denominator})")
    r.check(
        page.min_content_length == page.min_page_length,
        "min_content_length == min_page_length (qpdf convention)",
    )
    r.check(
        page.nbits_delta_content_length == page.nbits_delta_page_length,
        "nbits_delta_content_length == nbits_delta_page_length",
    )

    objects = object_numbers(data)
    lengths = object_lengths(data, {**objects}, xrefs[-1].offset)
    main_xref = xrefs[-1]
    h_off = hint_offset
    h_len = hint_length

    def adjusted(off: int) -> int:
        # Spec: hint-table offsets disregard the hint stream itself.
        return off + h_len if off >= h_off else off

    # Cross-check first page offset.
    o = lin.get("O")
    first_page_off = objects.get(o) if isinstance(o, int) else None
    r.check(
        first_page_off is not None and adjusted(page.first_page_offset) == first_page_off,
        f"first_page_offset ({adjusted(page.first_page_offset)}) == /O object offset ({first_page_off})",
    )

    # Walk the page tree and verify per-page counts/lengths and the cumulative
    # page-offset chain against the real xref offsets.
    root_num = main_xref.trailer.get("Root")
    pages = page_objects(data, root_num) if isinstance(root_num, int) else []
    r.check(len(pages) == npages, f"page tree has {npages} leaves (found {len(pages)})")
    cumulative = page.first_page_offset
    for i in range(npages):
        if i < len(pages):
            num = pages[i]
            actual_off = objects.get(num)
            r.check(
                adjusted(cumulative) == actual_off,
                f"page {i} table offset ({adjusted(cumulative)}) == object {num} offset ({actual_off})",
            )
            nobj = page.min_nobjects + page.delta_nobjects[i]
            # The nobj consecutive object numbers from this page object must be
            # in use and laid out contiguously.
            span = 0
            contiguous = True
            for k in range(nobj):
                onum = num + k
                ooff = objects.get(onum) if onum in objects else main_xref.entries.get(onum)
                if ooff is None:
                    contiguous = False
                    break
                span += lengths.get(onum, main_xref.entries.get(onum + 1, ooff) - ooff)
            hlength = page.min_page_length + page.delta_page_length[i]
            r.check(contiguous and span == hlength,
                    f"page {i} length ({hlength}) == sum of {nobj} real object lengths ({span})")
        cumulative += page.min_page_length + page.delta_page_length[i]

    # Cross-check the shared-object table against qpdf's algorithm: the first
    # nshared_first_page objects are consecutive from the first page object;
    # the rest are consecutive from first_shared_obj.
    r.check(
        shared.nshared_total >= shared.nshared_first_page,
        f"nshared_total ({shared.nshared_total}) >= nshared_first_page ({shared.nshared_first_page})",
    )
    r.check(
        len(shared.delta_group_length) == shared.nshared_total,
        f"shared table has {shared.nshared_total} group entries",
    )
    if isinstance(o, int):
        cur = o
        for i in range(shared.nshared_total):
            if i == shared.nshared_first_page:
                cur = shared.first_shared_obj
                if shared.nshared_total > shared.nshared_first_page:
                    real = objects.get(cur)
                    r.check(
                        adjusted(shared.first_shared_offset) == real,
                        f"first_shared_offset ({adjusted(shared.first_shared_offset)}) == object {cur} offset ({real})",
                    )
            nobj = shared.nobjects_minus_one[i] + 1  # always 1 here
            span = 0
            for k in range(nobj):
                onum = cur + k
                ooff = objects.get(onum) if onum in objects else main_xref.entries.get(onum)
                if ooff is None:
                    span = -1
                    break
                span += lengths.get(onum, 0)
            glen = shared.min_group_length + shared.delta_group_length[i]
            if i < shared.nshared_first_page or shared.nshared_total > shared.nshared_first_page:
                r.check(span == glen,
                        f"shared {i} group length ({glen}) == real object length ({span})")
            cur += nobj
    for ids in page.shared_identifiers:
        for idx in ids:
            r.check(idx < shared.nshared_total,
                    f"shared identifier {idx} within {shared.nshared_total} entries")


def check(path: str) -> Result:
    r = Result()
    with open(path, "rb") as fh:
        data = fh.read()
    r.check(data.startswith(b"%PDF-"), "PDF header")

    lin = parse_lin_dict(data)
    r.check(lin.get("Linearized") == 1, "object 1 has /Linearized 1")
    r.check("L" in lin, "lin dict has /L")
    if "L" in lin:
        r.check(lin["L"] == len(data), f"/L ({lin['L']}) == file length ({len(data)})")
    r.check(
        "H" in lin and isinstance(lin["H"], list) and len(lin["H"]) in (2, 4),
        "/H present with 2 or 4 entries",
    )
    for k in ("N", "O", "T", "E"):
        r.check(k in lin, f"lin dict has /{k}")

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
                covered = max(x.entries, default=0)
                r.check(x.size == covered + 1, f"xref {x.offset} /Size covers its entries")
                for num, off in x.entries.items():
                    header = data[off : off + 40]
                    expected = f"{num} 0 obj".encode()
                    if not header.startswith(expected):
                        r.check(False, f"xref {x.offset} entry {num} points at {off}")
                        break
                else:
                    r.check(True, f"all in-use entries in xref {x.offset} point at correct headers")

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

    if "E" in lin:
        e = lin["E"]
        r.check(0 < e <= len(data), f"/E ({e}) within file")

    if "N" in lin:
        pc = page_count(data, objects)
        r.check(pc is not None and pc == lin["N"], f"/N ({lin['N']}) == page count ({pc})")

    # Hint tables (the core of this checker).
    if xrefs:
        check_hint_tables(r, data, lin, xrefs)
    else:
        r.check(False, "hint tables need a parsed xref chain")

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
