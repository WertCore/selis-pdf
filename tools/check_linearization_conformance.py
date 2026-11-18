#!/usr/bin/env python3
"""Independent Annex F linearization conformance checker (no qpdf, no network).

WHAT THIS IS
------------
A from-scratch verifier of the ISO 32000-1 Annex F "fast web view" contract.
It shares **no code** with the writer (`crates/selis-pdf-optimize/src/linearize.rs`)
and **no code** with the earlier self-consistency checker
(`tools/check_linearization.py`). It re-derives every value it checks from the
raw bytes of the file under test using its own byte-level PDF scanner and its
own MSB-first bit reader, then asserts that the writer's declared structures
match that independently derived ground truth.

Specifically it validates, from raw bytes only:

  * the /Linearized dictionary on object 1 within the first 1024 bytes, with
    *all* seven parameters /L /H /O /E /N /T /P and their semantics:
      - /L == real file length
      - /H == [ offset, length ] of a real hint *stream* object
      - /O == the first leaf /Page object number (and its type is /Page)
      - /N == the independently counted number of leaf pages
      - /T == the byte offset of the whitespace before entry 0 of the *first*
        xref section
      - /E == the byte offset where the first-page section ends
      - /P == 1
  * the two classic xref sections and their /Prev chain: startxref targets the
    first-page xref, exactly two sections exist, the first /Prev points at the
    main xref, the main xref has no /Prev, the chain is acyclic, /Root agrees,
    and **every in-use xref entry points at the `N 0 obj` header that an
    independent byte scan found at that offset**;
  * the page tree, walked from /Root via /Pages /Kids with an independent raw
    parser, yielding the ordered leaf page object numbers and each page's
    /Contents object;
  * hint-stream tables (F.4/F.5/F.6-F.9) decoded bit-for-bit from the stream
    payload, cross-checked against the independently derived object graph:
      - F.4 page-offset table: first-page offset, per-page object counts,
        per-page contiguous byte lengths (sum of the *actual* serialized object
        lengths), cumulative page offsets, shared id ranges;
      - F.5 shared-object table: counts, first-shared offset, group lengths;
      - F.6 outline, F.7 thumbnail, F.8 named-destination and F.9 page-label
        tables when present, against the real outline tree, /Thumb streams,
        /Dests name tree and /PageLabels number tree;
  * the physical layout contract: first-page object, then every page's content
    physically before later pages, all objects below the hint stream, hint
    stream below the main xref;
  * negative controls: mutate /L, /H, /N, one F.4 hint field and /T and assert
    the checker rejects each mutation (``--negative-controls``).

WHAT THIS IS NOT
----------------
This is **not** `qpdf --check-linearization`. qpdf, mutool, pdftk and pikepdf
are not installed here and are not fetched from the network. A pass is a
rigorous independent conformance verdict over the Annex F fields this writer
emits; it is not a claim of bit-for-bit compatibility with qpdf's much broader
checker (which also validates object streams, xref streams, MD5 group
signatures, and many structural cases this writer does not produce).

``--interop`` additionally opens the file with pypdf and pypdfium2 (independent
implementations) to prove it loads and renders with the right page count. That
is explicitly labelled INTEROP, not conformance.

Usage:
    python tools/check_linearization_conformance.py FILE.pdf [FILE.pdf ...]
    python tools/check_linearization_conformance.py --negative-controls FILE.pdf
    python tools/check_linearization_conformance.py --interop FILE.pdf --pages N
    python tools/check_linearization_conformance.py --json FILE.pdf

Exit 0 when every requested check passes, 1 on any failure, 2 on usage error.
Core conformance uses only the Python standard library.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass, field

# ---------------------------------------------------------------------------
# Fresh byte-level PDF primitives
# ---------------------------------------------------------------------------

# An indirect object header, e.g. b"12 0 obj". The negative look-behind avoids
# matching the tail of a longer number.
OBJ_HDR = re.compile(rb"(?<![0-9])(\d+)\s+(\d+)\s+obj\b")
STREAM_KW = re.compile(rb"\bstream\r?\n")
ENDSTREAM = b"\nendstream"


@dataclass
class Check:
    ok: bool
    category: str
    label: str


@dataclass
class Result:
    path: str = ""
    checks: list[Check] = field(default_factory=list)

    def add(self, ok: bool, category: str, label: str) -> bool:
        self.checks.append(Check(bool(ok), category, label))
        return bool(ok)

    @property
    def ok(self) -> bool:
        return all(c.ok for c in self.checks)

    def failed(self) -> list[str]:
        return [f"[{c.category}] {c.label}" for c in self.checks if not c.ok]

    def by_category(self) -> dict[str, tuple[int, int]]:
        out: dict[str, tuple[int, int]] = {}
        for c in self.checks:
            passed, total = out.get(c.category, (0, 0))
            out[c.category] = (passed + int(c.ok), total + 1)
        return out


def _int(token: bytes | str) -> int | None:
    try:
        return int(token)
    except (ValueError, TypeError):
        return None


def parse_lin_dict(data: bytes) -> dict[str, object]:
    """Parse object 1's /Linearized dictionary from the leading 1024 bytes."""
    head = data[:1024]
    m = re.search(rb"(?<![0-9])1\s+0\s+obj\b", head)
    if not m:
        return {}
    end = data.find(b"endobj", m.end())
    if end < 0:
        return {}
    body = data[m.end() : end]
    out: dict[str, object] = {}
    lm = re.search(rb"/Linearized(?![A-Za-z])\s*(-?\d+)", body)
    if lm:
        out["Linearized"] = int(lm.group(1))
    for key in (b"L", b"O", b"E", b"N", b"T", b"P"):
        km = re.search(rb"/" + key + rb"(?![A-Za-z])\s*(-?\d+)", body)
        if km:
            out[key.decode()] = int(km.group(1))
    hm = re.search(rb"/H(?![A-Za-z])\s*\[([^\]]*)\]", body)
    if hm:
        vals = [_int(x) for x in hm.group(1).split()]
        out["H"] = vals if all(v is not None for v in vals) else None
    return out


