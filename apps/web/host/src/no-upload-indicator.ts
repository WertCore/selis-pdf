/**
 * The "Your file never left this device" indicator (SL-1A.UI.03, ADR-P0017).
 *
 * This is the visible half of the no-upload guarantee, and it is deliberately
 * bound to the *actual* mechanical check ({@link assertNoUpload}) plus an
 * explicit record of any consented external contact. It is never a decorative
 * badge:
 *
 * - `verified` — the request recorder ran and no opened document's bytes
 *   appeared in any outgoing request. Only this state may say "never left".
 * - `external-hash` — a user-consented time/revocation provider was contacted
 *   and only a hash (or a certificate-derived identifier) was sent, exactly as
 *   `apps/cli/src/tsa.rs` prints before connecting. A hash DID leave the
 *   device, so the claim changes; it does not say "never left".
 * - `watching` — the recorder is installed but nothing has been verified yet.
 * - `unknown` — a check failed, or an external contact happened without a
 *   recorded consent. The claim is withdrawn rather than softened.
 *
 * ADR-P0017 is not weakened: there is still no silent egress. The external
 * state exists so that an opt-in TSA/OCSP request cannot make the badge a lie.
 * The DOM seam is tiny and typed so the indicator can be rendered and asserted
 * without jsdom (the pattern from `editor/js-consent.ts`).
 */

import {
	assertNoUpload,
	type DocumentBytes,
	type RecordedRequest,
} from "./no-upload.js";

/** The four honest states of the indicator. */
export type NoUploadState = "watching" | "verified" | "external-hash" | "unknown";

/** What an external time/revocation call was for. */
export type ExternalPurpose = "timestamp" | "revocation";

/**
 * The only data that may cross the boundary for a consented external call.
 * Mirrors `tsa.rs`: a SHA-256 imprint of the signature, or a certificate-
 * derived identifier for OCSP. No document bytes, no filenames, no text.
 */
export type ExternalPayload = "signature-hash" | "certificate-identifier";

/** One recorded external contact. `consented` must be explicitly true. */
export interface ExternalHashEvent {
	readonly provider: string;
	readonly purpose: ExternalPurpose;
	readonly sent: ExternalPayload;
	readonly consented: boolean;
}

/** The view a UI renders. Copy is fixed, not assembled from remote data. */
export interface IndicatorView {
	readonly state: NoUploadState;
	readonly claim: string;
	readonly detail: string;
	/** Mirrors the existing `data-no-upload` hook for the tools browser check. */
	readonly dataNoUpload: NoUploadState;
	readonly external: readonly ExternalHashEvent[];
}

/** The one claim allowed only in the pure-local verified state. */
export const NEVER_LEFT_CLAIM = "Your file never left this device.";
/** The honest copy once a consented hash has left. */
export const EXTERNAL_HASH_CLAIM = "External time provider contacted — with your consent.";

function externalDetail(event: ExternalHashEvent): string {
	const what =
		event.sent === "signature-hash"
			? "a hash of the signature"
			: "an identifier derived from the certificate";
	return `${what} was sent to ${event.provider} for ${event.purpose}; no document bytes were sent.`;
}

/** Build the fixed copy for one state, without ever claiming "never left" for an external contact. */
export function viewFor(state: NoUploadState, external: readonly ExternalHashEvent[]): IndicatorView {
	const last = external.length > 0 ? external[external.length - 1] : undefined;
	switch (state) {
		case "verified":
			return {
				state,
				claim: NEVER_LEFT_CLAIM,
				detail: "Every outgoing request was checked; no document bytes were sent.",
				dataNoUpload: state,
				external,
			};
		case "external-hash":
			return {
				state,
				claim: EXTERNAL_HASH_CLAIM,
				detail:
					last === undefined
						? "A consented external time provider was contacted."
						: externalDetail(last),
				dataNoUpload: state,
				external,
			};
		case "watching":
			return {
				state,
				claim: "Watching this device's network traffic.",
				detail: "No upload check has completed yet; nothing has been confirmed.",
				dataNoUpload: state,
				external,
			};
		case "unknown":
			return {
				state,
				claim: "We can't confirm where your file went.",
				detail:
					last !== undefined && !last.consented
						? "An external provider was contacted without a recorded consent; the privacy claim is withdrawn."
						: "The upload check did not pass; the privacy claim is withdrawn.",
				dataNoUpload: state,
				external,
			};
	}
}

