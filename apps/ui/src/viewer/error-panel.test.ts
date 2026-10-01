/**
 * SL-4.UI.12: the failure panel shows what happened, what it means for the
 * document, and what to do next - and it never invents any of the three.
 */
import { describe, expect, it } from "vitest";
import type { DocState } from "../platform/errors.js";
import { REGISTRY } from "./error-codes.js";
import { type FailureStrings, failurePanel, failurePanelFromWire } from "./error-panel.js";
import { errorState } from "./errors.js";

/** Stands in for the host catalogue. Never real sentences - see UI.02. */
const strings: FailureStrings = {
	actionLabel: (action) => `do:${action}`,
	docStateLabel: (state) => `doc:${state}`,
	unknownCode: (code) => `unknown ${code}`,
};

describe("failurePanel", () => {
	it("shows the engine's sentence without rewording it", () => {
		// The registry authors one sentence per code. A paraphrase here would be
		// a second phrasing that drifts, and the registry's would be dead.
		const panel = failurePanel(errorState(1000), "This file does not look like a PDF.", strings);
		expect(panel.message).toBe("This file does not look like a PDF.");
	});

	it("always offers an action, because a dead end is the bug being fixed", () => {
		for (const code of REGISTRY.keys()) {
			const panel = failurePanel(errorState(code), "x", strings);
			expect(panel.actionLabel, `code ${code} has no action label`).not.toBe("");
		}
	});

	it("always states what happened to the document", () => {
		// "The operation failed" and "your document is unaffected" are different
		// facts. Leaving the second out is how a panel loses a reader's trust.
		for (const code of REGISTRY.keys()) {
			const panel = failurePanel(errorState(code), "x", strings);
			expect(panel.docStateLabel, `code ${code} has no document line`).not.toBe("");
		}
	});

	it("carries the severity through, so the host can style the panel", () => {
		expect(failurePanel(errorState(1000), "x", strings).severity).toBe("blocked");
		expect(failurePanel(errorState(1000, "PartiallyLoaded"), "x", strings).severity).toBe(
			"degraded",
		);
	});

	it("surfaces the number only when the registry does not know it", () => {
		// For a known code the sentence is what a reader needs and the number is
		// noise; for an unknown one it is the only route to a bug report.
		const known = failurePanel(errorState(1000), "x", strings);
		expect(known.showCode).toBe(false);
		const unknown = failurePanel(errorState(424_242), undefined, strings);
		expect(unknown.showCode).toBe(true);
	});

	it("falls back to the catalogue sentence for an unknown code", () => {
		// There is no registry message to fall back to, and an empty panel would
		// tell the user nothing at all.
		const panel = failurePanel(errorState(424_242), undefined, strings);
		expect(panel.message).toBe("unknown 424242");
		expect(panel.unknown).toBe(true);
	});

	it("leaves a known code with no engine message EMPTY rather than inventing one", () => {
		// A known code whose sentence went missing means the shell dropped a
		// field. Substituting a generic phrase would hide that, so the panel is
		// visibly short instead and the host can notice.
		const panel = failurePanel(errorState(1000), undefined, strings);
		expect(panel.message).toBe("");
		expect(panel.actionLabel).not.toBe("");
	});

	it("marks a non-retryable retry as not actionable", () => {
		for (const row of REGISTRY.values()) {
			if (row.retryable) continue;
			const panel = failurePanel(errorState(row.id), "x", strings);
			expect(panel.action, `code ${row.id} offers retry`).not.toBe("retry");
		}
	});
});

describe("failurePanelFromWire", () => {
	it("takes the code, message and docState off one wire reply", () => {
		// Taking the wire shape rather than loose arguments is what stops a
		// caller pairing one failure's code with another failure's sentence.
		const wire = {
			code: 1000,
			message: "This file does not look like a PDF.",
			docState: "NotLoaded",
		};
		const panel = failurePanelFromWire(wire, strings);
		expect(panel.code).toBe(1000);
		expect(panel.message).toBe("This file does not look like a PDF.");
		expect(panel.docState).toBe("NotLoaded");
	});

	it("treats a wire reply with no code as unknown, not as fine", () => {
		// `code: null` means the shell could not read it. Defaulting to a
		// known-looking state would be a guess about the user's document.
		const panel = failurePanelFromWire({ code: null }, strings);
		expect(panel.unknown).toBe(true);
		expect(panel.severity).toBe("blocked");
	});

	it("lets the engine's docState win over the registry default", () => {
		const wire: { code: number; docState: DocState } = { code: 1000, docState: "Loaded" };
		const panel = failurePanelFromWire(wire, strings);
		expect(panel.docState).toBe("Loaded");
		expect(panel.severity).toBe("info");
	});
});