def scan_objects(data: bytes) -> tuple[dict[int, int], dict[int, int]]:
    """Independent byte scan: object number -> (offset, generation).

    Returns (offsets, generations). Duplicate object numbers are recorded by
    keeping the first occurrence; the caller checks for duplicates explicitly.
    """
    offsets: dict[int, int] = {}
    gens: dict[int, int] = {}
    for m in OBJ_HDR.finditer(data):
        num = int(m.group(1))
        if num not in offsets:
            offsets[num] = m.start()
            gens[num] = int(m.group(2))
    return offsets, gens


@dataclass
class Xref:
    offset: int
    entries: dict[int, int] = field(default_factory=dict)
    trailer: dict[str, object] = field(default_factory=dict)
    size: int = 0
    root: int | None = None
    prev: int | None = None


def parse_xref_at(data: bytes, offset: int) -> Xref | None:
    """Parse one classic (non-stream) xref section and its trailer."""
    if data[offset : offset + 4] != b"xref":
        return None
    x = Xref(offset=offset)
    pos = offset + 4
    while True:
        sm = re.match(rb"\s+(\d+)\s+(\d+)\s*\r?\n", data[pos:])
        if not sm:
            break
        first, count = int(sm.group(1)), int(sm.group(2))
        pos += sm.end()
        for i in range(count):
            line = data[pos : pos + 20]
            fm = re.match(rb"(\d{10})\s+(\d{5})\s+([nf])\s*\r?\n", line)
            if not fm:
                return None
            pos += fm.end()
            if fm.group(3) == b"n":
                x.entries[first + i] = int(fm.group(1))
        # Another subsection?
        if re.match(rb"\s+\d+\s+\d+\s*\r?\n", data[pos:]):
            continue
        break
    if data[pos : pos + 7] != b"trailer":
        return None
    pos += 7
    tm = re.search(rb"<<(.*?)>>", data[pos:], re.S)
    if not tm:
        return None
    body = tm.group(1)
    for key in (b"Size", b"Prev"):
        km = re.search(rb"/" + key + rb"(?![A-Za-z])\s*(-?\d+)", body)
        if km:
            x.trailer[key.decode()] = int(km.group(1))
    rm = re.search(rb"/Root\s+(\d+)\s+(\d+)\s+R", body)
    if rm:
        x.root = int(rm.group(1))
    x.size = int(x.trailer.get("Size", 0))
    prev = x.trailer.get("Prev")
    x.prev = int(prev) if isinstance(prev, int) else None
    return x


def parse_xref_chain(data: bytes, startxref: int) -> tuple[list[Xref], bool]:
    """Follow /Prev from startxref. Returns (sections, cycle_detected)."""
    sections: list[Xref] = []
    seen: set[int] = set()
    cycle = False
    cur = startxref
    while cur and cur not in seen:
        seen.add(cur)
        x = parse_xref_at(data, cur)
        if x is None:
            break
        sections.append(x)
        nxt = x.prev or 0
        if nxt and nxt in seen:
            cycle = True
            break
        cur = nxt
    return sections, cycle


@dataclass
class Stream:
    dict_bytes: bytes
    payload: bytes
    length_field: int | None


def read_stream(data: bytes, offset: int) -> Stream | None:
    """Read the stream object beginning at `offset`."""
    if not re.match(rb"\d+\s+\d+\s+obj\b", data[offset:]):
        return None
    sm = STREAM_KW.search(data, offset)
    if not sm:
        return None
    dict_bytes = data[offset : sm.start()]
    start = sm.end()
    end = data.find(ENDSTREAM, start)
    if end < 0:
        return None
    payload = data[start:end]
    lm = re.search(rb"/Length\s+(\d+)", dict_bytes)
    return Stream(dict_bytes, payload, int(lm.group(1)) if lm else None)


class BitReader:
    """MSB-first bit reader with byte-aligned rows (Annex F convention)."""

    def __init__(self, data: bytes, bitpos: int = 0):
        self.data = data
        self.pos = bitpos

    def get(self, bits: int) -> int:
        if bits < 0 or bits > 64:
            raise ValueError(f"bad field width {bits}")
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


# ---------------------------------------------------------------------------
# Independent object-graph derivation (raw bytes, no writer code)
# ---------------------------------------------------------------------------


def obj_body(data: bytes, offsets: dict[int, int], num: int) -> bytes:
    off = offsets.get(num)
    if off is None:
        return b""
    end = data.find(b"endobj", off)
    return data[off : end if end >= 0 else off + 4096]


def _refs_in(body: bytes, key: bytes) -> list[int]:
    """Indirect refs inside the value of a dictionary key (best effort)."""
    m = re.search(rb"/" + key + rb"(?![A-Za-z])\s*", body)
    if not m:
        return []
    return [int(r.group(1)) for r in re.finditer(rb"(\d+)\s+\d+\s+R", body[m.end() :])]


def _one_ref(body: bytes, key: bytes) -> int | None:
    refs = _refs_in(body, key)
    return refs[0] if refs else None


