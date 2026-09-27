/**
 * The no-upload guard (SL-4.WEB.03 DoD).
 *
 * "The one thing this app must never do is upload a document, and a CI test
 * asserts no request body ever contains document bytes."
 *
 * This module is that assertion, as a pure function over recorded outgoing
 * requests so it runs in Vitest (Node) and in the browser shell alike: hand it
 * every request the app issued plus the document bytes it opened, and it throws
 * when any of those bytes ride along in a request.
 *
 * What counts as an upload:
 * — Any request, whatever its method, whose body or whose URL carries a window
 *   of an opened document's bytes. `fetch` refuses a body on GET/HEAD,
 *   `XMLHttpRequest.send` does not, so a method allow-list would be a hole
 *   rather than a policy: the rule is "no document bytes in a request", not
 *   "no document bytes in a POST". Short JSON telemetry bodies (no document
 *   bytes) pass; a request carrying the PDF — whole, sliced, or base64'd into
 *   the query string — fails.
 * — A body the guard cannot read as bytes (a `ReadableStream`, a `Request`
 *   with an unread body) is treated as an upload. The gate fails closed: an
 *   uninspectable body is never evidence of innocence.
 * — Bodies are compared as bytes, never as strings, so binary PDFs are caught
 *   exactly as text PDFs are, and each document is fingerprinted by several
 *   windows ({@link fingerprintsFor}) so shipping the middle of a document -
 *   one page, one decoded image stream — is caught too.
 *
 * Non-goals: this does not police the Worker's `http-range` GETs (those
 * *download* into the local engine — still local processing, ADR-P0016), nor
 * the opt-in crash/telemetry pings (those carry no document bytes by type,
 * ADR-P0017, and would fail here if they ever did).
 */

/** One recorded outgoing request. */
export interface RecordedRequest {
	readonly url: string;
	readonly method: string;
	readonly body?: ArrayBuffer | Uint8Array | string | null;
	/**
	 * Set when the body could not be read as bytes. {@link assertNoUpload}
	 * fails closed on these — see the module doc.
	 */
	readonly uninspectable?: boolean;
}

/** One opened document's bytes. */
export type DocumentBytes = Uint8Array | ArrayBuffer;

/** Anything `fetch` or `XMLHttpRequest.send` accepts as a body. */
export type RequestBody =
	| string
	| URLSearchParams
	| Blob
	| FormData
	| ArrayBuffer
	| ArrayBufferView
	| ReadableStream<Uint8Array>
	| null
	| undefined;

/** The result of reading a request body as bytes. */
export interface InspectedBody {
	readonly bytes: Uint8Array | null;
	readonly uninspectable: boolean;
}

const FINGERPRINT_LEN = 64;

/** Windows cut from a document so a partial upload still matches. */
const FINGERPRINT_WINDOWS = 16;

function toBytes(input: ArrayBuffer | Uint8Array | string | null | undefined): Uint8Array | null {
	if (input === undefined || input === null) {
		return null;
	}
	if (typeof input === "string") {
		return new TextEncoder().encode(input);
	}
	if (input instanceof Uint8Array) {
		return input;
	}
	return new Uint8Array(input);
}

function toDocBytes(input: DocumentBytes): Uint8Array {
	return input instanceof Uint8Array ? input : new Uint8Array(input);
}

function concat(left: Uint8Array, right: Uint8Array): Uint8Array {
	const out = new Uint8Array(left.length + right.length);
	out.set(left, 0);
	out.set(right, left.length);
	return out;
}

/** Whether `haystack` contains `needle` as a contiguous subsequence. */
export function containsBytes(haystack: Uint8Array, needle: Uint8Array): boolean {
	if (needle.length === 0 || haystack.length < needle.length) {
		return false;
	}
	// Naive scan is fine: bodies under test are small, and correctness
	// matters more than speed for a CI assertion.
	for (let i = 0; i <= haystack.length - needle.length; i += 1) {
		let match = true;
		for (let j = 0; j < needle.length; j += 1) {
			if (haystack[i + j] !== needle[j]) {
				match = false;
				break;
			}
		}
		if (match) {
			return true;
		}
	}
	return false;
}

/**
 * The windows of `doc` that identify it inside a request: the first
 * {@link FINGERPRINT_LEN} bytes plus evenly spaced windows after it. An
 * uploader that sends only the tail of a document still matches one of these,
 * which a prefix-only fingerprint would miss.
 */
export function fingerprintsFor(doc: Uint8Array): Uint8Array[] {
	if (doc.length === 0) {
		return [];
	}
	if (doc.length <= FINGERPRINT_LEN) {
		return [doc.slice()];
	}
	const windows: Uint8Array[] = [doc.slice(0, FINGERPRINT_LEN)];
	const span = doc.length - FINGERPRINT_LEN;
	for (let i = 1; i < FINGERPRINT_WINDOWS; i += 1) {
		const start = Math.floor((span * i) / FINGERPRINT_WINDOWS);
		windows.push(doc.slice(start, start + FINGERPRINT_LEN));
	}
	return windows;
}

