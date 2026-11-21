/**
 * The client for the dedicated annotation chunk (SL-5.ANNOT.02/03).
 *
 * It reuses the tools wire codec but must keep three promises on its own: the
 * chunk declares no imports (no new JS capability, ADR-P0011), every guest
 * buffer is freed on every exit path, and the typed `annotation` report is read
 * defensively — a rebuilt count the guest did not send must be a typed error,
 * never a status line that claims work was done.
 */

import { describe, expect, it, vi } from "vitest";
import { ANNOT_OPS } from "./operations.js";
import {
	ANNOT_WASM_URL,
	instantiateAnnot,
	loadAnnot,
	readAnnotReport,
	runAnnot,
	type AnnotWasm,
} from "./annot-protocol.js";
import { RESPONSE_MAGIC, encodeToolRequest, type Verification } from "../tools/protocol.js";

function u32(value: number): number[] {
	return [value & 0xff, (value >>> 8) & 0xff, (value >>> 16) & 0xff, (value >>> 24) & 0xff];
}

function block(bytes: number[]): number[] {
	return [...u32(bytes.length), ...bytes];
}

function successResponse(name: string, bytes: number[], annotation: object): Uint8Array {
	const message = new TextEncoder().encode(
		JSON.stringify({
			pages_out: 1,
			annotations_out: 1,
			fields_out: 0,
			ocgs_out: 0,
			outline_entries_out: 0,
			embedded_files_out: 0,
			bytes_in: 10,
			bytes_out: bytes.length,
			verdict: "ok",
			checked: ["pages"],
			annotation,
			outputs: [{ name, kind: "pdf", pages: 1, bytes: bytes.length, verdict: "ok" }],
		}),
	);
	return new Uint8Array([
		...u32(RESPONSE_MAGIC),
		...u32(0),
		...block(Array.from(message)),
		...u32(1),
		...block(Array.from(new TextEncoder().encode(name))),
		...block(bytes),
	]);
}

/** A fake guest that owns one 64 KiB linear memory and records every free. */
function fakeWasm(): { wasm: AnnotWasm; freed: number[][] } {
	const memory = new WebAssembly.Memory({ initial: 2 });
	const freed: number[][] = [];
	let next = 8;
	const wasm: AnnotWasm = {
		exports: {
			memory,
			selis_annot_alloc(len: number): number {
				const ptr = next;
				next += Math.ceil(len / 8) * 8;
				return ptr;
			},
			selis_annot_free(ptr: number, len: number): void {
				freed.push([ptr, len]);
			},
			selis_annot_run(): number {
				return 4096;
			},
			selis_annot_capabilities: () => 2,
			selis_annot_chunk_id: () => 0,
		},
	};
	return { wasm, freed };
}

describe("the annotation-chunk client", () => {
	it("routes exactly the two deferred ops to the annotation module", () => {
		expect(ANNOT_OPS.size).toBe(2);
		expect(ANNOT_OPS.has("annotate_appearances")).toBe(true);
		expect(ANNOT_OPS.has("annot_ink")).toBe(true);
		expect(ANNOT_OPS.has("annotate")).toBe(false);
		expect(ANNOT_WASM_URL).toBe("/wasm/selis_pdf_wasm_annot.wasm");
		// The codes continue the tools table after the content chunk's 48..=62.
		const file = { name: "doc.pdf", bytes: new Uint8Array([37, 80, 68, 70]) };
		expect(encodeToolRequest({ op: "annotate_appearances", files: [file] })[8]).toBe(63);
		expect(encodeToolRequest({ op: "annot_ink", files: [file] })[8]).toBe(64);
	});

	it("decodes a rebuild run, attaches the typed report and frees every buffer", () => {
		const response = successResponse("document-appearances.pdf", [1, 2, 3, 4], {
			kind: "appearances",
			pages: 1,
			annotations: 2,
			rebuilt: 1,
			need_appearances: true,
		});
		const { wasm, freed } = fakeWasm();
		wasm.exports.selis_annot_run = (_ptr: number, _len: number, out: number): number => {
			new Uint8Array(wasm.exports.memory.buffer, 4096, response.length).set(response);
			new DataView(wasm.exports.memory.buffer).setUint32(out, response.length, true);
			return 4096;
		};
		const result = runAnnot(wasm, {
			op: "annotate_appearances",
			options: {},
			files: [{ name: "doc.pdf", bytes: new Uint8Array([37, 80, 68, 70]) }],
		});
		expect(result.outputs[0]?.name).toBe("document-appearances.pdf");
		expect(result.report).toEqual({
			kind: "appearances",
			pages: 1,
			annotations: 2,
			rebuilt: 1,
			need_appearances: true,
		});
		// Request, out-word and response: all three guest buffers are freed.
		expect(freed.length).toBe(3);
	});

	it("reads both report shapes and refuses a malformed one", () => {
		const verification = {
			pages_out: 1,
			annotations_out: 1,
			fields_out: 0,
			ocgs_out: 0,
			outline_entries_out: 0,
			embedded_files_out: 0,
			bytes_in: 0,
			bytes_out: 0,
			verdict: "ok",
			checked: [],
			annotation: { kind: "ink", page: 3, strokes: 4, annots: 1 },
		} as unknown as Verification;
		expect(readAnnotReport(verification)).toEqual({
			kind: "ink",
			page: 3,
			strokes: 4,
			annots: 1,
		});
		const broken = {
			...verification,
			annotation: { kind: "appearances", rebuilt: 2, need_appearances: true },
		} as unknown as Verification;
		expect(() => readAnnotReport(broken)).toThrow(/malformed page count/);
		expect(() =>
			readAnnotReport({ ...verification, annotation: undefined } as unknown as Verification),
		).toThrow(/no report/);
	});

	it("refuses a guest that declares a host import", async () => {
		const withImport = new Uint8Array([
			0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x02,
			0x07, 0x01, 0x01, 0x65, 0x01, 0x66, 0x00, 0x00,
		]);
		await expect(instantiateAnnot(withImport)).rejects.toThrow(/unexpected import\(s\)/);
	});

	it("surfaces a failed fetch as a typed error", async () => {
		const failingFetch = vi.fn().mockResolvedValue({ ok: false, status: 404 });
		await expect(loadAnnot(failingFetch as unknown as typeof fetch)).rejects.toThrow(
			/unavailable/,
		);
	});
});