def page_tree(data: bytes, offsets: dict[int, int], root_num: int) -> list[int]:
    """Walk /Root /Pages /Kids in order; return ordered leaf /Page numbers."""
    leaves: list[int] = []
    visited: set[int] = set()

    def walk(num: int, depth: int) -> None:
        if depth > 64 or num in visited:
            return
        visited.add(num)
        body = obj_body(data, offsets, num)
        if re.search(rb"/Type\s*/Page(?![sA-Za-z])", body):
            leaves.append(num)
            return
        for kid in _refs_in(body, b"Kids"):
            walk(kid, depth + 1)

    catalog = obj_body(data, offsets, root_num)
    pages_root = _one_ref(catalog, b"Pages")
    if pages_root is not None:
        walk(pages_root, 0)
    return leaves


def page_content_obj(data: bytes, offsets: dict[int, int], page_num: int) -> int | None:
    body = obj_body(data, offsets, page_num)
    return _one_ref(body, b"Contents")


def outline_item_objects(
    data: bytes, offsets: dict[int, int], root_num: int
) -> set[int]:
    """All outline item object numbers via /First + /Next + /First chains."""
    catalog = obj_body(data, offsets, root_num)
    root = _one_ref(catalog, b"Outlines")
    if root is None:
        return set()
    result: set[int] = set()
    seen: set[int] = set()

    def walk(num: int) -> None:
        cur = num
        while cur and cur not in seen:
            seen.add(cur)
            result.add(cur)
            body = obj_body(data, offsets, cur)
            child = _one_ref(body, b"First")
            if child is not None:
                walk(child)
            cur = _one_ref(body, b"Next")

    first = _one_ref(obj_body(data, offsets, root), b"First")
    if first is not None:
        walk(first)
    return result


def _name_tree_strings(data: bytes, offsets: dict[int, int], root_num: int) -> set[bytes]:
    catalog = obj_body(data, offsets, root_num)
    names_root = _one_ref(catalog, b"Names")
    if names_root is None:
        return set()
    names_obj = obj_body(data, offsets, names_root)
    dests = _one_ref(names_obj, b"Dests")
    if dests is None:
        return set()
    body = obj_body(data, offsets, dests)
    out: set[bytes] = set()
    # Scan the *whole* object: a /Names array holds nested [...] destination
    # arrays, so a non-greedy bracket match would stop at the first one.
    for s in re.finditer(rb"\(([^()]*)\)\s*\[", body):
        out.add(s.group(1))
    return out


def named_dest_names(data: bytes, offsets: dict[int, int], root_num: int) -> set[bytes]:
    catalog = obj_body(data, offsets, root_num)
    out: set[bytes] = set()
    dests = _one_ref(catalog, b"Dests")
    if dests is not None:
        body = obj_body(data, offsets, dests)
        for s in re.finditer(rb"/([^\s/<>\[\]()]+)\s*\[", body):
            out.add(s.group(1))
    out |= _name_tree_strings(data, offsets, root_num)
    return out


def page_label_starts(data: bytes, offsets: dict[int, int], root_num: int) -> list[int]:
    catalog = obj_body(data, offsets, root_num)
    labels = _one_ref(catalog, b"PageLabels")
    if labels is None:
        return []
    body = obj_body(data, offsets, labels)
    nm = re.search(rb"/Nums\s*\[(.*?)\]", body, re.S)
    if not nm:
        return []
    return sorted(int(x) for x in re.findall(rb"(\d+)\s+<<", nm.group(1)))


def thumbnail_pages(
    data: bytes, offsets: dict[int, int], pages: list[int]
) -> set[int]:
    out: set[int] = set()
    for num in pages:
        body = obj_body(data, offsets, num)
        thumb = _one_ref(body, b"Thumb")
        if thumb is not None:
            out.add(num)
    return out


# ---------------------------------------------------------------------------
# Hint table structures and parsers
# ---------------------------------------------------------------------------


@dataclass
class PageOffsetTable:
    min_nobjects: int
    first_page_offset: int
    nbits_nobjects: int
    min_length: int
    nbits_length: int
    min_content_offset: int
    nbits_content_offset: int
    min_content_length: int
    nbits_content_length: int
    nbits_nshared: int
    nbits_shared_id: int
    nbits_shared_num: int
    shared_denominator: int
    nobjects: list[int]
    lengths: list[int]
    nshared: list[int]
    shared_ids: list[list[int]]
    content_lengths: list[int]


@dataclass
class SharedTable:
    first_shared_obj: int
    first_shared_offset: int
    nshared_first_page: int
    nshared_total: int
    nbits_nobjects: int
    min_group_length: int
    nbits_group: int
    group_lengths: list[int]
    signatures: list[int]


@dataclass
class OffsetTable:
    headers: list[int]
    offsets: list[int]
    extras: list[int]


