/**
 * SL-5.JS.02 — the visible consent gate in front of the browser sandbox.
 *
 * The prompt is exercised with a small jsdom-style DOM double (the module's
 * typed DOM seam is the only surface it touches), and the runner is driven
 * with a Worker whose bootstrap is executed in a fresh Node `vm` realm, so the
 * "field value computed" evidence is the real sandbox output — not a canned
 * reply. Every path asserts the Worker-creation count.
 */

import { runInNewContext } from "node:vm";
import { describe, expect, it } from "vitest";
import { documentIdentity } from "./js-policy.js";
import {
	type ConsentElement,
	CONSENT_QUESTION,
	ScriptConsentController,
	declinePrompter,
	detectFormScripts,
	runFormScriptWithConsent,
	showConsentPrompt,
} from "./js-consent.js";
import { type FormScriptRequest, type WorkerFactory, type WorkerLike } from "./js-sandbox.js";
import { type SandboxRequest, type SandboxResponse } from "./js-worker.js";

const docA = documentIdentity("form-a", "https://forms.example");
const docB = documentIdentity("form-b", "https://forms.example");

/** A minimal DOM double: enough for the prompt and for read-back assertions. */
class MockElement implements ConsentElement {
	textContent = "";
	className = "";
	hidden = false;
	type = "";
	checked = false;
	dataset: Record<string, string | undefined> = {};
	readonly children: MockElement[] = [];
	readonly removed: boolean[] = [];
	private readonly listeners = new Map<string, Array<() => void>>();

	constructor(readonly tagName: string) {}

	append(...children: ConsentElement[]): void {
		for (const child of children) {
			this.children.push(child as MockElement);
		}
	}

	addEventListener(type: "click" | "change", handler: () => void): void {
		const list = this.listeners.get(type) ?? [];
		list.push(handler);
		this.listeners.set(type, list);
	}

	fire(type: "click" | "change"): void {
		for (const handler of this.listeners.get(type) ?? []) {
			handler();
		}
	}

	remove(): void {
		this.removed.push(true);
	}

	find(role: string): MockElement | undefined {
		if (this.dataset.role === role) {
			return this;
		}
		for (const child of this.children) {
			const found = child.find(role);
			if (found !== undefined) {
				return found;
			}
		}
		return undefined;
	}

	findByClass(name: string): MockElement | undefined {
		if (this.className.split(/\s+/).includes(name)) {
			return this;
		}
		for (const child of this.children) {
			const found = child.findByClass(name);
			if (found !== undefined) {
				return found;
			}
		}
		return undefined;
	}
}

function mockFactory(seen: MockElement[]): (tag: string) => ConsentElement {
	return (tag: string): ConsentElement => {
		const element = new MockElement(tag);
		seen.push(element);
		return element;
	};
}

/** A Worker whose bootstrap actually runs in a fresh realm, counting creations. */
function realmWorkerFactory(): { factory: WorkerFactory; created: () => number } {
	let created = 0;
	const factory: WorkerFactory = (bootstrapSource: string): WorkerLike => {
		created += 1;
		const worker: WorkerLike = {
			onmessage: null,
			onerror: null,
			postMessage(message: unknown): void {
				let sent: SandboxResponse | null = null;
				const realm: Record<string, unknown> = {
					__SLJS_REQUEST__: message as SandboxRequest,
					__SLJS_SEND__: (response: SandboxResponse): void => {
						sent = response;
					},
				};
				realm.self = realm;
				runInNewContext(bootstrapSource, realm, { timeout: 500 });
				if (sent !== null) {
					worker.onmessage?.({ data: sent });
				}
			},
			terminate(): void {},
		};
		return worker;
	};
	return { factory, created: () => created };
}

function calcRequest(): FormScriptRequest {
	return {
		script:
			"var sub = Number(this.getField('Subtotal').value); " +
			"var rate = Number(this.getField('TaxRate').value); " +
			"event.value = (sub + sub * rate).toFixed(2);",
		event: { type: "calculate", targetName: "Total", value: "0" },
		fields: [
			{ name: "Subtotal", value: "100" },
			{ name: "TaxRate", value: "0.08" },
			{ name: "Total", value: "0" },
		],
	};
}

