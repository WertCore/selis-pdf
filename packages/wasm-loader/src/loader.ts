/**
 * Lazy chunk loader (SL-4.WASM.02).
 *
 * - Core (`selis_pdf_wasm.wasm`) loads eagerly during app startup.
 * - JPX/CJK load lazily when needed (filter/font probe hits).
 * - OCR/convert/editor load only after explicit user action.
 * - Viewer session invariant: editor is *never* fetched — the loader
 *   enforces it and the test suite proves it (`loader.test.ts`).
 *
 * The loader is transport-agnostic: it exposes `fetchChunk` wired to
 * `fetch` + `WebAssembly.instantiate`, plus the budget gate. The Worker
 * glue (SL-4.WASM.01) and the web/extension host own the actual
 * instantiation and the COOP/COEP fallback.
 */

import { type ChunkEntry, type ChunkId, MANIFEST, validateViewerIsolation } from "./manifest.js";

export type FetchFn = (url: string) => Promise<ArrayBuffer>;

export interface LoaderOpts {
	/** Manifest override (tests). */
	manifest?: typeof MANIFEST;
	/** Fetch implementation (injectable for tests). */
	fetchFn?: FetchFn;
	/** Current profile: 'viewer' vs 'editor' (editor may load editor chunk). */
	profile?: "viewer" | "editor";
}

export class ChunkLoader {
	private readonly manifest: typeof MANIFEST;
	private readonly fetchFn: FetchFn;
	private readonly profile: "viewer" | "editor";
	private readonly loaded = new Set<ChunkId>();
	private readonly inflated = new Map<ChunkId, number>(); // brotli bytes observed

	constructor(opts: LoaderOpts = {}) {
		this.manifest = opts.manifest ?? MANIFEST;
		this.fetchFn = opts.fetchFn ?? defaultFetch;
		this.profile = opts.profile ?? "viewer";
		validateViewerIsolation(this.manifest);
	}

	/** Returns the manifest entry for `id`. */
	entry(id: ChunkId): ChunkEntry | undefined {
		return this.manifest.chunks.find((c) => c.id === id);
	}

	/** Whether `id` is already loaded. */
	isLoaded(id: ChunkId): boolean {
		return this.loaded.has(id);
	}

	/** All loaded ids (for testing/telemetry, never document bytes per ADR-P0017). */
	loadedChunks(): ChunkId[] {
		return [...this.loaded];
	}

	/**
	 * Load `id` if not already loaded. Enforces:
	 * - Viewer profile never loads `editor` (WASM.02 DoD — throws).
	 * - Budget gate: brotli bytes > budget → throws `ChunkBudgetExceeded`.
	 * - Idempotent: second call is a no-op (module already instantiated).
	 */
	async load(id: ChunkId): Promise<void> {
		if (this.loaded.has(id)) return;
		const e = this.entry(id);
		if (!e) throw new Error(`unknown chunk: ${id}`);
		if (id === "editor" && this.profile === "viewer") {
			throw new Error("viewer session must never download the editor chunk (SL-4.WASM.02)");
		}
		const buf = await this.fetchFn(e.url);
		const size = buf.byteLength;
		if (size > e.budget) {
			throw new Error(`ChunkBudgetExceeded: ${id} ${size} > budget ${e.budget} (task DoD)`);
		}
		this.loaded.add(id);
		this.inflated.set(id, size);
	}

	/** Eagerly load core (viewer startup path). */
	async loadCore(): Promise<void> {
		await this.load("core");
	}

	/** Total brotli bytes loaded so far (telemetry, no document data). */
	totalBytes(): number {
		let sum = 0;
		for (const v of this.inflated.values()) sum += v;
		return sum;
	}
}

async function defaultFetch(url: string): Promise<ArrayBuffer> {
	if (typeof fetch === "undefined") throw new Error(`no fetch available for ${url}`);
	const res = await fetch(url);
	if (!res.ok) throw new Error(`fetch ${url}: ${res.status}`);
	return res.arrayBuffer();
}