/**
 * Read a request body as bytes, for the recorder and for direct assertions.
 *
 * `Blob` and `FormData` bodies are read in full: a multipart upload carries
 * the file verbatim, and one 64 MiB read is cheaper than missing the request
 * this gate exists to catch. A `ReadableStream` — or anything else the guard
 * does not understand — comes back `uninspectable`, which
 * {@link assertNoUpload} treats as an upload.
 */
export async function inspectBody(body: RequestBody): Promise<InspectedBody> {
	if (body === null || body === undefined) {
		return { bytes: null, uninspectable: false };
	}
	if (typeof body === "string") {
		return { bytes: new TextEncoder().encode(body), uninspectable: false };
	}
	if (typeof URLSearchParams !== "undefined" && body instanceof URLSearchParams) {
		return { bytes: new TextEncoder().encode(body.toString()), uninspectable: false };
	}
	if (body instanceof ArrayBuffer) {
		return { bytes: new Uint8Array(body.slice(0)), uninspectable: false };
	}
	if (ArrayBuffer.isView(body)) {
		const view = body as { buffer: ArrayBuffer; byteOffset: number; byteLength: number };
		const start = view.byteOffset;
		const slice = view.buffer.slice(start, start + view.byteLength);
		return { bytes: new Uint8Array(slice), uninspectable: false };
	}
	if (typeof Blob !== "undefined" && body instanceof Blob) {
		return { bytes: new Uint8Array(await body.arrayBuffer()), uninspectable: false };
	}
	if (typeof FormData !== "undefined" && body instanceof FormData) {
		return { bytes: await formDataBytes(body), uninspectable: false };
	}
	return { bytes: null, uninspectable: true };
}

/** Concatenate a `FormData` body: a multipart upload carries the file verbatim. */
async function formDataBytes(form: FormData): Promise<Uint8Array> {
	// `entries()` is the FormData API that reaches every entry, string-valued
	// or not; a multipart upload carries the file verbatim either way.
	const strings: string[] = [];
	const files: Blob[] = [];
	for (const [, value] of form.entries()) {
		if (typeof value === "string") {
			strings.push(value);
			continue;
		}
		files.push(value);
	}
	const parts: Uint8Array[] = [new TextEncoder().encode(strings.join(""))];
	for (const file of files) {
		parts.push(new Uint8Array(await file.arrayBuffer()));
	}
	let total = 0;
	for (const part of parts) {
		total += part.length;
	}
	const out = new Uint8Array(total);
	let offset = 0;
	for (const part of parts) {
		out.set(part, offset);
		offset += part.length;
	}
	return out;
}

/**
 * Assert no recorded request uploads any opened document's bytes.
 * Throws an `Error` naming the offending request; returns normally when clean.
 *
 * Every method is checked, the URL is scanned alongside the body (a base64
 * query parameter is still an upload), and a body the guard could not read
 * fails the assertion rather than passing it.
 */
export function assertNoUpload(
	requests: readonly RecordedRequest[],
	documents: readonly DocumentBytes[],
): void {
	const prints: Uint8Array[] = [];
	for (const document of documents) {
		for (const window of fingerprintsFor(toDocBytes(document))) {
			prints.push(window);
		}
	}
	if (prints.length === 0) {
		return;
	}
	for (const request of requests) {
		const method = request.method.toUpperCase();
		if (request.uninspectable === true) {
			throw new Error(
				`upload detected: ${method} ${request.url} body cannot be read as bytes; the guard fails closed`,
			);
		}
		const body = toBytes(request.body);
		const url = new TextEncoder().encode(request.url);
		const haystack = body === null || body.length === 0 ? url : concat(url, body);
		for (const print of prints) {
			if (containsBytes(haystack, print)) {
				const size = body === null ? 0 : body.length;
				throw new Error(
					`upload detected: ${method} ${request.url} carries document bytes (${size} body bytes)`,
				);
			}
		}
	}
}

/**
 * Wrap `fetch` so every call is recorded for {@link assertNoUpload}.
 * The wrapper performs no network policy itself — it only records.
 */
export function createFetchRecorder(): {
	readonly requests: RecordedRequest[];
	record(url: string, method: string, body?: RecordedRequest["body"]): void;
} {
	const requests: RecordedRequest[] = [];
	return {
		requests,
		record(url: string, method: string, body?: RecordedRequest["body"]): void {
			requests.push({ url, method, body: body ?? null });
		},
	};
}

/** A live recorder installed over `fetch` and `XMLHttpRequest`. */
export interface RequestRecorder {
	/** Requests seen so far, in issue order. */
	readonly requests: readonly RecordedRequest[];
	/** Await every pending body read and return the ordered list. */
	settle(): Promise<readonly RecordedRequest[]>;
	/** Settle, then assert that nothing uploaded any of `documents`. */
	assertNoUpload(documents: readonly DocumentBytes[]): Promise<void>;
	/** Put the original `fetch` and `XMLHttpRequest.prototype.send` back. */
	restore(): void;
}

