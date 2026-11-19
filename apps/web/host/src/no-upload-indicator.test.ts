/**
 * SL-1A.UI.03 — the no-upload indicator must never lie.
 *
 * The two halves are exercised: a pure-local run (the recorder saw no document
 * bytes) may say "never left"; a consented TSA/OCSP call sends a hash, so the
 * claim must change even though the egress was allowed. The consent model is
 * the one in `apps/cli/src/tsa.rs`: opt-in, notice first, hash only.
 */

import { describe, expect, it } from "vitest";
import {
	EXTERNAL_HASH_CLAIM,
	NEVER_LEFT_CLAIM,
	NoUploadIndicator,
	type ExternalHashEvent,
	type IndicatorElement,
	claimsNeverLeft,
	indicatorFromRequests,
	renderNoUploadIndicator,
	viewFor,
} from "./no-upload-indicator.js";
import type { RecordedRequest } from "./no-upload.js";

const DOC = new TextEncoder().encode("%PDF-1.7\nthis is the local document\n%%EOF");
const LOCAL_REQUEST: RecordedRequest = { url: "/wasm/selis_pdf_wasm.wasm", method: "GET" };

function consentedHash(overrides: Partial<ExternalHashEvent> = {}): ExternalHashEvent {
	return {
		provider: "http://tsa.example/tsa",
		purpose: "timestamp",
		sent: "signature-hash",
		consented: true,
		...overrides,
	};
}

class MockElement implements IndicatorElement {
	textContent = "";
	className = "";
	dataset: Record<string, string | undefined> = {};
	readonly children: MockElement[] = [];
	removed = 0;

	constructor(readonly tagName: string) {}

	append(...children: IndicatorElement[]): void {
		for (const child of children) {
			this.children.push(child as MockElement);
		}
	}

	remove(): void {
		this.removed += 1;
	}

	find(className: string): MockElement | undefined {
		if (this.className.split(/\s+/).includes(className)) {
			return this;
		}
		for (const child of this.children) {
			const found = child.find(className);
			if (found !== undefined) {
				return found;
			}
		}
		return undefined;
	}
}

describe("SL-1A.UI.03 no-upload indicator", () => {
	it("claims 'never left' only for a verified pure-local run", () => {
		const view = indicatorFromRequests({ requests: [LOCAL_REQUEST], documents: [DOC] });
		expect(view.state).toBe("verified");
		expect(view.claim).toBe(NEVER_LEFT_CLAIM);
		expect(view.dataNoUpload).toBe("verified");
		expect(claimsNeverLeft(view)).toBe(true);
	});

	it("withdraws the claim when the recorder finds document bytes", () => {
		const upload: RecordedRequest = {
			url: "https://elsewhere.example/collect",
			method: "POST",
			body: DOC,
		};
		const view = indicatorFromRequests({ requests: [upload], documents: [DOC] });
		expect(view.state).toBe("unknown");
		expect(claimsNeverLeft(view)).toBe(false);
		expect(view.detail).toMatch(/withdrawn/);
	});

	it("a consented hash changes the claim but stays honest about bytes", () => {
		// The TSA request carries a 32-byte hash, not document bytes, so the
		// mechanical no-upload assertion still passes.
		const hashRequest: RecordedRequest = {
			url: "http://tsa.example/tsa",
			method: "POST",
			body: new Uint8Array(32).fill(7),
		};
		const view = indicatorFromRequests({
			requests: [LOCAL_REQUEST, hashRequest],
			documents: [DOC],
			external: [consentedHash()],
		});
		expect(view.state).toBe("external-hash");
		expect(view.claim).toBe(EXTERNAL_HASH_CLAIM);
		expect(claimsNeverLeft(view)).toBe(false);
		expect(view.claim.toLowerCase()).not.toContain("never left");
		expect(view.detail).toContain("http://tsa.example/tsa");
		expect(view.detail).toContain("hash of the signature");
		expect(view.detail).toContain("no document bytes were sent");
		expect(view.dataNoUpload).toBe("external-hash");
	});

	it("an unconsented external contact withdraws the claim even with no upload", () => {
		const view = indicatorFromRequests({
			requests: [],
			documents: [DOC],
			external: [consentedHash({ consented: false })],
		});
		expect(view.state).toBe("unknown");
		expect(claimsNeverLeft(view)).toBe(false);
		expect(view.detail).toMatch(/without a recorded consent/);
	});

	it("a consented OCSP identifier is described by what actually left", () => {
		const view = viewFor(
			"external-hash",
			[consentedHash({ purpose: "revocation", sent: "certificate-identifier" })],
		);
		expect(view.detail).toContain("identifier derived from the certificate");
		expect(view.detail).toContain("revocation");
	});

	it("the staged indicator starts watching, then verifies locally", () => {
		const indicator = new NoUploadIndicator();
		expect(indicator.snapshot().state).toBe("watching");
		expect(claimsNeverLeft(indicator.snapshot())).toBe(false);
		indicator.noteVerified();
		expect(indicator.snapshot().state).toBe("verified");
		expect(claimsNeverLeft(indicator.snapshot())).toBe(true);
	});

	it("a consented external event moves the staged indicator off 'verified' permanently", () => {
		const indicator = new NoUploadIndicator();
		indicator.noteVerified();
		indicator.noteExternalHash(consentedHash());
		const first = indicator.snapshot();
		expect(first.state).toBe("external-hash");
		expect(claimsNeverLeft(first)).toBe(false);
		expect(first.external).toHaveLength(1);
		indicator.noteVerified();
		expect(indicator.snapshot().state).toBe("external-hash");
	});

	it("an unconsented contact is sticky: a later local success cannot erase it", () => {
		const indicator = new NoUploadIndicator();
		indicator.noteExternalHash(consentedHash({ consented: false }));
		expect(indicator.snapshot().state).toBe("unknown");
		indicator.noteVerified();
		indicator.noteWatching();
		expect(indicator.snapshot().state).toBe("unknown");
		expect(claimsNeverLeft(indicator.snapshot())).toBe(false);
	});

	it("render exposes the state and copy through the typed DOM seam", () => {
		const host = new MockElement("div");
		const created: MockElement[] = [];
		const factory = (tag: string): IndicatorElement => {
			const element = new MockElement(tag);
			created.push(element);
			return element;
		};
		const view = indicatorFromRequests({ requests: [], documents: [DOC] });
		const root = renderNoUploadIndicator(view, host, factory) as MockElement;
		expect(root.dataset.role).toBe("no-upload-indicator");
		expect(root.dataset.state).toBe("verified");
		expect(root.dataset.noUpload).toBe("verified");
		expect(root.find("no-upload-claim")?.textContent).toBe(NEVER_LEFT_CLAIM);
		expect(root.find("no-upload-detail")?.textContent).toContain("checked");
		expect(created.map((element) => element.tagName)).toEqual(["div", "strong", "span"]);

		// A state change replaces, never stacks, the claim.
		const external = renderNoUploadIndicator(
			viewFor("external-hash", [consentedHash()]),
			host,
			factory,
		) as MockElement;
		expect(root.removed).toBe(1);
		expect(external.dataset.state).toBe("external-hash");
		expect(external.find("no-upload-claim")?.textContent).toBe(EXTERNAL_HASH_CLAIM);
	});
});
