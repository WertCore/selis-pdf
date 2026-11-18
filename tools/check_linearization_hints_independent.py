#!/usr/bin/env python3
"""Fresh independent decoder for the OPTIONAL Annex F hint tables (F.6-F.10).

WHY THIS EXISTS
---------------
`qpdf --check-linearization` validates the two mandatory tables (F.4 page
offsets, F.5 shared objects) and F.6 outlines, but the qpdf build used in this
repository **ignores** F.7 thumbnails, F.8 named destinations, F.9 page labels
and F.10 interactive forms. Acrobat Reader cannot be driven to a verdict in
this environment (its COM automation returns E_NOINTERFACE). The honest gap
is therefore: nobody outside the writer has confirmed the *bit layout* of the
optional tables.

This tool closes that gap as far as is achievable offline: it is a SECOND,
INDEPENDENT implementation of both the writer side (table serialization) and
the reader side (table decoding + object-graph cross-check). It re-derives
every expected value directly from the raw bytes of the file under test.

INDEPENDENCE GUARANTEE
----------------------
This file imports only the Python standard library. It shares NO code with:

  * `tools/check_linearization.py`
  * `tools/check_linearization_conformance.py`
  * `crates/selis-pdf-optimize/src/linearize.rs` (the Rust writer)

Its bit reader, object scanner, name-tree walker and table decoders are all
written from scratch here. A PASS is an *independent self-consistency* result:
the optional tables, decoded with a layout reconstructed from ISO 32000-1
Annex F, agree with the real structures in the emitted bytes. It is NOT an
Acrobat verdict and NOT a qpdf verdict for F.7-F.10 -- qpdf ignores those
tables and Acrobat is not automatable here. See the docs for the honest limits.

USAGE
-----
  python tools/check_linearization_hints_independent.py FILE.pdf [FILE.pdf ...]
  python tools/check_linearization_hints_independent.py --negative-controls FILE.pdf [FILE.pdf ...]

Exit code 0 when every file passes (and, with --negative-controls, every
mutation is rejected), 1 otherwise. No third-party modules.
"""

from __future__ import annotations

import argparse
import re
import sys
import zlib


# ---------------------------------------------------------------------------
# Own MSB-first bit reader. The Annex F tables write fields most-significant
# bit first and byte-align the header and each row.
# ---------------------------------------------------------------------------
class BitStream:
    def __init__(self, buf: bytes, byte_offset: int = 0) -> None:
        self.buf = buf
        self.pos = byte_offset * 8

    def read(self, width: int) -> int:
        if width < 0 or width > 64:
            raise ValueError("width out of range")
        value = 0
        for _ in range(width):
            byte = self.buf[self.pos >> 3] if (self.pos >> 3) < len(self.buf) else 0
            bit = (byte >> (7 - (self.pos & 7))) & 1
            value = (value << 1) | bit
            self.pos += 1
        return value

    def align(self) -> None:
        self.pos = (self.pos + 7) & ~7

    def at_byte(self) -> int:
        self.align()
        return self.pos >> 3


# ---------------------------------------------------------------------------
# Small result container.
# ---------------------------------------------------------------------------
class Verdict:
    def __init__(self, label: str) -> None:
        self.label = label
        self.checks: list[tuple[bool, str]] = []
        self.tables: list[str] = []          # which optional tables were decoded
        self.control_results: list[tuple[bool, str]] = []

    def add(self, ok: bool, label: str) -> None:
        self.checks.append((bool(ok), label))

    @property
    def ok(self) -> bool:
        return all(ok for ok, _ in self.checks) and all(
            ok for ok, _ in self.control_results
        )

    def failures(self) -> list[str]:
        return [label for ok, label in self.checks if not ok] + [
            label for ok, label in self.control_results if not ok
        ]


