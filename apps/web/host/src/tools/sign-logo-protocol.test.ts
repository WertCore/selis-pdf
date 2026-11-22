import { describe, expect, it, vi } from "vitest";
import { ToolProtocolError, type Verification } from "./protocol.js";
import {
	MAX_SIGN_LOGO_NAME_BYTES,
	SIGN_LOGO_PROTOCOL_VERSION,
	SIGN_LOGO_REQUEST_MAGIC,
	SIGN_LOGO_RESPONSE_MAGIC,
	SIGN_LOGO_WASM_URL,
	decodeSignLogoResponse,
	encodeSignLogoRequest,
	instantiateSignLogo,
	loadSignLogo,
	readLogoBytes,
	runSignLogo,
	selectedLogoFile,
	signLogoRows,
} from "./sign-logo-protocol.js";
import type { SignLogoWasm } from "./sign-logo-protocol.js";

function u32(value: number): number[] {
	return [value & 0xff, (value >>> 8) & 0xff, (value >>> 16) & 0xff, (value >>> 24) & 0xff];
}
function text(value: string): number[] {
	return Array.from(new TextEncoder().encode(value));
}
function block(bytes: number[]): number[] {
	return [...u32(bytes.length), ...bytes];
}
function readU32(bytes: Uint8Array, offset: number): number {
	return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(offset, true);
}

const verification: Verification = {
	pages_in: 1,
	pages_out: 1,
	annotations_out: 0,
	fields_out: 1,
	ocgs_out: 0,
	outline_entries_out: 0,
	embedded_files_out: 0,
	bytes_in: 100,
	bytes_out: 200,
	verdict: "ok",
	checked: ["pages"],
};

const pdf = new Uint8Array([0x25, 0x50, 0x44, 0x46]);
const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a]);
const file = { name: "in.pdf", bytes: pdf };
const logo = { name: "logo.png", bytes: png };

function signRequest(options: Record<string, unknown> = { page: 1 }) {
	return { op: "sign-logo" as const, options, file, second: logo };
}

function documentResponse(message: Record<string, unknown>, bytes = pdf): Uint8Array {
	const outputs = [{ name: "document-signed-logo.pdf", kind: "pdf", pages: 1, bytes: bytes.length, verdict: "ok" }];
	return new Uint8Array([
		...u32(SIGN_LOGO_RESPONSE_MAGIC),
		...u32(0),
		...block(text(JSON.stringify({ ...verification, operation: "sign-logo", field: "Signature1", page: 1, subFilter: "adbe.pkcs7.detached", ...message, outputs }))),
		...u32(1),
		...block(text("document-signed-logo.pdf")),
		...block(Array.from(bytes)),
	]);
}

describe("encodeSignLogoRequest", () => {
	it("writes the documented little-endian layout with exactly two files", () => {
		const encoded = encodeSignLogoRequest(signRequest({ page: 2 }));
		expect(readU32(encoded, 0)).toBe(SIGN_LOGO_REQUEST_MAGIC);
		expect(readU32(encoded, 4)).toBe(SIGN_LOGO_PROTOCOL_VERSION);
		expect(readU32(encoded, 8)).toBe(1);
		const optionsLength = readU32(encoded, 12);
		expect(new TextDecoder().decode(encoded.slice(16, 16 + optionsLength))).toBe(
			JSON.stringify({ page: 2 }),
		);
		let pos = 16 + optionsLength;
		expect(readU32(encoded, pos)).toBe(2);
		pos += 4;
		for (const expected of [file, logo]) {
			const nameLength = readU32(encoded, pos);
			pos += 4;
			expect(new TextDecoder().decode(encoded.slice(pos, pos + nameLength))).toBe(expected.name);
			pos += nameLength;
			const dataLength = readU32(encoded, pos);
			pos += 4;
			expect(Array.from(encoded.slice(pos, pos + dataLength))).toEqual(Array.from(expected.bytes));
			pos += dataLength;
		}
		expect(pos).toBe(encoded.byteLength);
	});

	it("numbers the two operations 1 and 2", () => {
		const base = { file, second: logo };
		expect(readU32(encodeSignLogoRequest({ ...base, op: "sign-logo" }), 8)).toBe(1);
		expect(
			readU32(encodeSignLogoRequest({ ...base, op: "verify-appearance", options: { fieldName: "Signature1" } }), 8),
		).toBe(2);
	});

	it("refuses an empty logo before the chunk is fetched", () => {
		expect(() => encodeSignLogoRequest({ ...signRequest(), second: { name: "logo.png", bytes: new Uint8Array(0) } })).toThrow(
			ToolProtocolError,
		);
	});

	it("refuses an oversized input name", () => {
		expect(() =>
			encodeSignLogoRequest({
				...signRequest(),
				second: { name: "x".repeat(MAX_SIGN_LOGO_NAME_BYTES + 1), bytes: png },
			}),
		).toThrow(ToolProtocolError);
	});
});

