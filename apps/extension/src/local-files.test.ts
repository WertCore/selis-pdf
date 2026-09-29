/**
 * SL-4.EXT.09 — the `file://` flow's decisions, asserted as decisions.
 *
 * Everything here is pure, so the whole state space of a permission flow is
 * checkable without a browser and without a DOM. That matters more than usual
 * for this task: the flow's job is to say four *different* things correctly —
 * granted, not granted, granted-but-still-failing, and the browser would not say
 * — and a page that collapsed any two of them into one sentence would still look
 * fine in a screenshot of the happy path.
 *
 * The assertions that matter most are the negative ones: a reader function that
 * is never called, and a URL builder that refuses to build.
 */

import { describe, expect, it } from "vitest";
import { ALLOWED_OPTIONAL_HOST_PERMISSIONS, DENIED_HOST_PATTERNS } from "./permissions.js";
import {
	EXTENSIONS_PAGE,
	FILE_ACCESS_TOGGLE,
	type FileAccess,
	type LocalRead,
	classifySourceUrl,
	extensionDetailsUrl,
	isExtensionId,
	isLocalFileUrl,
	planLocalFileAccess,
	readLocalFile,
} from "./local-files.js";

/** A reader that records whether it was called and refuses to succeed. */
function spyReader(): { calls: string[]; read: (url: string) => Promise<ArrayBuffer> } {
	const calls: string[] = [];
	return {
		calls,
		read: (url) => {
			calls.push(url);
			return Promise.reject(new Error("the file could not be read"));
		},
	};
}

