/**
 * SL-4.EXT.05 — the size budget, proven falsifiable.
 *
 * A budget nobody can fail is a comment, so this file has the two halves that
 * make a gate credible:
 *
 * - **It fails.** Every rule has a planted violation that crosses it, and each
 *   is asserted to produce its named finding. A neutered evaluator — a
 *   comparison that became `<=`, a `findings` array that is never pushed to, a
 *   budget set to `Number.MAX_SAFE_INTEGER` — fails these immediately.
 * - **It stays quiet** on a package shaped like the real one, because a gate
 *   that fires on every honest build gets deleted rather than fixed.
 *
 * The last section runs the rules over the **real built package** and then
 * over that same package with a violation planted in it, which is the only
 * version of "the gate works" that says anything about the artefact a store
 * would receive.
 */

import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { constants, brotliCompressSync } from "node:zlib";
import { describe, expect, it } from "vitest";
import { BUILD_WORKSPACE, PACKAGE_ENTRIES, SHIPPED_FILES } from "./bundle-paths.js";
import { type BundleFile, describeViolations, isBinaryAsset, scanBundle } from "./bundle-scan.js";
import { CORE_CHUNK_PATH } from "./ext/wasm-worker.js";
import {
	CORE_CHUNK_PACKAGE_PATH,
	FONT_EXTENSIONS,
	FORBIDDEN_CHUNK_PATHS,
	SIZE_BUDGET,
	type SizeFindingClass,
	type SizeMeasurement,
	describeSizeReport,
	evaluateSizeBudget,
	extensionOf,
} from "./size-budget.js";

const pkgRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const distRoot = join(pkgRoot, "dist");

/**
 * A package shaped like the real one and comfortably inside every budget.
 *
 * The engine is sized from the measurement that matters: ~1.31 MB brotli
 * (`xtask/size-baseline.json`) against this extension's 2 MB core budget, with
 * a raw size consistent with a ~4× compression ratio. The point of the fixture
 * is that the *rules* are what is under test, so the numbers only have to be
 * plausible and legal.
 */
const ENGINE_RAW = 4_135_676;
const ENGINE_BROTLI = 1_313_250;

/** One measured file. */
function file(
	path: string,
	rawBytes: number,
	brotliBytes: number | null = rawBytes,
): SizeMeasurement {
	return { path, rawBytes, brotliBytes };
}

/** A legal package: the engine, a shell, and nothing else. */
function cleanPackage(): SizeMeasurement[] {
	return [
		file(CORE_CHUNK_PACKAGE_PATH, ENGINE_RAW, ENGINE_BROTLI),
		file("manifest.json", 640),
		file("viewer.html", 1_225),
		file("extension/src/ext/wasm-worker.js", 8_181),
	];
}

/** The classes a measurement set produces. */
function classesOf(measurements: readonly SizeMeasurement[]): SizeFindingClass[] {
	return evaluateSizeBudget(measurements).findings.map((finding) => finding.class);
}