# ---------------------------------------------------------------------------
# Own object scanner. Reads object headers from raw bytes; also decodes
# /Type /ObjStm members so the tool works on xref/object-stream output too.
# ---------------------------------------------------------------------------
_OBJ_HEADER = re.compile(rb"(\d+)\s+(\d+)\s+obj\b")
_REF = re.compile(rb"(\d+)\s+(\d+)\s+R")
_TYPE_PAGE = re.compile(rb"/Type\s*/Pages?(?![A-Za-z])")


class PdfGraph:
    def __init__(self, data: bytes) -> None:
        self.data = data
        self.bodies: dict[int, bytes] = {}
        self.offsets: dict[int, int] = {}
        self.compressed: set[int] = set()
        for match in _OBJ_HEADER.finditer(data):
            num = int(match.group(1))
            if num in self.bodies:
                continue
            start = match.start()
            stop = data.find(b"endobj", match.end())
            if stop < 0:
                stop = len(data)
            self.bodies[num] = data[match.end():stop]
            self.offsets[num] = start
        self._expand_object_streams()

    def _expand_object_streams(self) -> None:
        for num, body in list(self.bodies.items()):
            if b"/ObjStm" not in body:
                continue
            raw = self._stream_bytes(body)
            if raw is None:
                continue
            n_match = re.search(rb"/N\s+(\d+)", body)
            first_match = re.search(rb"/First\s+(\d+)", body)
            if not n_match or not first_match:
                continue
            count = int(n_match.group(1))
            first = int(first_match.group(1))
            header = raw[:first].split()
            if len(header) < count * 2:
                continue
            for i in range(count):
                obj_num = int(header[2 * i])
                rel = int(header[2 * i + 1])
                end = int(header[2 * i + 3]) + first if i + 1 < count else len(raw)
                self.bodies.setdefault(obj_num, raw[first + rel:end])
                self.offsets.setdefault(obj_num, self.offsets[num])
                self.compressed.add(obj_num)

    @staticmethod
    def _stream_bytes(body: bytes) -> bytes | None:
        marker = body.find(b"stream")
        if marker < 0:
            return None
        pos = marker + 6
        if body[pos:pos + 2] == b"\r\n":
            pos += 2
        elif body[pos:pos + 1] in (b"\n", b"\r"):
            pos += 1
        end = body.find(b"endstream", pos)
        if end < 0:
            return None
        raw = body[pos:end]
        if raw.endswith(b"\r\n"):
            raw = raw[:-2]
        elif raw.endswith(b"\n") or raw.endswith(b"\r"):
            raw = raw[:-1]
        if b"/FlateDecode" in body[:marker]:
            try:
                raw = zlib.decompress(raw)
            except zlib.error:
                return None
        return raw

    def body(self, num: int) -> bytes:
        return self.bodies.get(num, b"")

    def offset(self, num: int) -> int | None:
        return self.offsets.get(num)

    def refs(self, blob: bytes) -> list[int]:
        return [int(m.group(1)) for m in _REF.finditer(blob)]

    def catalog_number(self) -> int | None:
        match = re.search(rb"/Root\s+(\d+)\s+\d+\s+R", self.data)
        return int(match.group(1)) if match else None

    def leaf_pages(self) -> list[int]:
        """Leaf page object numbers in document (/Kids traversal) order."""
        catalog = self.catalog_number()
        if catalog is None:
            return []
        root = re.search(rb"/Pages\s+(\d+)\s+\d+\s+R", self.body(catalog))
        if not root:
            return []
        leaves: list[int] = []
        seen: set[int] = set()

        def visit(num: int) -> None:
            if num in seen:
                return
            seen.add(num)
            body = self.body(num)
            if re.search(rb"/Type\s*/Page(?![sA-Za-z])", body):
                leaves.append(num)
                return
            kids = re.search(rb"/Kids\s*\[(.*?)\]", body, re.S)
            if kids:
                for ref in self.refs(kids.group(1)):
                    visit(ref)

        visit(int(root.group(1)))
        return leaves


