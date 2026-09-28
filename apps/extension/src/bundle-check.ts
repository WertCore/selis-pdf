/**
 * SL-4.EXT.04 - the CI entry point: `node dist/pkg/bundle-check.js`.
 *
 * Reads the *built* package (never the sources), runs {@link scanBundle} over
 * it, prints every violation, and exits non-zero. Run it through
 * `pnpm --filter @selis/extension check:bundle`, which builds first, so the
 * thing being checked is the thing that would be zipped and uploaded.
 *
 * Thin on purpose: all the decisions live in `bundle-scan.ts` and are covered
 * by `bundle.test.ts`. A gate whose logic lived here would be logic no test
 * could reach.
 */

import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { BUILD_WORKSPACE, type BundleFile, describeViolations, scanBundle } from "./bundle-scan.js";

/**
 * The package root, found by walking up to the directory that holds
 * `manifest.json`.
 *
 * SL-4.EXT.06 moved this module two directories deeper: `tsc` now compiles
 * `apps/ui` in the same program, so `rootDir` is `apps/` and the emitted tree
 * is `dist/pkg/extension/src/…`. A hard-coded `../..` would keep working right
 * up until someone changed the layout again, and would then scan the wrong
 * directory — the worst failure mode a gate has. The manifest is the one file
 * that is always at the package root by definition, so it is the anchor.
 */
function findPackageRoot(from: string): string {
	let dir = from;
	for (;;) {
		if (existsSync(join(dir, "manifest.json"))) {
			return dir;
		}
		const parent = dirname(dir);
		if (parent === dir) {
			throw new Error(`no manifest.json above ${from} - the built package is not where it should be`);
		}
		dir = parent;
	}
}

const pkgRoot = findPackageRoot(dirname(fileURLToPath(import.meta.url)));
const distRoot = join(pkgRoot, "dist");

/** Every file under `dir`, as package-relative `/`-separated paths. */
function listPackageFiles(dir: string, prefix = ""): string[] {
	const found: string[] = [];
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		const relativePath = prefix === "" ? entry.name : `${prefix}/${entry.name}`;
		if (entry.isDirectory()) {
			if (relativePath === BUILD_WORKSPACE) {
				continue;
			}
			found.push(...listPackageFiles(join(dir, entry.name), relativePath));
			continue;
		}
		found.push(relativePath);
	}
	return found;
}

/** Read the built package; a missing build is a failure, never a pass. */
function readPackage(): BundleFile[] {
	if (!statSync(distRoot, { throwIfNoEntry: false })?.isDirectory()) {
		throw new Error(
			`no built package at ${distRoot} - run \`pnpm --filter @selis/extension build\` first; the gate never passes on a missing artefact`,
		);
	}
	return listPackageFiles(distRoot).map((path) => ({
		path,
		text: readFileSync(join(distRoot, ...path.split("/")), "utf8"),
	}));
}

function main(): void {
	const files = readPackage();
	const violations = scanBundle(files);
	const report = [
		"selis extension bundle gate (SL-4.EXT.04, ADR-P0028)",
		`  package: ${relative(pkgRoot, distRoot).split(sep).join("/")}`,
		`  files:   ${files.length}`,
		`  shipped: ${files
			.map((file) => file.path)
			.sort()
			.join(", ")}`,
		"",
		describeViolations(violations),
	].join("\n");
	if (violations.length === 0) {
		console.log(report);
		return;
	}
	console.error(report);
	console.error(`\n${violations.length} violation(s): the bundle is not shippable.`);
	process.exitCode = 1;
}

main();
