// pdf.js render driver for the pinned pdf.js oracle container (SL-0.ORACLE.01).
//
// CLI contract (must match `xtask oracle render`, which invokes the container
// with: node driver.mjs --page <n> --dpi <n> <in.pdf> <out.png>; the
// SL-3.CONF.01 text leg invokes: node driver.mjs --page <n> --text <out.txt>
// <in.pdf>).
//
// Renders page `--page` (1-based, default 1) at `--dpi` into a PNG using the
// pinned pdfjs-dist legacy build and @napi-rs/canvas (both integrity-checked
// via package-lock.json). Headless; no network access. This driver is an
// oracle process — it is never bundled with or invoked by Selis (ADR-P0009).
import { createRequire } from 'node:module';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';

const require = createRequire(import.meta.url);
// The native canvas module is loaded lazily: the `--text` leg never renders,
// so text extraction works without the platform canvas binary (and stays
// testable wherever only pdfjs-dist is installed).
function loadCanvas() {
  return require('@napi-rs/canvas');
}
// pdfjs-dist 6.x is ESM-only; the legacy build targets Node.
const pdfjs = await import('pdfjs-dist/legacy/build/pdf.mjs');

function arg(name, fallback) {
  const i = process.argv.indexOf(name);
  return i !== -1 && i + 1 < process.argv.length ? process.argv[i + 1] : fallback;
}

const page = parseInt(arg('--page', '1'), 10);
const dpi = parseInt(arg('--dpi', '150'), 10);
const positional = [];
// Text-extraction mode (SL-3.CONF.01): `--text <out.txt>` writes the page's
// Unicode text as UTF-8 instead of rendering. Added alongside the render
// path; the render branch below is untouched.
let textPath = null;
for (let i = 2; i < process.argv.length; i++) {
  if (process.argv[i] === '--page' || process.argv[i] === '--dpi' || process.argv[i] === '--text') {
    if (process.argv[i] === '--text' && i + 1 < process.argv.length) {
      textPath = process.argv[i + 1];
    }
    i++; // skip the flag's value
    continue;
  }
  positional.push(process.argv[i]);
}
if ((!textPath && positional.length < 2) || (textPath && positional.length < 1) || !Number.isFinite(page) || page < 1 || !Number.isFinite(dpi) || dpi <= 0) {
  console.error('usage: node driver.mjs --page <n> --dpi <n> <in.pdf> <out.png>');
  console.error('       node driver.mjs --page <n> --text <out.txt> <in.pdf>');
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
  if (textPath) {
    const tc = await pdfPage.getTextContent();
    const parts = [];
    for (const item of tc.items) {
      parts.push(item.str);
      if (item.hasEOL) parts.push('\n');
    }
    writeFileSync(textPath, parts.join(''), 'utf8');
    // Banner on stderr, never stdout: the text leg's stdout must stay
    // empty so the harness cannot interleave it into the written file.
    console.error(`pdf.js driver: extracted page ${page} of ${inPdf} -> ${textPath}`);
  } else {
    const { createCanvas } = loadCanvas();
    const viewport = pdfPage.getViewport({ scale: dpi / 72 });
    const canvas = createCanvas(Math.ceil(viewport.width), Math.ceil(viewport.height));
    const ctx = canvas.getContext('2d');
    ctx.fillStyle = 'white';
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    await pdfPage.render({ canvasContext: ctx, viewport, background: 'white' }).promise;
    writeFileSync(outPng, canvas.toBuffer('image/png'));
    console.log(`pdf.js driver: rendered page ${page} of ${inPdf} at ${dpi} dpi -> ${outPng}`);
  }
} finally {
  await loading.destroy();
}
