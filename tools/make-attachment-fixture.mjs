/**
 * Build the embedded-files fixture used by the tools-page browser check.
 *
 * The fixture carries two attachments so the page must expose per-file
 * downloads and a ZIP bundle:
 *
 *   - `notes.txt`, whose bytes are readable text;
 *   - `../../evil.txt`, whose name proves the engine flattens traversal
 *     markers before it becomes a download name.
 *
 * Written by hand rather than committed as an opaque blob: the xref offsets
 * are emitted here, so the file is correct by construction. Run with
 * `node tools/make-attachment-fixture.mjs`.
 */

import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const target = fileURLToPath(
  new URL("../xtask/browser/tools-page/attachments.pdf", import.meta.url),
);

const objects = [
  "1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Names 4 0 R >>\nendobj\n",
  "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
  "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>\nendobj\n",
  "4 0 obj\n<< /EmbeddedFiles 5 0 R >>\nendobj\n",
  "5 0 obj\n<< /Names [(../../evil.txt) 6 0 R (notes.txt) 7 0 R] >>\nendobj\n",
  "6 0 obj\n<< /Type /Filespec /UF (../../evil.txt) /F (../../evil.txt) /EF << /F 8 0 R >> >>\nendobj\n",
  "7 0 obj\n<< /Type /Filespec /UF (notes.txt) /F (notes.txt) /EF << /F 9 0 R >> >>\nendobj\n",
  streamObject(8, Buffer.from("safe name")),
  streamObject(9, Buffer.from("hello attachment")),
];

function streamObject(number, data) {
  return Buffer.concat([
    Buffer.from(
      `${number} 0 obj\n<< /Type /EmbeddedFile /Length ${data.length} >>\nstream\n`,
    ),
    data,
    Buffer.from("\nendstream\nendobj\n"),
  ]);
}

const chunks = [Buffer.from("%PDF-1.4\n")];
const offsets = [];
let length = chunks[0].length;
for (const object of objects) {
  const bytes = Buffer.isBuffer(object) ? object : Buffer.from(object);
  offsets.push(length);
  chunks.push(bytes);
  length += bytes.length;
}

const xref = length;
chunks.push(Buffer.from(`xref\n0 ${offsets.length + 1}\n`));
chunks.push(Buffer.from("0000000000 65535 f \n"));
for (const offset of offsets) {
  chunks.push(Buffer.from(`${String(offset).padStart(10, "0")} 00000 n \n`));
}
chunks.push(
  Buffer.from(
    `trailer\n<< /Size ${offsets.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`,
  ),
);

writeFileSync(target, Buffer.concat(chunks));
console.log(`wrote ${target}`);
