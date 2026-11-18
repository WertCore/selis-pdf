#!/usr/bin/env python3
"""Independent PDFium interop check for a Selis-linearized PDF.

WHAT THIS IS
------------
An INDEPENDENT VIEWER check. It opens the linearized file with pypdfium2
(PDFium, the PDF engine behind Chrome and Edge), asserts that the viewer sees
the expected number of pages, and renders every page to a bitmap that is not
blank. A pass proves that the bytes Selis wrote are readable and renderable by
a real, separately implemented PDF viewer - not just by Selis's own parser.

WHAT THIS IS NOT
----------------
This is NOT qpdf conformance. It does not run `qpdf --check-linearization`,
does not parse the hint tables, and does not validate /L, /H, /O, /N, /T, /E
or the xref /Prev chain. qpdf, mutool, pdftk and pikepdf are not installed in
this environment and are not fetched from the network. Selis's own structural,
hint-table-level self-consistency check lives in tools/check_linearization.py;
that checker is also not the qpdf binary. Neither tool may be reported as a
`qpdf --check-linearization` verdict.

Usage:
    python tools/check_linearization_pdfium.py FILE.pdf --pages 2

Exit code 0 when every assertion holds, 1 on a failed assertion or a viewer
error, and 2 on a usage error. Requires only pypdfium2 (already installed).
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path


def render_non_blank(page: object, scale: float = 1.0) -> tuple[bool, str]:
    """Render one page and report whether the bitmap holds visible ink.

    PdfBitmap.buffer is a ctypes byte array in BGRA order. A blank white page
    is every channel at 255; a drawn page has at least one channel below the
    ink threshold. The count is returned so a pass is evidence, not a bare
    boolean.
    """
    bitmap = page.render(scale=scale)
    try:
        data = bytes(bitmap.buffer)
        channels = int(bitmap.n_channels)
        width = int(bitmap.width)
        height = int(bitmap.height)
        if width <= 0 or height <= 0 or channels <= 0 or not data:
            return False, f"empty bitmap ({width}x{height}, {channels} channels)"
        ink = sum(1 for value in data if value < 250)
        if ink == 0:
            return False, f"all {len(data)} bytes were blank ({width}x{height})"
        return True, f"{width}x{height}, {channels} channels, {ink}/{len(data)} non-white bytes"
    finally:
        bitmap.close()


def check(path: Path, expected_pages: int) -> bool:
    data = path.read_bytes()
    ok = True

    # Input sanity, not an interop assertion: make sure the file under test is
    # the linearized artifact and not the original by mistake.
    linearized = b"/Linearized 1" in data[:1024]
    print(f"[{'ok' if linearized else 'FAIL'}] input carries a /Linearized 1 dictionary")
    ok &= linearized

    try:
        import pypdfium2 as pdfium
    except ImportError as exc:  # pragma: no cover - environment failure
        print(f"[FAIL] pypdfium2 is not importable: {exc}")
        return False

    try:
        document = pdfium.PdfDocument(str(path))
    except Exception as exc:  # noqa: BLE001 - report any viewer refusal verbatim
        print(f"[FAIL] PDFium could not open the file: {exc}")
        return False

    try:
        seen = len(document)
        match = seen == expected_pages
        print(f"[{'ok' if match else 'FAIL'}] PDFium sees {seen} page(s), expected {expected_pages}")
        ok &= match

        for index in range(seen):
            try:
                rendered, detail = render_non_blank(document[index])
            except Exception as exc:  # noqa: BLE001 - viewer render failure
                rendered, detail = False, str(exc)
            print(f"[{'ok' if rendered else 'FAIL'}] page {index + 1} renders: {detail}")
            ok &= rendered
    finally:
        document.close()

    print(f"PDFium interop: {'PASS' if ok else 'FAIL'} (not a qpdf conformance verdict)")
    return bool(ok)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("pdf", type=Path, help="the linearized PDF to open in PDFium")
    parser.add_argument("--pages", type=int, required=True, help="expected page count")
    args = parser.parse_args(argv)
    if args.pages < 1:
        parser.error("--pages must be at least 1")
    if not args.pdf.is_file():
        parser.error(f"no such file: {args.pdf}")
    return 0 if check(args.pdf, args.pages) else 1


if __name__ == "__main__":
    raise SystemExit(main())
