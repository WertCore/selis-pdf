/**
 * SL-4.EXT.03 - the Worker that owns the WASM engine.
 *
 * ## Why a Worker inside the offscreen document at all
 *
 * The offscreen document already outlives every viewer, so hosting the engine
 * on its own main thread would have the *same* lifetime. The Worker is here
 * for a different reason, and it is the reason EXT.06's transport has no
 * smooth progress: one WASM call occupies the thread it runs on until it
 * returns. On the offscreen document's main thread that means the
 * `chrome.runtime` port is not serviced during a render, so a `cancel` from
 * the viewer queues behind the very work it is meant to stop. In a Worker the
 * port is serviced by the document and the engine is not, which is what makes
 * the in-flight cancellation channel (the WASM.01 `cancel` message) real
 * rather than nominal.
 *
 * ## What it fetches, and why that is not remote code
 *
 * One thing: the core `.wasm` chunk, from a path inside this package
 * (ADR-P0028). It is the same bytes the store upload carries, requested from
 * the extension's own origin, so `host_permissions` stays `[]` - a worker
 * spawned by an extension page needs no host permission to read the
 * extension's own resources. `instantiateStreaming` is not used: it depends
 * on a MIME type the extension origin is not obliged to send, and an
 * `ArrayBuffer` compiles identically from bytes this package already proved
 * it ships.
 *
 * No `eval`, no dynamic import, no `importScripts`: a module worker with
 * static imports is the only shape that can pass the bundled-only gate, and
 * the gate runs on this file like any other.
 */

import { type GuestExports, createWasmGuest } from "./wasm-guest.js";

/**
 * The core WASM chunk, as the WASM.02 manifest names it.
 *
 * `packages/wasm-loader` is the single source of truth for chunk ids and
 * their budgets; this is the same string, and `wasm-worker.test.ts` asserts
 * the two files agree rather than trusting a copy to stay put.
 */
export const CORE_CHUNK_PATH = "wasm/selis_pdf_wasm.wasm";

/** What the offscreen document sends the worker. */
export type WorkerRequest =
	| { readonly kind: "init"; readonly wasmUrl: string }
	| {
			readonly kind: "request";
			readonly id: number;
			readonly request: unknown;
			readonly payload?: ArrayBuffer;
	  };

/** What the worker sends back. */
export type WorkerResponse =
	| { readonly kind: "ready" }
	| { readonly kind: "init-error"; readonly message: string }
	| {
			readonly kind: "reply";
			readonly id: number;
			readonly response: unknown;
			readonly attachment?: ArrayBuffer;
	  }
	| { readonly kind: "fault"; readonly id: number; readonly message: string };

/** A guest handle, narrowed to what this file calls. */
type GuestLike = {
	send(
		request: unknown,
		attachment?: Uint8Array,
	): {
		response: unknown;
		attachment: Uint8Array;
	};
};

/**
 * The globals this worker is allowed to touch, injected.
 *
 * Injected rather than reached for, so the message protocol below can be
 * driven by a test with a stub guest: the protocol is where the bugs are, and
 * it is the one thing here that is not a browser API.
 */
export interface WorkerScope {
	postMessage(message: WorkerResponse): void;
	onMessage(listener: (message: WorkerRequest) => void): void;
	/** Read a packaged resource. */
	fetchBytes(url: string): Promise<ArrayBuffer>;
	/** Compile the guest module. */
	compile(bytes: ArrayBuffer): Promise<WebAssembly.Instance>;
}

/** The production compile step: the platform's, with no wrapper of its own. */
async function compileWithWebAssembly(bytes: ArrayBuffer): Promise<WebAssembly.Instance> {
	const module = await WebAssembly.compile(bytes);
	return await WebAssembly.instantiate(module, {});
}

/**
 * The worker's whole body.
 *
 * `guest` is assigned by `init` and read by the message handler, which is why
 * this is a closure over module state rather than a class: the guest is a
 * process-wide singleton (SL-4.WASM.04's memory strategy accounts for one
 * live tally), and a second instance would be a second document registry
 * pretending to be the same engine.
 */
