/**
 * SL-4.EXT.03 - the `EnginePort` over the engine Worker.
 *
 * The engine here is a fake Worker channel, not a WASM guest. That is the
 * point: what can go wrong in this module is the *mapping* - a field renamed,
 * a rectangle dropped, a handle stringified wrongly, a cancellation that never
 * arrives - and every one of those is observable without a browser or a
 * multi-megabyte binary. The guest ABI itself is covered separately in
 * `wasm-guest.test.ts`, and the real guest is covered by `cargo xtask
 * wasm-protocol`.
 *
 * What the shared contract suite cannot see, and what is asserted here:
 * the tile rectangle's page-space-to-canvas flip, the engine's own text-layer
 * invariant re-checked at the boundary, `wholeWord` being refused rather than
 * silently ignored, and the engine failing to load at all.
 */

import { describe, expect, it } from "vitest";
import { AdapterError, ErrorCode } from "../../../ui/src/platform/errors.js";
import type { EngineChannel } from "./wasm-engine.js";
import { createWasmEngine } from "./wasm-engine.js";
import type { WorkerRequest, WorkerResponse } from "./wasm-worker.js";

/** One scripted answer from the fake guest: its reply, plus any attachment. */
interface Scripted {
	readonly response: unknown;
	readonly attachment?: Uint8Array;
}

/** A fake Worker: records what was posted, answers from a script. */
function createFakeWorker(script: {
	reply(id: number, request: Record<string, unknown>, payload: Uint8Array | undefined): Scripted | null;
	initFails?: string;
}) {
	const posted: WorkerRequest[] = [];
	const listeners = new Set<(message: unknown) => void>();
	let ready = false;
	const send = (message: WorkerResponse): void => {
		queueMicrotask(() => {
			for (const listener of listeners) {
				listener(message);
			}
		});
	};
	const channel: EngineChannel & { fail(error: Error): void } = {
		post(message) {
			const request = message as WorkerRequest;
			posted.push(request);
			if (request.kind === "init") {
				queueMicrotask(() => {
					if (script.initFails === undefined) {
						ready = true;
						send({ kind: "ready" });
					} else {
						send({ kind: "init-error", message: script.initFails });
					}
				});
				return;
			}
			const payload =
				request.payload === undefined ? undefined : new Uint8Array(request.payload);
			const body = request.request as Record<string, unknown>;
			if (ready) {
				const answer = script.reply(request.id, body, payload);
			if (answer === null) {
				// A guest that has not answered yet: the request stays in
				// flight, which is the only state in which cancellation is
				// observable.
				return;
			}
				send({
					kind: "reply",
					id: request.id,
					response: answer.response,
					...(answer.attachment === undefined
						? {}
						: { attachment: answer.attachment.buffer as ArrayBuffer }),
				});
			} else {
				send({ kind: "fault", id: request.id, message: "not ready" });
			}
		},
		onMessage(listener) {
			listeners.add(listener);
			return () => listeners.delete(listener);
		},
		onError() {
			return () => {};
		},
		terminate() {},
		fail(error) {
			for (const listener of listeners) {
				listener(error);
			}
		},
	};
	return { channel, posted };
}

/** The `open` answer the scripted guest gives: one page, LETTER at 72 dpi. */
function openReply(id: number): unknown {
	return {
		v: 1,
		id,
		ok: true,
		value: {
			doc: 7,
			pages: 1,
			pageSizes: [{ width: 612, height: 792 }],
		},
	};
}

/** A text layer for the word "Hi", quads one code unit each. */
function layerValue(): unknown {
	return {
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
					{ rect: { x: 100, y: 688, width: 10, height: 24 }, advance: 10, inked: true },
					{ rect: { x: 110, y: 688, width: 10, height: 24 }, advance: 10, inked: true },
				],
			},
		],
	};
}

/** The op a posted message carries, or `""` for an `init`. */
function opOf(message: WorkerRequest): string {
	return (message as { request?: { op?: string } }).request?.op ?? "";
}

/** The attachment a posted request carried, if any. */
function payloadOf(message: WorkerRequest): ArrayBuffer | undefined {
	return (message as { payload?: ArrayBuffer }).payload;
}

/** The JSON body a posted request carried. */
function bodyOf(message: WorkerRequest): unknown {
	return (message as { request?: unknown }).request;
}

