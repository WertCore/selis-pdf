/**
 * Build the browser-check fixture for the extension leg of SL-4.UI.06.
 *
 * The extension's browser check drives the REAL packed viewer in a real engine
 * (`--load-extension`), opening a document through the page's `?src=` route.
 * Its two claims — "the panels render" and "a bookmark moves the reader" —
 * need a document that can answer both:
 *
 *   - **Two pages whose rasters differ.** The web leg's fixture (`TWO_PAGE_PDF`)
 *     paints the same rectangles on both pages, so a press that changed nothing
 *     would leave the pixels identical and the check green. Here page 1 is red
 *     over blue at the left margin and page 2 is green over red at the right,
 *     so "the pixels changed" cannot be satisfied by repainting the same page.
 *   - **Two outline entries, each a direct `/Dest`.** The mount presses a row
 *     and measures the ink; a bookmark pointing at the current page is
 *     indistinguishable from a dead one (the lesson of `make-tagged-fixture`).
 *     Direct destinations rather than a named tree: the named-destination
 *     resolution already has its own coverage on the web leg, and this fixture
 *     exists to prove the ROW works, not the resolver.
 *
 * Page size is 200x200pt for the same cost reason the web leg records: at the
 * viewer's render DPI a Letter page is tens of megabytes of raster, and a
 * check that takes four minutes does not run on a pull request.
 *
 * Written by hand rather than committed as an opaque blob, exactly as
 * `make-tagged-fixture.mjs` argues: a committed PDF is unreadable in a diff and
 * its xref offsets are unverifiable by eye. This emits the offsets it needs, so
 * the file it writes is correct by construction.
 *
 * Run: node tools/make-ui06-extension-fixture.mjs
 */
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const outPath = join(repoRoot, "xtask", "browser", "extension", "doc.pdf");

/** Objects, in order. Index 0 is the free head. */
const objects = [
	null,
	// 1: the catalog, carrying the outline root.
	"<< /Type /Catalog /Pages 2 0 R /Outlines 7 0 R >>",
	// 2: the page tree.
	"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
	// 3: page one — red over blue, both at the left margin.
	"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 5 0 R >>",
	// 4: page two — green over red, both at the right, so the two rasters
	// cannot be mistaken for one another.
	"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 6 0 R >>",
	// 5/6: the content streams. Each carries its own /Length, computed below
	// rather than counted by hand - the difference between the two pages is the
	// point of the fixture, and a hand-counted length is how a page silently
	// renders blank.
	"0.85 0.2 0.1 rg 20 120 80 40 re f\n0.1 0.3 0.8 rg 20 20 80 40 re f\n",
	"0.1 0.7 0.2 rg 100 120 80 40 re f\n0.85 0.2 0.1 rg 100 20 80 40 re f\n",
	// 7: the outline root, two entries counted.
	"<< /Type /Outlines /First 8 0 R /Last 9 0 R /Count 2 >>",
	// 8: entry one — page one.
	"<< /Title (First layer) /Parent 7 0 R /Dest [3 0 R /Fit] /Next 9 0 R >>",
	// 9: entry two — page two.
	"<< /Title (Second layer) /Parent 7 0 R /Dest [4 0 R /Fit] >>",
];

const parts = [];
let offset = 0;
const push = (text) => {
	const bytes = Buffer.isBuffer(text) ? text : Buffer.from(text, "latin1");
	parts.push(bytes);
	offset += bytes.length;
};
const offsets = [];

// The binary comment above the first object marks the file as binary, so
// tools that sniff text-vs-binary do not mangle it.
push("%PDF-1.7\n");
push(Buffer.from([0x25, 0xe2, 0xe3, 0xcf, 0xd3, 0x0a]));
for (let index = 1; index < objects.length; index += 1) {
	offsets[index] = offset;
	const body = objects[index];
	const length = index === 5 || index === 6 ? Buffer.byteLength(body, "latin1") : null;
	push(`${index} 0 obj\n`);
	if (length !== null) {
		push(`<< /Length ${length} >>\nstream\n`);
		push(body);
		push("endstream\n");
	} else {
		push(`${body}\n`);
	}
	push("endobj\n");
}

const xrefStart = offset;
push(`xref\n0 ${objects.length}\n`);
push("0000000000 65535 f \n");
for (let index = 1; index < objects.length; index += 1) {
	push(`${String(offsets[index]).padStart(10, "0")} 00000 n \n`);
}
push(`trailer\n<< /Size ${objects.length} /Root 1 0 R >>\nstartxref\n${xrefStart}\n%%EOF\n`);

mkdirSync(dirname(outPath), { recursive: true });
writeFileSync(outPath, Buffer.concat(parts));
console.log(`wrote ${outPath} (${offset} bytes)`);