# ---------------------------------------------------------------------------
# Hint stream location + dictionary parsing.
# ---------------------------------------------------------------------------
class HintStream:
    def __init__(
        self,
        obj_offset: int,
        dictionary: bytes,
        payload: bytes,
        payload_abs: int,
        keys: dict[str, int],
    ) -> None:
        self.obj_offset = obj_offset
        self.dictionary = dictionary
        self.payload = payload
        self.payload_abs = payload_abs
        self.keys = keys


def locate_hint(data: bytes) -> HintStream | None:
    head = data[:1024]
    h = re.search(rb"/H\s*\[\s*(\d+)\s+(\d+)\s*\]", head)
    if not h:
        return None
    off = int(h.group(1))
    header = re.match(rb"\s*(\d+)\s+(\d+)\s+obj\b", data[off:])
    if not header:
        return None
    body_start = off + header.end()
    marker = data.find(b"stream", body_start)
    endobj = data.find(b"endobj", body_start)
    if marker < 0 or endobj < 0 or marker > endobj:
        return None
    dictionary = data[body_start:marker]
    keys: dict[str, int] = {}
    for m in re.finditer(rb"/([A-Za-z]+)\s+(\d+)", dictionary):
        keys[m.group(1).decode("ascii")] = int(m.group(2))
    pos = marker + 6
    if data[pos:pos + 2] == b"\r\n":
        pos += 2
    elif data[pos:pos + 1] in (b"\n", b"\r"):
        pos += 1
    length = keys.get("Length")
    if length is None or length < 0 or pos + length > len(data):
        return None
    return HintStream(off, dictionary, data[pos:pos + length], pos, keys)


# ---------------------------------------------------------------------------
# Independent derivations of the real structures (from the emitted bytes).
# ---------------------------------------------------------------------------
def real_outline(graph: PdfGraph) -> tuple[int | None, set[int]]:
    catalog = graph.catalog_number()
    if catalog is None:
        return None, set()
    match = re.search(rb"/Outlines\s+(\d+)\s+\d+\s+R", graph.body(catalog))
    if not match:
        return None, set()
    root = int(match.group(1))
    reached: set[int] = set()
    pending = [root]
    while pending:
        num = pending.pop()
        if num in reached:
            continue
        body = graph.body(num)
        if not body and num not in graph.bodies:
            continue
        if _TYPE_PAGE.search(body):
            continue
        reached.add(num)
        pending.extend(graph.refs(body))
    return root, reached


def real_thumbnails(graph: PdfGraph, pages: list[int]) -> list[bytes | None]:
    out: list[bytes | None] = []
    for num in pages:
        match = re.search(rb"/Thumb\s+(\d+)\s+\d+\s+R", graph.body(num))
        if not match:
            out.append(None)
            continue
        stream_body = graph.body(int(match.group(1)))
        out.append(PdfGraph._stream_bytes(stream_body))
    return out


def real_named_dests(graph: PdfGraph) -> dict[bytes, int | None]:
    """Map destination name -> zero-based page index (or None)."""
    catalog = graph.catalog_number()
    if catalog is None:
        return {}
    dests_body = b""
    direct = re.search(rb"/Dests\s+(\d+)\s+\d+\s+R", graph.body(catalog))
    if direct:
        dests_body = graph.body(int(direct.group(1)))
    if not dests_body:
        names = re.search(rb"/Names\s+(\d+)\s+\d+\s+R", graph.body(catalog))
        if names:
            tree = graph.body(int(names.group(1)))
            entry = re.search(rb"/Dests\s+(\d+)\s+\d+\s+R", tree)
            if entry:
                dests_body = graph.body(int(entry.group(1)))
    if not dests_body:
        return {}
    pages = graph.leaf_pages()
    result: dict[bytes, int | None] = {}
    # /Names [(name) [pageref ...] (name2) [pageref2 ...]] -- pair each
    # parenthesised name with the bracketed array that immediately follows.
    for m in re.finditer(rb"\(([^()]*)\)\s*\[([^\]]*)\]", dests_body):
        name = m.group(1)
        refs = graph.refs(m.group(2))
        page_index = None
        if refs and refs[0] in pages:
            page_index = pages.index(refs[0])
        result[name] = page_index
    return result


