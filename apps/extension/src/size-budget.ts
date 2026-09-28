/**
 * SL-4.EXT.05 — the extension size budget, as data and pure functions.
 *
 * `tools/pack.mjs` writes the shipped package; this module says how heavy that
 * package is allowed to be, and `size-check.ts` measures the built bytes and
 * exits non-zero when they are not. Nothing here touches `fs` — the
 * measurements arrive as arguments — so every rule below is unit-testable and
 * the entry point holds no logic a test could not reach.
 *
 * The policy this enforces is written out in `BUNDLING.md`; the CJK half of it
 * lives in `ext/cjk-payload.ts`.
 */

/**
 * What the packaged extension is allowed to weigh.
 *
 * ## What is measured, and why that rather than something else
 *
 * The bytes in `dist/` after `tools/pack.mjs` has run: **raw, uncompressed,
 * every declared row, nothing else**. That is the quantity a store counts. A
 * store accepts an upload and unpacks it on the user's machine, so a package
 * is what lands on disk, and the install is paid in those bytes whatever the
 * transport did to them. It is also the only quantity that can be measured
 * without `wasm-opt`, a network, or a `brotli` implementation in the loop.
 *
 * Brotli is measured as well, and one rule is expressed in it — the core
 * chunk's. That is not an inconsistency, it is two different questions.
 * `03-CONVENTIONS.md` §12 and `xtask/size-budgets.toml` state the engine's
 * budget in brotli because a compressed figure is what a user on a slow
 * connection pays, and that number has to mean the same thing in both shells
 * or the two drift apart. Everything *around* the engine is a packaging
 * question, and packaging is not compressed.
 *
 * ## Where each number comes from
 *
 * | Budget | Value | Why |
 * |---|---|---|
 * | {@link SIZE_BUDGET.coreBrotliBytes} | 2 MB | Tighter than the web app's 3 MB core budget (§12, `xtask/size-budgets.toml`). The same engine ships in both, so the extension holds it to a smaller number and any growth has to be decided twice rather than inherited once. Measured today: ~1.31 MB. |
 * | {@link SIZE_BUDGET.packageRawBytes} | 8 MB | Below WEB.02's 12 MB per-entry runtime-cache cap, which WEB.02 derived from §12 *and this task*. The relationship is the argument: a cache entry is evictable and a package install is not, so nothing this extension writes permanently to a user's disk may exceed what the web app was willing to hold transiently — and the entire package is smaller still. |
 * | {@link SIZE_BUDGET.fileRawBytes} | 6 MB | The per-file half of the same rule, so one file cannot consume the package budget and hide inside a passing total. |
 * | {@link SIZE_BUDGET.shellRawBytes} | 1 MB | Every packaged file *except* the engine. Without it, a viewer quietly tripling its own JavaScript is a regression the engine's number would absorb. |
 *
 * The CJK payload is deliberately absent from every number here, because it is
 * not in the package: see `ext/cjk-payload.ts`. A rule that budgeted it would
 * be budgeting the opposite of what this task decides.
 */
export const SIZE_BUDGET = {
	/** The whole package, raw. The store-facing number. */
	packageRawBytes: 8_000_000,
	/** Any single packaged file, raw. */
	fileRawBytes: 6_000_000,
	/** Every packaged file except the core chunk, raw. */
	shellRawBytes: 1_000_000,
	/** The core chunk, brotli. Tighter than the web app's 3 MB (§12). */
	coreBrotliBytes: 2_000_000,
} as const;

/**
 * The one WASM chunk the package carries.
 *
 * The same string as `ext/wasm-worker.ts`'s `CORE_CHUNK_PATH`, which
 * `wasm-worker.test.ts` asserts against the WASM.02 manifest in
 * `packages/wasm-loader`, and which the `PACKAGE_ENTRIES` row in
 * `bundle-paths.ts` has to agree with. It is spelled out a third time here
 * rather than imported, because the packer and this gate read paths from the
 * *built* package; a budget that depended on an import chain would be one
 * refactor away from measuring a path the browser never reads.
 * `size-budget.test.ts` asserts the three copies agree, so the duplication
 * cannot rot.
 */
export const CORE_CHUNK_PACKAGE_PATH = "wasm/selis_pdf_wasm.wasm";

/**
 * The chunks that must never appear in the package.
 *
 * `ocr`, `convert` and `editor` are `viewerAuto: false` in the WASM.02
 * manifest: a viewer session does not fetch them, and in an extension there
 * is no origin to fetch them *from*. Shipping them would spend install bytes
 * on code no viewer can reach, which is the failure SL-4.WASM.02's DoD names.
 *
 * `jpx` and `cjk` are auto-loadable in the web app and absent here for a
 * different reason: the extension holds no host permission, so there is no
 * remote origin to lazily fetch a chunk from, and no code path that asks.
 * They are listed anyway, because "we did not ship it" should be a checked
 * fact rather than a claim in a comment — a future task that decides to ship
 * `jpx` has to delete a line here on purpose.
 */
