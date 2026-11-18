"""Build a small fixture with /PageLabels and a page /Thumb for hint-table tests."""
from pypdf import PdfWriter
from pypdf.generic import (
    ArrayObject, DecodedStreamObject, DictionaryObject, NameObject,
    NumberObject, TextStringObject,
)

w = PdfWriter()
w.add_blank_page(200, 200)
w.add_blank_page(200, 200)
# Give each page real ink (a filled rectangle, no font needed) so the
# independent PDFium render check is meaningful.
for i, page in enumerate(w.pages):
    stream = DecodedStreamObject()
    stream.set_data(
        f"0 0 1 rg\n{20 + i * 10} 20 100 100 re f\n".encode()
    )
    page[NameObject("/Contents")] = w._add_object(stream)

# /PageLabels number tree: page 0 onward uses style /D, prefix "A-", start 1.
label = DictionaryObject()
label[NameObject("/S")] = NameObject("/D")
label[NameObject("/P")] = TextStringObject("A-")
label[NameObject("/St")] = NumberObject(1)
tree = DictionaryObject()
tree[NameObject("/Nums")] = ArrayObject([NumberObject(0), label])
w._root_object[NameObject("/PageLabels")] = w._add_object(tree)

# A tiny 1x1 grayscale thumbnail image stream on page 1.
thumb = DecodedStreamObject()
thumb.set_data(b"\x00")
thumb.update({
    NameObject("/Type"): NameObject("/XObject"),
    NameObject("/Subtype"): NameObject("/Image"),
    NameObject("/Width"): NumberObject(1),
    NameObject("/Height"): NumberObject(1),
    NameObject("/ColorSpace"): NameObject("/DeviceGray"),
    NameObject("/BitsPerComponent"): NumberObject(8),
})
w.pages[0][NameObject("/Thumb")] = w._add_object(thumb)

with open("corpus/fixtures/lin_optional_hints.pdf", "wb") as f:
    w.write(f)
print("wrote corpus/fixtures/lin_optional_hints.pdf")