def real_page_label_dicts(graph: PdfGraph, pages: list[int]) -> list[bytes | None]:
    """Per-page body of the /PageLabels range dictionary governing that page.

    Returns one entry per page: the raw `<< ... >>` bytes of the effective
    label dictionary, or None for an unlabelled page. Derived only by walking
    the /PageLabels /Nums number tree in the emitted bytes.
    """
    none_list: list[bytes | None] = [None] * len(pages)
    catalog = graph.catalog_number()
    if catalog is None:
        return none_list
    match = re.search(rb"/PageLabels\s+(\d+)\s+\d+\s+R", graph.body(catalog))
    if not match:
        return none_list
    tree = graph.body(int(match.group(1)))
    nums = re.search(rb"/Nums\s*\[(.*?)\]", tree, re.S)
    if not nums:
        return none_list
    ranges: list[tuple[int, bytes]] = []
    for mm in re.finditer(rb"(\d+)\s*(<<.*?>>)", nums.group(1), re.S):
        ranges.append((int(mm.group(1)), mm.group(2)))
    if not ranges:
        return none_list
    result: list[bytes | None] = []
    for i in range(len(pages)):
        applicable = [(s, d) for s, d in ranges if s <= i]
        result.append(max(applicable, key=lambda x: x[0])[1] if applicable else None)
    return result


def _norm_dict(blob: bytes) -> bytes | None:
    """Normalize a direct `<< ... >>` dictionary body for byte comparison.

    Runs of whitespace are collapsed to single spaces (PDF whitespace is not
    significant between tokens); the delimiters themselves are kept so a
    non-dictionary record cannot compare equal.
    """
    start = blob.find(b"<<")
    if start < 0:
        return None
    end = blob.find(b">>", start + 2)
    if end < 0:
        return None
    return re.sub(rb"\s+", b" ", blob[start + 2:end]).strip()


def real_form_fields(graph: PdfGraph, pages: list[int]) -> dict[int, int]:
    catalog = graph.catalog_number()
    if catalog is None:
        return {}
    match = re.search(rb"/AcroForm\s+(\d+)\s+\d+\s+R", graph.body(catalog))
    if not match:
        return {}
    acro = graph.body(int(match.group(1)))
    fields = re.search(rb"/Fields\s*\[(.*?)\]", acro, re.S)
    if not fields:
        return {}

    widget_page: dict[int, int] = {}
    for idx, pnum in enumerate(pages):
        annots = re.search(rb"/Annots\s*\[(.*?)\]", graph.body(pnum), re.S)
        if annots:
            for ref in graph.refs(annots.group(1)):
                widget_page[ref] = idx

    def page_of(num: int, path: set[int]) -> int:
        if num in widget_page:
            return widget_page[num]
        if num in path:
            return 0xFFFFFFFF
        path.add(num)
        kids = re.search(rb"/Kids\s*\[(.*?)\]", graph.body(num), re.S)
        if kids:
            for ref in graph.refs(kids.group(1)):
                found = page_of(ref, path)
                if found != 0xFFFFFFFF:
                    return found
        return 0xFFFFFFFF

    result: dict[int, int] = {}
    seen: set[int] = set()

    def walk(blob: bytes) -> None:
        for ref in graph.refs(blob):
            if ref in seen:
                continue
            seen.add(ref)
            result[ref] = page_of(ref, set())
            kids = re.search(rb"/Kids\s*\[(.*?)\]", graph.body(ref), re.S)
            if kids:
                walk(kids.group(1))

    walk(fields.group(1))
    return result