describe("decodeSignLogoResponse", () => {
	it("decodes a signed document with an integrity-checked output", () => {
		const result = decodeSignLogoResponse(documentResponse({}));
		expect(result.kind).toBe("document");
		if (result.kind !== "document") return;
		expect(result.output.bytes).toEqual(pdf);
		expect(result.report.bytes).toBe(pdf.length);
		expect(result.outcome.field).toBe("Signature1");
		expect(result.outcome.subFilter).toBe("adbe.pkcs7.detached");
	});

	it("decodes a read-back result without an output document", () => {
		const response = new Uint8Array([
			...u32(SIGN_LOGO_RESPONSE_MAGIC),
			...u32(0),
			...block(text(JSON.stringify({ operation: "verify-appearance", field: "Signature1", verified: true, hasLogo: true, hasSMask: true, appearanceBytes: 412 }))),
			...u32(0),
		]);
		expect(decodeSignLogoResponse(response)).toEqual({
			kind: "appearance-verification",
			field: "Signature1",
			hasLogo: true,
			hasSMask: true,
			appearanceBytes: 412,
		});
	});

	it("surfaces the guest's refusal text rather than replacing it", () => {
		const response = new Uint8Array([
			...u32(SIGN_LOGO_RESPONSE_MAGIC),
			...u32(1),
			...block(text("the logo is not a PNG or JPEG this build can decode")),
			...u32(0),
		]);
		expect(() => decodeSignLogoResponse(response)).toThrow(
			"the logo is not a PNG or JPEG this build can decode",
		);
	});

	it("rejects a chunk that claims a byte count it did not write", () => {
		const outputs = [{ name: "document-signed-logo.pdf", kind: "pdf", pages: 1, bytes: 9999, verdict: "ok" }];
		const body = new Uint8Array([
			...u32(SIGN_LOGO_RESPONSE_MAGIC),
			...u32(0),
			...block(text(JSON.stringify({ ...verification, operation: "sign-logo", field: "Signature1", outputs }))),
			...u32(1),
			...block(text("document-signed-logo.pdf")),
			...block(Array.from(pdf)),
		]);
		expect(() => decodeSignLogoResponse(body)).toThrow(/malformed output metadata/);
	});
});

describe("instantiateSignLogo / loadSignLogo", () => {
	it("rejects a module that declares a host import (ADR-P0011)", async () => {
		// (module (type (func)) (import "env" "clock" (func (type 0))))
		const withImport = new Uint8Array([
			0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
			0x01, 0x04, 0x01, 0x60, 0x00, 0x00,
			0x02, 0x0d, 0x01, 0x03, 0x65, 0x6e, 0x76, 0x05, 0x63, 0x6c, 0x6f, 0x63, 0x6b, 0x00, 0x00,
		]);
		await expect(instantiateSignLogo(withImport)).rejects.toThrow(/unexpected import/);
	});

	it("fetches the dedicated chunk from the same origin", async () => {
		const bytes = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
		const fetchImpl = vi.fn(async () => new Response(bytes) as Response);
		await expect(loadSignLogo(fetchImpl as unknown as typeof fetch)).resolves.toBeDefined();
		expect(fetchImpl).toHaveBeenCalledWith(SIGN_LOGO_WASM_URL, { credentials: "same-origin" });
	});
});

describe("runSignLogo", () => {
	it("zeroes the request buffer (the key travels in it) and frees every allocation", () => {
		const memoryBuffer = new ArrayBuffer(1024);
		const written: Uint8Array[] = [];
		const freed: Array<[number, number]> = [];
		let response = documentResponse({});
		const exports = {
			memory: new WebAssembly.Memory({ initial: 1 }),
			selis_sign_logo_alloc: (len: number) => (len === 4 ? 128 : 16),
			selis_sign_logo_free: (ptr: number, len: number) => freed.push([ptr, len]),
			selis_sign_logo_run: (ptr: number, len: number, outLen: number) => {
				const view = new Uint8Array(exports.memory.buffer);
				written.push(view.slice(ptr, ptr + len));
				view.set(response, 256);
				new DataView(exports.memory.buffer).setUint32(outLen, response.length, true);
				return 256;
			},
		};
		void memoryBuffer;
		const wasm = { exports } as unknown as SignLogoWasm;
		const result = runSignLogo(wasm, signRequest());
		expect(result.kind).toBe("document");
		expect(written[0]?.some((byte) => byte !== 0)).toBe(true);
		expect(new Uint8Array(exports.memory.buffer, 16, written[0]?.length ?? 0).every((b) => b === 0)).toBe(true);
		expect(freed).toEqual([
			[16, written[0]?.length ?? 0],
			[128, 4],
			[256, response.length],
		]);
	});
});

describe("typed DOM seam", () => {
	it("a null input means no logo, so the caller never loads this chunk", () => {
		expect(selectedLogoFile(null)).toBeNull();
	});

	it("reads the selected local file without a network call", async () => {
		const file = new File([png], "logo.png", { type: "image/png" }) as File;
		const fake = { files: [file] } as unknown as HTMLInputElement;
		expect(selectedLogoFile(fake)).toBe(file);
		expect(Array.from(await readLogoBytes(file))).toEqual(Array.from(png));
	});

	it("shows the honest read-back rows on the result card", () => {
		const result = decodeSignLogoResponse(documentResponse({}));
		if (result.kind !== "document") throw new Error("expected a document");
		const rows = signLogoRows(result);
		expect(rows.some((row) => row.label === "Appearance" && row.value.includes("byte-for-byte"))).toBe(true);
	});
});
