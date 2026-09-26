import { describe, expect, it } from "vitest";
import { assertNoUpload, containsBytes, createFetchRecorder } from "./no-upload.js";

const PDF = new TextEncoder().encode("%PDF-1.4 hello document bytes for the no-upload gate");

describe("containsBytes", () => {
	it("finds a contiguous subsequence", () => {
		expect(containsBytes(new Uint8Array([1, 2, 3, 4]), new Uint8Array([2, 3]))).toBe(true);
		expect(containsBytes(new Uint8Array([1, 2, 3]), new Uint8Array([2, 4]))).toBe(false);
		expect(containsBytes(new Uint8Array([1]), new Uint8Array([1, 2]))).toBe(false);
		expect(containsBytes(new Uint8Array([1, 2]), new Uint8Array([]))).toBe(false);
	});
});

describe("assertNoUpload (the WEB.03 CI gate)", () => {
	it("passes when nothing was requested", () => {
		expect(() => assertNoUpload([], [PDF])).not.toThrow();
		expect(() => assertNoUpload([], [])).not.toThrow();
	});

	it("passes for GETs and empty bodies", () => {
		expect(() =>
			assertNoUpload(
				[
					{ url: "https://example.org/r.pdf", method: "GET" },
					{ url: "https://app.selis/ping", method: "POST", body: null },
					{
						url: "https://app.selis/telemetry",
						method: "POST",
						body: JSON.stringify({ name: "session_start", metrics: { pages_shown: 1 } }),
					},
				],
				[PDF],
			),
		).not.toThrow();
	});

	it("fails when a POST body contains the document bytes", () => {
		expect(() =>
			assertNoUpload([{ url: "https://evil.example/upload", method: "POST", body: PDF }], [PDF]),
		).toThrow(/upload detected/);
	});

	it("fails when a POST carries a slice of the document (prefix fingerprint)", () => {
		const slice = PDF.slice(0, 80);
		expect(() =>
			assertNoUpload([{ url: "https://evil.example/x", method: "post", body: slice }], [PDF]),
		).toThrow(/upload detected/);
	});

	it("fails for string bodies embedding the PDF header", () => {
		const text = "%PDF-1.4 hello document bytes for the no-upload gate plus trailing JSON";
		expect(() =>
			assertNoUpload([{ url: "https://evil.example/y", method: "PUT", body: text }], [PDF]),
		).toThrow(/upload detected/);
	});

	it("passes for unrelated binary bodies", () => {
		const other = new Uint8Array([9, 9, 9, 9, 9, 9, 9, 9]);
		expect(() =>
			assertNoUpload([{ url: "https://app.selis/save", method: "POST", body: other }], [PDF]),
		).not.toThrow();
	});

	it("checks every document, not just the first", () => {
		const second = new TextEncoder().encode("%PDF-1.4 second document entirely different");
		expect(() =>
			assertNoUpload(
				[{ url: "https://evil.example/z", method: "POST", body: second }],
				[PDF, second],
			),
		).toThrow(/upload detected/);
	});
});

describe("createFetchRecorder", () => {
	it("records requests for the gate (handoff flow emits no uploads)", () => {
		const recorder = createFetchRecorder();
		// The handoff flow: a range GET (download into the local engine) and
		// an opt-in telemetry ping with no document data.
		recorder.record("https://example.org/r.pdf", "GET");
		recorder.record(
			"https://app.selis/telemetry",
			"POST",
			JSON.stringify({ name: "open", metrics: { pages: 3 } }),
		);
		expect(() => assertNoUpload(recorder.requests, [PDF])).not.toThrow();
		expect(recorder.requests).toHaveLength(2);
	});
});