def parse_page_offset_table(buf: bytes, npages: int) -> PageOffsetTable:
    r = BitReader(buf)
    t = PageOffsetTable(
        min_nobjects=r.get(32),
        first_page_offset=r.get(32),
        nbits_nobjects=r.get(16),
        min_length=r.get(32),
        nbits_length=r.get(16),
        min_content_offset=r.get(32),
        nbits_content_offset=r.get(16),
        min_content_length=r.get(32),
        nbits_content_length=r.get(16),
        nbits_nshared=r.get(16),
        nbits_shared_id=r.get(16),
        nbits_shared_num=r.get(16),
        shared_denominator=r.get(16),
        nobjects=[],
        lengths=[],
        nshared=[],
        shared_ids=[],
        content_lengths=[],
    )
    if max(
        t.nbits_nobjects,
        t.nbits_length,
        t.nbits_content_offset,
        t.nbits_content_length,
        t.nbits_nshared,
        t.nbits_shared_id,
        t.nbits_shared_num,
    ) > 32:
        raise ValueError("implausible hint-table field width")
    dno = r.row(npages, t.nbits_nobjects)
    dl = r.row(npages, t.nbits_length)
    nsh = r.row(npages, t.nbits_nshared)
    t.nobjects = [t.min_nobjects + d for d in dno]
    t.lengths = [t.min_length + d for d in dl]
    t.nshared = nsh
    # Annex F row order: shared identifiers, shared numerators (0 bits),
    # content-offset deltas (0 bits), then content-length deltas.
    r.align()
    t.shared_ids = [[r.get(t.nbits_shared_id) for _ in range(n)] for n in nsh]
    r.align()
    _ = r.row(0, 0)  # shared numerators (zero width in this writer)
    r.align()
    _ = r.row(0, 0)  # content-offset deltas (zero width in this writer)
    r.align()
    t.content_lengths = [
        t.min_content_length + d for d in r.row(npages, t.nbits_content_length)
    ]
    return t


def parse_shared_table(buf: bytes, s: int) -> SharedTable:
    r = BitReader(buf, s * 8)
    t = SharedTable(
        first_shared_obj=r.get(32),
        first_shared_offset=r.get(32),
        nshared_first_page=r.get(32),
        nshared_total=r.get(32),
        nbits_nobjects=r.get(16),
        min_group_length=r.get(32),
        nbits_group=r.get(16),
        group_lengths=[],
        signatures=[],
    )
    if t.nbits_group > 32 or t.nbits_nobjects > 32:
        raise ValueError("implausible shared-table field width")
    n = t.nshared_total
    if n > 1_000_000:
        raise ValueError("implausible shared-object count")
    t.group_lengths = [t.min_group_length + d for d in r.row(n, t.nbits_group)]
    t.signatures = r.row(n, 1)
    r.align()
    return t


def parse_offset_table(
    buf: bytes, off: int, header_bits: list[int], nrows: int, extra_bits: int
) -> OffsetTable:
    """Parse an Annex F optional table: headers, then fixed-width rows."""
    if nrows < 0 or nrows > 1_000_000 or extra_bits > 32:
        raise ValueError("implausible optional-table dimensions")
    r = BitReader(buf, off * 8)
    headers = [r.get(b) for b in header_bits]
    r.align()
    offsets: list[int] = []
    extras: list[int] = []
    for _ in range(nrows):
        offsets.append(r.get(32))
        extras.append(r.get(extra_bits) if extra_bits else 0)
        r.align()
    return OffsetTable(headers, offsets, extras)


# ---------------------------------------------------------------------------
# Core conformance check
# ---------------------------------------------------------------------------


