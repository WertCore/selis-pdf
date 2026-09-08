// pdf.js render driver for the pinned pdf.js oracle container (SL-0.ORACLE.01).
//
// CLI contract (must match `xtask oracle render`, which invokes the container
// with: node driver.mjs --page <n> --dpi <n> <in.pdf> <out.png>).
//
// Renders page `--page` (1-based, default 1) at `--dpi` into a PNG using the
// pinned pdfjs-dist legacy build and @napi-rs/canvas (both integrity-checked
// via package-lock.json). Headless; no network access. This driver is an
// oracle process — it is never bundled with or invoked by Selis (ADR-P0009).
import { createRequire } from 'node:module';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { createCanvas } = require('@napi-rs/canvas');
// pdfjs-dist 6.x is ESM-only; the legacy build targets Node.
const pdfjs = await import('pdfjs-dist/legacy/build/pdf.mjs');

function arg(name, fallback) {
  const i = process.argv.indexOf(name);
  return i !== -1 && i + 1 < process.argv.length ? process.argv[i + 1] : fallback;
}

const page = parseInt(arg('--page', '1'), 10);
const dpi = parseInt(arg('--dpi', '150'), 10);
const positional = [];
for (let i = 2; i < process.argv.length; i++) {
  if (process.argv[i] === '--page' || process.argv[i] === '--dpi') {
    i++; // skip the flag's value
    continue;
  }
  positional.push(process.argv[i]);
}
if (positional.length < 2 || !Number.isFinite(page) || page < 1 || !Number.isFinite(dpi) || dpi <= 0) {
  console.error('usage: node driver.mjs --page <n> --dpi <n> <in.pdf> <out.png>');
  process.exit(2);
}
const [inPdf, outPng] = positional;

// Standard-14 font data ships inside the pdfjs-dist package; pdf.js requires
// it as a directory URL with a trailing slash.
const fontDir = path.join(
  path.dirname(require.resolve('pdfjs-dist/package.json')),
  'standard_fonts',
);
const standardFontDataUrl = existsSync(fontDir)
  ? new URL(`file://${fontDir.replace(/\\/g, '/')}/`).href
  : undefined;

const loading = pdfjs.getDocument({
  data: new Uint8Array(readFileSync(inPdf)),
  standardFontDataUrl,
  isEvalSupported: false,
  verbosity: 0,
});
try {
  const doc = await loading.promise;
  const pdfPage = await doc.getPage(page);
  const viewport = pdfPage.getViewport({ scale: dpi / 72 });
  const canvas = createCanvas(Math.ceil(viewport.width), Math.ceil(viewport.height));
  const ctx = canvas.getContext('2d');
  ctx.fillStyle = 'white';
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  await pdfPage.render({ canvasContext: ctx, viewport, background: 'white' }).promise;
  writeFileSync(outPng, canvas.toBuffer('image/png'));
  console.log(`pdf.js driver: rendered page ${page} of ${inPdf} at ${dpi} dpi -> ${outPng}`);
} finally {
  await loading.destroy();
}