# ---------------------------------------------------------------------------
# The five independent table decoders. Each returns nothing; it adds checks to
# the verdict. Every field is read from `bits` (our own reader), never from the
# other checkers' helpers.
# ---------------------------------------------------------------------------
def decode_outline_table(v: Verdict, payload: bytes, off: int, graph: PdfGraph) -> None:
    bits = BitStream(payload, off)
    first_object = bits.read(32)
    first_offset = bits.read(32)
    count = bits.read(32)
    group_length = bits.read(32)
    root, members = real_outline(graph)
    v.add(first_object == root, f"F.6 first_object ({first_object}) == /Outlines object ({root})")
    v.add(count == len(members), f"F.6 nobjects ({count}) == outline objects ({len(members)})")
    actual = graph.offset(root) if root is not None else None
    if actual is not None and root not in graph.compressed:
        v.add(first_offset == actual,
              f"F.6 first_object_offset ({first_offset}) == real object offset ({actual})")
    else:
        v.add(0 <= first_offset < len(graph.data),
              f"F.6 first_object_offset ({first_offset}) within file")
    v.add(group_length > 0, f"F.6 group_length ({group_length}) positive")
    if actual is not None and group_length > 0:
        lo, hi = first_offset, first_offset + group_length
        inside = all(
            lo <= (graph.offset(m) or 0) <= hi for m in members if m in graph.offsets
        )
        v.add(inside, "F.6 every outline object offset lies inside the declared group span")


def _offset_rows(
    bits: BitStream, count: int, extra_width: int
) -> tuple[list[int], list[int]]:
    """Decode a header-aligned run of (32-bit offset, extra) rows."""
    offsets: list[int] = []
    extras: list[int] = []
    for _ in range(count):
        offsets.append(bits.read(32))
        extras.append(bits.read(extra_width) if extra_width else 0)
        bits.align()
    return offsets, extras


def decode_thumbnail_table(v: Verdict, payload: bytes, off: int, graph: PdfGraph,
                           pages: list[int]) -> None:
    bits = BitStream(payload, off)
    count = bits.read(32)
    bits.align()
    offsets, lengths = _offset_rows(bits, count, 16)
    real = real_thumbnails(graph, pages)
    v.add(count == len(pages), f"F.7 thumbnail row count ({count}) == page count ({len(pages)})")
    present = sum(1 for o in offsets if o)
    expected = sum(1 for x in real if x is not None)
    v.add(present == expected,
          f"F.7 populated thumbnail rows ({present}) == pages with /Thumb ({expected})")
    matched = 0
    for i, o in enumerate(offsets):
        if not o:
            continue
        record = payload[o:o + lengths[i]] if 0 <= o < len(payload) else b""
        if lengths[i] > 0 and record == real[i]:
            matched += 1
    v.add(matched == expected,
          f"F.7 thumbnail inline bytes equal real /Thumb streams ({matched}/{expected})")


def decode_named_dest_table(v: Verdict, payload: bytes, off: int, graph: PdfGraph) -> None:
    bits = BitStream(payload, off)
    count = bits.read(32)
    bits.align()
    offsets, name_lengths = _offset_rows(bits, count, 16)
    real = real_named_dests(graph)
    v.add(count == len(real), f"F.8 named-dest count ({count}) == /Dests entries ({len(real)})")
    names: list[bytes] = []
    pages_from_table: list[int | None] = []
    for o, nlen in zip(offsets, name_lengths):
        if 0 <= o < len(payload):
            names.append(payload[o:o + nlen])
            tail = payload[o + nlen:o + nlen + 4]
            pages_from_table.append(
                int.from_bytes(tail, "big") if len(tail) == 4 else None
            )
        else:
            names.append(b"")
            pages_from_table.append(None)
    v.add(sorted(names) == sorted(real),
          f"F.8 named-dest names match /Dests tree ({len(names)}/{len(real)})")
    page_mismatch = 0
    for name, page in zip(names, pages_from_table):
        if name in real and real[name] is not None and page != real[name]:
            page_mismatch += 1
    v.add(page_mismatch == 0,
          f"F.8 named-dest inline page numbers match /Dests ({page_mismatch} mismatches)")


