import { describe, expect, it, vi } from "vitest";
import { ChunkLoader } from "./loader.js";
import { MANIFEST, validateViewerIsolation } from "./manifest.js";

describe("SL-4.WASM.02 — code splitting", () => {
	it("core budget is ≤3 MB brotli (viewer gate)", () => {
		const core = MANIFEST.chunks.find((c) => c.id === "core");
		if (!core) throw new Error("manifest missing core chunk");
		expect(core.budget).toBe(3_000_000);
		expect(core.budget).toBeLessThanOrEqual(3_000_000);
	});

	it("viewer manifest never auto-loads editor (DoD)", () => {
		expect(() => validateViewerIsolation()).not.toThrow();
		const editor = MANIFEST.chunks.find((c) => c.id === "editor");
		if (!editor) throw new Error("manifest missing editor chunk");
		expect(editor.viewerAuto).toBe(false);
	});

	it("the optimise chunk is manifest-registered and explicit-action-only", () => {
		const optimize = MANIFEST.chunks.find((c) => c.id === "optimize");
		if (!optimize) throw new Error("manifest missing optimize chunk");
		expect(optimize.url).toBe("wasm/selis_pdf_wasm_optimize.wasm");
		expect(optimize.budget).toBe(200_000);
		expect(optimize.viewerAuto).toBe(false);
	});

	it("the compare chunk is manifest-registered and explicit-action-only", () => {
		const compare = MANIFEST.chunks.find((c) => c.id === "compare");
		if (!compare) throw new Error("manifest missing compare chunk");
		expect(compare.url).toBe("wasm/selis_pdf_wasm_compare.wasm");
		expect(compare.budget).toBe(700_000);
		expect(compare.viewerAuto).toBe(false);
	});

	it("the pattern-redaction chunk is manifest-registered and explicit-action-only", () => {
		const chunk = MANIFEST.chunks.find((c) => c.id === "redact-patterns");
		if (!chunk) throw new Error("manifest missing redact-patterns chunk");
		expect(chunk.url).toBe("wasm/selis_pdf_wasm_redact_patterns.wasm");
		expect(chunk.budget).toBe(600_000);
		expect(chunk.viewerAuto).toBe(false);
		expect(MANIFEST.chunks).toHaveLength(9);
	});

	it("viewer session never downloads the editor chunk (loader invariant)", async () => {
		const fetchFn = vi.fn(async () => new ArrayBuffer(10));
		const loader = new ChunkLoader({ fetchFn, profile: "viewer" });
		await expect(loader.load("editor")).rejects.toThrow(
			/viewer session must never download the editor/,
		);
		expect(fetchFn).not.toHaveBeenCalled();
		expect(loader.isLoaded("editor")).toBe(false);
	});

	it("editor profile may load editor after explicit action", async () => {
		const fetchFn = vi.fn(async () => new ArrayBuffer(10));
		const loader = new ChunkLoader({ fetchFn, profile: "editor" });
		await loader.load("editor");
		expect(fetchFn).toHaveBeenCalledWith("wasm/selis_pdf_wasm_editor.wasm");
		expect(loader.isLoaded("editor")).toBe(true);
	});

	it("jpx/cjk are lazily loaded, not eagerly", async () => {
		const fetchFn = vi.fn(async () => new ArrayBuffer(100));
		const loader = new ChunkLoader({ fetchFn, profile: "viewer" });
		await loader.loadCore();
		expect(loader.loadedChunks()).toEqual(["core"]);
		expect(loader.isLoaded("jpx")).toBe(false);
		await loader.load("jpx");
		expect(loader.isLoaded("jpx")).toBe(true);
	});

	it("budget gate: chunk exceeding its budget throws", async () => {
		const big = new ArrayBuffer(500_000); // jpx budget 400k
		const fetchFn = vi.fn(async () => big);
		const loader = new ChunkLoader({ fetchFn, profile: "viewer" });
		await expect(loader.load("jpx")).rejects.toThrow(/ChunkBudgetExceeded/);
	});

	it("second load is idempotent (no second fetch)", async () => {
		const fetchFn = vi.fn(async () => new ArrayBuffer(10));
		const loader = new ChunkLoader({ fetchFn, profile: "viewer" });
		await loader.load("core");
		await loader.load("core");
		expect(fetchFn).toHaveBeenCalledTimes(1);
	});
});
