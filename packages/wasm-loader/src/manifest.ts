/**
 * Chunk manifest (SL-4.WASM.02).
 *
 * Mirrors `crates/selis-pdf-wasm/src/chunks.rs` — the shell imports this
 * manifest at build time to know which wasm modules exist, their budgets,
 * and the viewer isolation rule. The manifest is the single source of truth
 * for `xtask/size-budgets.toml` and the Rust `Manifest::canonical()`.
 */

export type ChunkId = "core" | "jpx" | "cjk" | "ocr" | "convert" | "editor";

export interface ChunkEntry {
	id: ChunkId;
	/** Relative URL from the app shell. */
	url: string;
	/** Brotli budget in bytes. */
	budget: number;
	/** May the viewer auto-fetch this chunk? */
	viewerAuto: boolean;
	/** Optional SRI integrity (populated at build). */
	integrity?: string;
}

export interface Manifest {
	v: number;
	engine: string;
	chunks: ChunkEntry[];
}

/** The canonical 6-chunk split (core + 5 lazy). */
export const MANIFEST: Manifest = {
	v: 1,
	engine: "0.1.0",
	chunks: [
		{ id: "core", url: "wasm/selis_pdf_wasm.wasm", budget: 3_000_000, viewerAuto: true },
		{ id: "jpx", url: "wasm/selis_pdf_wasm_jpx.wasm", budget: 400_000, viewerAuto: true },
		{ id: "cjk", url: "wasm/selis_pdf_wasm_cjk.wasm", budget: 500_000, viewerAuto: true },
		{ id: "ocr", url: "wasm/selis_pdf_wasm_ocr.wasm", budget: 800_000, viewerAuto: false },
		{ id: "convert", url: "wasm/selis_pdf_wasm_convert.wasm", budget: 600_000, viewerAuto: false },
		{ id: "editor", url: "wasm/selis_pdf_wasm_editor.wasm", budget: 900_000, viewerAuto: false },
	],
};

/** Returns true iff the viewer may auto-load `id`. */
export function viewerMayLoad(id: ChunkId): boolean {
	const e = MANIFEST.chunks.find((c) => c.id === id);
	return e ? e.viewerAuto : false;
}

/** Validates viewer never downloads editor (WASM.02 DoD). Throws on violation. */
export function validateViewerIsolation(manifest: Manifest = MANIFEST): void {
	for (const e of manifest.chunks) {
		if (e.id === "editor" && e.viewerAuto) {
			throw new Error("viewer must not auto-load the editor chunk (SL-4.WASM.02)");
		}
	}
}

/** All chunk ids. */
export const ALL_CHUNKS: ChunkId[] = ["core", "jpx", "cjk", "ocr", "convert", "editor"];