def decode_page_label_table(v: Verdict, payload: bytes, off: int, graph: PdfGraph,
                            pages: list[int]) -> None:
    bits = BitStream(payload, off)
    min_len = bits.read(32)
    delta_bits = bits.read(16)
    bits.align()
    offsets, deltas = _offset_rows(bits, len(pages), delta_bits)
    real = real_page_label_dicts(graph, pages)
    expected = sum(1 for x in real if x is not None)
    present = sum(1 for o in offsets if o)
    v.add(present == expected,
          f"F.9 populated page-label rows ({present}) == labelled pages ({expected})")
    good = 0
    for i, o in enumerate(offsets):
        if not o:
            continue
        length = min_len + deltas[i]
        record = payload[o:o + length] if 0 <= o < len(payload) else b""
        decoded = _norm_dict(record)
        actual = _norm_dict(real[i]) if real[i] is not None else None
        if decoded is not None and decoded == actual:
            good += 1
    v.add(good == expected,
          f"F.9 inline record bytes match /PageLabels dictionaries ({good}/{expected})")


def decode_form_table(v: Verdict, payload: bytes, off: int, graph: PdfGraph,
                      pages: list[int]) -> None:
    real = real_form_fields(graph, pages)
    bits = BitStream(payload, off)
    count = bits.read(32)
    bits.align()
    v.add(bool(real), "F.10 /F present only when /AcroForm fields exist")
    v.add(count == len(real), f"F.10 form field count ({count}) == /AcroForm fields ({len(real)})")
    got: dict[int, int] = {}
    for _ in range(count):
        obj = bits.read(32)
        page = bits.read(32)
        bits.align()
        got[obj] = page
    v.add(set(got) == set(real),
          f"F.10 form field object numbers match /AcroForm tree ({len(got)}/{len(real)})")
    mismatches = [o for o, p in got.items() if o in real and real[o] != p]
    v.add(not mismatches, f"F.10 form field page numbers match widgets ({len(mismatches)} mismatches)")


# ---------------------------------------------------------------------------
# Top-level analysis.
# ---------------------------------------------------------------------------
_TABLE_KEYS = ("O", "T", "A", "E", "F")


def analyze(data: bytes, label: str) -> Verdict:
    v = Verdict(label)
    hint = locate_hint(data)
    if hint is None:
        v.add(False, "hint stream located from /H")
        return v
    v.add(True, "hint stream located from /H")

    graph = PdfGraph(data)
    pages = graph.leaf_pages()
    v.add(bool(pages), "leaf page tree walked")
    v.add("S" in hint.keys and 0 <= hint.keys["S"] < len(hint.payload),
          f"/S ({hint.keys.get('S')}) within hint payload ({len(hint.payload)} bytes)")

    structure_present = {
        "O": real_outline(graph)[0] is not None,
        "T": any(x is not None for x in real_thumbnails(graph, pages)),
        "A": bool(real_named_dests(graph)),
        "E": any(x is not None for x in real_page_label_dicts(graph, pages)),
        "F": bool(real_form_fields(graph, pages)),
    }

    for key in _TABLE_KEYS:
        off = hint.keys.get(key)
        if off is not None:
            v.add(0 <= off < len(hint.payload),
                  f"/{key} optional-table offset ({off}) within hint payload")
        if off is not None and 0 <= off < len(hint.payload):
            v.tables.append(key)
            if key == "O":
                decode_outline_table(v, hint.payload, off, graph)
            elif key == "T":
                decode_thumbnail_table(v, hint.payload, off, graph, pages)
            elif key == "A":
                decode_named_dest_table(v, hint.payload, off, graph)
            elif key == "E":
                decode_page_label_table(v, hint.payload, off, graph, pages)
            elif key == "F":
                decode_form_table(v, hint.payload, off, graph, pages)
        elif structure_present[key]:
            v.add(False, f"/{key} table key present (source structure exists)")

    v.add(sorted(v.tables) == sorted(k for k in _TABLE_KEYS if structure_present[k]),
          f"optional tables present match source structures ({','.join(v.tables) or 'none'})")
    return v


