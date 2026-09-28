/**
 * SL-4.EXT.05 — the CI entry point: `node dist/pkg/extension/src/size-check.js`.
 *
 * Measures the **built package** — the same `dist/` the store upload would be
 * made from — and applies {@link evaluateSizeBudget} to it. It runs after the
 * bundled-only gate inside `pnpm build`, so a package cannot be shipped over
 * budget and cannot be shipped without its engine: one build, two gates, and
 * the second one is the reason `pnpm build` is a release gate rather than a
 * compile.
 *
 * Thin on purpose, for the reason `bundle-check.ts` is thin: the rules live
 * in `size-budget.ts` where a test can reach them, and this file only reads
 * bytes and sets an exit code.
 */

import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { brotliCompressSync } from "node:zlib";
import { BUILD_WORKSPACE } from "./bundle-paths.js";
import { type SizeMeasurement, describeSizeReport, evaluateSizeBudget } from "./size-budget.js";

/**
 * The package root, found by walking up from this module.
 *
 * The same `package.json` + `dist/manifest.json` pair `bundle-check.ts` uses,
 * and for the same reason: `pack.mjs` copies the manifest into `dist/`, so a
 * walk looking only for that file stops one directory too high.
 */
function findPackageRoot(from: string): string {
	let dir = from;
	for (;;) {
		if (existsSync(join(dir, "package.json")) && existsSync(join(dir, "dist", "manifest.json"))) {
			return dir;
		}
		const parent = dirname(dir);
		if (parent === dir) {
			throw new Error(
				`no package.json + dist/manifest.json pair above ${from} - the built package is not where it should be`,
			);
		}
		dir = parent;
	}
}

/** Every shipped file under `dir`, as package-relative `/`-separated paths. */
function listPackageFiles(dir: string, prefix = ""): string[] {
	const found: string[] = [];
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		const path = prefix === "" ? entry.name : `${prefix}/${entry.name}`;
		if (entry.isDirectory()) {
			// The raw `tsc` output is not shipped and is not measured: the
			// budget is about the store upload, and measuring the build
			// workspace would count every test file twice.
			if (path === BUILD_WORKSPACE) {
				continue;
			}
			found.push(...listPackageFiles(join(dir, entry.name), path));
			continue;
		}
		found.push(path);
	}
	return found;
}

/**
 * Brotli-compress one buffer.
 *
 * `brotliCompressSync` with no options, which is exactly what
 * `xtask/src/size_check.rs` calls for the WASM.02 baseline. The two must
 * produce comparable numbers: the baseline records 1,313,250 bytes for the
 * core chunk, and a budget evaluated against a differently-configured
 * compressor would drift from the figure it is derived from without anybody
 * deciding anything.
 */
function brotliLength(bytes: Buffer): number {
	return brotliCompressSync(bytes).length;
}

/** Read the built package; a missing build is a failure, never a pass. */
function measurePackage(distRoot: string): SizeMeasurement[] {
	if (!statSync(distRoot, { throwIfNoEntry: false })?.isDirectory()) {
		throw new Error(
			`no built package at ${distRoot} - run \`pnpm --filter @selis/extension build\` first; the gate never passes on a missing artefact`,
		);
	}
	return listPackageFiles(distRoot)
		.sort()
		.map((path) => {
			const bytes = readFileSync(join(distRoot, ...path.split("/")));
			return {
				path,
				rawBytes: bytes.length,
				// Every file is compressed, not only the engine: the report
				// quotes a package total, and a total that quietly excluded
				// the small files would not be a total.
				brotliBytes: brotliLength(bytes),
			};
		});
}

function main(): void {
	const pkgRoot = findPackageRoot(dirname(fileURLToPath(import.meta.url)));
	const distRoot = join(pkgRoot, "dist");
	const measurements = measurePackage(distRoot);
	const report = evaluateSizeBudget(measurements);
	const output = [
		"selis extension size budget (SL-4.EXT.05)",
		`  package: ${relative(pkgRoot, distRoot).split(sep).join("/")}`,
		`  files:   ${measurements.length}`,
		"",
		describeSizeReport(report),
	].join("\n");
	if (report.findings.length === 0) {
		console.log(output);
		return;
	}
	console.error(output);
	console.error(
		`\n${report.findings.length} finding(s): the package is not shippable at this size.`,
	);
	process.exitCode = 1;
}

main();