/** The clean package with one file replaced or added. */
function withFile(
	path: string,
	rawBytes: number,
	brotliBytes: number | null = rawBytes,
): SizeMeasurement[] {
	const rest = cleanPackage().filter((m) => m.path !== path);
	return [...rest, file(path, rawBytes, brotliBytes)];
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. The rules, each one crossed on purpose.
// ─────────────────────────────────────────────────────────────────────────────

describe("EXT.05 the size rules fail when they should", () => {
	it("is quiet on a package shaped like the real one", () => {
		const report = evaluateSizeBudget(cleanPackage());
		expect(report.findings).toEqual([]);
		expect(report.totalRawBytes).toBeGreaterThan(ENGINE_RAW);
		expect(report.shellRawBytes).toBe(report.totalRawBytes - ENGINE_RAW);
		expect(describeSizeReport(report)).toContain("clean");
	});

	it("fails a package with no engine at all — the EXT.03 state", () => {
		// The exact regression this task exists to prevent: EXT.03 shipped a
		// viewer whose engine could not load, named the missing path, and had
		// every gate green. "No engine" must be a failure, not an absence of one.
		const withoutEngine = cleanPackage().filter((m) => m.path !== CORE_CHUNK_PACKAGE_PATH);
		const report = evaluateSizeBudget(withoutEngine);
		expect(report.findings.map((f) => f.class)).toContain("missing-core-wasm");
		expect(report.coreRawBytes).toBeNull();
		expect(describeSizeReport(report)).toContain("renders nothing");
	});

	it("fails an engine over the extension's brotli budget", () => {
		const over = withFile(CORE_CHUNK_PACKAGE_PATH, ENGINE_RAW, SIZE_BUDGET.coreBrotliBytes + 1);
		const finding = evaluateSizeBudget(over).findings.find(
			(f) => f.class === "over-core-brotli-budget",
		);
		expect(finding).toBeDefined();
		expect(finding?.measured).toBe(SIZE_BUDGET.coreBrotliBytes + 1);
		// The web app's own number is named, so a reader can see this is
		// tighter rather than a different unit.
		expect(finding?.detail).toContain("3,000,000");
	});

	it("accepts an engine exactly on the budget", () => {
		const exact = withFile(CORE_CHUNK_PACKAGE_PATH, ENGINE_RAW, SIZE_BUDGET.coreBrotliBytes);
		expect(classesOf(exact)).not.toContain("over-core-brotli-budget");
	});

	it("fails a package over the store budget", () => {
		// Two legal files that together exceed the total, neither over the
		// per-file cap: the case a per-file rule alone would miss.
		const heavy = [
			file(CORE_CHUNK_PACKAGE_PATH, ENGINE_RAW, ENGINE_BROTLI),
			file("extension/src/big-a.js", SIZE_BUDGET.fileRawBytes),
			file("extension/src/big-b.js", SIZE_BUDGET.fileRawBytes),
		];
		expect(classesOf(heavy)).toContain("over-package-budget");
	});

	it("fails a single file over the per-file budget", () => {
		const over = withFile("extension/src/huge.js", SIZE_BUDGET.fileRawBytes + 1);
		expect(classesOf(over)).toContain("over-file-budget");
	});

	it("fails a shell that grew past its own budget", () => {
		// Under the package total, over the shell budget: the rule that keeps a
		// viewer quietly tripling its own JavaScript from hiding inside the
		// engine's number.
		const shell = SIZE_BUDGET.shellRawBytes + 1;
		const report = evaluateSizeBudget([
			file(CORE_CHUNK_PACKAGE_PATH, ENGINE_RAW, ENGINE_BROTLI),
			file("extension/src/a.js", shell),
		]);
		expect(report.findings.map((f) => f.class)).toEqual(["over-shell-budget"]);
		expect(report.totalRawBytes).toBeLessThan(SIZE_BUDGET.packageRawBytes);
	});

	it("fails a bundled font — the CJK payload is a post-install download", () => {
		for (const extension of FONT_EXTENSIONS) {
			const planted = withFile(`cjk/hiragana${extension}`, 400_000);
			expect(classesOf(planted), extension).toContain("bundled-font");
		}
	});

	it("fails a chunk the viewer may not auto-load", () => {
		for (const path of FORBIDDEN_CHUNK_PATHS) {
			expect(classesOf(withFile(path, 450_000)), path).toContain("forbidden-chunk");
		}
	});

	it("fails rather than passes when a file's brotli size is unmeasurable", () => {
		// A rule that cannot be evaluated must not read as satisfied. This is
		// the same inversion `missing-core-wasm` exists for, applied to the
		// measurement rather than to the file.
		const unmeasured = withFile(CORE_CHUNK_PACKAGE_PATH, ENGINE_RAW, null);
		const report = evaluateSizeBudget(unmeasured);
		expect(report.findings.map((f) => f.class)).toEqual(["unmeasured-brotli"]);
		// And the total refuses to quote a number it could not complete.
		expect(report.totalBrotliBytes).toBe(0);
	});

	it("classifies extensions the way the font rule assumes", () => {
		expect(extensionOf("cjk/core.ttf")).toBe(".ttf");
		expect(extensionOf("wasm/selis_pdf_wasm.wasm")).toBe(".wasm");
		expect(extensionOf("ui-kit/css/tokens.css")).toBe(".css");
		// A dotfile and an extensionless file have no extension; treating
		// ".gitignore" as one would make the font rule fire on noise.
		expect(extensionOf("LICENSE")).toBe("");
		expect(extensionOf(".gitignore")).toBe("");
	});
});

// ─────────────────────────────────────────────────────────────────────────────
// 2. The three copies of the engine's path agree.
// ─────────────────────────────────────────────────────────────────────────────

describe("EXT.05 the engine path is the same string everywhere", () => {
	it("agrees with the worker that fetches it", () => {
		expect(CORE_CHUNK_PACKAGE_PATH).toBe(CORE_CHUNK_PATH);
	});

	it("agrees with the WASM.02 manifest's core chunk", () => {
		// The manifest's *source text*, read rather than imported: this
		// package's `tsc` program has `rootDir: apps/`, so importing across
		// into `packages/` would pull a foreign tree into the build. This is
		// `engine-host.test.ts`'s existing check, and it is a string equality
		// either way.
		const manifest = readFileSync(
			join(pkgRoot, "..", "..", "packages", "wasm-loader", "src", "manifest.ts"),
			"utf8",
		);
		expect(manifest).toContain(`url: "${CORE_CHUNK_PACKAGE_PATH}"`);
	});

	it("is a ship-list row that names where its bytes come from", () => {
		// Not merely present: `from: "wasm-artifact"` is what tells `pack.mjs`
		// to look in a cargo target directory rather than the build output.
		const row = PACKAGE_ENTRIES.find((entry) => entry.out === CORE_CHUNK_PACKAGE_PATH);
		expect(row?.from).toBe("wasm-artifact");
		expect(row?.source).toBe("selis_pdf_wasm.opt.wasm");
	});

	it("keeps the five lazy chunks off the ship list", () => {
		for (const path of FORBIDDEN_CHUNK_PATHS) {
			expect(SHIPPED_FILES, path).not.toContain(path);
		}
	});
});

// ─────────────────────────────────────────────────────────────────────────────
// 3. The real built package, and the same package with something planted in it.
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Measure what is actually in `dist/`.
 *
 * Only the engine is brotli-compressed, and only because it is the only file
 * a rule reads a brotli number from. Every other file reports its raw size as
 * its brotli size, which over-states it: a conservative substitution, chosen
 * so this suite does not spend seconds compressing four megabytes to learn
 * nothing.
 *
 * The engine is compressed at brotli **quality 1**, not the default
 * `size-check.ts` uses. That is not a different measurement dressed up as the
 * same one — quality 1 produces a *larger* buffer than the default for the
 * same input (1.89 MB against 1.33 MB on today's engine), so a package that
 * passes here passes the real rule too. It costs 26 ms instead of 15 s, and
 * the printed figures in `pnpm build` remain the authority.
 *
 * Memoised because four assertions in this file want the same measurement and
 * recomputing it per assertion is how a 60-second suite happens.
 */
let builtCache: SizeMeasurement[] | null = null;

function measureBuiltPackage(dir: string): SizeMeasurement[] {
	if (builtCache !== null) {
		return builtCache;
	}
	const found: SizeMeasurement[] = [];
	const walk = (current: string, prefix: string): void => {
		for (const entry of readdirSync(current, { withFileTypes: true })) {
			const path = prefix === "" ? entry.name : `${prefix}/${entry.name}`;
			const full = join(current, entry.name);
			if (entry.isDirectory()) {
				if (path === BUILD_WORKSPACE) {
					continue;
				}
				walk(full, path);
				continue;
			}
			const rawBytes = statSync(full).size;
			found.push({
				path,
				rawBytes,
				brotliBytes:
					path === CORE_CHUNK_PACKAGE_PATH
						? brotliCompressSync(readFileSync(full), {
								params: { [constants.BROTLI_PARAM_QUALITY]: 1 },
							}).length
						: rawBytes,
			});
		}
	};
	walk(dir, "");
	builtCache = found.sort((a, b) => a.path.localeCompare(b.path));
	return builtCache;
}

// The built package only exists after `pnpm --filter @selis/extension build`,
// which `pnpm test` runs first. Asserting the prerequisite rather than skipping
// keeps this from decaying into a test that passes because it did nothing.
describe("EXT.05 the real built package", () => {
	it("was built (run `pnpm --filter @selis/extension build` first)", () => {
		expect(existsSync(join(distRoot, "manifest.json"))).toBe(true);
		expect(existsSync(join(distRoot, CORE_CHUNK_PACKAGE_PATH))).toBe(true);
	});

	it("is within budget, and carries a real engine", () => {
		const report = evaluateSizeBudget(measureBuiltPackage(distRoot));
		expect(describeSizeReport(report)).toBeTruthy();
		expect(report.findings, describeSizeReport(report)).toEqual([]);
		// Not "absent" and not "0": a few megabytes of actual WASM, which is
		// the difference between this task shipping and EXT.03's viewer
		// rendering nothing.
		expect(report.coreRawBytes ?? 0).toBeGreaterThan(1_000_000);
		expect(report.coreBrotliBytes ?? 0).toBeGreaterThan(100_000);
		// And the engine is the bulk of the package, which is the fact the
		// budget numbers are chosen around.
		expect(report.coreRawBytes ?? 0).toBeGreaterThan(report.shellRawBytes);
	});

	it("bundles no font, because the CJK payload is a post-install download", () => {
		const fonts = measureBuiltPackage(distRoot).filter((m) =>
			FONT_EXTENSIONS.includes(extensionOf(m.path)),
		);
		expect(fonts.map((m) => m.path)).toEqual([]);
	});

	it("fails when a font is added to the real package", () => {
		// Falsifiability on the real artefact rather than on a fixture: the
		// same bytes `size-check.ts` would see, plus the file a future task
		// might be tempted to add.
		const planted = [...measureBuiltPackage(distRoot), file("cjk/core.ttf", 900_000)];
		expect(classesOf(planted)).toContain("bundled-font");
	});

	it("fails when the real package loses its engine", () => {
		// The other direction, and the one that matters most: delete the row
		// from the ship list and this is what the gate says.
		const withoutEngine = measureBuiltPackage(distRoot).filter(
			(m) => m.path !== CORE_CHUNK_PACKAGE_PATH,
		);
		expect(classesOf(withoutEngine)).toContain("missing-core-wasm");
	});
});

/**
 * The built package as the bundled-only gate sees it: every file, utf8-decoded,
 * including the multi-megabyte binary.
 *
 * The size gate is a new gate next to an old one, and the old one just gained
 * a file it has to skip. This is the assertion that skipping it did not cost
 * the gate its teeth.
 */
function readBuiltPackage(dir: string): BundleFile[] {
	const found: BundleFile[] = [];
	const walk = (current: string, prefix: string): void => {
		for (const entry of readdirSync(current, { withFileTypes: true })) {
			const path = prefix === "" ? entry.name : `${prefix}/${entry.name}`;
			if (entry.isDirectory()) {
				if (path === BUILD_WORKSPACE) {
					continue;
				}
				walk(join(current, entry.name), path);
				continue;
			}
			found.push({ path, text: readFileSync(join(current, entry.name), "utf8") });
		}
	};
	walk(dir, "");
	return found;
}

describe("EXT.05 the binary in the package does not neuter the bundled-only gate", () => {
	const built = (): BundleFile[] => readBuiltPackage(distRoot);

	it("confirms the exemption is load-bearing: a real binary really is skipped", () => {
		// If this ever stops holding, the "the gate skipped a file" story above
		// has changed shape and the rest of this file is measuring the wrong
		// thing.
		const engine = built().find((file) => file.path === CORE_CHUNK_PACKAGE_PATH);
		expect(engine).toBeDefined();
		expect(isBinaryAsset(engine?.text ?? "")).toBe(true);
	});

	it("still scans everything around the binary", () => {
		expect(scanBundle(built())).toEqual([]);
	});

	it("still fails on a violation planted in a module beside the binary", () => {
		// Planted with a leading newline on purpose: `tsc` ends every emitted
		// module with `//# sourceMappingURL=…`, and a plant appended without
		// one joins that comment — a "violation" that is a comment, which is
		// the way to verify this gate wrong (see `BUNDLING.md`).
		const planted = built().map((file) =>
			file.path === "extension/src/ext/wasm-worker.js"
				? { ...file, text: `${file.text}\neval("1+1");\n` }
				: file,
		);
		const violations = scanBundle(planted);
		expect(violations.map((violation) => violation.class)).toContain("dynamic-code");
		expect(describeViolations(violations)).toContain("eval");
	});
});
