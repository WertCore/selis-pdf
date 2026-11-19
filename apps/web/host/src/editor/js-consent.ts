/**
 * Visible, per-document scripting consent for the browser Fill Forms flow
 * (SL-5.JS.02, ADR-P0020).
 *
 * Nothing here runs a script. This module decides *whether* a script may be
 * handed to the sandboxed worker: a fresh controller grants nothing, a prompt
 * must be shown and affirmatively accepted, and the grant is keyed on one
 * document id. The worker runner (`runFormScript`) re-checks the same gate, so
 * a worker is never created without consent even if this layer is bypassed.
 *
 * The prompt is built from a tiny typed DOM surface so it can be exercised in
 * a jsdom-style mock without a real browser (see js-consent.test.ts). The
 * default factory uses the real `document`, and the markup is dark-theme
 * classed to match the rest of the host.
 */

import {
	ApprovedOrigins,
	ConsentPolicy,
	type ConsentScope,
	type DocumentIdentity,
	type ScriptAuthorization,
} from "./js-policy.js";
import {
	type FormScriptOutcome,
	type FormScriptRequest,
	type JsBudget,
	type RunOptions,
	type Timers,
	type WorkerFactory,
	runFormScript,
} from "./js-sandbox.js";

/** The question a reader must affirm before any script runs. */
export const CONSENT_QUESTION = "This document contains JavaScript. Allow it to run?";
/** The explicit decline action. */
export const CONSENT_DECLINE = "No";
/** The affirmative, single-document action. */
export const CONSENT_ALLOW_ONCE = "Allow once for this document";
/** The acknowledgment beside the checkbox. */
export const CONSENT_ACK =
	"I understand that JavaScript from this document will run in a restricted sandbox on my device.";

/**
 * The minimal DOM surface the prompt needs. A real `HTMLElement` satisfies it
 * structurally; tests supply a small jsdom-style double.
 */
export interface ConsentElement {
	textContent: string;
	className: string;
	hidden: boolean;
	tagName: string;
	type: string;
	checked: boolean;
	dataset: Record<string, string | undefined>;
	append(...children: ConsentElement[]): void;
	addEventListener(type: "click" | "change", handler: () => void): void;
	remove(): void;
}

/** Creates an element; the real factory wraps `document.createElement`. */
export type ConsentElementFactory = (tag: string) => ConsentElement;

/** The production element factory (the only place the DOM is touched). */
export const browserConsentElement: ConsentElementFactory = (tag: string): ConsentElement =>
	document.createElement(tag) as unknown as ConsentElement;

/**
 * Holds reader consent and the (empty by default) admin pre-approval list.
 * A fresh controller exposes a deny-all gate until `grant` is called.
 */
export class ScriptConsentController {
	private readonly grants = new ConsentPolicy();
	private readonly origins = new ApprovedOrigins();

	/** A fresh authorization view over the current policy state. */
	authorization(): ScriptAuthorization {
		return { consent: this.grants, origins: this.origins };
	}

	/** Whether this exact document has been consented. */
	isConsented(doc: DocumentIdentity): boolean {
		return this.grants.isConsented(doc);
	}

	/** Record an explicit reader grant for one document (default: this session). */
	grant(doc: DocumentIdentity, scope: ConsentScope = "session"): void {
		this.grants.grant(doc, scope);
	}

	/** Withdraw one document's grant. */
	revoke(doc: DocumentIdentity): boolean {
		return this.grants.revoke(doc);
	}

	/** Withdraw every grant (e.g. a "block all scripts" setting). */
	clear(): void {
		this.grants.clear();
	}

	/** How many documents are currently consented. */
	get grantCount(): number {
		return this.grants.grantCount;
	}

	/** An admin origin pre-approval (the narrow ADR-P0020 exception). */
	approveOrigin(origin: string): void {
		this.origins.approveOrigin(origin);
	}
}

const SCRIPT_MARKERS = /\/(?:JS|AA|CO)\b/;

/** Decode PDF bytes as Latin-1 in chunks (script markers are ASCII tokens). */
function latin1(bytes: Uint8Array): string {
	let out = "";
	const chunk = 0x8000;
	for (let i = 0; i < bytes.length; i += chunk) {
		out += String.fromCharCode(...bytes.subarray(i, i + chunk));
	}
	return out;
}

/**
 * Conservative detection of active-content markers in a filled result: PDF
 * name tokens `/JS`, `/AA` and `/CO`. This is a *prompting* signal, not a
 * security boundary — the runner's forbidden-API scan and the consent gate are
 * the boundary. A caller may also pass a typed `hasScripts` from the tools
 * layer (the form-field report carries one) to avoid byte guessing.
 */
export function detectFormScripts(source: Uint8Array | string): boolean {
	const text = typeof source === "string" ? source : latin1(source);
	return SCRIPT_MARKERS.test(text);
}

