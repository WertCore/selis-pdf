/**
 * The client for the dedicated content chunk (TEXTEDIT.05/06/07/08).
 *
 * It reuses the tools wire codec but must keep two promises on its own: the
 * chunk declares no imports (no new JS capability, ADR-P0011), and every guest
 * buffer is freed on every exit path (the no-upload editor never leaks memory
 * into a long-lived tab).
 */

import { describe, expect, it, vi } from "vitest";
import { CONTENT_OPS } from "./operations.js";
import {
	CONTENT_WASM_URL,
	instantiateContent,
	loadContent,
	runContent,
	type ContentWasm,
} from "./content-protocol.js";
import { RESPONSE_MAGIC } from "../tools/protocol.js";

function u32(value: number): number[] {
	return [value & 0xff, (value >>> 8) & 0xff, (value >>> 16) & 0xff, (value >>> 24) & 0xff];
}

function block(bytes: number[]): number[] {
	return [...u32(bytes.length), ...bytes];
}

function successResponse(name: string, bytes: number[]): Uint8Array {
	const message = new TextEncoder().encode(
		JSON.stringify({
			pages_out: 1,
			annotations_out: 0,
			fields_out: 0,
			ocgs_out: 0,
			outline_entries_out: 0,
			embedded_files_out: 0,
			bytes_in: 10,
			bytes_out: bytes.length,
			verdict: "ok",
			checked: ["pages"],
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
function fakeWasm(response: Uint8Array): { wasm: ContentWasm; freed: number[][] } {
	const memory = new WebAssembly.Memory({ initial: 2 });
	const freed: number[][] = [];
	let next = 8;
	const wasm: ContentWasm = {
		exports: {
			memory,
			selis_content_alloc(len: number): number {
				const ptr = next;
				next += Math.ceil(len / 8) * 8;
				return ptr;
			},
			selis_content_free(ptr: number, len: number): void {
				freed.push([ptr, len]);
			},
			selis_content_run(): number {
				const ptr = 4096;
				new Uint8Array(memory.buffer, ptr, response.length).set(response);
				// The out-word was allocated last, at the 4-byte slot before
				// `next`; encode its address as the run call would.
				return ptr;
			},
			selis_content_capabilities: () => 15,
			selis_content_chunk_id: () => 0,
		},
	};
	return { wasm, freed };
}

describe("the content-chunk client", () => {
	it("routes exactly the first-class ops to the content module", () => {
		expect(CONTENT_OPS.size).toBe(15);
		for (const op of [
			"add_text_box",
			"add_image",
			"add_shape",
			"add_link",
			"add_header_footer",
			"add_watermark",
			"add_bates",
			"add_background",
			"list_objects",
			"list_vectors",
			"move_object",
			"resize_object",
			"delete_object",
			"replace_image",
			"extract_image",
		]) {
			expect(CONTENT_OPS.has(op), op).toBe(true);
		}
		expect(CONTENT_OPS.has("rotate")).toBe(false);
		expect(CONTENT_WASM_URL).toBe("/wasm/selis_pdf_wasm_content.wasm");
	});

	it("decodes a run and frees every guest buffer it allocated", () => {
		const response = successResponse("doc-text-box.pdf", [1, 2, 3, 4]);
		const { wasm, freed } = fakeWasm(response);
		// The guest writes its response at 4096 and the response length into the
		// out-word slot the client handed it, mimicking the real ABI.
		wasm.exports.selis_content_run = (_ptr: number, _len: number, out: number): number => {
			new Uint8Array(wasm.exports.memory.buffer, 4096, response.length).set(response);
			new DataView(wasm.exports.memory.buffer).setUint32(out, response.length, true);
			return 4096;
		};
		const result = runContent(wasm, {
			op: "add_text_box",
			options: { page: 0 },
			files: [{ name: "doc.pdf", bytes: new Uint8Array([37, 80, 68, 70]) }],
		});
		expect(result.outputs[0]?.name).toBe("doc-text-box.pdf");
		// Request, out-word and response: all three guest buffers are freed.
		expect(freed.length).toBe(3);
	});

	it("refuses a guest that declares a host import", async () => {
		const withImport = new Uint8Array([
			0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x02,
			0x07, 0x01, 0x01, 0x65, 0x01, 0x66, 0x00, 0x00,
		]);
		await expect(instantiateContent(withImport)).rejects.toThrow(/unexpected import\(s\)/);
	});

	it("surfaces a failed fetch as a typed error", async () => {
		const failingFetch = vi.fn().mockResolvedValue({ ok: false, status: 404 });
		await expect(loadContent(failingFetch as unknown as typeof fetch)).rejects.toThrow(
			/unavailable/,
		);
	});
});