def conformance_check(data: bytes, path: str = "<bytes>") -> Result:
    r = Result(path=path)
    r.add(data.startswith(b"%PDF-"), "HEADER", "file begins with %PDF-")

    lin = parse_lin_dict(data)
    r.add(lin.get("Linearized") == 1, "LIN", "object 1 carries /Linearized 1")
    for key in ("L", "H", "O", "E", "N", "T", "P"):
        r.add(key in lin, "LIN", f"linearization dict has /{key}")

    # --- /L, /P ----------------------------------------------------------
    if "L" in lin:
        r.add(
            lin["L"] == len(data),
            "LIN",
            f"/L ({lin['L']}) equals real file length ({len(data)})",
        )
    if "P" in lin:
        r.add(lin["P"] == 1, "LIN", f"/P == 1 (got {lin['P']})")

    # --- independent object scan -----------------------------------------
    offsets, gens = scan_objects(data)
    if not offsets:
        r.add(False, "OBJ", "at least one indirect object found")
        return r
    r.add(1 in offsets, "OBJ", "object 1 found by independent byte scan")
    if 1 in offsets:
        r.add(gens.get(1) == 0, "OBJ", "object 1 has generation 0")

    boundaries = sorted(offsets.values()) + [len(data)]

    def obj_length(num: int) -> int | None:
        off = offsets.get(num)
        if off is None:
            return None
        nxt = next((b for b in boundaries if b > off), len(data))
        return nxt - off

    # --- startxref / xref chain ------------------------------------------
    sx = list(re.finditer(rb"startxref\s+(\d+)\s+%%EOF", data))
    r.add(len(sx) >= 1, "XREF", "startxref/%%EOF trailer present")
    xrefs: list[Xref] = []
    cycle = False
    if sx:
        startxref = int(sx[-1].group(1))
        xrefs, cycle = parse_xref_chain(data, startxref)
    r.add(not cycle, "XREF", "xref /Prev chain is acyclic")
    r.add(len(xrefs) == 2, "XREF", f"exactly two xref sections (found {len(xrefs)})")
    if len(xrefs) == 2:
        first, main = xrefs
        r.add(
            first.offset == int(sx[-1].group(1)),
            "XREF",
            "startxref targets the first-page xref",
        )
        r.add(first.prev == main.offset, "XREF", "first xref /Prev targets main xref")
        r.add(main.prev is None, "XREF", "main xref has no /Prev")
        r.add(
            first.root is not None and first.root == main.root,
            "XREF",
            "both trailers agree on /Root",
        )
        if main.root is not None:
            r.add(main.root in offsets, "XREF", f"/Root object {main.root} exists")
        for x in xrefs:
            covered = max(x.entries, default=0)
            r.add(
                x.size == covered + 1,
                "XREF",
                f"xref {x.offset} /Size covers entries through {covered}",
            )
            bad = [
                n
                for n, off in x.entries.items()
                if not data[off : off + 40].startswith(f"{n} 0 obj".encode())
            ]
            r.add(
                not bad,
                "XREF",
                f"all in-use entries in xref {x.offset} point at real object headers"
                + (f" (bad: {bad[:5]})" if bad else ""),
            )
        # Main xref must cover every object the byte scan found.
        missing = sorted(set(offsets) - set(main.entries))
        r.add(not missing, "XREF", f"main xref covers all {len(offsets)} scanned objects")

    root_num = xrefs[-1].root if xrefs and xrefs[-1].root is not None else 0
    pages = page_tree(data, offsets, root_num) if root_num else []
    r.add(len(pages) > 0, "TREE", "page tree yields at least one leaf page")
    if "N" in lin:
        r.add(
            len(pages) == lin["N"],
            "TREE",
            f"independently walked page tree has /N ({lin['N']}) leaves (found {len(pages)})",
        )

    # --- /O and /T -------------------------------------------------------
    if "O" in lin and offsets:
        o = lin["O"]
        off = offsets.get(o)
        r.add(off is not None, "LIN", f"/O ({o}) names a scanned object")
        if off is not None:
            body = obj_body(data, offsets, o)
            r.add(
                bool(re.search(rb"/Type\s*/Page(?![sA-Za-z])", body)),
                "LIN",
                f"/O ({o}) is a /Page object (not /Pages)",
            )
            r.add(pages and pages[0] == o, "TREE", "/O is the first leaf of the page tree")
    if "T" in lin and xrefs:
        t = lin["T"]
        r.add(0 <= t < len(data), "LIN", f"/T ({t}) within file")
        if 0 <= t < len(data):
            p = t
            while p < len(data) and data[p : p + 1] in (b" ", b"\r", b"\n", b"\t"):
                p += 1
            first_xref = xrefs[0].offset
            hdr = re.match(rb"\s*xref\s+\d+\s+\d+\s*\r?\n", data[first_xref:])
            entry0 = first_xref + (hdr.end() if hdr else 0)
            r.add(
                data[t : t + 1] in (b" ", b"\r", b"\n", b"\t") and p == entry0,
                "LIN",
                f"/T points at whitespace before first xref entry 0 "
                f"(byte at /T = {data[t : t + 1]!r}, skips to {p}, entry 0 at {entry0})",
            )

    # --- hint stream /H ---------------------------------------------------
    h = lin.get("H")
    if not isinstance(h, list) or len(h) != 2 or any(not isinstance(v, int) for v in h):
        r.add(False, "HINT", "/H is a two-element [ offset length ] array")
        return r
    hint_offset, hint_length = h
    r.add(
        hint_offset in set(offsets.values()),
        "HINT",
        f"/H offset ({hint_offset}) points exactly at a scanned object header",
    )
    stream = read_stream(data, hint_offset)
    r.add(stream is not None, "HINT", f"hint stream at /H ({hint_offset}) is a stream object")
    if stream is None:
        return r
    dict_bytes, payload = stream.dict_bytes, stream.payload
    r.add(
        hint_length == len(payload),
        "HINT",
        f"/H length ({hint_length}) equals actual payload length ({len(payload)})",
    )
    if stream.length_field is not None:
        r.add(
            stream.length_field == len(payload),
            "HINT",
            f"hint /Length ({stream.length_field}) equals payload length ({len(payload)})",
        )
    sm = re.search(rb"/S\s+(\d+)", dict_bytes)
    r.add(sm is not None, "HINT", "hint stream dictionary has /S")
    if sm is None:
        return r
    s = int(sm.group(1))
    r.add(0 <= s < len(payload), "HINT", f"/S ({s}) within hint buffer ({len(payload)} bytes)")
    if not (0 <= s < len(payload)):
        return r

    def adjusted(stored: int) -> int:
        # Annex F: hint-table offsets are measured "as if the hint stream were
        # absent". This writer places all objects before the hint stream, so
        # stored offsets equal real offsets; adjustment is a no-op here.
        return stored + hint_length if stored >= hint_offset else stored

    # --- F.4 / F.5 --------------------------------------------------------
    try:
        f4 = parse_page_offset_table(payload, len(pages))
    except Exception as exc:  # noqa: BLE001 - checker must degrade, not crash
        r.add(False, "F.4", f"page-offset hint table parses ({exc})")
        return r
    r.add(len(f4.nobjects) == len(pages), "F.4", f"page-offset table has {len(pages)} rows")
    r.add(f4.shared_denominator == 4, "F.4", "shared_denominator == 4")
    r.add(
        f4.min_content_length == f4.min_length
        and f4.nbits_content_length == f4.nbits_length,
        "F.4",
        "content-length fields mirror page-length fields (writer convention)",
    )

    # /O object offset vs F.4 first_page_offset.
    o_off = offsets.get(lin.get("O")) if isinstance(lin.get("O"), int) else None
    r.add(
        o_off is not None and adjusted(f4.first_page_offset) == o_off,
        "F.4",
        f"first_page_offset ({adjusted(f4.first_page_offset)}) equals /O object offset ({o_off})",
    )

    # Walk the page-offset chain against the independently scanned objects.
    cumulative = f4.first_page_offset
    layout_ok = True
    for i, num in enumerate(pages):
        actual = offsets.get(num)
        r.add(
            actual is not None and adjusted(cumulative) == actual,
            "F.4",
            f"page {i} cumulative hint offset ({adjusted(cumulative)}) equals object {num} offset ({actual})",
        )
        layout_ok &= actual is not None and adjusted(cumulative) == actual
        nobj = f4.nobjects[i]
        r.add(nobj >= 1, "F.4", f"page {i} object count ({nobj}) >= 1")
        span = 0
        contiguous = True
        for k in range(nobj):
            ln = obj_length(num + k)
            if ln is None:
                contiguous = False
                break
            span += ln
        plen = f4.lengths[i]
        r.add(
            contiguous and span == plen,
            "F.4",
            f"page {i} hint length ({plen}) equals sum of {nobj} real object lengths ({span})",
        )
        r.add(
            f4.content_lengths[i] == plen,
            "F.4",
            f"page {i} content length ({f4.content_lengths[i]}) equals page length ({plen})",
        )
        for idx in f4.shared_ids[i]:
            r.add(
                idx < 1_000_000,
                "F.4",
                f"page {i} shared id {idx} is representable",
            )
        cumulative += plen

    # Physical ordering: every page and its content before the hint stream,
    # first page before later pages.
    page_offsets = [offsets.get(n) for n in pages]
    if all(x is not None for x in page_offsets):
        r.add(
            all(a < b for a, b in zip(page_offsets, page_offsets[1:])),
            "LAYOUT",
            "page objects are physically ordered first-page-first",
        )
        r.add(
            max(page_offsets) < hint_offset,
            "LAYOUT",
            "all page objects lie before the hint stream",
        )
    contents = [page_content_obj(data, offsets, n) for n in pages]
    content_offsets = [offsets.get(c) if c is not None else None for c in contents]
    if all(x is not None for x in content_offsets):
        r.add(
            all(a < b for a, b in zip(content_offsets, content_offsets[1:])),
            "LAYOUT",
            "first page content is physically before later pages' content",
        )
        r.add(
            max(content_offsets) < hint_offset,
            "LAYOUT",
            "all page content streams lie before the hint stream",
        )
    main_offset = xrefs[-1].offset if xrefs else 0
    r.add(
        bool(main_offset) and hint_offset < main_offset,
        "LAYOUT",
        "hint stream lies before the main xref section",
    )

    # /E: end of the first-page section. With the Annex F contiguous numbering
    # this is the offset of object (O + nobjects[0]), or the hint stream itself
    # when there is only one page section.
    if "E" in lin and pages:
        e = lin["E"]
        o = lin.get("O")
        expected = None
        if isinstance(o, int) and f4.nobjects:
            nxt = o + f4.nobjects[0]
            expected = offsets.get(nxt, hint_offset)
        else:
            expected = hint_offset
        r.add(
            e == expected,
            "LIN",
            f"/E ({e}) equals end of first-page section ({expected})",
        )
        r.add(
            o_off is not None and 0 < e <= len(data) and e > o_off,
            "LIN",
            "/E lies after the first page object and within the file",
        )

    try:
        f5 = parse_shared_table(payload, s)
    except Exception as exc:  # noqa: BLE001
        r.add(False, "F.5", f"shared-object hint table parses ({exc})")
        return r
    r.add(
        f5.nshared_total >= f5.nshared_first_page,
        "F.5",
        f"nshared_total ({f5.nshared_total}) >= nshared_first_page ({f5.nshared_first_page})",
    )
    r.add(
        len(f5.group_lengths) == f5.nshared_total,
        "F.5",
        f"shared table has {f5.nshared_total} group rows",
    )
    if isinstance(lin.get("O"), int):
        o = lin["O"]
        # first_shared_obj/offset are only meaningful when a part-8 (cross-page
        # shared) table exists. With nshared_total == nshared_first_page the
        # writer correctly leaves both at 0 (the spec's "unused" value).
        if f5.nshared_total > f5.nshared_first_page:
            first_shared_offset = offsets.get(f5.first_shared_obj)
            r.add(
                first_shared_offset is not None
                and adjusted(f5.first_shared_offset) == first_shared_offset,
                "F.5",
                f"first_shared_offset ({adjusted(f5.first_shared_offset)}) equals object {f5.first_shared_obj} offset ({first_shared_offset})",
            )
        cur = o
        for i in range(f5.nshared_total):
            if i == f5.nshared_first_page:
                cur = f5.first_shared_obj
            ln = obj_length(cur) if cur is not None else None
            glen = f5.group_lengths[i]
            r.add(
                ln == glen,
                "F.5",
                f"shared {i} hint length ({glen}) equals real object length ({ln})",
            )
            if cur is not None:
                cur += 1 + f5.nbits_nobjects  # nobjects_minus_one is 0 bits here

    for ids in f4.shared_ids:
        for idx in ids:
            r.add(
                idx < f5.nshared_total,
                "F.5",
                f"shared id {idx} within {f5.nshared_total} entries",
            )

    # --- Optional Annex F tables -----------------------------------------
    opt: dict[str, int] = {}
    for key in (b"O", b"T", b"A", b"E"):
        m = re.search(rb"/" + key + rb"\s+(\d+)", dict_bytes)
        if m:
            opt[key.decode()] = int(m.group(1))
            r.add(
                0 <= opt[key.decode()] < len(payload),
                "OPT",
                f"optional-table /{key.decode()} offset ({opt[key.decode()]}) within hint buffer",
            )

    # F.7 thumbnails
    if "T" in opt and 0 <= opt["T"] < len(payload):
        try:
            tbl = parse_offset_table(payload, opt["T"], [32], len(pages), 16)
        except Exception as exc:  # noqa: BLE001
            r.add(False, "F.7", f"thumbnail table parses ({exc})")
        else:
            r.add(tbl.headers[0] == len(pages), "F.7", f"thumbnail count == {len(pages)}")
            real = thumbnail_pages(data, offsets, pages)
            present = sum(1 for x in tbl.offsets if x)
            r.add(
                present == len(real),
                "F.7",
                f"thumbnail rows with data ({present}) == pages with /Thumb ({len(real)})",
            )
            for i, x in enumerate(tbl.offsets):
                if x:
                    r.add(
                        0 < x < len(payload) and tbl.extras[i] > 0,
                        "F.7",
                        f"thumbnail page {i} inline record offset/length readable",
                    )
    elif thumbnail_pages(data, offsets, pages):
        r.add(False, "F.7", "document has /Thumb but hint stream has no /T table")

    # F.9 page labels
    if "E" in opt and 0 <= opt["E"] < len(payload):
        try:
            hr = BitReader(payload, opt["E"] * 8)
            min_len, nb = hr.get(32), hr.get(16)
            tbl = parse_offset_table(payload, opt["E"], [32, 16], len(pages), nb)
        except Exception as exc:  # noqa: BLE001
            r.add(False, "F.9", f"page-label table parses ({exc})")
        else:
            starts = page_label_starts(data, offsets, root_num)
            labelled = sum(1 for i in range(len(pages)) if any(s <= i for s in starts))
            present = sum(1 for x in tbl.offsets if x)
            r.add(
                present == labelled,
                "F.9",
                f"page-label rows with data ({present}) == labelled pages ({labelled})",
            )
            for i, x in enumerate(tbl.offsets):
                if x:
                    rec = payload[x : x + min_len + tbl.extras[i]]
                    r.add(
                        rec.startswith(b"<<") and b"/S" in rec,
                        "F.9",
                        f"page-label page {i} inline record is a label dictionary",
                    )
    elif page_label_starts(data, offsets, root_num):
        r.add(False, "F.9", "document has /PageLabels but hint stream has no /E table")

    # F.6 outlines
    if "O" in opt and 0 <= opt["O"] < len(payload):
        try:
            hr = BitReader(payload, opt["O"] * 8)
            nitems = hr.get(32)
            tbl = parse_offset_table(payload, opt["O"], [32], nitems, 0)
        except Exception as exc:  # noqa: BLE001
            r.add(False, "F.6", f"outline table parses ({exc})")
        else:
            real = outline_item_objects(data, offsets, root_num)
            r.add(nitems == len(real), "F.6", f"outline count ({nitems}) == real items ({len(real)})")
            got = set()
            for x in tbl.offsets:
                rec = payload[x : x + 8]
                if len(rec) == 8:
                    got.add(int.from_bytes(rec[:4], "big"))
            r.add(got == real, "F.6", "outline inline item object numbers match real tree")
    elif outline_item_objects(data, offsets, root_num):
        r.add(False, "F.6", "document has an outline tree but hint stream has no /O table")

    # F.8 named destinations
    if "A" in opt and 0 <= opt["A"] < len(payload):
        try:
            hr = BitReader(payload, opt["A"] * 8)
            nitems = hr.get(32)
            tbl = parse_offset_table(payload, opt["A"], [32], nitems, 16)
        except Exception as exc:  # noqa: BLE001
            r.add(False, "F.8", f"named-destination table parses ({exc})")
        else:
            real = named_dest_names(data, offsets, root_num)
            r.add(
                nitems == len(real),
                "F.8",
                f"named-dest count ({nitems}) == real destinations ({len(real)})",
            )
            got = [payload[x : x + n] for x, n in zip(tbl.offsets, tbl.extras)]
            r.add(sorted(got) == sorted(real), "F.8", "named-dest names match real /Dests")
    elif named_dest_names(data, offsets, root_num):
        r.add(False, "F.8", "document has named destinations but hint stream has no /A table")

    return r