# ---------------------------------------------------------------------------
# Negative controls: for every present table, flip the low bit of several of
# its fields (header count/object, inline offsets, lengths and page indices)
# using SAME-LENGTH edits, and require that the decoder rejects with a failure
# attributed to that table. Byte positions are relative to the table start.
# Offsets/lengths/page indices are big-endian, so byte 3 of a 32-bit field,
# byte 9 of a 16-bit field, etc. hold the value's low bit.
#
# F.9's header is (min_len:32, delta_bits:16); flipping min_len's LSB is
# parity-dependent (an even min_len grows and still parses), so we mutate the
# first row's 32-bit inline offset instead (byte 6 + 3 = 9).
# ---------------------------------------------------------------------------
_MUTATIONS: dict[str, list[tuple[int, str]]] = {
    "O": [(3, "first_object"), (11, "nobjects")],
    # A 1-byte 0x00 thumbnail makes an offset LSB flip land on an adjacent zero
    # byte, so we mutate the deterministic row count and row length instead.
    "T": [(3, "row count"), (9, "row0 length")],
    "A": [(3, "row count"), (9, "row0 name length")],
    "E": [(9, "row0 inline offset")],
    "F": [(3, "field count"), (7, "row0 field object"), (11, "row0 page index")],
}
_TABLE_PREFIX = {"O": "F.6", "T": "F.7", "A": "F.8", "E": "F.9", "F": "F.10"}


def negative_controls(data: bytes, label: str) -> Verdict:
    result = Verdict(label + " [negative controls]")
    baseline = analyze(data, label)
    result.control_results.append(
        (baseline.ok, "baseline decode passes before any mutation")
    )
    hint = locate_hint(data)
    if hint is None:
        result.control_results.append((False, "hint stream locatable for mutation"))
        return result
    for key in _TABLE_KEYS:
        off = hint.keys.get(key)
        if off is None or not (0 <= off < len(hint.payload)):
            continue
        prefix = _TABLE_PREFIX[key]
        for position, field_name in _MUTATIONS[key]:
            absolute = hint.payload_abs + off + position
            if not (0 <= absolute < len(data)):
                result.control_results.append(
                    (False, f"/{key} {field_name} mutation target within payload"))
                continue
            mutated = bytearray(data)
            mutated[absolute] ^= 0x01
            verdict = analyze(bytes(mutated), label + f" [mutated /{key} {field_name}]")
            rejected = not verdict.ok
            reasons = [f for f in verdict.failures() if prefix in f]
            result.control_results.append(
                (rejected and bool(reasons),
                 f"/{key} {field_name} corruption rejected by {prefix} check "
                 f"(rejected={rejected}, reason={reasons[0] if reasons else 'none'})")
            )
    result.tables = baseline.tables
    return result


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def _print(v: Verdict, mode: str) -> None:
    checks = v.control_results if mode == "negative" else v.checks
    status = "PASS" if v.ok else "FAIL"
    tables = ",".join(v.tables) or "none"
    print(f"== {v.label}: {status}  tables=[{tables}]  "
          f"checks={sum(1 for o, _ in checks if o)}/{len(checks)} ==")
    for ok, name in checks:
        print(f"  [{'ok' if ok else 'FAIL'}] {name}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("files", nargs="+", help="linearized PDF(s) to decode")
    parser.add_argument("--negative-controls", action="store_true",
                        help="also mutate each present table and require rejection")
    args = parser.parse_args(argv)

    all_ok = True
    for name in args.files:
        try:
            data = open(name, "rb").read()
        except OSError as exc:
            print(f"== {name}: FAIL (cannot read: {exc}) ==")
            all_ok = False
            continue
        verdict = analyze(data, name)
        _print(verdict, "tables")
        all_ok &= verdict.ok
        if args.negative_controls:
            controls = negative_controls(data, name)
            _print(controls, "negative")
            all_ok &= controls.ok
    print(f"\nOVERALL: {'PASS' if all_ok else 'FAIL'}")
    return 0 if all_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
