/**
 * Worker glue (SL-4.WASM.01 wire + SL-4.WASM.05 adapters, JS side).
 *
 * Maps the handoff sources to the Rust Worker's `SourceDescriptor` wire
 * shape (`24-BINDINGS-SPEC.md §2`, ADR-P0042) and builds the `open` request
 * JSON. Pure and transfer-free: it never touches a `Worker`, `Blob`, OPFS
 * handle, or `fetch` — the shell entry point performs the transfers this
 * module describes.
 *
 * Wire shapes (kebab-case `kind`, camelCase fields — must match
 * `crates/selis-pdf-wasm/src/protocol.rs` exactly):
 * - `{ kind: "bytes", len }` — inline bytes as the request attachment.
 * - `{ kind: "blob", sourceId }` — a Blob the shell registered with the
 *   Worker via `register_blob(sourceId, bytes)` (read with `FileReaderSync`
 *   in the Worker, never on the main thread).
 * - `{ kind: "opfs", path }` — an OPFS path registered via
 *   `register_opfs(path, bytes)` (sync access handle in the Worker).
 * - `{ kind: "fsa", handleId }` — a File System Access handle registered via
 *   `register_fsa(handleId, bytes)` (save-in-place path, Phase 5 builds it).
 * - `{ kind: "http-range", url, size? }` — remote bytes the Worker's range
 *   fetcher streams (WASM.06 driver; still local processing, never upload).
 */

/** The wire `SourceDescriptor` (must match the Rust `protocol.rs` enum). */
export type WireSourceDescriptor =
	| { readonly kind: "bytes"; readonly len: number }
	| { readonly kind: "blob"; readonly sourceId: string }
	| { readonly kind: "opfs"; readonly path: string }
	| { readonly kind: "fsa"; readonly handleId: string }
	| { readonly kind: "http-range"; readonly url: string; readonly size?: number };

/** The UI-side source the glue maps (mirrors `handoff.ts` + UI descriptors). */
export type GlueSource =
	| { readonly kind: "bytes"; readonly len: number }
	| { readonly kind: "blob"; readonly sourceId: string }
	| { readonly kind: "opfs"; readonly path: string }
	| { readonly kind: "fsa"; readonly handleId: string }
	| { readonly kind: "http-range"; readonly url: string; readonly size?: number };

/** How the shell must deliver the source to the Worker before `open`. */
export type Delivery =
	| { readonly via: "attachment"; readonly len: number }
	| { readonly via: "register-blob"; readonly sourceId: string }
	| { readonly via: "register-opfs"; readonly path: string }
	| { readonly via: "register-fsa"; readonly handleId: string }
	| { readonly via: "fetch-ranges"; readonly url: string };

/** Map a glue source to its wire descriptor (1:1 today — the mapping is the contract). */
export function toWireDescriptor(source: GlueSource): WireSourceDescriptor {
	switch (source.kind) {
		case "bytes":
			return { kind: "bytes", len: source.len };
		case "blob":
			return { kind: "blob", sourceId: source.sourceId };
		case "opfs":
			return { kind: "opfs", path: source.path };
		case "fsa":
			return { kind: "fsa", handleId: source.handleId };
		case "http-range":
			return source.size === undefined
				? { kind: "http-range", url: source.url }
				: { kind: "http-range", url: source.url, size: source.size };
	}
}

/** How the shell delivers the source bytes before dispatching `open`. */
export function deliveryFor(source: GlueSource): Delivery {
	switch (source.kind) {
		case "bytes":
			return { via: "attachment", len: source.len };
		case "blob":
			return { via: "register-blob", sourceId: source.sourceId };
		case "opfs":
			return { via: "register-opfs", path: source.path };
		case "fsa":
			return { via: "register-fsa", handleId: source.handleId };
		case "http-range":
			return { via: "fetch-ranges", url: source.url };
	}
}

/** Budget surface names (must match the Rust `SurfaceName` enum, lowercase). */
export type BudgetSurface = "thumbnail" | "viewer" | "editor" | "batch" | "server";

/**
 * Build the `open` request JSON for `selis_dispatch`.
 * The caller copies the request bytes + attachment into guest memory.
 */
export function buildOpenRequest(
	id: number,
	source: GlueSource,
	surface: BudgetSurface = "viewer",
): Record<string, unknown> {
	return {
		v: 1,
		id,
		op: "open",
		src: toWireDescriptor(source),
		budget: { surface },
	};
}

/** Parse and validate an `open` response value (`{ doc, pages }`). */
export function parseOpenResponse(response: unknown): { doc: number; pages: number } {
	if (typeof response !== "object" || response === null) {
		throw new Error("open response is not an object");
	}
	const record = response as Record<string, unknown>;
	const value = record.value as Record<string, unknown> | undefined;
	if (typeof value !== "object" || value === null) {
		throw new Error("open response carries no value");
	}
	const doc = (value as Record<string, unknown>).doc;
	const pages = (value as Record<string, unknown>).pages;
	if (typeof doc !== "number" || typeof pages !== "number") {
		throw new Error("open response value has no numeric doc/pages");
	}
	return { doc, pages };
}
