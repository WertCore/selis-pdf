/**
 * Browser-side mirror of `selis-policy`'s scripting consent + admin origin
 * policy (SL-5.JS.02/03, ADR-P0020). The Rust types are the authority; this
 * mirror exists so the web host can make the SAME decision before it ever
 * creates a Worker. Scripts never auto-run.
 */

/** How long a reader's explicit consent lasts, as recorded by the host. */
export type ConsentScope = "session" | "persistent";

/** The identity a scripting decision is keyed on (mirrors Rust). */
export interface DocumentIdentity {
	readonly id: string;
	readonly origin: string;
}

/** Construct a document identity from an id and an origin. */
export function documentIdentity(id: string, origin: string): DocumentIdentity {
	return { id, origin };
}

/** Reader consent, keyed per document. A fresh policy grants nothing. */
export class ConsentPolicy {
	private readonly grants = new Map<string, ConsentScope>();

	/** Whether the reader explicitly consented for this document. */
	isConsented(doc: DocumentIdentity): boolean {
		return this.grants.has(doc.id);
	}

	/** The recorded consent scope, if any. */
	scope(doc: DocumentIdentity): ConsentScope | undefined {
		return this.grants.get(doc.id);
	}

	/** Record an explicit reader consent for one document. */
	grant(doc: DocumentIdentity, scope: ConsentScope): void {
		this.grants.set(doc.id, scope);
	}

	/** Withdraw consent for one document; true if a grant was removed. */
	revoke(doc: DocumentIdentity): boolean {
		return this.grants.delete(doc.id);
	}

	/** Withdraw every grant. */
	clear(): void {
		this.grants.clear();
	}

	/** Number of documents with a recorded grant. */
	get grantCount(): number {
		return this.grants.size;
	}
}

/** Admin pre-approval list. Empty means no document may auto-run. */
export class ApprovedOrigins {
	private readonly origins = new Set<string>();
	private readonly ids = new Set<string>();

	/** Pre-approve a document origin. */
	approveOrigin(origin: string): void {
		this.origins.add(origin);
	}

	/** Pre-approve one document id. */
	approveDocument(id: string): void {
		this.ids.add(id);
	}

	/** Whether an admin pre-approved this document's origin or id. */
	isApproved(doc: DocumentIdentity): boolean {
		return this.origins.has(doc.origin) || this.ids.has(doc.id);
	}

	/** Remove an approved origin. */
	revokeOrigin(origin: string): boolean {
		return this.origins.delete(origin);
	}

	/** Remove an approved document id. */
	revokeDocument(id: string): boolean {
		return this.ids.delete(id);
	}

	/** Total number of admin entries. */
	get size(): number {
		return this.origins.size + this.ids.size;
	}
}

/** The combined gate consulted before any script is created. */
export interface ScriptAuthorization {
	readonly consent: ConsentPolicy;
	readonly origins: ApprovedOrigins;
}

/** The decision returned by the gate. */
export type ScriptDecision =
	| { readonly kind: "allow" }
	| { readonly kind: "deny"; readonly code: "js-consent-required" };

/**
 * Decide whether a script may run. A run is allowed only when the reader
 * consented for this document OR an administrator pre-approved its origin/id.
 */
export function authorizeScripting(
	auth: ScriptAuthorization,
	doc: DocumentIdentity,
): ScriptDecision {
	if (auth.consent.isConsented(doc) || auth.origins.isApproved(doc)) {
		return { kind: "allow" };
	}
	return { kind: "deny", code: "js-consent-required" };
}

/** A deny-all gate — the default used when a caller supplies no policy. */
export function denyAllAuthorization(): ScriptAuthorization {
	return { consent: new ConsentPolicy(), origins: new ApprovedOrigins() };
}
