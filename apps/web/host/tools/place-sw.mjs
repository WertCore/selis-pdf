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

import { createHash } from "node:crypto";
import {
	copyFileSync,
	existsSync,
	mkdirSync,
	readFileSync,
	readdirSync,
	statSync,
	writeFileSync,
} from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
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

/**
 * The build's identity: a hash of the bytes this build serves.
 *
 * ## Why the cache namespace has to follow the build
 *
 * The runtime cache is CACHE-FIRST: a module that has been fetched once is
 * served from the cache, and the network is never consulted again. That is
 * what makes the offline story work, and it is also how a reader can be
 * left running code from three deploys ago: the browser only re-installs the
 * worker when the WORKER SCRIPT's bytes change, and the worker's bytes do
 * not change when a module does.
 *
 * So the namespace is stamped with the build's identity. The worker's bytes
 * change with it, the browser installs the new worker, and `activate` drops
 * the old caches - and the reader's next load has the new modules.
 *
 * It has to be stamped into BOTH files: the version goes into the policy
 * module (which decides the cache NAMES), and a marker goes into the worker
 * script itself, because that is the file the browser compares.
 */
function buildId() {
	const hash = createHash("sha256");
	hash.update("selis-build-v1");
	const roots = [distRoot, join(pkgRoot, "assets-src")];
	const wasmDir = join(resolve(pkgRoot, "../../.."), "target", "wasm32-unknown-unknown", "release");
	for (const root of roots) {
		hashTree(hash, root, (name) => name.endsWith(".js") || name.endsWith(".css"));
	}
	// The WASM chunks: a Rust change is a change to what the page serves.
	if (existsSync(wasmDir)) {
		hashTree(hash, wasmDir, (name) => name.endsWith(".wasm"));
	}
	return hash.digest("hex").slice(0, 16);
}

/** Hash every file under a directory that the filter accepts, in a stable order. */
function hashTree(hash, root, accept) {
	if (!existsSync(root)) {
		return;
	}
	const walk = (directory) => {
		const entries = readdirSync(directory, { withFileTypes: true }).sort((a, b) =>
			a.name < b.name ? -1 : a.name > b.name ? 1 : 0,
		);
		for (const entry of entries) {
			const full = join(directory, entry.name);
			if (entry.isDirectory()) {
				walk(full);
				continue;
			}
			if (!accept(entry.name)) {
				continue;
			}
			hash.update(relative(root, full));
			hash.update(readFileSync(full));
		}
	};
	walk(root);
}

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

// The cache namespace follows the build, and the worker's OWN bytes change
// with it: the browser compares the worker SCRIPT's bytes to decide whether
// there is an update, so a marker has to live in the script itself.
const id = buildId();
const policyPath = join(publicRoot, "sw-policy.js");
if (existsSync(policyPath)) {
	const policy = readFileSync(policyPath, "utf8");
	const stamped = policy.replace(/(export const CACHE_VERSION = ")[^"]*(";)/, `$1${id}$2`);
	if (stamped === policy) {
		console.error(
			"place-sw: the compiled sw-policy.js has no CACHE_VERSION to stamp - " +
				"the worker's cache namespace would not follow the build.",
		);
		process.exit(1);
	}
	writeFileSync(policyPath, stamped, "utf8");
}
const workerPath = join(publicRoot, "sw.js");
if (existsSync(workerPath)) {
	writeFileSync(workerPath, `${readFileSync(workerPath, "utf8")}\n// selis build: ${id}\n`, "utf8");
}

// A tiny marker so a deployed root can be checked for the worker's presence
// without guessing whether the build ran.
writeFileSync(
	join(publicRoot, "sw-manifest.json"),
	`${JSON.stringify({ worker: "/sw.js", entry: "/assets/boot.js", files: copied.sort() }, null, 2)}\n`,
	"utf8",
);

console.log(`place-sw: copied ${copied.sort().join(", ")} into public/ (build ${id})`);
