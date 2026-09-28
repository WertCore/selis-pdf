/**
 * SL-4.EXT.06 - the wire codec and the reply validator.
 *
 * Two things are being defended here, and they are the two places this
 * transport can be attacked without a browser:
 *
 * - **The base64 decoder is a trust boundary.** It turns a string off the wire
 *   into an allocation, so it rejects rather than guesses: a bad length, a
 *   misplaced pad and an out-of-alphabet character are three distinct, named
 *   failures. "Accept whatever Node accepts" is not a policy, and a decoder
 *   that silently truncates a tile would show up as a rendering artefact
 *   somewhere else entirely.
 * - **The reply validator is a trust boundary.** A `chrome.runtime` port
 *   delivers `unknown`, and anything in the extension can post on it. The
 *   `version` and `shape` rejections are kept apart because they mean different
 *   things: a bad shape is a bug in one of our halves, while a good shape at
 *   the wrong version is a stale build meeting a new one.
 */

import { describe, expect, it } from "vitest";
import { replyRejection } from "./engine-link.js";
import { base64ErrorReason, decodeBase64, encodeBase64 } from "./engine-protocol.js";

/** Every byte value, in order - the alphabet's worst case. */
const EVERY_BYTE = Uint8Array.from({ length: 256 }, (_value, index) => index);

describe("base64 codec (SL-4.EXT.06)", () => {
	it("round-trips every byte value", () => {
		expect(Array.from(decodeBase64(encodeBase64(EVERY_BYTE)))).toEqual(Array.from(EVERY_BYTE));
	});

	it("round-trips every length up to 1000", () => {
		for (let length = 0; length <= 1000; length += 1) {
			const bytes = new Uint8Array(length);
			for (let i = 0; i < length; i += 1) {
				bytes[i] = (i * 37 + length) & 0xff;
			}
			const decoded = decodeBase64(encodeBase64(bytes));
			expect(Array.from(decoded), `length ${length}`).toEqual(Array.from(bytes));
		}
	});

	it("matches the platform encoder, so the wire is not a private dialect", () => {
		// The point of hand-rolling was identical behaviour in Node and in the
		// offscreen document, not a different format. If these ever diverge, one
		// of the two ends is speaking a language the other does not.
		for (let length = 0; length <= 64; length += 1) {
			const bytes = new Uint8Array(length).map((_value, index) => (index * 91) & 0xff);
			const ours = encodeBase64(bytes);
			const theirs = Buffer.from(bytes).toString("base64");
			expect(ours, `length ${length}`).toBe(theirs);
		}
	});

	it("pads to a multiple of four", () => {
		expect(encodeBase64(new Uint8Array([1]))).toBe("AQ==");
		expect(encodeBase64(new Uint8Array([1, 2]))).toBe("AQI=");
		expect(encodeBase64(new Uint8Array([1, 2, 3]))).toBe("AQID");
	});

	it("encodes the empty input as the empty string", () => {
		expect(encodeBase64(new Uint8Array(0))).toBe("");
		expect(decodeBase64("")).toEqual(new Uint8Array(0));
	});

	it("survives the block-splitting flush with a long input", () => {
		// BLOCK_CHARS is 8192, so this crosses it several times; a flush bug
		// would drop or reorder a block and show up here as a length mismatch.
		const bytes = new Uint8Array(40_000).map((_value, index) => (index * 7) & 0xff);
		const encoded = encodeBase64(bytes);
		expect(encoded.length % 4).toBe(0);
		expect(Array.from(decodeBase64(encoded))).toEqual(Array.from(bytes));
	});

	describe("refuses rather than guesses", () => {
		it("names a length that is not a multiple of four", () => {
			expect(() => decodeBase64("AQIDB")).toThrow(/not a multiple of 4/);
			expect(base64ErrorReason(new Error("base64 length 5 is not a multiple of 4"))).toBe(
				"bad-length",
			);
		});

		it("names a pad in a non-final quad", () => {
			expect(() => decodeBase64("AQ==AQID")).toThrow(/padding/);
			expect(base64ErrorReason(new Error("base64 padding at offset 2"))).toBe("bad-padding");
		});

		it("names a pad in the third position of a quad", () => {
			expect(() => decodeBase64("A=ID")).toThrow(/padding/);
		});

		it("names a character outside the alphabet", () => {
			expect(() => decodeBase64("AQ*D")).toThrow(/not in the alphabet/);
			expect(
				base64ErrorReason(new Error("base64 character at offset 2 is not in the alphabet")),
			).toBe("bad-character");
		});

		it("names a non-ASCII character rather than indexing past the table", () => {
			// The decode table is 128 entries; anything above must be refused, not
			// read as `undefined` and turned into a plausible-looking byte.
			expect(() => decodeBase64("AQéD")).toThrow(/not in the alphabet/);
		});

		it("falls back to bad-character for an unknown throw", () => {
			expect(base64ErrorReason("not an Error at all")).toBe("bad-character");
		});
	});
});

describe("reply validation (SL-4.EXT.06)", () => {
	const good = { v: 1 as const, id: 1, ok: true, value: { done: true as const } };

	it("accepts a well-formed reply", () => {
		expect(replyRejection(good)).toBeNull();
	});

	it("names a non-object", () => {
		for (const message of [null, undefined, 7, "ok", true]) {
			expect(replyRejection(message), String(message)).toBe("not-an-object");
		}
	});

	it("names a wrong protocol version, which is a stale build rather than a bug", () => {
		expect(replyRejection({ ...good, v: 2 })).toBe("version");
		expect(replyRejection({ ...good, v: undefined })).toBe("version");
	});

	it("names a malformed shape", () => {
		expect(replyRejection({ ...good, id: "1" })).toBe("shape");
		expect(replyRejection({ ...good, id: 1.5 })).toBe("shape");
		expect(replyRejection({ ...good, ok: "yes" })).toBe("shape");
	});

	it("refuses an ok reply carrying no value, because that is a dropped reply", () => {
		// This is why WireAck exists: a client cannot tell "the host sent nothing"
		// from "the host sent a result-less reply", and the terminator is what
		// removes the ambiguity.
		expect(replyRejection({ v: 1, id: 1, ok: true })).toBe("shape");
	});

	it("refuses a failure reply with an incomplete error", () => {
		const base = { v: 1 as const, id: 1, ok: false };
		expect(replyRejection({ ...base, error: { code: 6000, message: "x" } })).toBe("shape");
		expect(
			replyRejection({
				...base,
				error: { code: 6000, message: "x", docState: "Unchanged", retryable: false },
			}),
		).toBeNull();
	});
});
