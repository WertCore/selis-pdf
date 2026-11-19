#!/usr/bin/env python3
"""Regenerate manifest.toml and two_paragraphs.pdf for the TEXTEDIT.01 harness.

This is a *synthetic* labelled set: pages are described at the assembled-line
level (baseline y, em size, font, MCID and text) with known paragraph ends. It
is deliberately small; the DoD's 500-page hand-labelled set is not available.
Run `python generate.py` from this directory to reproduce both files.
"""
from pathlib import Path

HERE = Path(__file__).resolve().parent

# Each page: (id, tagged, [(paragraph line-counts, within-gap factor, font, size) ...]).
# A paragraph break uses a between-gap factor well above the 1.6 em grouping bar.
PAGES = [
    ("p01", True,  [(2, 1.4, "F1", 10.0), (2, 1.4, "F1", 10.0)]),
    ("p02", False, [(3, 1.3, "F1", 11.0), (2, 1.3, "F1", 11.0)]),
    ("p03", True,  [(2, 1.5, "F1", 9.0),  (3, 1.5, "F1", 9.0),  (2, 1.5, "F1", 9.0)]),
    ("p04", False, [(2, 1.4, "F2", 12.0), (2, 1.4, "F2", 12.0)]),
    ("p05", True,  [(3, 1.2, "F1", 10.0), (3, 1.2, "F1", 10.0)]),
    ("p06", False, [(2, 1.4, "F1", 10.0), (2, 1.4, "F2", 10.0), (2, 1.4, "F2", 10.0)]),
    ("p07", True,  [(2, 1.5, "F1", 14.0), (2, 1.5, "F1", 14.0), (2, 1.5, "F1", 14.0)]),
    ("p08", False, [(4, 1.3, "F1", 10.0), (2, 1.3, "F1", 10.0)]),
    ("p09", True,  [(2, 1.4, "F1", 11.0), (3, 1.4, "F1", 11.0)]),
    ("p10", False, [(2, 1.4, "F3", 9.0),  (2, 1.4, "F3", 9.0)]),
    ("p11", True,  [(3, 1.5, "F1", 10.0), (2, 1.5, "F1", 10.0), (2, 1.5, "F1", 10.0)]),
    ("p12", False, [(2, 1.3, "F1", 12.0), (2, 1.3, "F1", 12.0), (2, 1.3, "F1", 12.0)]),
]

BETWEEN_FACTOR = 3.4
WORDS = [
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
    "india", "juliet", "kilo", "lima", "mike", "november", "oscar", "papa",
]


def build_pages():
    manifest = ['version = 1',
                'note = "Synthetic assembled-line pages with known paragraph ends (TEXTEDIT.01). Not the DoD 500-page set."',
                '']
    for pid, tagged, paras in PAGES:
        lines = []
        ends = []
        y = 800.0
        mcid = 1
        word = 0
        for count, within, font, size in paras:
            for _ in range(count):
                text = f"{WORDS[word % len(WORDS)]} {WORDS[(word + 1) % len(WORDS)]}"
                word += 1
                lines.append((y, size, font, text, mcid if tagged else -1))
                y -= size * within
                mcid += 1
            ends.append(len(lines))
            y -= size * BETWEEN_FACTOR
        manifest.append(f'[[page]]')
        manifest.append(f'id = "{pid}"')
        manifest.append(f'tagged = {str(tagged).lower()}')
        manifest.append(f'paragraph_ends = [{", ".join(str(e) for e in ends)}]')
        manifest.append('lines = [')
        for (ly, size, font, text, mc) in lines:
            manifest.append(
                f'  {{ y = {ly:.2f}, size = {size:.1f}, font = "{font}", '
                f'text = "{text}", mcid = {mc} }},')
        manifest.append(']')
        manifest.append('')
    return "\n".join(manifest) + "\n"


def build_pdf():
    # A real, hand-built PDF with four text lines in two paragraphs: two tight
    # 14 pt baselines, a wide blank gap, two more tight baselines. /Helvetica is
    # undeclared on purpose (the interpreter recovers standard-14 ASCII).
    streams = [
        (800.0, "First paragraph line one"),
        (786.0, "First paragraph line two"),
        (700.0, "Second paragraph line one"),
        (686.0, "Second paragraph line two"),
    ]
    content = "0 0 0 rg\n"
    for y, text in streams:
        content += f"BT /Helvetica 12 Tf 72 {y} Td ({text}) Tj ET\n"
    content = content.encode("latin-1")

    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
        b"/Contents 4 0 R /Resources << >> >>",
        b"<< /Length " + str(len(content)).encode() + b" >>\nstream\n" + content + b"endstream",
    ]
    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = [0]
    for i, obj in enumerate(objects, 1):
        offsets.append(len(out))
        out += f"{i} 0 obj\n".encode() + obj + b"\nendobj\n"
    xref = len(out)
    out += f"xref\n0 {len(objects) + 1}\n".encode()
    out += b"0000000000 65535 f \n"
    for off in offsets[1:]:
        out += f"{off:010d} 00000 n \n".encode()
    out += (f"trailer\n<< /Root 1 0 R /Size {len(objects) + 1} >>\n"
            f"startxref\n{xref}\n%%EOF\n").encode()
    return bytes(out)


(HERE / "manifest.toml").write_text(build_pages(), encoding="utf-8")
(HERE / "two_paragraphs.pdf").write_bytes(build_pdf())
print("wrote manifest.toml and two_paragraphs.pdf")