/** Options for the visible consent dialog. */
export interface ConsentPromptOptions {
	/** Where the dialog is appended (a stable host element in the card). */
	readonly host: ConsentElement;
	/** The document being approved. */
	readonly document: DocumentIdentity;
	/** The controller whose grant the dialog records. */
	readonly consent: ScriptConsentController;
	/** Element factory; defaults to the real `document`. */
	readonly createElement?: ConsentElementFactory;
}

/**
 * Show the dark-themed consent dialog and resolve to the reader's decision.
 *
 * The "Allow once for this document" button is inert until the checkbox is
 * ticked, so consent is an unambiguous affirmative action. On allow, a session
 * grant is recorded for *this document id only*. On decline, nothing is
 * granted. The dialog removes itself in either case.
 */
export function showConsentPrompt(options: ConsentPromptOptions): Promise<boolean> {
	const create = options.createElement ?? browserConsentElement;
	return new Promise<boolean>((resolve) => {
		const dialog = create("section");
		dialog.className = "consent-prompt";
		dialog.dataset.role = "js-consent-prompt";

		const question = create("p");
		question.className = "consent-question";
		question.textContent = CONSENT_QUESTION;

		const ack = create("label");
		ack.className = "consent-ack";
		const check = create("input");
		check.type = "checkbox";
		check.className = "consent-checkbox";
		check.dataset.role = "js-consent-checkbox";
		check.addEventListener("change", () => {
			allow.hidden = !check.checked;
		});
		const ackText = create("span");
		ackText.textContent = CONSENT_ACK;
		ack.append(check, ackText);

		const actions = create("div");
		actions.className = "consent-actions";
		const decline = create("button");
		decline.className = "consent-decline";
		decline.textContent = CONSENT_DECLINE;
		decline.dataset.role = "js-consent-decline";
		const allow = create("button");
		allow.className = "consent-allow";
		allow.textContent = CONSENT_ALLOW_ONCE;
		allow.dataset.role = "js-consent-allow";
		allow.hidden = true; // enabled only after the checkbox is ticked
		actions.append(decline, allow);

		let settled = false;
		const finish = (granted: boolean): void => {
			if (settled) {
				return;
			}
			settled = true;
			dialog.remove();
			resolve(granted);
		};
		decline.addEventListener("click", () => finish(false));
		allow.addEventListener("click", () => {
			if (!check.checked) {
				return;
			}
			options.consent.grant(options.document, "session");
			finish(true);
		});

		dialog.append(question, ack, actions);
		options.host.append(dialog);
	});
}

/** A prompter that resolves to `false` without touching the DOM (headless shells/tests). */
export const declinePrompter: (doc: DocumentIdentity) => Promise<boolean> = async () => false;

/** Options for the gated runner. */
export interface ConsentRunOptions {
	readonly document: DocumentIdentity;
	readonly consent: ScriptConsentController;
	/** Shows the prompt; injected so tests can supply a mock reader. */
	readonly prompter: (doc: DocumentIdentity) => Promise<boolean>;
	/** Typed detection from the tools layer, OR-ed with the byte scan. */
	readonly hasScripts?: boolean;
	readonly budget?: JsBudget;
	readonly workerFactory?: WorkerFactory;
	readonly timers?: Timers;
}

/** What the gated runner did, for status/reporting. */
export interface ConsentRunResult {
	readonly outcome: FormScriptOutcome;
	readonly hadScripts: boolean;
	readonly promptShown: boolean;
	readonly consentGranted: boolean;
}

/**
 * Detect scripts, ask for consent, and only then run the script in the sandbox.
 *
 * No worker is created when: there are no scripts, the reader declines, or the
 * gate denies. A prior grant for this document skips the prompt but is still
 * re-checked by `runFormScript` before a worker is created.
 */
export async function runFormScriptWithConsent(
	filled: Uint8Array | string,
	request: FormScriptRequest,
	options: ConsentRunOptions,
): Promise<ConsentRunResult> {
	const hadScripts = options.hasScripts === true || detectFormScripts(filled);
	if (!hadScripts) {
		return {
			outcome: { ok: false, code: "js-script-rejected" },
			hadScripts: false,
			promptShown: false,
			consentGranted: false,
		};
	}

	let promptShown = false;
	if (!options.consent.isConsented(options.document)) {
		promptShown = true;
		const granted = await options.prompter(options.document);
		if (!granted) {
			return {
				outcome: { ok: false, code: "js-consent-required" },
				hadScripts: true,
				promptShown: true,
				consentGranted: false,
			};
		}
		options.consent.grant(options.document, "session");
	}

	// exactOptionalPropertyTypes: only set knobs the caller actually supplied.
	const runOptions: RunOptions = {
		authorization: options.consent.authorization(),
		document: options.document,
		...(options.budget !== undefined ? { budget: options.budget } : {}),
		...(options.workerFactory !== undefined ? { workerFactory: options.workerFactory } : {}),
		...(options.timers !== undefined ? { timers: options.timers } : {}),
	};
	const outcome = await runFormScript(request, runOptions);
	return { outcome, hadScripts: true, promptShown, consentGranted: true };
}