describe("EXT.09 the declared pattern is Chrome's, and only Chrome's", () => {
	it("declares the three-slash form, which is the one Chrome documents", () => {
		// Match-pattern reference: `file:///` "allows your extension to run on local
		// files. This pattern requires the user to manually grant access. Note that
		// this case requires three slashes, not two."
		expect(ALLOWED_OPTIONAL_HOST_PERMISSIONS).toEqual(["file:///"]);
		expect(ALLOWED_OPTIONAL_HOST_PERMISSIONS[0]?.match(/\//g)).toHaveLength(3);
	});

	it("keeps the two-slash file spellings denied, in both fields", () => {
		// The `file:` entries in the denylist are the two-slash forms, and the
		// denylist applies to `host_permissions` and `optional_host_permissions`
		// alike. Neither of them is the pattern we ship.
		const filePatterns = DENIED_HOST_PATTERNS.filter((pattern) => pattern.startsWith("file:"));
		expect(filePatterns).toHaveLength(2);
		for (const pattern of filePatterns) {
			expect(pattern, pattern).not.toBe("file:///");
			expect(ALLOWED_OPTIONAL_HOST_PERMISSIONS, pattern).not.toContain(pattern);
		}
	});
});

describe("EXT.09 the extension details deep link", () => {
	const ID = "abcdefghijklmnopabcdefghijklmnop";

	it("addresses this extension's own page on Chrome's extensions screen", () => {
		expect(extensionDetailsUrl(ID)).toBe(`${EXTENSIONS_PAGE}?id=${ID}`);
	});

	it("builds nothing at all for a value that is not an extension id", () => {
		// `chrome://extensions/?id=` would open a list of *every* extension, which
		// reads as a broken deep link rather than as "could not be identified" — so
		// the flow gets an empty string and falls back on the written steps.
		for (const bad of ["", "   ", "not-an-id", ID.toUpperCase(), `${ID}extra`, "../etc"]) {
			expect(extensionDetailsUrl(bad), bad).toBe("");
		}
	});

	it("uses Chrome's own id alphabet, so nothing else can be an id", () => {
		expect(isExtensionId(ID)).toBe(true);
		// `z` is outside a-p, and an unpacked extension has no id at all.
		expect(isExtensionId("zbcdefghijklmnopabcdefghijklmnop")).toBe(false);
		expect(isExtensionId("")).toBe(false);
	});
});


describe("EXT.09 readLocalFile asks before it reads, and does not read without the grant", () => {
	it("never calls the reader when the grant is withheld", async () => {
		const { calls, read } = spyReader();
		expect(await readLocalFile("withheld", "file:///C:/x.pdf", read)).toBe("skipped");
		expect(calls, "a file the extension may not read must not be read").toEqual([]);
	});

	it("never calls the reader when the browser would not say", async () => {
		const { calls, read } = spyReader();
		expect(await readLocalFile("unknown", "file:///C:/x.pdf", read)).toBe("skipped");
		expect(calls).toEqual([]);
	});

	it("calls the reader when the grant is on", async () => {
		const calls: string[] = [];
		const read = await readLocalFile("granted", "file:///C:/x.pdf", (url) => {
			calls.push(url);
			return Promise.resolve(new ArrayBuffer(4));
		});
		expect(read).toBe("ok");
		expect(calls).toEqual(["file:///C:/x.pdf"]);
	});

	it("reports a failed read as failed, not as a refusal", async () => {
		const { read } = spyReader();
		expect(await readLocalFile("granted", "file:///C:/x.pdf", read)).toBe("failed");
	});
});

describe("EXT.09 planLocalFileAccess names the cause, and only the cause it knows", () => {
	const table: readonly (readonly [FileAccess, LocalRead, string])[] = [
		["granted", "ok", "open"],
		["granted", "failed", "unreadable"],
		["granted", "skipped", "unreadable"],
		["withheld", "ok", "open"],
		["withheld", "failed", "request"],
		["withheld", "skipped", "request"],
		["unknown", "ok", "open"],
		["unknown", "failed", "unchecked"],
		["unknown", "skipped", "unchecked"],
	];

	it.each(table)("access %s with a read of %s is %s", (access, read, expected) => {
		expect(planLocalFileAccess(access, read)).toBe(expected);
	});

	it("covers every state of the flow, and no verdict is unreachable", () => {
		// The table above *is* the claim: nine cells, four verdicts. If a state were
		// missing it would be a state the page has no sentence for, and if a verdict
		// were unreachable it would be copy nothing can show.
		expect(new Set(table.map(([, , verdict]) => verdict))).toEqual(
			new Set(["open", "unreadable", "request", "unchecked"]),
		);
		for (const access of ["granted", "withheld", "unknown"] as const) {
			for (const read of ["ok", "failed", "skipped"] as const) {
				expect(typeof planLocalFileAccess(access, read)).toBe("string");
			}
		}
	});

	it("opens on a successful read whatever the grant says", () => {
		// A read that worked cannot have needed permission, so `ok` wins outright.
		// This is the "on grant it proceeds" half, stated as a claim.
		for (const access of ["granted", "withheld", "unknown"] as const) {
			expect(planLocalFileAccess(access, "ok"), access).toBe("open");
		}
	});

	it("does not claim a refusal from a browser that would not answer", () => {
		expect(planLocalFileAccess("unknown", "skipped")).toBe("unchecked");
	});
});


describe("EXT.09 classifySourceUrl", () => {
	it("separates web from local from refused", () => {
		expect(classifySourceUrl("https://example.com/a.pdf")?.kind).toBe("web");
		expect(classifySourceUrl("http://example.com/a.pdf")?.kind).toBe("web");
		expect(classifySourceUrl("file:///C:/a.pdf")?.kind).toBe("local");
		for (const url of ["data:application/pdf;base64,JVBER", "blob:https://x.test/1", "wss://x.test/a"]) {
			expect(classifySourceUrl(url)?.kind, url).toBe("unsupported");
		}
	});

	it("returns null for something that is not a URL, which is a different failure", () => {
		// `null` and `unsupported` get different sentences: one is a typo and the
		// other is a scheme this extension will never open.
		expect(classifySourceUrl("not a url")).toBeNull();
		expect(classifySourceUrl("")).toBeNull();
	});

	it("re-serialises rather than passing a raw string through", () => {
		// The value the page ends up reading is a parsed URL, so a crafted `?src=`
		// cannot carry a second parameter or a truncated path into a fetch.
		const classified = classifySourceUrl("file:///C:/My Documents/a b.pdf#page=3");
		expect(classified?.url).toBe("file:///C:/My%20Documents/a%20b.pdf#page=3");
	});

	it("agrees with the one-line form", () => {
		expect(isLocalFileUrl("file:///C:/a.pdf")).toBe(true);
		expect(isLocalFileUrl("https://example.com/a.pdf")).toBe(false);
		expect(isLocalFileUrl("nonsense")).toBe(false);
	});
});

describe("EXT.09 the copy names the browser's own switch", () => {
	it("uses Chrome's exact wording, so the reader is looking for the right control", () => {
		expect(FILE_ACCESS_TOGGLE).toBe("Allow access to file URLs");
	});
});
