/**
 * SL-4.WEB.02 - put the compiled service worker where the browser can register it.
 *
 * The deployment root is `public/` (that is what `public/_headers` and
 * `vercel.json` describe), and a service worker has to be served from the site
 * root to control it. So `tsc` writes `dist/sw.js` and this script copies the
 * worker and the modules it imports into `public/`.
 *
 * It is deliberately a copy of a *declared* set, like the extension's
 * `tools/pack.mjs`: a file that is not reachable from `dist/sw.js` cannot appear
 * in `public/`, so the shipped worker cannot quietly grow an import that the
 * review did not see. The import walk is the declaration - if `sw.ts` grows an
 * import, it is copied, and the SRI/asset review sees it.
 *
 * Run through `pnpm --filter @selis/web-host build` (after `tsc`).
 */

import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const pkgRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const distRoot = join(pkgRoot, "dist");
const publicRoot = join(pkgRoot, "public");

/**
 * The compiled entries copied to the deployment root, and the modules they
 * import (followed from here).
 *
 * `sw.js` is the worker itself, served from the site root so its scope can be
 * `/`. `sw-register.js` is here because `public/assets/boot.js` imports it to
 * register the worker: the page and the worker are one feature, and splitting
 * them across two builds would let the page ship without the worker.
 */
const ENTRIES = ["sw.js", "sw-register.js"];

/** `import ... from "./x.js"` specifiers, relative and same-directory only. */
function localImports(source) {
	const found = [];
	for (const match of source.matchAll(/(?:from|import)\s*\(?\s*["']([^"']+)["']/g)) {
		const specifier = match[1];
		if (specifier.startsWith("./")) {
			found.push(specifier.replace(/^\.\//, ""));
		} else if (!specifier.startsWith(".")) {
			console.error(
				`place-sw: a worker module imports "${specifier}". The worker must be bundled-free and same-origin (ADR-P0016, ADR-P0028); no bare or remote specifier may reach public/.`,
			);
			process.exit(1);
		}
	}
	return found;
}

for (const entry of ENTRIES) {
	if (!existsSync(join(distRoot, entry))) {
		console.error(`place-sw: no compiled ${entry} in dist/ - run \`tsc -p tsconfig.json\` first.`);
		process.exit(1);
	}
}

const copied = [];
const queue = [...ENTRIES];
const seen = new Set();
while (queue.length > 0) {
	const name = queue.pop();
	if (seen.has(name)) continue;
	seen.add(name);
	const from = join(distRoot, name);
	if (!existsSync(from)) {
		console.error(`place-sw: a worker module imports ${name}, which tsc did not emit.`);
		process.exit(1);
	}
	const to = join(publicRoot, name);
	mkdirSync(dirname(to), { recursive: true });
	// Copied byte for byte: a rewritten worker is a worker nobody reviewed.
	copyFileSync(from, to);
	copied.push(name);
	for (const dependency of localImports(readFileSync(from, "utf8"))) {
		queue.push(dependency);
	}
}

// A tiny marker so a deployed root can be checked for the worker's presence
// without guessing whether the build ran.
writeFileSync(
	join(publicRoot, "sw-manifest.json"),
	`${JSON.stringify({ worker: "/sw.js", entry: "/assets/boot.js", files: copied.sort() }, null, 2)}\n`,
	"utf8",
);

console.log(`place-sw: copied ${copied.sort().join(", ")} into public/`);