const WASM_URL = "chrome-extension://selis/wasm/selis_pdf_wasm.wasm";
describe("the engine over a Worker (SL-4.EXT.03)", () => {
	it("sends the document as the request attachment, not as JSON", async () => {
		const { channel, posted } = createFakeWorker({
			reply: (id) => ({ response: openReply(id) }),
		});
		const engine = createWasmEngine({ channel, wasmUrl: WASM_URL });
		const bytes = new Uint8Array([37, 80, 68, 70]);
		const doc = await engine.open({
			kind: "bytes",
			bytes: bytes.buffer as ArrayBuffer,
			name: "a.pdf",
		});
		expect(doc).toEqual({
			id: "7",
			pageCount: 1,
			pageSizes: [{ width: 612, height: 792 }],
		});
		const open = posted.find((m) => opOf(m) === "open");
		expect(payloadOf(open as WorkerRequest)).toBeDefined();
		expect(Array.from(new Uint8Array(payloadOf(open as WorkerRequest) as ArrayBuffer))).toEqual([
			37, 80, 68, 70,
		]);
		// The wire never carries the bytes twice: the JSON body declares the
		// length and the attachment carries the bytes.
		expect(JSON.stringify(bodyOf(open as WorkerRequest))).not.toContain("PDF");
	});

	it("refuses a source that is not inline bytes", async () => {
		const { channel } = createFakeWorker({ reply: (id) => ({ response: openReply(id) }) });
		const engine = createWasmEngine({ channel, wasmUrl: WASM_URL });
		await expect(
			engine.open({ kind: "opfs", path: "/doc.pdf" }),
		).rejects.toMatchObject({ code: ErrorCode.BindingBadArgument });
	});

	it("converts scale to dpi and flips a page-space rect into canvas coordinates", async () => {
		// A rect in page space is y-up from the MediaBox's bottom-left; the
		// guest's tile indexes a raster top-down. Getting this wrong renders
		// the right pixels from the wrong part of the page, silently.
		const { channel, posted } = createFakeWorker({
			reply: (id) => ({ response: openReply(id) }),
		});
		const engine = createWasmEngine({ channel, wasmUrl: WASM_URL });
		const doc = await engine.open({
			kind: "bytes",
			bytes: new ArrayBuffer(4),
			name: "a.pdf",
		});
		await engine
			.renderTile({
				doc,
				page: 0,
				scale: 2,
				rect: { x: 10, y: 100, width: 50, height: 20 },
			})
			.catch(() => undefined);
		const render = posted.find((m) => opOf(m) === "render");
		const params = (bodyOf(render as WorkerRequest) as { params: Record<string, unknown> }).params;
		expect(params.dpi).toBe(144);
		// (792 - 100 - 20) * 2 = 1344: the same 20pt band, measured from the top.
		expect(params.tile).toEqual({ x: 20, y: 1344, w: 100, h: 40 });
	});

	it("returns a page text layer with its quads and its text", async () => {
		const { channel } = createFakeWorker({
			reply: (id, request) => ({
				response:
					(request.op as string) === "open"
						? openReply(id)
						: { v: 1, id, ok: true, value: layerValue() },
			}),
		});
		const engine = createWasmEngine({ channel, wasmUrl: WASM_URL });
		const doc = await engine.open({
			kind: "bytes",
			bytes: new ArrayBuffer(4),
			name: "a.pdf",
		});
		const layer = await engine.textLayer(doc, 0);
		expect(layer.text).toBe("Hi");
		expect(layer.lines[0]?.chars).toHaveLength(2);
		expect(layer.lines[0]?.chars[0]?.advance).toBe(10);
		expect(layer.lines[0]?.rect.width).toBe(20);
	});

	it("refuses a layer whose characters do not line up with its text", async () => {
		// The engine guarantees index alignment; a shell that trusted the
		// shape would let a selection and a copied string disagree with no
		// error anywhere.
		const broken = layerValue() as { lines: { chars: unknown[] }[] };
		broken.lines[0]?.chars.pop();
		const { channel } = createFakeWorker({
			reply: (id, request) => ({
				response:
					(request.op as string) === "open"
						? openReply(id)
						: { v: 1, id, ok: true, value: broken },
			}),
		});
		const engine = createWasmEngine({ channel, wasmUrl: WASM_URL });
		const doc = await engine.open({
			kind: "bytes",
			bytes: new ArrayBuffer(4),
			name: "a.pdf",
		});
		await expect(engine.textLayer(doc, 0)).rejects.toMatchObject({
			code: ErrorCode.BindingBadArgument,
		});
	});

	it("refuses whole-word search rather than answering a different question", async () => {
		const { channel } = createFakeWorker({
			reply: (id, request) => ({
				response:
					(request.op as string) === "open"
						? openReply(id)
						: { v: 1, id, ok: true, value: { matches: [] } },
			}),
		});
		const engine = createWasmEngine({ channel, wasmUrl: WASM_URL });
		const doc = await engine.open({
			kind: "bytes",
			bytes: new ArrayBuffer(4),
			name: "a.pdf",
		});
		const iterate = async (): Promise<void> => {
			for await (const _batch of engine.search(doc, "x", { wholeWord: true })) {
				// The refusal happens before the first batch.
			}
		};
		await expect(iterate()).rejects.toMatchObject({ code: ErrorCode.BindingBadArgument });
	});

	it("slices a search into batches and ends with done", async () => {
		const matches = Array.from({ length: 130 }, (_unused, at) => ({
			page: 0,
			start: at,
			end: at + 1,
		}));
		const { channel } = createFakeWorker({
			reply: (id, request) => ({
				response:
					(request.op as string) === "open"
						? openReply(id)
						: { v: 1, id, ok: true, value: { matches } },
			}),
		});
		const engine = createWasmEngine({ channel, wasmUrl: WASM_URL });
		const doc = await engine.open({
			kind: "bytes",
			bytes: new ArrayBuffer(4),
			name: "a.pdf",
		});
		const batches = [];
		for await (const batch of engine.search(doc, "x")) {
			batches.push(batch);
		}
		expect(batches.map((b) => b.matches.length)).toEqual([64, 64, 2, 0]);
		expect(batches.at(-1)?.done).toBe(true);
	});

	it("cancels promptly: the caller is released and the guest is told", async () => {
		const { channel, posted } = createFakeWorker({
			reply: (id, request) => {
				if ((request.op as string) === "open") {
					return { response: openReply(id) };
				}
				// A render that never answers, so only cancellation can end it.
				return null;
			},
		});
		const engine = createWasmEngine({ channel, wasmUrl: WASM_URL });
		const doc = await engine.open({
			kind: "bytes",
			bytes: new ArrayBuffer(4),
			name: "a.pdf",
		});
		const controller = new AbortController();
		const pending = engine.renderTile({ doc, page: 0, scale: 1 }, { signal: controller.signal });
		// The request has to be in flight before the abort, or there is
		// nothing in the guest to cancel and nothing to assert.
		await new Promise((resolve) => setTimeout(resolve, 0));
		controller.abort();
		await expect(pending).rejects.toMatchObject({ code: ErrorCode.Cancelled });
		const cancel = posted.find((m) => opOf(m) === "cancel");
		expect(cancel).toBeDefined();
	});

	it("surfaces a registry code from the engine, not a bare string", async () => {
		const { channel } = createFakeWorker({
			reply: (id, request) => ({
				response:
					(request.op as string) === "open"
						? openReply(id)
						: {
								v: 1,
								id,
								ok: false,
								code: 1301,
								message: "page 9 does not exist",
								docState: "Loaded",
							},
			}),
		});
		const engine = createWasmEngine({ channel, wasmUrl: WASM_URL });
		const doc = await engine.open({
			kind: "bytes",
			bytes: new ArrayBuffer(4),
			name: "a.pdf",
		});
		await expect(engine.extractText(doc, 9)).rejects.toMatchObject({
			code: 1301,
			docState: "Loaded",
		});
	});

	it("reports a missing engine once, and every later request with the same reason", async () => {
		const { channel, posted } = createFakeWorker({
			reply: () => ({ response: {} }),
			initFails: "the engine at chrome-extension://selis/wasm/selis_pdf_wasm.wasm did not load: 404",
		});
		const engine = createWasmEngine({ channel, wasmUrl: WASM_URL });
		const first = engine.open({ kind: "bytes", bytes: new ArrayBuffer(4), name: "a.pdf" });
		await expect(first).rejects.toMatchObject({ docState: "NotLoaded" });
		await expect(
			engine.open({ kind: "bytes", bytes: new ArrayBuffer(4), name: "a.pdf" }),
		).rejects.toBeInstanceOf(AdapterError);
		// One compile attempt, not one per request: the same bytes fail the
		// same way, and retrying would only multiply the failure.
		expect(posted.filter((m) => m.kind === "init")).toHaveLength(1);
	});
});