export const FORBIDDEN_CHUNK_PATHS: readonly string[] = [
	"wasm/selis_pdf_wasm_ocr.wasm",
	"wasm/selis_pdf_wasm_convert.wasm",
	"wasm/selis_pdf_wasm_editor.wasm",
	"wasm/selis_pdf_wasm_jpx.wasm",
	"wasm/selis_pdf_wasm_cjk.wasm",
];

/**
 * Extensions that would mean a font shipped in the package.
 *
 * The CJK payload is an optional post-install download into extension storage
 * (ADR-P0043, `ext/cjk-payload.ts`). Listing the extensions is what makes
 * that decision falsifiable rather than a sentence in a design document: a
 * `.ttf` in the package is a build failure, so "we did not bundle the fonts"
 * is re-checked on every build instead of asserted once.
 */
export const FONT_EXTENSIONS: readonly string[] = [
	".ttf",
	".otf",
	".ttc",
	".otc",
	".woff",
	".woff2",
	".eot",
];

/** One measured file of the built package. */
export interface SizeMeasurement {
	/** Path inside the package (`/`-separated, relative to `dist/`). */
	readonly path: string;
	/** Bytes on disk. The number the store counts. */
	readonly rawBytes: number;
	/**
	 * Bytes after brotli. Measured for every file; one rule consumes it.
	 *
	 * `null` when it could not be measured, which is a failure rather than a
	 * pass: a budget rule that cannot be evaluated must not read as satisfied.
	 */
	readonly brotliBytes: number | null;
}

/** Every way the size gate can fail. */
export type SizeFindingClass =
	/** The whole package is over budget. */
	| "over-package-budget"
	/** One file is over the per-file budget. */
	| "over-file-budget"
	/** Everything except the engine is over budget. */
	| "over-shell-budget"
	/** The engine's brotli size is over the extension's tighter core budget. */
	| "over-core-brotli-budget"
	/** The core chunk is not in the package at all. */
	| "missing-core-wasm"
	/** A file's brotli size could not be measured, so its rule cannot hold. */
	| "unmeasured-brotli"
	/** A font is in the package; the CJK payload is a post-install download. */
	| "bundled-font"
	/** A chunk no viewer may auto-load (or that nothing can fetch) is packaged. */
	| "forbidden-chunk";

/** One gate failure: what, how much, and against which limit. */
export interface SizeFinding {
	readonly class: SizeFindingClass;
	/** The file the finding is about, or a parenthesised scope for a total. */
	readonly path: string;
	readonly measured: number | null;
	readonly budget: number;
	readonly detail: string;
}

/** A size report: the totals a human reads, and the findings a gate fails on. */
export interface SizeReport {
	readonly totalRawBytes: number;
	readonly totalBrotliBytes: number;
	readonly shellRawBytes: number;
	readonly coreRawBytes: number | null;
	readonly coreBrotliBytes: number | null;
	readonly findings: readonly SizeFinding[];
}

/** Lowercased extension of a package path, `""` when it has none. */
export function extensionOf(path: string): string {
	const name = path.slice(path.lastIndexOf("/") + 1);
	const dot = name.lastIndexOf(".");
	return dot <= 0 ? "" : name.slice(dot).toLowerCase();
}

/**
 * Apply the budget to a set of measurements.
 *
 * `measurements` is the built package, walked by the caller: this function
 * does not care where the bytes came from, which is what lets the same rules
 * run against the real `dist/` and against a planted violation in a test.
 *
 * A measurement set without the core chunk is a failure, not an absence of
 * one. That inversion is the point of the rule: EXT.03 shipped an engine that
 * reported "the `.wasm` is not in this build" while every gate was green, so
 * *the engine's bytes exist* has to be a check rather than an assumption.
 */
