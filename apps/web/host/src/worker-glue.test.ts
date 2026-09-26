import { describe, expect, it } from "vitest";
import {
	buildOpenRequest,
	deliveryFor,
	parseOpenResponse,
	toWireDescriptor,
} from "./worker-glue.js";

describe("wire descriptor mapping (must match protocol.rs)", () => {
	it("maps bytes inline", () => {
		expect(toWireDescriptor({ kind: "bytes", len: 10 })).toEqual({ kind: "bytes", len: 10 });
	});

	it("maps blob / opfs / fsa handles opaquely (never bytes)", () => {
		expect(toWireDescriptor({ kind: "blob", sourceId: "b1" })).toEqual({
			kind: "blob",
			sourceId: "b1",
		});
		expect(toWireDescriptor({ kind: "opfs", path: "/doc.pdf" })).toEqual({
			kind: "opfs",
			path: "/doc.pdf",
		});
		expect(toWireDescriptor({ kind: "fsa", handleId: "h1" })).toEqual({
			kind: "fsa",
			handleId: "h1",
		});
	});

	it("maps http-range with and without size", () => {
		expect(toWireDescriptor({ kind: "http-range", url: "https://x/y.pdf" })).toEqual({
			kind: "http-range",
			url: "https://x/y.pdf",
		});
		expect(toWireDescriptor({ kind: "http-range", url: "https://x/y.pdf", size: 5 })).toEqual({
			kind: "http-range",
			url: "https://x/y.pdf",
			size: 5,
		});
	});
});

describe("delivery", () => {
	it("bytes go as the request attachment; handles register; http-range fetches", () => {
		expect(deliveryFor({ kind: "bytes", len: 3 })).toEqual({ via: "attachment", len: 3 });
		expect(deliveryFor({ kind: "blob", sourceId: "b1" })).toEqual({
			via: "register-blob",
			sourceId: "b1",
		});
		expect(deliveryFor({ kind: "opfs", path: "/a.pdf" })).toEqual({
			via: "register-opfs",
			path: "/a.pdf",
		});
		expect(deliveryFor({ kind: "fsa", handleId: "h" })).toEqual({
			via: "register-fsa",
			handleId: "h",
		});
		expect(deliveryFor({ kind: "http-range", url: "https://x/y.pdf" })).toEqual({
			via: "fetch-ranges",
			url: "https://x/y.pdf",
		});
	});
});

describe("open request shape", () => {
	it("matches the Rust protocol envelope (v/id/op/src/budget)", () => {
		const request = buildOpenRequest(7, { kind: "bytes", len: 10 });
		expect(request).toEqual({
			v: 1,
			id: 7,
			op: "open",
			src: { kind: "bytes", len: 10 },
			budget: { surface: "viewer" },
		});
	});

	it("honours the budget surface", () => {
		const request = buildOpenRequest(1, { kind: "opfs", path: "/a.pdf" }, "thumbnail");
		expect(request.budget).toEqual({ surface: "thumbnail" });
	});

	it("round-trips every adapter kind through JSON (the wire is JSON)", () => {
		for (const src of [
			{ kind: "bytes", len: 1 },
			{ kind: "blob", sourceId: "b" },
			{ kind: "opfs", path: "/p" },
			{ kind: "fsa", handleId: "h" },
			{ kind: "http-range", url: "https://x/y.pdf" },
		] as const) {
			const request = buildOpenRequest(1, src);
			const back = JSON.parse(JSON.stringify(request)) as Record<string, unknown>;
			expect(back).toEqual(request);
		}
	});
});

describe("parseOpenResponse", () => {
	it("accepts { value: { doc, pages } }", () => {
		expect(parseOpenResponse({ value: { doc: 3, pages: 5 } })).toEqual({ doc: 3, pages: 5 });
	});

	it("rejects shapes without numeric doc/pages", () => {
		expect(() => parseOpenResponse(null)).toThrow();
		expect(() => parseOpenResponse({})).toThrow();
		expect(() => parseOpenResponse({ value: { doc: "3", pages: 1 } })).toThrow();
	});
});
