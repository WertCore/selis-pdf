/**
 * The linearize card's presentation helpers.
 *
 * DOM-free and side-effect free, so the option copy and the result rows can be
 * tested without a browser. Op 43 returns the same generic `Verification` every
 * other rewrite does — it does not carry the linearisation dictionary — so the
 * linearisation facts here are read straight out of the *output bytes* the
 * engine produced. That is deliberate: the card reports what is actually in
 * the download, not a claim copied from the request. Only the before/after
 * sizes and the structural verdict come from the engine's verification.
 *
 * This is Selis's own fast-web-view layout. Nothing here claims a
 * `qpdf --check-linearization` verdict: no external linearisation checker is
 * part of this build.
 */

import { type Verification, type VerificationRow, formatBytes } from "./protocol.js";

/**
 * The honest option note. It names what is written, what is checked, the
 * local-only guarantee, and the limit of the claim in one paragraph.
 */
export const LINEARIZE_NOTE =
	"Reorders the PDF into Selis's fast-web-view layout: object 1 carries a /Linearized dictionary and hint tables, the first page's objects come first, and two classic xref sections let a reader show page 1 after fetching only the leading bytes. Pages and content are re-opened and checked before the download, and the report reads /L, /N and the page objects straight from the output bytes. Runs on this device only — the file is never uploaded. This is Selis's own linearization, not a qpdf --check-linearization verdict.";

/** The linearisation facts read back from an output PDF. */
export interface LinearizationFacts {
	/** Whether object 1 declares `/Linearized 1`. */
	readonly linearized: boolean;
	/** `/L` from the lin dictionary: the declared file length in bytes. */
	readonly length: number | null;
	/** `/N` from the lin dictionary: the declared page count. */
	readonly pages: number | null;
	/** `/O` from the lin dictionary: the first page's object number. */
	readonly firstPageObject: number | null;
	/** The largest leaf `/Page` object number, i.e. the last page's object. */
	readonly lastPageObject: number | null;
	/** How many `N 0 obj` headers the output contains. */
	readonly objectCount: number;
}

function isDigit(byte: number): boolean {
	return byte >= 0x30 && byte <= 0x39;
}

function isSpace(byte: number): boolean {
	return (
		byte === 0x20 || byte === 0x09 || byte === 0x0a || byte === 0x0d || byte === 0x00 || byte === 0x0c
	);
}

function isNameChar(byte: number): boolean {
	return (
		(byte >= 0x41 && byte <= 0x5a) ||
		(byte >= 0x61 && byte <= 0x7a) ||
		isDigit(byte)
	);
}

/** Decode a byte range as Latin-1: PDF object syntax is ASCII. */
function ascii(bytes: Uint8Array, start: number, end: number): string {
	let out = "";
	for (let i = start; i < end; i += 1) {
		out += String.fromCharCode(bytes[i] ?? 0);
	}
	return out;
}

interface PdfObject {
	readonly num: number;
	readonly start: number;
	readonly end: number;
}

/** Find the `endobj` that closes the object whose header ended at `from`. */
function findEndobj(bytes: Uint8Array, from: number): number {
	const marker = [0x65, 0x6e, 0x64, 0x6f, 0x62, 0x6a]; // "endobj"
	for (let i = from; i + marker.length <= bytes.length; i += 1) {
		let match = true;
		for (let k = 0; k < marker.length; k += 1) {
			if (bytes[i + k] !== marker[k]) {
				match = false;
				break;
			}
		}
		if (match) {
			return i + marker.length;
		}
	}
	return bytes.length;
}

/**
 * Locate every `N 0 obj` header and the extent of its object. Binary stream
 * data is skipped by jumping to the closing `endobj` after each header, so a
 * string inside a content stream cannot be mistaken for another object.
 */