export function serveWorker(scope: WorkerScope, guest: GuestLike | null = null): void {
	/** The live guest, once `init` has compiled it; `null` before that. */
	let engine: GuestLike | null = guest;
	/** In-flight `init`s, so a second one waits for the first. */
	let compiling: Promise<void> | null = null;

	/**
	 * Instantiate the guest, or explain why not.
	 *
	 * The failure text is what reaches a user when the engine is missing, so
	 * it names the path: someone reading a bug report should be able to tell
	 * "the WASM did not compile" from "the package does not carry it" without
	 * opening the sources.
	 */
	const init = async (wasmUrl: string): Promise<void> => {
		if (engine !== null) {
			return;
		}
		if (compiling === null) {
			compiling = (async () => {
				const bytes = await scope.fetchBytes(wasmUrl);
				const instance = await scope.compile(bytes);
				engine = createWasmGuest(instance.exports as unknown as GuestExports);
			})();
		}
		await compiling;
	};

	scope.onMessage((message) => {
		if (message.kind === "init") {
			void init(message.wasmUrl).then(
				() => scope.postMessage({ kind: "ready" }),
				(error: unknown) =>
					scope.postMessage({
						kind: "init-error",
						message: `the engine at ${message.wasmUrl} did not load: ${
							error instanceof Error ? error.message : String(error)
						}`,
					}),
			);
			return;
		}
		const active = engine;
		if (active === null) {
			// Answering beats hanging: a request that arrives before `ready`
			// is a bug in the host, and a typed fault surfaces it where the
			// viewer can report it rather than where it cannot.
			scope.postMessage({
				kind: "fault",
				id: message.id,
				message: "the engine worker has not finished loading",
			});
			return;
		}
		try {
			const { response, attachment } = active.send(
				message.request,
				message.payload === undefined ? undefined : new Uint8Array(message.payload),
			);
			scope.postMessage({
				kind: "reply",
				id: message.id,
				response,
				// Transferred, not copied: a rendered page is megabytes, and the
				// document side owns the result.
				...(attachment.byteLength === 0 ? {} : { attachment: attachment.buffer as ArrayBuffer }),
			});
		} catch (error) {
			scope.postMessage({
				kind: "fault",
				id: message.id,
				message: error instanceof Error ? error.message : String(error),
			});
		}
	});
}

/**
 * Install {@link serveWorker} on the real `DedicatedWorkerGlobalScope`.
 *
 * Called only when this module is loaded *as* a worker. The guard is what lets
 * a test import the module in Node: importing it must not touch `self`.
 */
function main(): void {
	const scope = globalThis as unknown as {
		postMessage(message: WorkerResponse): void;
		addEventListener(type: "message", listener: (event: MessageEvent) => void): void;
		fetch: typeof fetch;
	};
	serveWorker({
		postMessage: (message) => {
			scope.postMessage(message);
		},
		onMessage: (listener) => {
			scope.addEventListener("message", (event) => {
				listener((event as MessageEvent).data as WorkerRequest);
			});
		},
		fetchBytes: async (url) => {
			const response = await scope.fetch(url);
			if (!response.ok) {
				throw new Error(`fetch ${url}: ${response.status}`);
			}
			return await response.arrayBuffer();
		},
		compile: compileWithWebAssembly,
	});
}

/**
 * Whether this module was loaded *as* the Worker.
 *
 * Structural rather than `instanceof WorkerGlobalScope`, which the DOM lib
 * does not declare: the one thing every worker global has and no document
 * has is the absence of `document`. Using the absence is also the honest
 * test - it is the condition that matters, and it is what keeps a Node test
 * that imports this module for its message protocol from trying to serve a
 * worker that does not exist.
 */
function loadedAsWorker(): boolean {
	return (
		typeof self !== "undefined" &&
		typeof (globalThis as { document?: unknown }).document === "undefined" &&
		typeof (globalThis as { postMessage?: unknown }).postMessage === "function"
	);
}

if (loadedAsWorker()) {
	main();
}
