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

/**
 * Reader consent, keyed per document. A fresh policy grants nothing. It also
 * records explicit declines (SL-5.JS.03): a decline is absolute and outranks
 * both a stale grant and any admin allow-list entry.
 */
export class ConsentPolicy {
	private readonly grants = new Map<string, ConsentScope>();
	private readonly declined = new Set<string>();

	/** Whether the reader explicitly consented for this document. */
	isConsented(doc: DocumentIdentity): boolean {
		return this.grants.has(doc.id);
	}

	/** Whether the reader explicitly declined this document. */
	isDeclined(doc: DocumentIdentity): boolean {
		return this.declined.has(doc.id);
	}

	/** The recorded consent scope, if any. */
	scope(doc: DocumentIdentity): ConsentScope | undefined {
		return this.grants.get(doc.id);
	}

	/**
	 * Record an explicit reader consent for one document. A recorded decline is
	 * never silently overwritten; it must be explicitly retracted first.
	 */
	grant(doc: DocumentIdentity, scope: ConsentScope): void {
		if (this.isDeclined(doc)) {
			return;
		}
		this.grants.set(doc.id, scope);
	}

	/** Record an explicit reader "No" for one document (clears any grant). */
	decline(doc: DocumentIdentity): void {
		this.grants.delete(doc.id);
		this.declined.add(doc.id);
	}

	/** Explicitly retract a recorded decline. */
	undecline(doc: DocumentIdentity): boolean {
		return this.declined.delete(doc.id);
	}

	/** Withdraw consent for one document; true if a grant was removed. */
	revoke(doc: DocumentIdentity): boolean {
		return this.grants.delete(doc.id);
	}

	/** Withdraw every grant and decline. */
	clear(): void {
		this.grants.clear();
		this.declined.clear();
	}

	/** Number of documents with a recorded grant. */
	get grantCount(): number {
		return this.grants.size;
	}

	/** Number of documents with a recorded, explicit decline. */
	get declineCount(): number {
		return this.declined.size;
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

	/** Explicit entries for the audit trail, sorted. */
	entries(): ReadonlyArray<readonly ["origin" | "id", string]> {
		const out: Array<readonly ["origin" | "id", string]> = [];
		for (const value of this.origins) {
			out.push(["origin", value]);
		}
		for (const value of this.ids) {
			out.push(["id", value]);
		}
		out.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0));
		return out;
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
	| { readonly kind: "deny"; readonly code: "js-consent-required" }
	| { readonly kind: "deny"; readonly code: "js-declined" };

/**
 * Decide whether a script may run. An explicit reader decline is checked FIRST
 * and is absolute (SL-5.JS.03): no admin allow-list entry can override it.
 * Otherwise a run is allowed only when the reader consented OR an administrator
 * pre-approved this exact origin/id (the explicit ADR-P0020 exception).
 */
export function authorizeScripting(
	auth: ScriptAuthorization,
	doc: DocumentIdentity,
): ScriptDecision {
	if (auth.consent.isDeclined(doc)) {
		return { kind: "deny", code: "js-declined" };
	}
	if (auth.consent.isConsented(doc) || auth.origins.isApproved(doc)) {
		return { kind: "allow" };
	}
	return { kind: "deny", code: "js-consent-required" };
}

/**
 * A typed refusal for a malformed administrator policy. The text never echoes
 * a policy value (only a 1-based line number), so a hostile config cannot
 * smuggle content into a log (ADR-P0017).
 */
export class AdminPolicyError extends Error {
	readonly code = "js-policy-malformed";
	readonly line: number;

	constructor(line: number, reason: string) {
		super(`admin scripting policy line ${line}: ${reason}`);
		this.name = "AdminPolicyError";
		this.line = line;
	}
}

/**
 * Parse an EXPLICIT administrator-supplied allow-list from text (SL-5.JS.03).
 * The host must supply this from configuration; the consent UI never guesses
 * an origin. Blank lines and `#` comments are ignored; every other line must be
 * `origin <value>` or `id <value>` — a malformed line is a typed refusal and
 * enables nothing. This mirrors `selis_policy::ScriptPolicy` in Rust.
 */
export function parseAdminScriptPolicy(text: string): ApprovedOrigins {
	const origins = new ApprovedOrigins();
	const lines = text.split(/\r?\n/);
	for (let i = 0; i < lines.length; i += 1) {
		const line = i + 1;
		const trimmed = (lines[i] ?? "").trim();
		if (trimmed === "" || trimmed.startsWith("#")) {
			continue;
		}
		if (trimmed.length > 4096) {
			throw new AdminPolicyError(line, "line is too long");
		}
		const parts = trimmed.split(/\s+/);
		if (parts.length < 2) {
			throw new AdminPolicyError(line, "missing value");
		}
		if (parts.length > 2) {
			throw new AdminPolicyError(line, "trailing tokens");
		}
		const directive: string | undefined = parts[0];
		const value: string | undefined = parts[1];
		if (directive === undefined || value === undefined) {
			throw new AdminPolicyError(line, "missing value");
		}
		if (directive === "origin") {
			origins.approveOrigin(value);
		} else if (directive === "id") {
			origins.approveDocument(value);
		} else {
			throw new AdminPolicyError(line, "not an 'origin' or 'id' entry");
		}
	}
	return origins;
}

/** A deny-all gate — the default used when a caller supplies no policy. */
export function denyAllAuthorization(): ScriptAuthorization {
	return { consent: new ConsentPolicy(), origins: new ApprovedOrigins() };
}
