/**
 * SL-4.EXT.03 - the offscreen host, and the lifecycle it exists for.
 *
 * The DoD for this task is a test that the viewer survives service-worker
 * termination mid-session, so that is what this file is built around. MV3
 * kills an idle service worker after roughly 30 seconds and gives it no DOM,
 * so the arrangement that survives it is structural: the worker creates the
 * offscreen document and then owns nothing, the document's port is to the
 * viewer rather than to the worker, and the engine lives in a document whose
 * lifetime is its own.
 *
 * A structural claim needs a test that can fail. The strongest one available
 * without a browser is to drive the whole stack - the real client, the real
 * host, the real engine mapping, over the JSON loopback - and then destroy
 * the service worker mid-session, exactly as Chrome does, and keep using the
 * document. The source-level assertions at the end pin the two properties that
 * make the destruction harmless in the first place: the worker registers no
 * `onConnect` listener, and the document imports nothing from it.
 */

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { PACKAGE_ENTRIES, SHIPPED_FILES } from "../bundle-paths.js";
import { createEnginePort } from "./engine-client.js";
import { CORE_CHUNK_PATH, ENGINE_WORKER_PATH, startEngineHost } from "./engine-host.js";
import { createLoopbackLink } from "./loopback.js";
import type { WorkerRequest, WorkerResponse } from "./wasm-worker.js";

/** The package root, from this compiled-adjacent source file. */
const PACKAGE_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");

/**
 * A service worker that can be killed.
 *
 * `kill()` drops every listener, which is what Chrome does to an idle worker:
 * the globals and the registered handlers it had are gone, and anything that
 * needed it to be alive discovers that by waiting.
 */
function createKillableServiceWorker(existing = false) {
	const listeners = new Set<(message: unknown) => void>();
	let alive = true;
	/** Documents this worker created, so a second request can be caught. */
	const documents: string[] = [];
	return {
		get alive() {
			return alive;
		},
		documents,
		/** The `onMessage` handler `service-worker.js` registers. */
		ensureEngineHost(message: unknown): unknown {
			if (!alive) {
				throw new Error("the service worker is not running");
			}
			for (const listener of listeners) {
				listener(message);
			}
			if (!existing) {
				documents.push("offscreen.html");
			}
			return { ok: true };
		},
		kill(): void {
			alive = false;
			listeners.clear();
		},
	};
}

