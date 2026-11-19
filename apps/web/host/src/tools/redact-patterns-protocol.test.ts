import { describe, expect, it } from "vitest";
import { TOOLS_WASM_URL, encodeToolRequest } from "./protocol.js";
import {
	REDACT_PATTERNS_WASM_URL,
	type RedactPatternsWasm,
	instantiateRedactPatterns,
	runRedactPatterns,
} from "./redact-patterns-protocol.js";

function u32(value: number): number[] {
	return [value & 0xff, (value >>> 8) & 0xff, (value >>> 16) & 0xff, (value >>> 24) & 0xff];
}

function block(bytes: number[]): number[] {
	return [...u32(bytes.length), ...bytes];
}

function frame(
	message: Record<string, unknown>,
	outputs: ReadonlyArray<readonly [string, number[]]>,
): Uint8Array {
	// The shared decoder needs per-output metadata or it infers every non-ZIP
	// output as a PDF, which would mislabel the scan's JSON report.
	const reports = outputs.map(([name, data]) =>
		name.endsWith(".json")
			? { name, kind: "report", pages: 0, bytes: data.length, verdict: "review" }
			: { name, kind: "pdf", pages: 1, bytes: data.length, verdict: "ok" },
	);
	const encoded = Array.from(
		new TextEncoder().encode(JSON.stringify({ ...message, outputs: reports })),
	);
	const bytes = [...u32(0x52544c53), ...u32(0), ...block(encoded), ...u32(outputs.length)];
	for (const [name, data] of outputs) {
		bytes.push(...block(Array.from(new TextEncoder().encode(name))), ...block(data));
	}
	return new Uint8Array(bytes);
}

/** A fake scan guest: a JSON report only, never a PDF. */
function scanResponse(): Uint8Array {
	const review = {
		schema: 1,
		source: "in/0",
		source_len: 100,
		source_fnv: 42,
		candidates: [
			{
				id: "p0-l0-c0",
				page: 0,
				line: 0,
				start: 0,
				end: 11,
				pattern: "ssn",
				masked: "***-**-6789",
				region: [10, 20, 30, 40],
			},
		],
	};
	return frame({ pages_out: 0, verdict: "review", checked: [], bytes_in: 100, bytes_out: 100 }, [
		["doc-review.json", Array.from(new TextEncoder().encode(JSON.stringify(review)))],
	]);
}

/** A fake apply guest: the redacted PDF. */
function applyResponse(): Uint8Array {
	return frame({ pages_out: 1, verdict: "ok", checked: ["pages"], bytes_in: 100, bytes_out: 90 }, [
		["doc-redacted.pdf", [0x25, 0x50, 0x44, 0x46]],
	]);
}

function fakeGuest(response: Uint8Array): {
	wasm: RedactPatternsWasm;
	requestBytes: () => Uint8Array;
} {
	const memory = new WebAssembly.Memory({ initial: 2 });
	let next = 1024;
	let lastRequest = new Uint8Array(0);
	const exports = {
		memory,
		selis_redact_patterns_alloc(len: number): number {
			const ptr = next;
			next += len;
			return ptr;
		},
		selis_redact_patterns_free(): void {},
		selis_redact_patterns_run(ptr: number, len: number, outLen: number): number {
			lastRequest = new Uint8Array(memory.buffer, ptr, len).slice();
			const out = next;
			next += response.length;
			new Uint8Array(memory.buffer, out, response.length).set(response);
			new DataView(memory.buffer).setUint32(outLen, response.length, true);
			return out;
		},
		selis_redact_patterns_chunk_id(): number {
			return 0;
		},
		selis_redact_patterns_available(): number {
			return 1;
		},
	};
	return { wasm: { exports } as unknown as RedactPatternsWasm, requestBytes: () => lastRequest };
}

const file = { name: "doc.pdf", bytes: new Uint8Array([0x25, 0x50, 0x44, 0x46]) };

describe("pattern-redaction chunk protocol", () => {
	it("fetches its own same-origin module, distinct from the tools chunk", () => {
		expect(REDACT_PATTERNS_WASM_URL).toBe("/wasm/selis_pdf_wasm_redact_patterns.wasm");
		expect(REDACT_PATTERNS_WASM_URL).not.toBe(TOOLS_WASM_URL);
	});

	it("keeps op 47 stable on the wire for both phases", () => {
		for (const options of [
			{ phase: "scan", patterns: ["ssn"] },
			{ phase: "apply", review: { schema: 1 }, approved_ids: ["p0-l0-c0"] },
		]) {
			const request = encodeToolRequest({ op: "redact_patterns", options, files: [file] });
			expect(new DataView(request.buffer).getUint32(8, true)).toBe(47);
		}
	});

	it("returns the scan review JSON as a report output, never a PDF", () => {
		const { wasm, requestBytes } = fakeGuest(scanResponse());
		const result = runRedactPatterns(wasm, {
			op: "redact_patterns",
			options: { phase: "scan", patterns: ["ssn", "card", "email"] },
			files: [file],
		});
		expect(result.reports[0]?.kind).toBe("report");
		expect(result.outputs[0]?.name).toBe("doc-review.json");
		const review = JSON.parse(new TextDecoder().decode(result.outputs[0]?.bytes));
		expect(review.candidates[0].masked).toBe("***-**-6789");
		expect(new DataView(requestBytes().buffer).getUint32(8, true)).toBe(47);
	});

	it("sends the explicitly approved ids back on the apply phase", () => {
		const { wasm, requestBytes } = fakeGuest(applyResponse());
		const result = runRedactPatterns(wasm, {
			op: "redact_patterns",
			options: {
				phase: "apply",
				review: { schema: 1 },
				approved_ids: ["p0-l0-c0"],
			},
			files: [file],
		});
		expect(result.verification.verdict).toBe("ok");
		expect(result.outputs[0]?.name).toBe("doc-redacted.pdf");
		const sent = new TextDecoder().decode(requestBytes());
		expect(sent).toContain('"approved_ids":["p0-l0-c0"]');
	});

	it("rejects a guest that declares a host import", async () => {
		const withImport = new Uint8Array([
			0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x02,
			0x07, 0x01, 0x01, 0x65, 0x01, 0x66, 0x00, 0x00,
		]);
		await expect(instantiateRedactPatterns(withImport)).rejects.toThrow(/unexpected import\(s\)/);
	});
});
