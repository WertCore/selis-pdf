/**
 * Wire-level evidence for the first-class content ops (TEXTEDIT.05/06/07/08).
 *
 * The op codes are part of the guest/host contract: a drift here would send a
 * watermark request to a different engine operation. The tools chunk also
 * keeps its zero-import promise — the new ops are pure Rust with no
 * wasm-bindgen or host shim, so `instantiateTools` still refuses any module
 * that declares an import.
 */

import { describe, expect, it } from "vitest";
import {
	RESPONSE_MAGIC,
	type ToolOp,
	decodeToolResponse,
	encodeToolRequest,
	instantiateTools,
} from "./protocol.js";

const NEW_OPS: readonly (readonly [ToolOp, number])[] = [
	["add_text_box", 48],
	["add_image", 49],
	["add_shape", 50],
	["add_link", 51],
	["add_header_footer", 52],
	["add_watermark", 53],
	["add_bates", 54],
	["add_background", 55],
	["list_objects", 56],
	["list_vectors", 57],
	["move_object", 58],
	["resize_object", 59],
	["delete_object", 60],
	["replace_image", 61],
	["extract_image", 62],
];

function responseWith(kind: string, name: string, bytes: Uint8Array): Uint8Array {
	const encoder = new TextEncoder();
	const message = encoder.encode(
		JSON.stringify({
			pages_out: 0,
			annotations_out: 0,
			fields_out: 0,
			ocgs_out: 0,
			outline_entries_out: 0,
			embedded_files_out: 0,
			bytes_in: 10,
			bytes_out: bytes.length,
			verdict: "read",
			checked: [],
			outputs: [{ name, kind, pages: 0, bytes: bytes.length, verdict: "read" }],
		}),
	);
	const u32 = (value: number): number[] => [
		value & 0xff,
		(value >>> 8) & 0xff,
		(value >>> 16) & 0xff,
		(value >>> 24) & 0xff,
	];
	const block = (data: Uint8Array): number[] => [...u32(data.length), ...Array.from(data)];
	const body = [
		...u32(RESPONSE_MAGIC),
		...u32(0),
		...block(message),
		...u32(1),
		...block(encoder.encode(name)),
		...block(bytes),
	];
	return new Uint8Array(body);
}

describe("first-class content ops on the wire", () => {
	it("encodes every new op with its stable guest op code", () => {
		for (const [op, code] of NEW_OPS) {
			const request = encodeToolRequest({
				op,
				options: { page: 0 },
				files: [{ name: "doc.pdf", bytes: new Uint8Array([0x25, 0x50, 0x44, 0x46]) }],
			});
			// magic (4), version (4), then the operation number.
			expect(new DataView(request.buffer).getUint32(8, true), op).toBe(code);
		}
	});

	it("decodes a report and an image artifact the read-back ops return", () => {
		const report = decodeToolResponse(
			responseWith("report", "doc-images.json", new TextEncoder().encode("{}")),
		);
		expect(report.reports[0]?.kind).toBe("report");
		expect(report.outputs[0]?.name).toBe("doc-images.json");

		const image = decodeToolResponse(responseWith("file", "doc-image.png", new Uint8Array([1, 2])));
		expect(image.reports[0]?.kind).toBe("file");
	});

	it("still refuses a tools module that declares a host import", async () => {
		// magic + type section + import section (one imported function).
		const withImport = new Uint8Array([
			0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x02,
			0x07, 0x01, 0x01, 0x65, 0x01, 0x66, 0x00, 0x00,
		]);
		await expect(instantiateTools(withImport)).rejects.toThrow(/unexpected import\(s\)/);
	});
});
