/**
 * The session store's own rules.
 *
 * The storage is the browser's ✓ — the test cannot reach it in Node ✓ — so
 * what is tested here is the rule the workspace applies to it: which
 * documents are kept, and what a kept document says.
 */

import { describe, expect, it } from "vitest";
import { MAX_KEPT_BYTES, canKeep } from "./session.js";

describe("the document the workspace keeps", () => {
	it("keeps a document of an ordinary size", () => {
		expect(canKeep(new Uint8Array(1024))).toBe(true);
	});

	it("keeps a document right at the limit", () => {
		expect(canKeep(new Uint8Array(MAX_KEPT_BYTES))).toBe(true);
	});

	it("does not keep a document past the limit", () => {
		// A document this big is still perfectly editable; it simply is not
		// written into the browser's storage on every change.
		expect(canKeep(new Uint8Array(MAX_KEPT_BYTES + 1))).toBe(false);
	});
});
