import { describe, expect, it } from "vitest";
import { TOOLS_WASM_URL, decodeToolResponse, encodeToolRequest } from "./protocol.js";
import {
	OPTIMIZE_WASM_URL,
	type OptimizeWasm,
	instantiateOptimize,
	runOptimize,
} from "./optimize-protocol.js";

function u32(value: number): number[] {
	return [value & 0xff, (value >>> 8) & 0xff, (value >>> 16) & 0xff, (value >>> 24) & 0xff];
}

function block(bytes: number[]): number[] {
	return [...u32(bytes.length), ...bytes];
}

/** A canned `SLTR` success response for the fake guest below. */
function successResponse(): Uint8Array {
	const message = JSON.stringify({
		pages_out: 1,
		annotations_out: 0,
		fields_out: 0,
		ocgs_out: 0,
		outline_entries_out: 0,
		embedded_files_out: 0,
		bytes_in: 10,
		bytes_out: 8,
		verdict: "ok",
		checked: ["pages"],
		fonts_subset: 1,
		glyphs_kept: 3,
		outputs: [
			{
				name: "doc-subset.pdf",
				kind: "pdf",
				pages: 1,
				bytes: 8,
				verdict: "ok",
				overTarget: false,
			},
		],
	});
	const bytes = [
		...u32(0x52544c53),
		...u32(0),
		...block(Array.from(new TextEncoder().encode(message))),
		...u32(1),
		...block(Array.from(new TextEncoder().encode("doc-subset.pdf"))),
		...block([0x25, 0x50, 0x44, 0x46, 0x2d, 0x31, 0x2e, 0x34]),
	];
	return new Uint8Array(bytes);
}

/**
 * A guest stand-in that records the request it received and serves a canned
 * response from its own linear memory, with the same alloc/run/free contract
 * as the real optimize module.
 */
function fakeGuest(response: Uint8Array): {
	wasm: OptimizeWasm;
	requestBytes: () => Uint8Array;
} {
	const memory = new WebAssembly.Memory({ initial: 2 });
	let next = 1024;
	let lastRequest = new Uint8Array(0);
	const exports = {
		memory,
		selis_optimize_alloc(len: number): number {
			const ptr = next;
			next += len;
			return ptr;
		},
		selis_optimize_free(): void {},
		selis_optimize_run(ptr: number, len: number, outLen: number): number {
			lastRequest = new Uint8Array(memory.buffer, ptr, len).slice();
			const out = next;
			next += response.length;
			new Uint8Array(memory.buffer, out, response.length).set(response);
			new DataView(memory.buffer).setUint32(outLen, response.length, true);
			return out;
		},
		selis_optimize_chunk_id(): number {
			return 0;
		},
		selis_optimize_available(): number {
			return 1;
		},
	};
	return { wasm: { exports } as unknown as OptimizeWasm, requestBytes: () => lastRequest };
}

const file = { name: "doc.pdf", bytes: new Uint8Array([0x25, 0x50, 0x44, 0x46]) };

describe("optimise chunk protocol", () => {
	it("fetches its own same-origin module, distinct from the tools chunk", () => {
		expect(OPTIMIZE_WASM_URL).toBe("/wasm/selis_pdf_wasm_optimize.wasm");
		expect(OPTIMIZE_WASM_URL).not.toBe(TOOLS_WASM_URL);
	});

	it("keeps op codes 41 and 43 stable on the wire", () => {
		for (const [op, code] of [
			["subset_fonts", 41],
			["linearize", 43],
		] as const) {
			const encoded = encodeToolRequest({ op, files: [file] });
			expect(new DataView(encoded.buffer).getUint32(8, true)).toBe(code);
		}
	});

	it("runs a request through its own exports with the shared decoder", () => {
		const { wasm, requestBytes } = fakeGuest(successResponse());
		const result = runOptimize(wasm, { op: "subset_fonts", files: [file] });
		expect(result.verification.verdict).toBe("ok");
		expect(result.verification.fonts_subset).toBe(1);
		expect(result.outputs[0]?.name).toBe("doc-subset.pdf");
		expect(new DataView(requestBytes().buffer).getUint32(8, true)).toBe(41);
		expect(decodeToolResponse(successResponse()).outputs).toHaveLength(1);
	});

	it("rejects a guest that declares a host import", async () => {
		// magic + type section + import section (one imported function).
		const withImport = new Uint8Array([
			0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
			0x02, 0x07, 0x01, 0x01, 0x65, 0x01, 0x66, 0x00, 0x00,
		]);
		await expect(instantiateOptimize(withImport)).rejects.toThrow(/unexpected import\(s\)/);
	});
});
