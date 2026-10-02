/**
 * Build a TAGGED PDF fixture for the accessibility walk (SL-4.UI.07).
 *
 * Every PDF in `corpus/` is untagged, including the one named
 * `structure_simple.pdf` — its catalog carries no `/StructTreeRoot` at all. That
 * left the DoD's "a screen-reader script walks a tagged document correctly" with
 * nothing to walk, and the check passing on an untagged document is not the same
 * claim: it verifies the viewer says "there is none", not that it can say what
 * there IS.
 *
 * Written by hand rather than committed as bytes, because a committed blob is
 * unreadable in a diff and its xref offsets are unverifiable. This emits the
 * offsets it needs, so the file it writes is correct by construction.
 *
 * The structure is chosen to exercise every rule `a11y.ts` and the browser gate
 * make a claim about:
 *   - an `H1` and an `H2`, so heading LEVELS come from the role;
 *   - a `Figure` WITH `/Alt` and one WITHOUT, so `needsDescription` is real;
 *   - a `Table` with `TR`/`TH`/`TD`, so cell semantics are real;
 *   - an `Artifact` containing a heading, so the skip-the-subtree rule has
 *     something to skip;
 *   - a custom role `Chart` mapped through `/RoleMap` to `Figure`, so the
 *     document's own mapping is the thing under test.
 *
 * Run: node tools/make-tagged-fixture.mjs
 */
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const outPath = join(repoRoot, "corpus", "fixtures", "tagged_structure.pdf");

/** Objects, in order. Index 0 is the free head. */
const objects = [
	null,
	"<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 6 0 R /MarkInfo << /Marked true >> >>",
	"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
	// The page's marked content carries the MCIDs the structure references, so
	// the tree and the content agree — a tree pointing at content that does not
	// exist is a fixture that passes the parser and fails the reader.
	"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
	null, // the content stream, filled in below (it needs its own length)
	"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
	// StructTreeRoot. `/RoleMap` is the document deciding how its own roles map,
	// which is exactly the precedence `semanticsOf` implements.
	"<< /Type /StructTreeRoot /K [7 0 R 8 0 R 9 0 R 10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 15 0 R 16 0 R] " +
		"/RoleMap << /Chart /Figure >> /ParentTree 6 0 R >>",
	// 7: Document
	"<< /Type /StructElem /S /Document /P 6 0 R /K [7 1 R] >>",
	// 8: H1 "Introduction", with its own kid paragraphs.
	"<< /Type /StructElem /S /H1 /P 6 0 R /T (Introduction) /K [9 0 R 10 0 R] /Pg 3 0 R >>",
	// 9: P
	"<< /Type /StructElem /S /P /P 6 0 R /T (First paragraph of the document.) /Pg 3 0 R >>",
	// 10: P
	"<< /Type /StructElem /S /P /P 6 0 R /T (Second paragraph of the document.) /Pg 3 0 R >>",
	// 11: H2 nested under the H1 — nesting depth must NOT change the level.
	"<< /Type /StructElem /S /H2 /P 6 0 R /T (A subsection) /K [12 0 R] /Pg 3 0 R >>",
	// 12: Figure WITH an /Alt.
	"<< /Type /StructElem /S /Figure /P 6 0 R /T (Revenue by quarter) /Alt (A bar chart of revenue by quarter) /Pg 3 0 R >>",
	// 13: Figure WITHOUT an /Alt - the case that must be flagged, not described.
	"<< /Type /StructElem /S /Figure /P 6 0 R /T (An undescribed chart) /Pg 3 0 R >>",
	// 14: Table with a header cell and a data cell.
	"<< /Type /StructElem /S /Table /P 6 0 R /T (Quarterly figures) /K [17 0 R 18 0 R] /Pg 3 0 R >>",
	// 15: Artifact containing a heading - decoration the author asked to skip.
	"<< /Type /StructElem /S /Artifact /P 6 0 R /K [16 0 R] /Pg 3 0 R >>",
	// 16: a heading INSIDE the artifact; a viewer that ignores the skip rule
	// puts "Hidden heading" in the outline.
	"<< /Type /StructElem /S /H1 /P 6 0 R /T (Hidden heading) /Pg 3 0 R >>",
	// 17: TR / TH
	"<< /Type /StructElem /S /TR /P 6 0 R /K [19 0 R] /Pg 3 0 R >>",
	// 18: TR / TD
	"<< /Type /StructElem /S /TR /P 6 0 R /K [20 0 R] /Pg 3 0 R >>",
	"<< /Type /StructElem /S /TH /P 6 0 R /T (Quarter) /Pg 3 0 R >>",
	"<< /Type /StructElem /S /TD /P 6 0 R /T (Q1) /Pg 3 0 R >>",
];

const content = `BT /F1 24 Tf 72 720 Td (Selis) Tj ET`;
objects[4] = `<< /Length ${content.length} >>\nstream\n${content}\nendstream`;

let pdf = "%PDF-1.7\n";
const offsets = [];
for (let i = 1; i < objects.length; i++) {
	offsets[i] = pdf.length;
	pdf += `${i} 0 obj\n${objects[i]}\nendobj\n`;
}
const startXref = pdf.length;
pdf += `xref\n0 ${objects.length}\n0000000000 65535 f \n`;
for (let i = 1; i < objects.length; i++) {
	pdf += `${String(offsets[i]).padStart(10, "0")} 00000 n \n`;
}
pdf += `trailer\n<< /Size ${objects.length} /Root 1 0 R >>\nstartxref\n${startXref}\n%%EOF\n`;

mkdirSync(dirname(outPath), { recursive: true });
writeFileSync(outPath, Buffer.from(pdf, "latin1"));
console.log(`make-tagged-fixture: wrote ${outPath} (${pdf.length} bytes, ${objects.length - 1} objects)`);