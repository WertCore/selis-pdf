/* Bundle the SHIPPED `apps/ui/dist/viewer/*.js` into one classic script.
 *
 * This exists so the benchmark runs the real modules -- the same files the
 * viewer loads -- rather than a re-implementation. The modules are ESM with
 * relative `.js` specifiers; this is a minimal inlining pass, not a bundler:
 * it walks the import graph from the entry points, strips import/export
 * statements, and concatenates. No transform, no minification, no shim.
 *
 * The output is an IIFE that publishes the handful of entry-point symbols on
 * `window.SELIS`, which is what `harness.js` drives.
 *
 * Run:  node xtask/bench/bundle.mjs <apps/ui/dist/viewer> <out.js>
 */
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

const distDir = resolve(process.argv[2] ?? "apps/ui/dist/viewer");
const outFile = resolve(process.argv[3] ?? join(distDir, "../../bench-app.js"));

/** Entry points: the symbols the benchmark needs, per module. */
const ENTRIES = {
  "page-list.js": ["createPageList"],
  "compositor.js": ["createTileCompositor"],
  "tile-ladder.js": [],
  "layout.js": [],
  "windowing.js": [],
  "anchoring.js": [],
  "strings.js": [],
  "keyboard.js": [],
  "surface.js": [],
  "platform/mock-adapter.js": [],
};

const seen = new Set();
const chunks = [];

/** Strip ESM syntax, collecting imported names. */
function processFile(file, dir) {
  if (seen.has(file)) return;
  seen.add(file);
  let src;
  try {
    src = readFileSync(file, "utf8");
  } catch {
    return; // optional module (e.g. the platform mock) may not be built
  }

  const imported = [];
  // `import { a, b } from "./x.js";` (possibly multi-line, possibly `type`).
  src = src.replace(
    /import\s+(type\s+)?\{([^}]*)\}\s+from\s+["'][^"']+["'];?/gs,
    (_m, _type, names) => {
      for (const raw of names.split(",")) {
        const n = raw.trim().replace(/^type\s+/, "").split(/\s+as\s+/).pop();
        if (n) imported.push(n);
      }
      return "";
    },
  );
  // Bare and default imports are unused by these modules; drop them.
  src = src.replace(/import\s+[^;]*?from\s+["'][^"']+["'];?/gs, "");

  // `export const/function/class/interface/type X` -> `const ... X`
  src = src.replace(/^export\s+(const|let|var|function|class|async function)\s/gm, "$1 ");
  // `export interface {...}` / `export type {...}` -> drop (types only).
  src = src.replace(/^export\s+(interface|type)\s+[A-Za-z0-9_]+[^{=]*\{[^}]*\}\s*;?/gms, "");
  src = src.replace(/^export\s+(interface|type)\s+[A-Za-z0-9_]+\s*;?\s*$/gm, "");
  // `export { a, b };` -> record the names, drop the statement.
  src = src.replace(/^export\s*\{([^}]*)\};?/gms, (_m, names) => {
    for (const raw of names.split(",")) {
      const n = raw.trim().split(/\s+as\s+/)[0].replace(/^type\s+/, "");
      if (n) imported.push(n);
    }
    return "";
  });
  src = src.replace(/^export\s+default\s+/gm, "const __default = ");

  chunks.push(`\n/* ---- ${file.slice(dir.length + 1)} ---- */\n${src}`);

  // Recurse into the specifiers this file imported.
  for (const m of readFileSync(file, "utf8").matchAll(/from\s+["'](\.[^"']+)["']/g)) {
    processFile(resolve(dirname(file), m[1]), dir);
  }
}

for (const [rel, symbols] of Object.entries(ENTRIES)) {
  processFile(join(distDir, rel), distDir);
  if (symbols.length) {
    chunks.push(`SELIS_EXPORTS.${symbols[0]} = ${symbols[0]};`);
  }
}

const body = chunks.join("\n");
const prelude = `var SELIS_EXPORTS = {};\n`;
const postlude = `\nwindow.SELIS = SELIS_EXPORTS;\n`;

writeFileSync(outFile, prelude + body + postlude, "utf8");
console.error(
  `bundled ${seen.size} module(s) -> ${outFile}`,
);
for (const f of seen) console.error(`  ${f.slice(distDir.length + 1)}`);