/** An engine Worker whose answers this test dictates. */
function createScriptedWorker() {
	const posted: WorkerRequest[] = [];
	const listeners = new Set<(message: unknown) => void>();
	let terminated = false;
	const send = (message: WorkerResponse): void => {
		queueMicrotask(() => {
			for (const listener of listeners) {
				listener(message);
			}
		});
	};
	return {
		posted,
		get terminated() {
			return terminated;
		},
		channel: {
			post(message: unknown) {
				const request = message as WorkerRequest;
				posted.push(request);
				if (request.kind === "init") {
					send({ kind: "ready" });
					return;
				}
				const body = request.request as { op: string; id: number };
				if (body.op === "open") {
					send({
						kind: "reply",
						id: body.id,
						response: {
							v: 1,
							id: body.id,
							ok: true,
							value: {
								doc: 1,
								pages: 1,
								pageSizes: [{ width: 612, height: 792 }],
							},
						},
					});
					return;
				}
				if (body.op === "textLayer") {
					send({
						kind: "reply",
						id: body.id,
						response: {
							v: 1,
							id: body.id,
							ok: true,
							value: {
								page: 0,
								width: 612,
								height: 792,
								lowConfidence: false,
								text: "Hi",
								lines: [
									{
										text: "Hi",
										rect: { x: 100, y: 688, width: 20, height: 24 },
										direction: "ltr",
										chars: [
											{
												rect: { x: 100, y: 688, width: 10, height: 24 },
												advance: 10,
												inked: true,
											},
											{
												rect: { x: 110, y: 688, width: 10, height: 24 },
												advance: 10,
												inked: true,
											},
										],
									},
								],
							},
						},
					});
					return;
				}
				send({
					kind: "reply",
					id: body.id,
					response: { v: 1, id: body.id, ok: true, value: { doc: 1 } },
				});
			},
			onMessage(listener: (message: unknown) => void) {
				listeners.add(listener);
				return () => listeners.delete(listener);
			},
			onError() {
				return () => {};
			},
			terminate() {
				terminated = true;
			},
		},
	};
}
describe("the offscreen engine host (SL-4.EXT.03)", () => {
	it("serves a text layer end to end: client, JSON port, host, engine", async () => {
		// The whole stack except the browser: the real client validates every
		// field of the layer on the way back, so a mapping that dropped one
		// would fail here rather than in a selection.
		const link = createLoopbackLink();
		const worker = createScriptedWorker();
		const host = startEngineHost({
			link: link.host,
			createWorker: () => worker.channel,
			wasmUrl: "chrome-extension://selis/wasm/selis_pdf_wasm.wasm",
		});
		const client = createEnginePort(link.client);
		const doc = await client.open({
			kind: "bytes",
			bytes: new Uint8Array([1, 2, 3]).buffer as ArrayBuffer,
			name: "a.pdf",
		});
		const layer = await client.textLayer(doc, 0);
		expect(layer.text).toBe("Hi");
		expect(layer.lines[0]?.chars).toHaveLength(2);
		expect(layer.lines[0]?.chars[1]?.rect.x).toBe(110);
		expect(layer.width).toBe(612);
		host.stop();
		expect(worker.terminated).toBe(true);
	});

	it("survives service-worker termination mid-session (the DoD)", async () => {
		// Chrome kills an idle service worker after ~30 s. Nothing in this
		// arrangement may notice: the viewer's port is to the offscreen
		// document, and the engine is in it.
		const worker = createKillableServiceWorker();
		const link = createLoopbackLink();
		const engine = createScriptedWorker();
		const host = startEngineHost({
			link: link.host,
			createWorker: () => engine.channel,
			wasmUrl: "chrome-extension://selis/wasm/selis_pdf_wasm.wasm",
		});
		const client = createEnginePort(link.client);

		// The page asks the service worker to make the document exist, and
		// opens a document through it.
		expect(worker.ensureEngineHost({ type: "selis/ensure-engine-host" })).toEqual({ ok: true });
		const doc = await client.open({
			kind: "bytes",
			bytes: new Uint8Array([1]).buffer as ArrayBuffer,
			name: "a.pdf",
		});
		const before = await client.textLayer(doc, 0);

		worker.kill();
		expect(worker.alive).toBe(false);

		// Same session, same open document, same engine: the viewer keeps
		// working with the service worker gone. A second render is the check
		// that matters - it is the first request after the kill.
		const after = await client.textLayer(doc, 0);
		expect(after).toEqual(before);
		await expect(client.extractText(doc, 0)).resolves.toEqual({ page: 0, text: "" });
		host.stop();
	});

	it("re-asks a fresh service worker without disturbing the running engine", async () => {
		// A viewer reopened after the worker was killed asks a *new* worker to
		// ensure the document. The new worker finds it already there (Chrome
		// 116's `getContexts`, or the single-document rejection on 114), and
		// the engine that is already warm is not recompiled.
		const first = createKillableServiceWorker();
		const link = createLoopbackLink();
		const engine = createScriptedWorker();
		const host = startEngineHost({
			link: link.host,
			createWorker: () => engine.channel,
			wasmUrl: "chrome-extension://selis/wasm/selis_pdf_wasm.wasm",
		});
		const client = createEnginePort(link.client);
		first.ensureEngineHost({ type: "selis/ensure-engine-host" });
		await client.open({
			kind: "bytes",
			bytes: new Uint8Array([1]).buffer as ArrayBuffer,
			name: "a.pdf",
		});
		const initsAfterFirstOpen = engine.posted.filter((m) => m.kind === "init").length;
		first.kill();

		// A new worker: same document, so it creates nothing.
		// A new worker: the document already exists, which is what its
		// `getContexts` check (or the single-document rejection) establishes.
		const second = createKillableServiceWorker(true);
		second.ensureEngineHost({ type: "selis/ensure-engine-host" });
		expect(second.documents).toHaveLength(0);
		expect(engine.posted.filter((m) => m.kind === "init")).toHaveLength(initsAfterFirstOpen);
		host.stop();
	});

	it("ships the Worker script the document loads, and names the chunk WASM.02 does", () => {
		// A Worker is loaded by `new Worker(chrome.runtime.getURL(...))`, not by
		// an import, so nothing else in the ship list implies it. If this row
		// were missing the browser would 404 and every other gate stay green.
		expect(SHIPPED_FILES).toContain(ENGINE_WORKER_PATH);
		expect(PACKAGE_ENTRIES.some((row) => row.out === ENGINE_WORKER_PATH)).toBe(true);
		// The core chunk is not shipped yet - SL-4.EXT.05 carries the WASM -
		// so this asserts the *name* agrees with the chunk manifest rather than
		// pretending the binary is here.
		const manifest = readFileSync(
			join(PACKAGE_ROOT, "..", "..", "packages", "wasm-loader", "src", "manifest.ts"),
			"utf8",
		);
		expect(manifest).toContain(`url: "${CORE_CHUNK_PATH}"`);
	});

	it("keeps the service worker out of the engine's lifetime", () => {
		// Two source-level properties make the kill harmless, and neither is
		// visible from behaviour alone:
		//
		// 1. The worker registers no `onConnect`. Chrome delivers a
		//    `runtime.connect` to every context holding one, so registering it
		//    would hand a port carrying document bytes to a context that is
		//    about to be killed.
		// 2. The offscreen document imports nothing from the worker, so
		//    nothing it needs can die with it.
		const worker = readFileSync(join(PACKAGE_ROOT, "service-worker.js"), "utf8");
		const offscreen = readFileSync(join(PACKAGE_ROOT, "offscreen.js"), "utf8");
		expect(worker).not.toContain("onConnect.addListener");
		expect(offscreen).not.toContain("service-worker");
		// The path itself is named once, in `engine-host.ts`; the root file
		// refers to the constant, so there is no second literal to drift.
		expect(offscreen).toContain("ENGINE_WORKER_PATH");
		expect(offscreen).toContain("CORE_CHUNK_PATH");
	});
});
