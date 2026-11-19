/** SL-5.JS.02/03 — the browser mirror of consent + admin origin policy. */

import { describe, expect, it } from "vitest";
import {
	ApprovedOrigins,
	ConsentPolicy,
	type DocumentIdentity,
	authorizeScripting,
	denyAllAuthorization,
	documentIdentity,
} from "./js-policy.js";

const doc: DocumentIdentity = documentIdentity("tax-form-2026", "https://forms.example");

describe("JS.02/03 scripting policy", () => {
	it("denies by default", () => {
		const decision = authorizeScripting(denyAllAuthorization(), doc);
		expect(decision).toEqual({ kind: "deny", code: "js-consent-required" });
	});

	it("keys consent on the document id", () => {
		const consent = new ConsentPolicy();
		consent.grant(doc, "session");
		expect(consent.isConsented(doc)).toBe(true);
		expect(consent.scope(doc)).toBe("session");
		expect(consent.isConsented(documentIdentity("other", doc.origin))).toBe(false);
		expect(consent.grantCount).toBe(1);
		expect(consent.revoke(doc)).toBe(true);
		expect(consent.isConsented(doc)).toBe(false);
	});

	it("admin origin pre-approval is the explicit exception", () => {
		const origins = new ApprovedOrigins();
		expect(origins.isApproved(doc)).toBe(false);
		origins.approveOrigin("https://forms.example");
		expect(origins.isApproved(doc)).toBe(true);
		expect(authorizeScripting({ consent: new ConsentPolicy(), origins }, doc)).toEqual({
			kind: "allow",
		});
		expect(origins.revokeOrigin("https://forms.example")).toBe(true);
		expect(origins.isApproved(doc)).toBe(false);
	});

	it("admin can approve a single document id", () => {
		const origins = new ApprovedOrigins();
		origins.approveDocument("tax-form-2026");
		expect(origins.isApproved(doc)).toBe(true);
		expect(origins.size).toBe(1);
		expect(origins.revokeDocument("tax-form-2026")).toBe(true);
		expect(origins.isApproved(doc)).toBe(false);
	});
});