# ---------------------------------------------------------------------------
# Interop (explicitly NOT conformance)
# ---------------------------------------------------------------------------


def interop_check(path: str, expected_pages: int, require_ink: bool = False) -> Result:
    """Open with pypdf / pypdfium2 and verify pages. Label: INTEROP.

    Rendering proves the bytes are renderable by a real viewer; whether a page
    has visible ink depends on the fixture (some are intentionally blank), so
    by default only a valid non-empty bitmap is required. ``require_ink``
    additionally demands visible ink on every page.
    """
    r = Result(path=path)
    try:
        from pypdf import PdfReader

        reader = PdfReader(path)
        seen = len(reader.pages)
        r.add(
            seen == expected_pages,
            "INTEROP/pypdf",
            f"pypdf opens the file with {seen} pages (expected {expected_pages})",
        )
        r.add(
            reader.trailer.get("/Root") is not None,
            "INTEROP/pypdf",
            "pypdf resolves a /Root catalog",
        )
    except Exception as exc:  # noqa: BLE001
        r.add(False, "INTEROP/pypdf", f"pypdf could not open file: {exc}")
    try:
        import pypdfium2 as pdfium

        with pdfium.PdfDocument(path) as doc:
            seen = len(doc)
            r.add(
                seen == expected_pages,
                "INTEROP/pdfium",
                f"PDFium opens the file with {seen} pages (expected {expected_pages})",
            )
            for i in range(seen):
                bitmap = doc[i].render(scale=0.5)
                try:
                    buf = bytes(bitmap.buffer)
                    width, height, channels = (
                        int(bitmap.width),
                        int(bitmap.height),
                        int(bitmap.n_channels),
                    )
                finally:
                    bitmap.close()
                ink = sum(1 for v in buf if v < 250)
                valid = width > 0 and height > 0 and channels > 0 and len(buf) > 0
                ok = valid and (ink > 0 if require_ink else True)
                r.add(
                    ok,
                    "INTEROP/pdfium",
                    f"PDFium page {i + 1} renders {width}x{height}x{channels} "
                    f"({ink} ink bytes)"
                    + ("" if not require_ink else " [ink required]"),
                )
    except Exception as exc:  # noqa: BLE001
        r.add(False, "INTEROP/pdfium", f"pypdfium2 could not open/render file: {exc}")
    return r


