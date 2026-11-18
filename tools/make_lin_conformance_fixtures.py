#!/usr/bin/env python3
"""Create a deterministic multi-page input fixture for the conformance test.

Only the multi-page fixture is generated here: the outline, form and optional
hint-table inputs already exist under corpus/fixtures/ and are reused as-is.
The generated fixture gives every page visible, distinct ink and a **shared**
Form XObject, so the linearizer must emit a part-8 shared-object table with a
real first-shared object/offset (F.5), not just the first-page group.
"""

from __future__ import annotations

import sys
from pathlib import Path

from pypdf import PdfWriter
from pypdf.generic import (
    ArrayObject,
    DecodedStreamObject,
    DictionaryObject,
    NameObject,
    NumberObject,
)


def make_multipage(path: Path, pages: int = 3) -> None:
    w = PdfWriter()
    for _ in range(pages):
        w.add_blank_page(240, 320)

    # One shared Form XObject referenced by every page -> part-8 shared object.
    form = DecodedStreamObject()
    form.set_data(b"0.2 0.4 0.9 rg\n10 10 60 60 re f\n")
    form.update(
        {
            NameObject("/Type"): NameObject("/XObject"),
            NameObject("/Subtype"): NameObject("/Form"),
            NameObject("/BBox"): ArrayObject(
                [NumberObject(0), NumberObject(0), NumberObject(70), NumberObject(70)]
            ),
        }
    )
    form_ref = w._add_object(form)
    xobjects = DictionaryObject({NameObject("/Shared"): form_ref})

    for i, page in enumerate(w.pages):
        stream = DecodedStreamObject()
        # Distinct visible ink on every page. Pages 1 and 2 additionally draw
        # the shared form, so it is referenced by two pages but not by page 0
        # and therefore becomes a genuine part-8 shared object.
        shade = 0.1 + i * 0.25
        content = f"{shade:.2f} 0.1 0.2 rg\n{20 + i * 15} 120 90 90 re f\n".encode()
        if i > 0:
            content += b"q 100 100 cm /Shared Do Q\n"
            page[NameObject("/Resources")] = DictionaryObject(
                {NameObject("/XObject"): xobjects}
            )
        stream.set_data(content)
        page[NameObject("/Contents")] = w._add_object(stream)

    with open(path, "wb") as fh:
        w.write(fh)
    print(f"wrote {path}")


def main() -> int:
    out = (
        Path(sys.argv[1])
        if len(sys.argv) > 1
        else Path("corpus/fixtures/lin_conformance_multipage.pdf")
    )
    make_multipage(out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