export function evaluateSizeBudget(measurements: readonly SizeMeasurement[]): SizeReport {
	const findings: SizeFinding[] = [];
	const byPath = new Map(measurements.map((m) => [m.path, m]));

	let totalRaw = 0;
	let totalBrotli = 0;
	let brotliComplete = true;
	let shellRaw = 0;
	for (const m of measurements) {
		totalRaw += m.rawBytes;
		if (m.brotliBytes === null) {
			brotliComplete = false;
		} else {
			totalBrotli += m.brotliBytes;
		}
		if (m.path !== CORE_CHUNK_PACKAGE_PATH) {
			shellRaw += m.rawBytes;
		}
		if (m.rawBytes > SIZE_BUDGET.fileRawBytes) {
			findings.push({
				class: "over-file-budget",
				path: m.path,
				measured: m.rawBytes,
				budget: SIZE_BUDGET.fileRawBytes,
				detail: `${m.path} is ${m.rawBytes} raw bytes; no single packaged file may exceed ${SIZE_BUDGET.fileRawBytes}`,
			});
		}
		if (m.brotliBytes === null) {
			findings.push({
				class: "unmeasured-brotli",
				path: m.path,
				measured: null,
				budget: SIZE_BUDGET.coreBrotliBytes,
				detail: `${m.path} has no brotli measurement, so no rule over it can be evaluated`,
			});
		}
		const extension = extensionOf(m.path);
		if (FONT_EXTENSIONS.includes(extension)) {
			findings.push({
				class: "bundled-font",
				path: m.path,
				measured: m.rawBytes,
				budget: 0,
				detail: `${m.path} is a font: the CJK payload is an optional post-install download into extension storage (ADR-P0043), never a bundled asset`,
			});
		}
		if (FORBIDDEN_CHUNK_PATHS.includes(m.path)) {
			findings.push({
				class: "forbidden-chunk",
				path: m.path,
				measured: m.rawBytes,
				budget: 0,
				detail: `${m.path} must not ship: the WASM.02 manifest does not let a viewer auto-load it, and the extension holds no host permission to fetch it on demand`,
			});
		}
	}

	const core = byPath.get(CORE_CHUNK_PACKAGE_PATH) ?? null;
	if (core === null) {
		findings.push({
			class: "missing-core-wasm",
			path: CORE_CHUNK_PACKAGE_PATH,
			measured: null,
			budget: SIZE_BUDGET.coreBrotliBytes,
			detail: `${CORE_CHUNK_PACKAGE_PATH} is not in the built package, so the engine EXT.03 hosts has nothing to compile and the viewer renders nothing`,
		});
	} else if (core.brotliBytes !== null && core.brotliBytes > SIZE_BUDGET.coreBrotliBytes) {
		findings.push({
			class: "over-core-brotli-budget",
			path: CORE_CHUNK_PACKAGE_PATH,
			measured: core.brotliBytes,
			budget: SIZE_BUDGET.coreBrotliBytes,
			detail: `${CORE_CHUNK_PACKAGE_PATH} is ${core.brotliBytes} brotli bytes; this extension's core budget is ${SIZE_BUDGET.coreBrotliBytes} (the web app's is 3,000,000)`,
		});
	}

	if (totalRaw > SIZE_BUDGET.packageRawBytes) {
		findings.push({
			class: "over-package-budget",
			path: "(package)",
			measured: totalRaw,
			budget: SIZE_BUDGET.packageRawBytes,
			detail: `the package is ${totalRaw} raw bytes; the store install budget is ${SIZE_BUDGET.packageRawBytes}`,
		});
	}
	if (shellRaw > SIZE_BUDGET.shellRawBytes) {
		findings.push({
			class: "over-shell-budget",
			path: "(package minus the engine)",
			measured: shellRaw,
			budget: SIZE_BUDGET.shellRawBytes,
			detail: `everything except ${CORE_CHUNK_PACKAGE_PATH} is ${shellRaw} raw bytes; the shell budget is ${SIZE_BUDGET.shellRawBytes}`,
		});
	}

	return {
		totalRawBytes: totalRaw,
		// A total that silently dropped an unmeasurable file would read as a
		// smaller package than the one on disk, which is the direction a gate
		// must never be wrong in. Zero, and the finding says why.
		totalBrotliBytes: brotliComplete ? totalBrotli : 0,
		shellRawBytes: shellRaw,
		coreRawBytes: core?.rawBytes ?? null,
		coreBrotliBytes: core?.brotliBytes ?? null,
		findings,
	};
}

/** Human-readable report lines: the numbers, then the findings. */
export function describeSizeReport(report: SizeReport): string {
	const pct = (value: number, budget: number): string => `${((value / budget) * 100).toFixed(1)}%`;
	const lines = [
		`  package: ${report.totalRawBytes} raw bytes of ${SIZE_BUDGET.packageRawBytes} (${pct(report.totalRawBytes, SIZE_BUDGET.packageRawBytes)}), ${report.totalBrotliBytes} brotli`,
		`  engine:  ${report.coreRawBytes ?? "absent"} raw / ${report.coreBrotliBytes ?? "unmeasured"} brotli of ${SIZE_BUDGET.coreBrotliBytes} brotli`,
		`  shell:   ${report.shellRawBytes} raw of ${SIZE_BUDGET.shellRawBytes} (${pct(report.shellRawBytes, SIZE_BUDGET.shellRawBytes)}), everything but the engine`,
	];
	if (report.findings.length === 0) {
		lines.push("clean: within budget, carries the engine, bundles no font");
		return lines.join("\n");
	}
	for (const finding of report.findings) {
		lines.push(`  [${finding.class}] ${finding.detail}`);
	}
	return lines.join("\n");
}
