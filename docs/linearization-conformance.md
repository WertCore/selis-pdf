# Independent linearization conformance checker

`tools/check_linearization_conformance.py` is a from-scratch verifier of the
ISO 32000-1 Annex F "fast web view" contract for PDFs produced by
`selis-pdf-optimize::write_linearized`.

## Independence — what "independent" means here

The checker:

* shares **no code** with the writer (`crates/selis-pdf-optimize/src/linearize.rs`);
* does **not import** the earlier self-consistency checker
  (`tools/check_linearization.py`) or its PDFium companion;
* re-derives every checked value from the **raw bytes** of the file under test
  using its own object scanner, xref parser, object-graph walker and MSB-first
  bit reader;
* needs only the Python standard library for the conformance verdict;
* does not use qpdf, mutool, pdftk, pikepdf, or any network install (none of
  these are installed in this environment and none are fetched).

It is therefore an independent implementation, but **it is not
`qpdf --check-linearization`**. See [Limitations](#limitations-exactly-what-this-does-not-prove).

## What the checker validates

All checks run from raw bytes and are reported by category.

**HEADER / OBJ / LIN**

* `%PDF-` header; object 1 is found by an independent `N 0 obj` byte scan with
  generation 0.
* Object 1 is a `/Linearized 1` dictionary carrying **all seven** parameters
  `/L /H /O /E /N /T /P`.
* `/L` equals the real file length; `/P == 1`.
* `/O` names a real object whose `/Type` is `/Page` (not `/Pages`) **and** is
  the first leaf of the independently walked page tree.
* `/N` equals the number of leaf pages obtained by walking `/Root → /Pages →
  /Kids` from the raw object graph.
* `/T` points at whitespace that skips exactly to entry 0 of the **first** xref
  section.
* `/E` equals the byte offset where the first-page section ends — the offset of
  object `O + nobjects(page 0)`, or the hint stream when the document has a
  single page section.

**XREF / TREE**

* Exactly two classic xref sections; `startxref` targets the first-page xref;
  the first section's `/Prev` targets the main xref; the main xref has no
  `/Prev`; the chain is acyclic; both trailers agree on `/Root`.
* Every in-use xref entry points at the `N 0 obj` header the byte scan actually
  found at that offset, and the main xref covers every scanned object.
* Trailer `/Size` covers the entries of each section.

**HINT / F.4 / F.5**

* `/H [ offset length ]` points exactly at a scanned object header that is a
  stream; its `/Length` and `/H` length both equal the real payload length;
  `/S` lies within the payload.
* **F.4 page-offset table** decoded bit-by-bit: header widths are sane,
  `shared_denominator == 4`, content-length fields mirror page-length fields;
  `first_page_offset` equals the `/O` object offset; for every page the
  cumulative hint offset equals the page object offset, the hinted page length
  equals the sum of the **actual serialized lengths** of that page's contiguous
  object run, object counts are at least 1, and shared ids are in range.
  Annex F row order (nobjects, page length, shared count, shared ids, shared
  numerators, content offsets, content lengths) is respected.
* **F.5 shared-object table**: counts are consistent, group lengths match the
  real object lengths (including the part-8 first-shared object/offset when a
  cross-page shared object exists), and shared ids reference real entries.
* **LAYOUT**: page objects are physically ordered first-page-first, each page's
  content stream is before later pages' content, all objects lie before the
  hint stream, and the hint stream lies before the main xref.

**OPT / F.6 / F.7 / F.8 / F.9**

When a hint-stream table key is present it is decoded and cross-checked against
the independently derived object graph:

* F.6 outline (`/O`): item count and inline item object numbers vs. the real
  outline tree;
* F.7 thumbnails (`/T`): row count and populated rows vs. real page `/Thumb`
  streams;
* F.8 named destinations (`/A`): count and names vs. the real `/Dests` /
  `/Names` name tree;
* F.9 page labels (`/E`): populated rows vs. real `/PageLabels` ranges, and
  inline records are label dictionaries.

Conversely, if the object graph has one of those structures the corresponding
table key must be present. A document with none emits no keys and that is
accepted.

## Negative controls

`--negative-controls` mutates four linearization parameters and one hint field
with **same-length** edits (so xref offsets stay valid and the failure must be
attributable to the mutated field). It requires the checker to reject each:

| Mutation | Expected failure |
|---|---|
| `/L` + 1 | `/L (…) … file length` |
| `/H` offset + 1 | `/H offset (…) points exactly at a scanned object header` |
| `/N` + 1 | `independently walked page tree …` |
| F.4 `first_page_offset` low bit flipped | `first_page_offset (…)` |
| `/T` + 1 | `/T points at whitespace …` |

All five controls pass on every fixture below (baseline passes, every mutation
fails with the expected label).

## Interop — explicitly NOT conformance

`--interop` opens the file with two independently implemented readers:

* pypdf: opens and reports the expected page count and a `/Root`;
* pypdfium2 (PDFium, the engine behind Chrome/Edge): opens, reports the
  expected page count, and renders every page to a valid non-empty bitmap
  (`--require-ink` additionally requires visible ink).

These prove the bytes are readable and renderable by real viewers. They are
labelled `INTEROP/*` and are **not** Annex F conformance evidence.

## Verified verdicts

Driver: `python tools/check_linearization_conformance_test.py` (regenerates with
the writer binary); `--no-build` checks committed snapshot vectors in
`corpus/fixtures/lin_conformance/`.

| Fixture class | Conformance | Negative controls | Interop | Notable tables |
|---|---|---|---|---|
| multi-page + part-8 shared object (3 pages) | PASS (72 checks) | PASS (6/6) | PASS | F.4 18/18, F.5 8/8 (real first-shared offset) |
| outline + named dests (5 pages) | PASS (77 checks) | PASS (6/6) | PASS | F.6, F.8 |
| AcroForm (2 pages) | PASS (81 checks) | PASS (6/6) | PASS | F.6 |
| hint tables: page labels + thumbnail (2 pages) | PASS (71 checks) | PASS (6/6) | PASS | F.7, F.9 |
| verification features (2 pages) | PASS (64 checks) | PASS (6/6) | PASS | F.6 |

(The per-category counts printed by the driver are the authoritative detail;
the totals above are the sums of those categories.)

## Limitations — exactly what this does NOT prove

Honesty matters more than a green tick:

1. **This is not qpdf.** qpdf, mutool, pdftk and pikepdf are absent and were not
   installed from the network. No `qpdf --check-linearization` verdict exists
   for these files. A pass here must not be reported as one.
2. **Optional-table bit layouts are the writer's documented reconstruction of
   Annex F.** For F.6–F.9 the checker proves that the inline records it decodes
   with that layout match the real structures, but it cannot prove that qpdf or
   Acrobat interprets those bytes the same way — that is precisely the external
   verdict we do not have. The core F.4/F.5 layouts and semantics are much more
   tightly specified and are cross-checked against independently derived byte
   offsets and object lengths.
3. **No object streams / xref streams.** The writer emits classic xref and
   plain objects only; the checker validates that world and does not exercise
   qpdf's checks for PDF 1.5+ cross-reference streams or object streams.
4. **No shared-object MD5 signatures.** F.5 `signature_present` bits are always
   0 in this writer; the checker validates that, but does not verify MD5 group
   signatures that a general checker would accept when present.
5. **No optional AcroForm hint table (F.10).** The writer does not emit it; the
   checker does not check for it. This remains a known capability gap.
6. **Not a full conformance suite.** It does not validate content-stream
   semantics, colour spaces, fonts, encryption (the writer refuses encrypted
   inputs), incremental updates, or whether a viewer actually *uses* the hint
   tables for progressive display. It checks the structural contract the
   writer claims, not every clause of ISO 32000-1.
7. **Interop is not conformance.** pypdf/PDFium reading the file successfully
   does not mean the Annex F tables are correct.

## Running

```bash
python tools/check_linearization_conformance.py FILE.pdf [-v] [--json]
python tools/check_linearization_conformance.py --negative-controls FILE.pdf
python tools/check_linearization_conformance.py --interop --pages N FILE.pdf [--require-ink]
python tools/check_linearization_conformance_test.py            # regenerate + full matrix
python tools/check_linearization_conformance_test.py --no-build # committed vectors
```
