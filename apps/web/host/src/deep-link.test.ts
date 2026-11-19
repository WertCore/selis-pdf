/**
 * SL-4.EXT.08 — deep links into the web app for edit actions.
 *
 * The parser is pure (no DOM, no storage, no fetch), so the "document id" is
 * asserted as data. These cases pin the local-only contract: a link yields an
 * `edit` action and a validated opaque handle id, and everything that could
 * smuggle bytes, a foreign scheme, or a traversal is refused before a caller
 * ever sees an action.
 */

import { describe, expect, it } from "vitest";
import {
	EDIT_ACTION,
	buildEditDeepLink,
	parseEditDeepLink,
	type EditDeepLinkResult,
} from "./deep-link.js";

const APP = "https://pdf.selis.com/edit-pdf";

function ok(result: EditDeepLinkResult) {
	if (!result.ok) {
		throw new Error(`expected an accepted deep link, got: ${result.reason}`);
	}
	return result;
}

describe("SL-4.EXT.08 edit deep links", () => {
	it("parses an edit action and the local document id", () => {
		const result = ok(parseEditDeepLink(`${APP}?action=edit&doc=opfs_7f3a9c`));
		expect(result.action).toBe(EDIT_ACTION);
		expect(result.action).toBe("edit");
		expect(result.documentId).toBe("opfs_7f3a9c");
		expect(result.handle).toBe("opfs");
	});

	it("accepts an explicit FSA handle and a root-relative reference", () => {
		const result = ok(parseEditDeepLink("/edit-pdf?action=edit&doc=abc123&handle=fsa", APP));
		expect(result.documentId).toBe("abc123");
		expect(result.handle).toBe("fsa");
	});

	it("round-trips through the canonical builder", () => {
		const built = buildEditDeepLink(APP, "Doc-42_x", "opfs");
		const result = ok(parseEditDeepLink(built));
		expect(result.documentId).toBe("Doc-42_x");
		expect(result.handle).toBe("opfs");
		expect(built).toContain("action=edit");
	});

	it("refuses a missing or unknown action", () => {
		expect(parseEditDeepLink(APP).ok).toBe(false);
		expect(parseEditDeepLink(`${APP}?action=view&doc=abc`).ok).toBe(false);
	});

	it("refuses a missing or malformed document id", () => {
		expect(parseEditDeepLink(`${APP}?action=edit`).ok).toBe(false);
		for (const doc of ["../secret", "a/b", "a b", "x?y", "a:b", "data:text/plain,hi", "-"]) {
			expect(parseEditDeepLink(`${APP}?action=edit&doc=${encodeURIComponent(doc)}`).ok, doc).toBe(
				false,
			);
		}
		expect(parseEditDeepLink(`${APP}?action=edit&doc=${"a".repeat(129)}`).ok).toBe(false);
	});

	it("refuses unknown handle kinds", () => {
		expect(parseEditDeepLink(`${APP}?action=edit&doc=abc&handle=remote`).ok).toBe(false);
	});

	it("refuses schemes that could smuggle a document or run script", () => {
		for (const link of [
			"javascript:alert(1)",
			"data:text/html,<script>1</script>",
			"file:///etc/passwd",
			"blob:https://pdf.selis.com/uuid",
		]) {
			expect(parseEditDeepLink(link, APP).ok, link).toBe(false);
		}
	});

	it("refuses a link that appears to carry document bytes", () => {
		const link = `${APP}?action=edit&doc=abc&payload=%25PDF-1.7`;
		expect(parseEditDeepLink(link).ok).toBe(false);
	});

	it("refuses userinfo, which is a phishing vector on a handoff link", () => {
		expect(parseEditDeepLink("https://user@pdf.selis.com/edit-pdf?action=edit&doc=abc").ok).toBe(
			false,
		);
	});

	it("the builder refuses the same malformed ids the parser does", () => {
		expect(() => buildEditDeepLink(APP, "../secret")).toThrow(/malformed document id/);
	});
});
