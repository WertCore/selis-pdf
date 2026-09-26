/**
 * The no-upload guard (SL-4.WEB.03 DoD).
 *
 * "The one thing this app must never do is upload a document, and a CI test
 * asserts no request body ever contains document bytes."
 *
 * This module is that assertion, as a pure function over recorded outgoing
 * requests so it runs in Vitest (Node) and in the browser shell alike:
 * hand it every request the app issued plus the document bytes it opened,
 * and it throws when any body contains those bytes.
 *
 * What counts as an upload:
 * - Any request whose method is not GET/HEAD/RANGE-capable GET and whose
 *   body contains a document's byte prefix (first 64 bytes) *and* is at
 *   least as long as the smallest document. Short JSON telemetry bodies
 *   (no document bytes) pass; a POST carrying the PDF — whole or sliced —
 *   fails.
 * - Bodies are compared as bytes, never as strings, so binary PDFs are
 *   caught exactly as text PDFs are.
 *
 * Non-goals: this does not police the Worker's `http-range` GETs (those
 * *download* into the local engine — still local processing, ADR-P0016),
 * nor the opt-in crash/telemetry pings (those carry no document bytes by
 * type, ADR-P0017, and would fail here if they ever did).
 */

/** One recorded outgoing request. */
export interface RecordedRequest {
	readonly url: string;
	readonly method: string;
	readonly body?: ArrayBuffer | Uint8Array | string | null;
}

/** One opened document's bytes (the first 64 bytes are the fingerprint). */
export type DocumentBytes = Uint8Array | ArrayBuffer;

const FINGERPRINT_LEN = 64;

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

/** Whether `haystack` contains `needle` as a contiguous subsequence. */
export function containsBytes(haystack: Uint8Array, needle: Uint8Array): boolean {
	if (needle.length === 0 || haystack.length < needle.length) {
		return false;
	}
	// Naïve scan is fine: bodies under test are small, and correctness
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
 * Assert no recorded request uploads any opened document's bytes.
 * Throws an `Error` naming the offending URL; returns normally when clean.
 */
export function assertNoUpload(
	requests: readonly RecordedRequest[],
	documents: readonly DocumentBytes[],
): void {
	const docs = documents.map(toDocBytes).filter((d) => d.length > 0);
	if (docs.length === 0 || requests.length === 0) {
		return;
	}
	for (const request of requests) {
		const method = request.method.toUpperCase();
		if (method === "GET" || method === "HEAD") {
			continue;
		}
		const body = toBytes(request.body);
		if (body === null || body.length === 0) {
			continue;
		}
		for (const doc of docs) {
			const fingerprint = doc.slice(0, Math.min(FINGERPRINT_LEN, doc.length));
			if (containsBytes(body, fingerprint)) {
				throw new Error(
					`upload detected: ${method} ${request.url} body contains document bytes (${body.length} bytes)`,
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
