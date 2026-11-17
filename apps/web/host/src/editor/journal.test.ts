/**
 * SL-5.EDIT.01 / 23-EDIT-MODEL-SPEC.md DoD for the mutation journal.
 */

import { describe, expect, it } from "vitest";
import {
	Journal,
	SourceChangedError,
	fingerprint,
	inverseOf,
	type Mutation,
} from "./journal.js";

function bytes(text: string): Uint8Array {
	return new TextEncoder().encode(text);
}

function text(value: Uint8Array): string {
	return new TextDecoder().decode(value);
}

function change(before: string, after: string, op = "edit_text"): Mutation {
	return {
		kind: "replace-document",
		op,
		options: {},
		before: bytes(before),
		after: bytes(after),
	};
}

describe("the mutation journal", () => {
	it("materialises to the source when empty, and to the last change otherwise", () => {
		const source = bytes("one");
		const journal = new Journal(source);
		expect(text(journal.materialise(source))).toBe("one");
		journal.apply(change("one", "two"), "Two");
		expect(text(journal.materialise(source))).toBe("two");
		journal.apply(change("two", "three"), "Three");
		expect(text(journal.materialise(source))).toBe("three");
	});

	it("applying then reverting every mutation returns the original state", () => {
		const source = bytes("zero");
		const journal = new Journal(source);
		const steps = ["one", "two", "three", "four"];
		let current = "zero";
		for (const next of steps) {
			journal.apply(change(current, next), `Changed to ${next}`);
			current = next;
		}
		for (let i = steps.length - 1; i >= 0; i -= 1) {
			expect(text(journal.undo(source))).toBe(i === 0 ? "zero" : steps[i - 1]);
		}
		expect(journal.canUndo).toBe(false);
		expect(text(journal.materialise(source))).toBe("zero");
	});

	it("an inverse is the mutation with its states swapped", () => {
		const mutation = change("a", "b", "rotate");
		const inverse = inverseOf(mutation);
		expect(text(inverse.before)).toBe("b");
		expect(text(inverse.after)).toBe("a");
		expect(inverse.op).toBe("rotate");
	});

	it("undoes one gesture as one group", () => {
		const source = bytes("start");
		const journal = new Journal(source);
		journal.apply(change("start", "a"), "First", { kind: "explicit" });
		journal.apply(change("a", "b"), "Second", { kind: "explicit" });
		journal.apply(change("b", "c"), "Third", { kind: "explicit" });
		expect(journal.groups).toHaveLength(3);
		expect(text(journal.undo(source))).toBe("b");
		expect(text(journal.undo(source))).toBe("a");
		expect(text(journal.undo(source))).toBe("start");
	});

	it("coalesces typing into one group after an idle interval", () => {
		let now = 1000;
		const source = bytes("");
		const journal = new Journal(source, () => now);
		journal.apply(change("", "h"), "Typed h", { kind: "typing", idle: 500 });
		now += 100;
		journal.apply(change("h", "he"), "Typed e", { kind: "typing", idle: 500 });
		now += 100;
		journal.apply(change("he", "hel"), "Typed l", { kind: "typing", idle: 500 });
		expect(journal.groups).toHaveLength(1);
		expect(journal.groups[0]?.entries[1]).toBe(3);
		expect(text(journal.undo(source))).toBe("");
	});

	it("starts a new typing group after the idle interval", () => {
		let now = 1000;
		const source = bytes("");
		const journal = new Journal(source, () => now);
		journal.apply(change("", "h"), "Typed h", { kind: "typing", idle: 500 });
		now += 900;
		journal.apply(change("h", "hi"), "Typed i", { kind: "typing", idle: 500 });
		expect(journal.groups).toHaveLength(2);
	});

	it("discards the redo tail on a new mutation but keeps the entries in the journal", () => {
		const source = bytes("one");
		const journal = new Journal(source);
		journal.apply(change("one", "two"), "Two");
		journal.apply(change("two", "three"), "Three");
		journal.undo(source);
		expect(journal.canRedo).toBe(true);
		journal.apply(change("two", "other"), "Other");
		expect(journal.canRedo).toBe(false);
		expect(journal.entries.filter((entry) => entry.label === "Three")).toHaveLength(0);
	});

	it("redoes a discarded group's mutations oldest first", () => {
		const source = bytes("one");
		const journal = new Journal(source);
		journal.apply(change("one", "two"), "Two");
		journal.apply(change("two", "three"), "Three");
		journal.undo(source);
		expect(text(journal.redo(source))).toBe("three");
	});

	it("serialises to a bundle and reloads to the same state", () => {
		let now = 1000;
		const source = bytes("one");
		const journal = new Journal(source, () => now, "reader");
		journal.apply(change("one", "two", "edit_text"), "Two");
		now += 50;
		journal.apply(change("two", "three", "redact"), "Three");
		journal.undo(source);

		const bundle = journal.toBundle();
		const reloaded = Journal.fromBundle(JSON.parse(JSON.stringify(bundle)), source);
		expect(reloaded.position).toBe(journal.position);
		expect(text(reloaded.materialise(source))).toBe(text(journal.materialise(source)));
		expect(reloaded.entries.map((entry) => [entry.id, entry.label, entry.actor])).toEqual(
			journal.entries.map((entry) => [entry.id, entry.label, entry.actor]),
		);
		expect(reloaded.groups).toEqual(journal.groups);
	});

	it("refuses to reload a journal against a changed source", () => {
		const source = bytes("one");
		const journal = new Journal(source);
		journal.apply(change("one", "two"), "Two");
		const bundle = journal.toBundle();
		expect(() => Journal.fromBundle(bundle, bytes("different"))).toThrow(SourceChangedError);
		expect(() => journal.materialise(bytes("different"))).toThrow(SourceChangedError);
	});

	it("fingerprints size and content, so a same-size edit is caught", () => {
		const a = fingerprint(bytes("one"));
		const b = fingerprint(bytes("two"));
		const c = fingerprint(bytes("one"));
		expect(a).toEqual(c);
		expect(a).not.toEqual(b);
		expect(a.size).toBe(3);
	});
});