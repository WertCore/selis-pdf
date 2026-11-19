import { describe, expect, it } from "vitest";
import {
	COMPARE_WASM_URL,
	type CompareWasm,
	instantiateCompare,
	runCompare,
} from "./compare-protocol.js";
import { TOOLS_WASM_URL, decodeToolResponse, encodeToolRequest } from "./protocol.js";

function u32(value: number): number[] {
	return [value & 0xff, (value >>> 8) & 0xff, (value >>> 16) & 0xff, (value >>> 24) & 0xff];
}

function block(bytes: number[]): number[] {
	return [...u32(bytes.length), ...bytes];
}

/** A canned `SLTR` success response for the fake guest below. */
function successResponse(): Uint8Array {
	const report = JSON.stringify({
		schema: "selis-compare/1",
		status: "ok",
		pages_a: 1,
		pages_b: 1,
		identical: true,
		text_identical: true,
		pages: [{ page: 1, present_in: "both", pixels_identical: true, diff_fraction: 0 }],
	});
	const message = JSON.stringify({
		pages_out: 0,
		annotations_out: 0,
		fields_out: 0,
		ocgs_out: 0,
		outline_entries_out: 0,
		embedded_files_out: 0,
		bytes_in: 16,
		bytes_out: report.length,
		verdict: "read",
		checked: [],
		outputs: [
			{
				name: "a-compare.json",
				kind: "report",
				pages: 0,
				bytes: report.length,
				verdict: "read",
			},
		],
	});
	const bytes = [
		...u32(0x52544c53),
		...u32(0),
		...block(Array.from(new TextEncoder().encode(message))),
		...u32(1),
		...block(Array.from(new TextEncoder().encode("a-compare.json"))),
		...block(Array.from(new TextEncoder().encode(report))),
	];
	return new Uint8Array(bytes);
}

/**
 * A guest stand-in that records the request it received and serves a canned
 * response from its own linear memory, with the same alloc/run/free contract
 * as the real compare module.
 */
function fakeGuest(response: Uint8Array): {
	wasm: CompareWasm;
	requestBytes: () => Uint8Array;
} {
	const memory = new WebAssembly.Memory({ initial: 2 });
	let next = 1024;
	let lastRequest = new Uint8Array(0);
	const exports = {
		memory,
		selis_compare_alloc(len: number): number {
			const ptr = next;
			next += len;
			return ptr;
		},
		selis_compare_free(): void {},
		selis_compare_run(ptr: number, len: number, outLen: number): number {
			lastRequest = new Uint8Array(memory.buffer, ptr, len).slice();
			const out = next;
			next += response.length;
			new Uint8Array(memory.buffer, out, response.length).set(response);
			new DataView(memory.buffer).setUint32(outLen, response.length, true);
			return out;
		},
		selis_compare_chunk_id(): number {
			return 0;
		},
		selis_compare_available(): number {
			return 1;
		},
	};
	return { wasm: { exports } as unknown as CompareWasm, requestBytes: () => lastRequest };
}

const a = { name: "a.pdf", bytes: new Uint8Array([0x25, 0x50, 0x44, 0x46]) };
const b = { name: "b.pdf", bytes: new Uint8Array([0x25, 0x50, 0x44, 0x46]) };

describe("compare chunk protocol", () => {
	it("fetches its own same-origin module, distinct from the tools chunk", () => {
		expect(COMPARE_WASM_URL).toBe("/wasm/selis_pdf_wasm_compare.wasm");
		expect(COMPARE_WASM_URL).not.toBe(TOOLS_WASM_URL);
	});

	it("keeps op 46 stable on the wire and carries two inputs and the dpi option", () => {
		const encoded = encodeToolRequest({
			op: "compare",
			options: { dpi: 72 },
			files: [a, b],
		});
		const view = new DataView(encoded.buffer);
		expect(view.getUint32(8, true)).toBe(46);
		const text = new TextDecoder().decode(encoded);
		expect(text).toContain('"dpi":72');
		expect(text).toContain("a.pdf");
		expect(text).toContain("b.pdf");
	});

	it("runs a two-file request through its own exports with the shared decoder", () => {
		const { wasm, requestBytes } = fakeGuest(successResponse());
		const result = runCompare(wasm, { op: "compare", files: [a, b] });
		expect(result.verification.verdict).toBe("read");
		expect(result.outputs[0]?.name).toBe("a-compare.json");
		const report = JSON.parse(
			new TextDecoder().decode(new Uint8Array(result.outputs[0]?.bytes ?? [])),
		);
		expect(report.schema).toBe("selis-compare/1");
		expect(new DataView(requestBytes().buffer).getUint32(8, true)).toBe(46);
		expect(decodeToolResponse(successResponse()).outputs).toHaveLength(1);
	});

	it("rejects a guest that declares a host import", async () => {
		// magic + type section + import section (one imported function).
		const withImport = new Uint8Array([
			0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x02,
			0x07, 0x01, 0x01, 0x65, 0x01, 0x66, 0x00, 0x00,
		]);
		await expect(instantiateCompare(withImport)).rejects.toThrow(/unexpected import\(s\)/);
	});
});