/** The slice of `XMLHttpRequest` this module touches. */
interface XhrLike {
	method: string;
	responseURL: string;
	open: (...args: unknown[]) => unknown;
	send: (...args: unknown[]) => unknown;
}

/** Mutable bookkeeping for one recorded request. */
interface RecordedEntry {
	readonly url: string;
	readonly method: string;
	bytes: Uint8Array | null;
	uninspectable: boolean;
	settled: Promise<void>;
}

/** Minimal global surface the recorder patches. */
interface RecorderScope {
	fetch?: typeof fetch;
	XMLHttpRequest?: { prototype: XhrLike };
}

function requestUrl(input: RequestInfo | URL): string {
	if (typeof input === "string") {
		return input;
	}
	if (input instanceof URL) {
		return input.toString();
	}
	return input.url;
}

function requestBody(input: RequestInfo | URL, init: RequestInit | undefined): RequestBody {
	if (init !== undefined && init.body !== undefined && init.body !== null) {
		return init.body;
	}
	if (input instanceof Request) {
		return input.body;
	}
	return null;
}

/**
 * Install the recorder over the live `fetch` and `XMLHttpRequest`, so the CI
 * gate observes the requests the app really issues instead of a hand-written
 * list. This is the mechanical half of the WEB.03 DoD: a request the shell
 * makes through either transport cannot dodge the gate, because the gate is
 * the transport.
 *
 * The wrappers are transparent — the original `fetch` and `send` still run
 * with the arguments they were given — and the recorder decides no policy; it
 * only records. Call {@link RequestRecorder.restore} in teardown.
 */
export function installRequestRecorder(): RequestRecorder {
	const scope = globalThis as unknown as RecorderScope;
	const originalFetch = scope.fetch;
	const XhrCtor = scope.XMLHttpRequest;
	const originalOpen = XhrCtor?.prototype.open;
	const originalSend = XhrCtor?.prototype.send;
	const entries: RecordedEntry[] = [];
	const targets = new WeakMap<object, { url: string; method: string }>();

	const record = (url: string, method: string, body: RequestBody): void => {
		const entry: RecordedEntry = {
			url,
			method,
			bytes: null,
			// Until the body has been read the guard knows nothing about it,
			// and "knows nothing" fails closed.
			uninspectable: true,
			settled: Promise.resolve(),
		};
		entry.settled = inspectBody(body).then((inspected) => {
			entry.bytes = inspected.bytes;
			entry.uninspectable = inspected.uninspectable;
		});
		entries.push(entry);
	};

	if (typeof originalFetch === "function") {
		const wrapped = (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
			const method = init?.method ?? (input instanceof Request ? input.method : "GET");
			record(requestUrl(input), method, requestBody(input, init));
			return originalFetch(input, init);
		};
		scope.fetch = wrapped as typeof fetch;
	}

	if (XhrCtor !== undefined && originalOpen !== undefined && originalSend !== undefined) {
		const proto = XhrCtor.prototype;
		const open = function (this: XhrLike, ...args: unknown[]): unknown {
			const method = typeof args[0] === "string" ? args[0] : "GET";
			const url = args[1] === undefined ? "" : String(args[1]);
			targets.set(this, { url, method });
			return Reflect.apply(originalOpen, this, args);
		};
		const send = function (this: XhrLike, ...args: unknown[]): unknown {
			const target = targets.get(this);
			const url = target?.url ?? this.responseURL;
			const method = target?.method ?? this.method;
			const body = args[0];
			record(url === "" ? "(no url)" : url, method, (body ?? null) as RequestBody);
			return Reflect.apply(originalSend, this, args);
		};
		proto.open = open as XhrLike["open"];
		proto.send = send as XhrLike["send"];
	}

	const snapshot = (): RecordedRequest[] =>
		entries.map((entry) => ({
			url: entry.url,
			method: entry.method,
			body: entry.bytes,
			uninspectable: entry.uninspectable,
		}));

	return {
		get requests(): readonly RecordedRequest[] {
			return snapshot();
		},
		async settle(): Promise<readonly RecordedRequest[]> {
			await Promise.all(entries.map((entry) => entry.settled));
			return snapshot();
		},
		async assertNoUpload(documents: readonly DocumentBytes[]): Promise<void> {
			assertNoUpload(await this.settle(), documents);
		},
		restore(): void {
			if (typeof originalFetch === "function") {
				scope.fetch = originalFetch;
			}
			if (XhrCtor !== undefined && originalOpen !== undefined && originalSend !== undefined) {
				XhrCtor.prototype.open = originalOpen;
				XhrCtor.prototype.send = originalSend;
			}
		},
	};
}