# ---------------------------------------------------------------------------
# Negative controls (same-length mutations keep xref offsets valid)
# ---------------------------------------------------------------------------


def _replace_once(data: bytes, old: bytes, new: bytes, window: slice | None = None) -> bytes:
    if len(old) != len(new):
        raise ValueError("mutation must preserve byte length")
    region = data if window is None else data[window]
    pos = region.find(old)
    if pos < 0:
        raise ValueError(f"mutation target not found: {old!r}")
    if window is not None:
        pos += window.start
    return data[:pos] + new + data[pos + len(old) :]


def mutate_L(data: bytes) -> bytes:
    lin = parse_lin_dict(data)
    old = f"/L {lin['L']}".encode()
    return _replace_once(data, old, f"/L {lin['L'] + 1}".encode(), slice(0, 1024))


def mutate_H(data: bytes) -> bytes:
    lin = parse_lin_dict(data)
    off, length = lin["H"]
    old = f"/H [ {off} {length} ]".encode()
    return _replace_once(data, old, f"/H [ {off + 1} {length} ]".encode(), slice(0, 1024))


def mutate_N(data: bytes) -> bytes:
    lin = parse_lin_dict(data)
    old = f"/N {lin['N']}".encode()
    return _replace_once(data, old, f"/N {lin['N'] + 1}".encode(), slice(0, 1024))


