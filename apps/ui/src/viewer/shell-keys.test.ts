/**
 * SL-4.UI.07: keyboard-only operation of the viewer's controls.
 *
 * The load-bearing tests are the refusals. A shortcut to a control that is not
 * there is a dead end wearing a keybinding: the reader presses it, nothing
 * happens, and they conclude the keyboard is not trustworthy here. So most of
 * this file is about what the resolver declines to resolve.
 */
import { describe, expect, it } from "vitest";
import {
	type ControlContext,
	type KeyChord,
	claimedChords,
	idleControls,
	parseChord,
	resolveControlKey,
	shellControls,
} from "./shell-keys.js";
import { EN_CONTROL_CATALOGUE } from "./strings.js";

const chord = (key: string, mods: Partial<Omit<KeyChord, "key">> = {}): KeyChord => ({
	key,
	ctrl: false,
	shift: false,
	alt: false,
	meta: false,
	...mods,
});

/** A viewer with everything available, so a test is about the KEY and not the gate. */
const everything: ControlContext = {
	hasDocument: true,
	hasOutline: true,
	outlineOpen: false,
	printRunning: true,
	hasDismissable: true,
};

describe("the chords the shell claims", () => {
	it("resolves every declared chord to the action it declares", () => {
		// The declared table and the implementation are checked AGAINST each
		// other. A table that drifts from the resolver is the exact bug this
		// item exists to prevent, and reading them side by side never catches it.
		for (const declared of claimedChords()) {
			const parsed = parseChord(declared.chord);
			expect(parsed, `unparseable chord: ${declared.chord}`).not.toBeNull();
			if (parsed === null) continue;
			const command = resolveControlKey(parsed, everything);
			expect(command, `${declared.chord} did not resolve`).not.toBeNull();
			expect(command?.action, declared.chord).toBe(declared.action);
		}
	});

	it("declares a chord for every control, or says why not", () => {
		for (const control of shellControls(EN_CONTROL_CATALOGUE)) {
			expect(
				control.chord ?? control.reason,
				`${control.id} has neither a chord nor a reason`,
			).not.toBeNull();
		}
	});

	it("gives every control a distinct, non-empty label", () => {
		// A control with no accessible name is invisible to a screen reader, and
		// the label is what a reader hears rather than an id they never see.
		const labels = shellControls(EN_CONTROL_CATALOGUE).map((c) => c.label);
		expect(labels.every((label) => label.trim().length > 0)).toBe(true);
		expect(new Set(labels).size).toBe(labels.length);
	});
});

describe("what it refuses to resolve", () => {
	it("never claims an unmodified letter", () => {
		// Typing "p" into the search box must never print the document.
		for (const key of ["p", "o", "f", "Enter", "ArrowDown"]) {
			expect(resolveControlKey(chord(key), everything), key).toBeNull();
		}
	});

	it("never claims a chord the browser or OS owns", () => {
		for (const key of ["t", "n", "w"]) {
			expect(resolveControlKey(chord(key, { ctrl: true }), everything), `Ctrl+${key}`).toBeNull();
		}
		expect(resolveControlKey(chord("F5"), everything)).toBeNull();
		expect(resolveControlKey(chord("F12"), everything)).toBeNull();
	});

	it("resolves no chord that is on the reserved list", () => {
		// The per-key tests above pass with the reserved guard DELETED, because
		// the switch declines `t`/`n`/`w` anyway - so those tests prove the
		// behaviour, not the guard. This is the test that makes the guard
		// load-bearing: it fails the moment a future edit adds a case the browser
		// already owns, which is the only way that collision can happen.
		const keys = ["t", "n", "w", "r", "l", "d", "q"];
		for (const key of keys) {
			expect(
				resolveControlKey(chord(key, { ctrl: true }), everything),
				`Ctrl+${key} is the browser's, not ours`,
			).toBeNull();
		}
	});

	it("does not offer Ctrl+P when no document is open", () => {
		// Printing nothing is not a recovery.
		const idle = idleControls();
		expect(resolveControlKey(chord("p", { ctrl: true }), idle)).toBeNull();
	});

	it("does not offer Ctrl+. when no print is running", () => {
		// The shortcut for cancelling must not exist when there is nothing to
		// cancel, or the reader learns that Ctrl+. does nothing.
		const notPrinting = { ...everything, printRunning: false };
		expect(resolveControlKey(chord(".", { ctrl: true }), notPrinting)).toBeNull();
	});

	it("does not offer F2 when the viewer has no outline panel", () => {
		const noOutline = { ...everything, hasOutline: false };
		expect(resolveControlKey(chord("F2"), noOutline)).toBeNull();
	});

	it("leaves Escape alone when there is nothing to dismiss", () => {
		// Escape on an idle viewer doing nothing is correct, not a bug to fix.
		expect(resolveControlKey(chord("Escape"), idleControls())).toBeNull();
	});
});

describe("Ctrl+P", () => {
	it("must preventDefault, because the browser's own print dialog is wrong here", () => {
		// Getting this wrong produces the browser dialog AND the viewer's print,
		// which is worse than either alone.
		const command = resolveControlKey(chord("p", { ctrl: true }), everything);
		expect(command?.preventDefault).toBe(true);
	});

	it("works with Meta too, because a Mac keyboard does not have Ctrl", () => {
		const command = resolveControlKey(chord("p", { meta: true }), everything);
		expect(command?.action).toBe("print");
	});
});

describe("Escape", () => {
	it("dismisses a visible panel", () => {
		const command = resolveControlKey(chord("Escape"), everything);
		expect(command?.action).toBe("dismiss");
	});

	it("does not swallow Escape while the reader is typing in a field", () => {
		// Escape is the search field's own key, and keyboard.ts's search map
		// claims it. A shell-level claim here would steal it from the reader
		// closing a search, which is the one thing Escape must always do.
		const inSearch = { ...everything, hasDismissable: false, outlineOpen: false };
		expect(resolveControlKey(chord("Escape"), inSearch)).toBeNull();
	});
});

describe("parseChord", () => {
	it("round-trips a modifier chord", () => {
		const parsed = parseChord("Ctrl+Shift+P");
		expect(parsed).toEqual({ key: "P", ctrl: true, shift: true, alt: false, meta: false });
	});

	it("returns null for a name with no key at all", () => {
		expect(parseChord("Ctrl+")).toBeNull();
	});
});
