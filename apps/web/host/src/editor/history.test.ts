/**
 * The editor's undo stack (pdf-plan/39).
 *
 * The property that matters is exactness: an undo restores the document's
 * bytes, not a reconstruction of them. These tests are about that, and about
 * the two things a bounded stack has to get right — what happens when it is
 * full, and what happens to the redo tail when the document changes again.
 */

import { describe, expect, it } from "vitest";
import { DocumentHistory, HISTORY_LIMIT } from "./history.js";

function bytes(text: string): Uint8Array {
	return new TextEncoder().encode(text);
}

function text(state: { readonly bytes: Uint8Array }): string {
	return new TextDecoder().decode(state.bytes);
}

describe("the journal", () => {
	it("records which operation made each state, when, and in what order", () => {
		let now = 1000;
		const history = new DocumentHistory(5, () => {
			now += 10;
			return now;
		});
		history.reset("Opened", bytes("one"));
		history.push("Changed the text", bytes("two"), { op: "edit_text" });
		history.push("Redacted a region", bytes("three"), { op: "redact" });

		const entries = history.entries;
		expect(entries.map((entry) => entry.operation?.op ?? null)).toEqual([
			null,
			"edit_text",
			"redact",
		]);
		expect(entries.map((entry) => entry.id)).toEqual([1, 2, 3]);
		expect(entries.map((entry) => entry.at)).toEqual([1010, 1020, 1030]);
	});

	it("the inverse of an entry is the bytes of the state before it", () => {
		const history = new DocumentHistory();
		history.reset("Opened", bytes("one"));
		history.push("Rotated", bytes("two"), { op: "rotate" });
		history.push("Deleted", bytes("three"), { op: "delete" });

		expect(history.inverseOf(0)).toBeNull();
		expect(
			new TextDecoder().decode(history.inverseOf(1) ?? new Uint8Array()),
		).toBe("one");
		expect(
			new TextDecoder().decode(history.inverseOf(2) ?? new Uint8Array()),
		).toBe("two");
		expect(history.inverseOf(3)).toBeNull();
	});

	it("keeps operation records across undo and redo", () => {
		const history = new DocumentHistory();
		history.reset("Opened", bytes("one"));
		history.push("Changed", bytes("two"), { op: "edit_text" });
		history.undo();
		expect(history.current?.operation).toBeNull();
		history.redo();
		expect(history.current?.operation?.op).toBe("edit_text");
	});
});

describe("the undo stack", () => {
	it("starts with the document as it was opened, and nothing to undo", () => {
		const history = new DocumentHistory();
		history.reset("Opened report.pdf", bytes("original"));
		expect(history.depth).toBe(1);
		expect(history.canUndo).toBe(false);
		expect(history.canRedo).toBe(false);
		expect(history.current?.label).toBe("Opened report.pdf");
		expect(text(history.current ?? { bytes: bytes("") })).toBe("original");
	});

	it("undoes and redoes by handing back the state to restore", () => {
		const history = new DocumentHistory();
		history.reset("Opened", bytes("one"));
		history.push("Rotated page 1", bytes("two"));
		history.push("Deleted page 2", bytes("three"));

		expect(history.canUndo).toBe(true);
		expect(history.undoLabel).toBe("Deleted page 2");
		expect(history.redoLabel).toBeNull();

		const undone = history.undo();
		expect(undone).not.toBeNull();
		expect(text(undone ?? { bytes: bytes("") })).toBe("two");
		expect(history.canRedo).toBe(true);
		expect(history.redoLabel).toBe("Deleted page 2");

		const redone = history.redo();
		expect(text(redone ?? { bytes: bytes("") })).toBe("three");
		expect(history.canRedo).toBe(false);
	});

	it("refuses to go past either end", () => {
		const history = new DocumentHistory();
		history.reset("Opened", bytes("one"));
		expect(history.undo()).toBeNull();
		history.push("Changed", bytes("two"));
		expect(history.redo()).toBeNull();
	});

	it("discards the redo tail when the document changes again", () => {
		// Once the document has moved on, the future it had is gone — the same
		// rule every editor follows.
		const history = new DocumentHistory();
		history.reset("Opened", bytes("one"));
		history.push("Changed", bytes("two"));
		history.undo();
		expect(history.canRedo).toBe(true);
		history.push("Changed differently", bytes("three"));
		expect(history.canRedo).toBe(false);
		expect(history.depth).toBe(2);
	});

	it("keeps its own copy of the bytes, so a later change cannot reach into it", () => {
		const history = new DocumentHistory();
		const original = bytes("original");
		history.reset("Opened", original);
		// The caller mutates the buffer it handed over.
		original[0] = 0x58;
		expect(text(history.current ?? { bytes: bytes("") })).toBe("original");
	});

	it("drops the oldest state once it is full, and stays within its depth", () => {
		const history = new DocumentHistory(3);
		history.reset("Opened", bytes("0"));
		history.push("one", bytes("1"));
		history.push("two", bytes("2"));
		history.push("three", bytes("3"));
		expect(history.depth).toBe(3);
		// The oldest state is gone, so the deepest undo reaches "one".
		history.undo();
		history.undo();
		expect(text(history.current ?? { bytes: bytes("") })).toBe("1");
		expect(history.canUndo).toBe(false);
	});

	it("refuses a depth that could hold nothing", () => {
		expect(() => new DocumentHistory(0)).toThrow(RangeError);
		expect(() => new DocumentHistory(1.5)).toThrow(RangeError);
	});

	it("defaults to a bounded depth", () => {
		const history = new DocumentHistory();
		expect(history.limit).toBe(HISTORY_LIMIT);
		expect(HISTORY_LIMIT).toBeGreaterThan(1);
	});
});

describe("the history as the reader sees it", () => {
	it("lists every state, oldest first, and says which one it is in", () => {
		const history = new DocumentHistory();
		history.reset("Opened", bytes("one"));
		history.push("Rotated page 1", bytes("two"));
		history.push("Added a text box", bytes("three"));

		expect(history.entries.map((entry) => entry.label)).toEqual([
			"Opened",
			"Rotated page 1",
			"Added a text box",
		]);
		expect(history.cursor).toBe(2);
	});

	it("moves the cursor to a chosen state without changing it", () => {
		const history = new DocumentHistory();
		history.reset("Opened", bytes("one"));
		history.push("Changed", bytes("two"));

		history.setCursor(0);
		expect(history.cursor).toBe(0);
		expect(history.canUndo).toBe(false);
		expect(history.canRedo).toBe(true);
		// The bytes are still the ones that were pushed.
		expect(text(history.current ?? { bytes: bytes("") })).toBe("one");
	});

	it("refuses a state that is not in the history", () => {
		const history = new DocumentHistory();
		history.reset("Opened", bytes("one"));
		expect(() => history.setCursor(1)).toThrow(RangeError);
		expect(() => history.setCursor(-1)).toThrow(RangeError);
	});
});