def mutate_T(data: bytes) -> bytes:
    lin = parse_lin_dict(data)
    old = f"/T {lin['T']}".encode()
    return _replace_once(data, old, f"/T {lin['T'] + 1}".encode(), slice(0, 1024))


def mutate_hint_field(data: bytes) -> bytes:
    """Flip the low bit of F.4 first_page_offset (4 bytes after the header)."""
    lin = parse_lin_dict(data)
    payload = read_stream(data, lin["H"][0]).payload
    pos = lin["H"][0] + data[lin["H"][0] :].find(b"stream\n") + len(b"stream\n")
    target = pos + 4  # min_nobjects is 4 bytes; first_page_offset follows
    old = data[target]
    return data[:target] + bytes([old ^ 1]) + data[target + 1 :]


MUTATIONS = [
    ("L", mutate_L, "/L ("),
    ("H", mutate_H, "/H offset ("),
    ("N", mutate_N, "independently walked page tree"),
    ("hint-field", mutate_hint_field, "first_page_offset"),
    ("T", mutate_T, "/T points at whitespace"),
]


def negative_controls(data: bytes, path: str) -> Result:
    r = Result(path=path)
    base = conformance_check(data, path)
    r.add(base.ok, "NEG", "baseline conformance check passes before mutation")
    if not base.ok:
        return r
    for name, mutate, expected_label in MUTATIONS:
        try:
            mutated = mutate(data)
        except Exception as exc:  # noqa: BLE001
            r.add(False, "NEG", f"{name}: mutation could not be applied ({exc})")
            continue
        result = conformance_check(mutated, path)
        failed = result.failed()
        r.add(
            not result.ok and any(expected_label in f for f in failed),
            "NEG",
            f"mutated {name}: checker fails as expected"
            + ("" if not result.ok else f" (expected a failure containing {expected_label!r})"),
        )
    return r


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def print_result(r: Result, verbose: bool) -> None:
    print(f"== {r.path}: {'PASS' if r.ok else 'FAIL'} ==")
    if r.path:
        summary = r.by_category()
        print("   " + ", ".join(f"{cat}:{p}/{t}" for cat, (p, t) in sorted(summary.items())))
    for c in r.checks:
        if verbose or not c.ok:
            print(f"  [{'ok' if c.ok else 'FAIL'}] [{c.category}] {c.label}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("pdfs", nargs="*", help="linearized PDF(s) to check")
    parser.add_argument("--negative-controls", action="store_true", help="run mutation controls")
    parser.add_argument("--interop", action="store_true", help="also run pypdf/PDFium interop checks")
    parser.add_argument("--pages", type=int, help="expected page count for --interop")
    parser.add_argument(
        "--require-ink",
        action="store_true",
        help="with --interop, require every PDFium page to render visible ink",
    )
    parser.add_argument("--json", action="store_true", help="emit machine-readable JSON")
    parser.add_argument("-v", "--verbose", action="store_true", help="print every check")
    args = parser.parse_args(argv)
    if not args.pdfs:
        parser.print_help()
        return 2
    if args.interop and args.pages is None:
        parser.error("--interop requires --pages N")

    all_ok = True
    payload: dict[str, object] = {}
    for path in args.pdfs:
        data = open(path, "rb").read()
        results: list[Result] = [conformance_check(data, path)]
        if args.negative_controls:
            results.append(negative_controls(data, path))
        if args.interop:
            results.append(interop_check(path, args.pages, args.require_ink))
        if args.json:
            payload[path] = {
                "results": [
                    {
                        "ok": res.ok,
                        "checks": [
                            {"ok": c.ok, "category": c.category, "label": c.label}
                            for c in res.checks
                        ],
                    }
                    for res in results
                ]
            }
        else:
            for res in results:
                print_result(res, args.verbose)
        all_ok &= all(res.ok for res in results)
    if args.json:
        print(json.dumps({"ok": all_ok, "files": payload}, indent=2))
    return 0 if all_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
