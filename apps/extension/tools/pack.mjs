/**
 * SL-4.EXT.04 - assemble the shippable package in `dist/`.
 *
 * The shipped extension is `dist/`: the MV3 files Chrome loads (manifest,
 * pages, the service worker) sit at its root and the compiled modules under
 * `src/`, which is exactly the layout `service-worker.js` and `viewer.html`
 * already import from. This script copies those files out of the build, and
 * nothing else: a file that is not declared in `PACKAGE_ENTRIES` cannot be in
 * the package, so tests, source maps and the gate's own modules stay out of the
 * store upload by construction rather than by remembering to exclude them.
 *
 * Run through `pnpm --filter @selis/extension build` (after `tsc`).
 */

import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const pkgRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const distRoot = join(pkgRoot, "dist");
const buildRoot = join(distRoot, "pkg");

if (!existsSync(buildRoot)) {
	console.error(
		`pack: no compiled output at ${buildRoot} - run \`tsc -p tsconfig.json\` first (pnpm run build does both)`,
	);
	process.exit(1);
}

const { BUILD_WORKSPACE, PACKAGE_ENTRIES, buildSourceOf } = await import(
	pathToFileURL(join(buildRoot, "extension", "src", "bundle-scan.js")).href
);

/**
 * Where one declared entry's bytes are read from, or `null` if the row is
 * malformed. A `null` here is a build failure rather than a skip: a ship list
 * row that cannot be located is a file the store upload would silently miss.
 */
function sourcePathOf(entry) {
	if (entry.from === "root") {
		return join(pkgRoot, ...entry.out.split("/"));
	}
	const inBuild = buildSourceOf(entry);
	if (inBuild !== null) {
		return join(buildRoot, ...inBuild.split("/"));
	}
	if (entry.from === "shared-asset" && typeof entry.source === "string") {
		return resolve(pkgRoot, entry.source);
	}
	return null;
}

// Read everything into memory before touching dist/: the wipe below would
// otherwise delete the compiled modules this script is about to copy.
const contents = new Map();
for (const entry of PACKAGE_ENTRIES) {
	const from = sourcePathOf(entry);
	if (from === null) {
		console.error(
			`pack: ${entry.out} declares from='${entry.from}' but pack.mjs cannot locate it (shared-asset rows need an explicit \`source\`)`,
		);
		process.exit(1);
	}
	if (!existsSync(from)) {
		console.error(`pack: ${entry.out} is declared in PACKAGE_ENTRIES but ${from} does not exist`);
		process.exit(1);
	}
	contents.set(entry.out, readFileSync(from));
}

for (const name of readdirSync(distRoot)) {
	if (name !== BUILD_WORKSPACE) {
		rmSync(join(distRoot, name), { recursive: true, force: true });
	}
}

let bytes = 0;
for (const [path, data] of contents) {
	const target = join(distRoot, ...path.split("/"));
	mkdirSync(dirname(target), { recursive: true });
	writeFileSync(target, data);
	bytes += data.length;
}

console.log(
	`pack: ${contents.size} file(s), ${bytes} bytes into dist/ (tsc output stays in dist/${BUILD_WORKSPACE}/ and is not shipped)`,
);