function scanObjects(bytes: Uint8Array): PdfObject[] {
	const out: PdfObject[] = [];
	const n = bytes.length;
	let i = 0;
	while (i < n) {
		if (!isDigit(bytes[i] ?? 0)) {
			i += 1;
			continue;
		}
		const numStart = i;
		let j = i;
		while (j < n && isDigit(bytes[j] ?? 0)) {
			j += 1;
		}
		const numEnd = j;
		if (j >= n || !isSpace(bytes[j] ?? 0)) {
			i += 1;
			continue;
		}
		while (j < n && isSpace(bytes[j] ?? 0)) {
			j += 1;
		}
		const genStart = j;
		while (j < n && isDigit(bytes[j] ?? 0)) {
			j += 1;
		}
		if (j === genStart || j >= n || !isSpace(bytes[j] ?? 0)) {
			i += 1;
			continue;
		}
		while (j < n && isSpace(bytes[j] ?? 0)) {
			j += 1;
		}
		const isObj =
			bytes[j] === 0x6f &&
			bytes[j + 1] === 0x62 &&
			bytes[j + 2] === 0x6a &&
			(j + 3 >= n || !isNameChar(bytes[j + 3] ?? 0));
		if (!isObj) {
			i += 1;
			continue;
		}
		const num = Number(ascii(bytes, numStart, numEnd));
		const end = findEndobj(bytes, j + 3);
		out.push({ num, start: numStart, end });
		i = end;
	}
	return out;
}

/** True when the object's dictionary carries a leaf `/Type /Page`. */
function isPageObject(bytes: Uint8Array, object: PdfObject): boolean {
	let body = ascii(bytes, object.start, Math.min(object.end, object.start + 2048));
	// A stream's payload is not dictionary syntax: text like "/Type /Page"
	// inside a content stream must not turn its container into a page.
	const stream = body.indexOf("stream");
	if (stream >= 0) {
		body = body.slice(0, stream);
	}
	return /\/Type\s*\/Page(?![A-Za-z0-9])/.test(body);
}

/**
 * Read the linearisation facts from the bytes of a produced PDF. Every value
 * is optional except the object count: a malformed or non-linearised output
 * reports "not reported" rather than inventing zero.
 */
export function readLinearization(bytes: Uint8Array): LinearizationFacts {
	const objects = scanObjects(bytes);
	const head = ascii(bytes, 0, Math.min(bytes.length, 1024));
	const header = /\b1\s+0\s+obj\b/.exec(head);
	let dict = "";
	if (header !== null) {
		const from = header.index + header[0].length;
		const end = head.indexOf("endobj", from);
		dict = end < 0 ? head.slice(from) : head.slice(from, end);
	}
	const intAfter = (key: string): number | null => {
		const match = new RegExp(`/${key}(?![A-Za-z])\\s*(\\d+)`).exec(dict);
		return match === null ? null : Number(match[1]);
	};
	const pageNums = objects
		.filter((object) => isPageObject(bytes, object))
		.map((object) => object.num)
		.sort((a, b) => a - b);
	return {
		linearized: /\/Linearized(?![A-Za-z])\s*1\b/.test(dict),
		length: intAfter("L"),
		pages: intAfter("N"),
		firstPageObject: intAfter("O"),
		lastPageObject: pageNums.length === 0 ? null : (pageNums[pageNums.length - 1] ?? null),
		objectCount: objects.length,
	};
}

/**
 * The rows the linearize card adds to its result. Called with the output PDF
 * bytes and the engine's measured verification. The lin-dictionary facts come
 * from the output bytes; the sizes and verdict come from the engine.
 */
export function linearizeReportRows(
	outputBytes: Uint8Array,
	verification: Verification,
): VerificationRow[] {
	const facts = readLinearization(outputBytes);
	if (!facts.linearized || facts.length === null || facts.pages === null) {
		return [{ label: "Linearization", value: "the output has no readable /Linearized dictionary" }];
	}
	return [
		{ label: "File length /L", value: `${facts.length} B (${formatBytes(facts.length)})` },
		{ label: "Pages /N", value: String(facts.pages) },
		{
			label: "First page object (/O)",
			value: facts.firstPageObject === null ? "not reported" : String(facts.firstPageObject),
		},
		{
			label: "Last page object",
			value: facts.lastPageObject === null ? "not found" : String(facts.lastPageObject),
		},
		{ label: "Objects", value: String(facts.objectCount) },
		{
			label: "Size (before → after)",
			value: `${formatBytes(verification.bytes_in)} → ${formatBytes(verification.bytes_out)}`,
		},
		{ label: "Structural verdict", value: verification.verdict },
	];
}
