/** SL-5.JS.02/03 — the browser mirror of consent + admin origin policy. */

import { describe, expect, it } from "vitest";
import {
	AdminPolicyError,
	ApprovedOrigins,
	ConsentPolicy,
	type DocumentIdentity,
	authorizeScripting,
	denyAllAuthorization,
	documentIdentity,
	parseAdminScriptPolicy,
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

	it("an explicit reader decline is never overridden by admin approval", () => {
		const consent = new ConsentPolicy();
		consent.grant(doc, "session");
		consent.decline(doc);
		expect(consent.isDeclined(doc)).toBe(true);
		expect(consent.isConsented(doc)).toBe(false);
		const origins = new ApprovedOrigins();
		origins.approveOrigin(doc.origin);
		origins.approveDocument(doc.id);
		expect(authorizeScripting({ consent, origins }, doc)).toEqual({
			kind: "deny",
			code: "js-declined",
		});
		// A grant cannot quietly undo a decline; only an explicit retraction can.
		consent.grant(doc, "session");
		expect(authorizeScripting({ consent, origins }, doc).kind).toBe("deny");
		expect(consent.undecline(doc)).toBe(true);
		expect(authorizeScripting({ consent, origins }, doc)).toEqual({ kind: "allow" });
	});

	it("an admin policy is explicit, scoped and auditable", () => {
		const origins = parseAdminScriptPolicy("# trusted\norigin https://forms.example\n");
		expect(origins.isApproved(doc)).toBe(true);
		expect(origins.isApproved(documentIdentity("other", "https://other.example"))).toBe(false);
		expect(origins.entries()).toEqual([["origin", "https://forms.example"]]);
		expect(parseAdminScriptPolicy("").size).toBe(0);
	});

	it("a malformed admin policy is a typed refusal", () => {
		for (const text of ["origin\n", "allow everything\n", "origin https://a.example extra\n"]) {
			expect(() => parseAdminScriptPolicy(text)).toThrowError(AdminPolicyError);
		}
	});
});