/** Inputs to the one-shot evaluation of the indicator. */
export interface IndicatorInputs {
	readonly requests: readonly RecordedRequest[];
	readonly documents: readonly DocumentBytes[];
	readonly external?: readonly ExternalHashEvent[];
}

/**
 * Pure evaluation: run the real no-upload assertion over the recorded requests
 * and the opened documents, then account for any external contact. A failed
 * assertion fails closed to `unknown` — the badge never claims success on a
 * check it could not complete.
 */
export function indicatorFromRequests(inputs: IndicatorInputs): IndicatorView {
	const external = inputs.external ?? [];
	const unconsented = external.some((event) => !event.consented);
	try {
		assertNoUpload(inputs.requests, inputs.documents);
	} catch {
		return viewFor("unknown", external);
	}
	if (unconsented) {
		return viewFor("unknown", external);
	}
	if (external.length > 0) {
		return viewFor("external-hash", external);
	}
	return viewFor("verified", external);
}

/**
 * Staged indicator for a session: the recorder starts in `watching`, a
 * successful operation calls `noteVerified`, and a signing flow that contacts
 * a TSA/OCSP provider calls `noteExternalHash` *after* showing consent.
 *
 * Once an unconsented external contact is recorded the state is sticky at
 * `unknown`: a later local success cannot erase what already left.
 */
export class NoUploadIndicator {
	private phase: NoUploadState = "watching";
	private readonly external: ExternalHashEvent[] = [];
	private violated = false;

	noteWatching(): void {
		if (!this.violated) {
			this.phase = "watching";
		}
	}

	noteVerified(): void {
		if (this.violated) {
			return;
		}
		if (this.external.length === 0) {
			this.phase = "verified";
		} else {
			this.phase = "external-hash";
		}
	}

	noteExternalHash(event: ExternalHashEvent): void {
		this.external.push(event);
		if (!event.consented) {
			this.violated = true;
			this.phase = "unknown";
		} else {
			this.phase = "external-hash";
		}
	}

	snapshot(): IndicatorView {
		return viewFor(this.phase, this.external);
	}
}

/** Minimal DOM surface the indicator needs; a real element satisfies it structurally. */
export interface IndicatorElement {
	textContent: string;
	className: string;
	dataset: Record<string, string | undefined>;
	append(...children: IndicatorElement[]): void;
	remove(): void;
}

/** Element factory; the production one wraps `document.createElement`. */
export type IndicatorElementFactory = (tag: string) => IndicatorElement;

/** The only place the real DOM is touched. */
export const browserIndicatorElement: IndicatorElementFactory = (tag: string): IndicatorElement =>
	document.createElement(tag) as unknown as IndicatorElement;

/** The last indicator rendered into each host, so a state change replaces it. */
const renderedByHost = new WeakMap<IndicatorElement, IndicatorElement>();

/**
 * Render one indicator view into a host element. Idempotent per host: an
 * existing indicator is removed first so a state change cannot stack two
 * contradictory claims on the page.
 */
export function renderNoUploadIndicator(
	view: IndicatorView,
	host: IndicatorElement,
	createElement: IndicatorElementFactory = browserIndicatorElement,
): IndicatorElement {
	renderedByHost.get(host)?.remove();
	const root = createElement("div");
	root.className = "no-upload-indicator";
	root.dataset.role = "no-upload-indicator";
	root.dataset.state = view.state;
	root.dataset.noUpload = view.dataNoUpload;

	const claim = createElement("strong");
	claim.className = "no-upload-claim";
	claim.textContent = view.claim;
	const detail = createElement("span");
	detail.className = "no-upload-detail";
	detail.textContent = view.detail;
	root.append(claim, detail);
	host.append(root);
	renderedByHost.set(host, root);
	return root;
}

/** Whether the view makes the strong "never left" claim (tests and audits). */
export function claimsNeverLeft(view: IndicatorView): boolean {
	return view.claim === NEVER_LEFT_CLAIM;
}