describe("SL-5.JS.02 visible consent gate", () => {
	it("detects /JS, /AA and /CO markers but not ordinary text", () => {
		expect(detectFormScripts("%PDF-1.4 /AcroForm << /CO [6 0 R] >>")).toBe(true);
		expect(detectFormScripts(new TextEncoder().encode("/AA << /C 6 0 R >>"))).toBe(true);
		expect(detectFormScripts(new TextEncoder().encode("/JS (app.alert\\(1\\))"))).toBe(true);
		expect(detectFormScripts("%PDF-1.4 /Type /Catalog /Fields []")).toBe(false);
	});

	it("(a) shows the dialog and runs nothing when the reader declines", async () => {
		const consent = new ScriptConsentController();
		const realm = realmWorkerFactory();
		let prompted = 0;
		const result = await runFormScriptWithConsent("/JS (event.value=1)", calcRequest(), {
			document: docA,
			consent,
			prompter: async () => {
				prompted += 1;
				return false;
			},
			workerFactory: realm.factory,
		});
		expect(prompted).toBe(1);
		expect(result.hadScripts).toBe(true);
		expect(result.promptShown).toBe(true);
		expect(result.consentGranted).toBe(false);
		expect(result.outcome).toEqual({ ok: false, code: "js-consent-required" });
		expect(realm.created()).toBe(0);
		expect(consent.grantCount).toBe(0);
	});

	it("(a) the dialog needs the checkbox and records only an explicit allow", async () => {
		const consent = new ScriptConsentController();
		const host = new MockElement("div");
		const seen: MockElement[] = [];
		const pending = showConsentPrompt({
			host,
			document: docA,
			consent,
			createElement: mockFactory(seen),
		});
		const dialog = host.find("js-consent-prompt");
		expect(dialog).toBeDefined();
		const question = dialog?.findByClass("consent-question");
		expect(question?.textContent).toContain(CONSENT_QUESTION);
		const allow = dialog?.find("js-consent-allow");
		const decline = dialog?.find("js-consent-decline");
		const check = dialog?.find("js-consent-checkbox");
		expect(allow?.hidden).toBe(true);

		// Allow is inert until the checkbox is affirmatively ticked.
		allow?.fire("click");
		expect(consent.isConsented(docA)).toBe(false);

		check!.checked = true;
		check!.fire("change");
		expect(allow?.hidden).toBe(false);
		allow!.fire("click");

		await expect(pending).resolves.toBe(true);
		expect(consent.isConsented(docA)).toBe(true);
		expect(dialog?.removed.length).toBe(1);
		// The grant is for this document only.
		expect(consent.isConsented(docB)).toBe(false);
		// Decline button was present too.
		expect(decline?.textContent).toBe("No");
	});

	it("(b) after consent the script runs in the sandbox and computes the field", async () => {
		const consent = new ScriptConsentController();
		const realm = realmWorkerFactory();
		const result = await runFormScriptWithConsent("/CO [6 0 R]", calcRequest(), {
			document: docA,
			consent,
			prompter: async () => true,
			workerFactory: realm.factory,
		});
		expect(result.promptShown).toBe(true);
		expect(result.consentGranted).toBe(true);
		expect(realm.created()).toBe(1);
		expect(result.outcome.ok).toBe(true);
		if (result.outcome.ok) {
			expect(result.outcome.eventValue).toBe("108.00");
		}
	});

	it("(c) default deny creates no worker even with a script present", async () => {
		const consent = new ScriptConsentController();
		const realm = realmWorkerFactory();
		const result = await runFormScriptWithConsent("/JS (event.value=1)", calcRequest(), {
			document: docA,
			consent,
			prompter: declinePrompter,
			workerFactory: realm.factory,
		});
		expect(result.outcome).toEqual({ ok: false, code: "js-consent-required" });
		expect(realm.created()).toBe(0);
	});

	it("(c) a document with no scripts is never prompted or run", async () => {
		const consent = new ScriptConsentController();
		const realm = realmWorkerFactory();
		let prompted = 0;
		const result = await runFormScriptWithConsent("/Type /Catalog /Fields []", calcRequest(), {
			document: docA,
			consent,
			prompter: async () => {
				prompted += 1;
				return true;
			},
			workerFactory: realm.factory,
		});
		expect(prompted).toBe(0);
		expect(realm.created()).toBe(0);
		expect(result.hadScripts).toBe(false);
		expect(result.consentGranted).toBe(false);
	});

	it("(d) an approval persists only for its document", async () => {
		const consent = new ScriptConsentController();
		const realm = realmWorkerFactory();
		let prompted = 0;
		const options = {
			consent,
			prompter: async () => {
				prompted += 1;
				return true;
			},
			workerFactory: realm.factory,
		};
		await runFormScriptWithConsent("/JS (1)", calcRequest(), { ...options, document: docA });
		expect(prompted).toBe(1);
		expect(consent.grantCount).toBe(1);

		// Same document: the grant is reused, no second prompt.
		const again = await runFormScriptWithConsent("/JS (1)", calcRequest(), {
			...options,
			document: docA,
		});
		expect(prompted).toBe(1);
		expect(again.promptShown).toBe(false);

		// A different id at the same origin is a fresh decision.
		const other = await runFormScriptWithConsent("/JS (1)", calcRequest(), {
			...options,
			document: docB,
		});
		expect(prompted).toBe(2);
		expect(other.promptShown).toBe(true);
		expect(consent.grantCount).toBe(2);
		// Three runs, three workers: each was gated, none created on a deny.
		expect(realm.created()).toBe(3);
	});
});
