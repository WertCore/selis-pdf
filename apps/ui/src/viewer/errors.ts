/**
 * How a viewer presents a failure (SL-4.UI.12).
 *
 * ## What this module is for
 *
 * The Do reads "every `Code` maps to a user-facing state with a recovery
 * action", and the two halves come from opposite directions:
 *
 *  - the SENTENCE is the engine's. `codes.toml` authors one `user_msg` per
 *    code and the engine sends it back inside the error. The viewer must not
 *    hold a second copy: two copies of one message is two things that drift,
 *    and UI.02 forbids viewer prose anyway.
 *  - the DECISION is the viewer's. Whether a failure is BLOCKED or merely
 *    DEGRADED, and whether the honest next step is "retry", "choose another
 *    file" or "tell us", depends on how the failure is being presented. The
 *    engine has no opinion about that, so it does not send it.
 *
 * So this module owns the second half, and owns no prose at all. `ErrorState`
 * has no `message` field by design: the caller renders the engine's sentence
 * inside a panel whose chrome comes from the catalogue.
 *
 * ## The three things this module refuses to do
 *
 *  1. **Dead ends.** Every state has an action, including the ones where the
 *     action is "report it". A panel that says only "something went wrong" has
 *     moved the problem rather than solved it.
 *  2. **Claim a document is safe when it is not.** `docState` comes from the
 *     engine and is passed through unchanged. A partially loaded document is
 *     reported as partially loaded, never as loaded.
 *  3. **Hide an unknown code.** An unregistered number resolves to a visible
 *     `unknown` state rather than being folded into a generic one, so the code
 *     survives to the bug report.
 *
 * ## Why this is pure
 *
 * Registry in, decisions out. No DOM, no host, no engine (ADR-P0044) - the same
 * rule as `health.ts` and `layout.ts`. What to OFFER is the part worth
 * testing; painting it is not.
 *
 * @see SL-4.UI.12
 */

import { type DocState, type ErrorKind, REGISTRY, type RegistryCode } from "./error-codes.js";

/** The action a user is offered for a failure. */
export type RecoveryAction =
	/** Try the same thing again. */
	| "retry"
	/** Choose a different file. */
	| "open-another"
	/** Free memory by closing something, then try again. */
	| "close-and-retry"
	/** Supply a password or permission the document demands. */
	| "grant-access"
	/** Nothing the user can do; the failure is ours to fix. */
	| "report";

/** How prominently a failure is shown. */
export type ErrorSeverity = "blocked" | "degraded" | "info";

/** One decision: how to present a failure. Carries no prose. */
export interface ErrorState {
	/**
	 * The registry id, or `null` when the failure never reached the registry.
	 *
	 * Nullable on purpose. The C-ABI open path refuses with a null pointer and
	 * no code at all, and pretending otherwise - by synthesising an id, or by
	 * rendering "unrecognised error code -1" - would put a claim about a bug
	 * on screen that the platform has not actually made.
	 */
	readonly code: number | null;
	/** The registry's symbolic name, for logs and bug reports. */
	readonly name: string;
	/** Where the failure came from, which decides whether a code is shown. */
	readonly source: "registry" | "shell";
	readonly severity: ErrorSeverity;
	/** What happened to the user's document, from the engine. */
	readonly docState: DocState;
	readonly action: RecoveryAction;
	/**
	 * Whether the action is genuinely available, as opposed to merely the best
	 * name we have. False only for a retry the registry says cannot help.
	 */
	readonly actionable: boolean;
	/** True when the registry has no row for this code. */
	readonly unknown: boolean;
}

/**
 * A failure that never reached the registry.
 *
 * The engine refused before it could classify anything - an allocation that
 * returned null, a module that would not import. There is no code to show, so
 * none is shown, and the panel says what the shell actually knows. The action
 * is "open another file" because that is the one thing a user can always do,
 * and it is real here rather than aspirational.
 */
export function shellFailure(): ErrorState {
	return {
		code: null,
		name: "SHELL",
		source: "shell",
		severity: "blocked",
		docState: "NotLoaded",
		action: "open-another",
		actionable: true,
		unknown: false,
	};
}

/**
 * The recovery action for a registry row.
 *
 * Derived from `kind` first, because the KIND is what says whether the user
 * can do anything at all: a cancelled operation never needed doing, a budget is
 * relieved by freeing memory, an auth failure needs a permission the user can
 * grant. Only then does `retryable` matter, and it merely gates the
 * retry-shaped answers - a row the registry says cannot be retried must not be
 * shown a retry control, because that is a dead end wearing a button.
 */
export function recoveryFor(code: RegistryCode): RecoveryAction {
	switch (code.kind) {
		case "Cancelled":
			return "retry";
		case "Budget":
			// Only where the registry says retrying can help. `BUDGET_DEPTH` and
			// `BUDGET_POISONED` are `retryable: false`, and the registry is
			// right: the same file nests just as deeply the second time, and a
			// poisoned guard is poisoned for the operation either way. Offering
			// "close and try again" there is a dead end wearing a button - it
			// promises progress and delivers a loop.
			return code.retryable ? "close-and-retry" : "report";
		case "Auth":
		case "Policy":
			return "grant-access";
		case "Io":
			return "open-another";
		case "Malformed":
		case "Invalid":
		case "Unsupported":
			// The document is what is wrong, so the file is the lever.
			return "open-another";
		case "Internal":
			return "report";
		default:
			return "report";
	}
}

/**
 * How loudly to show a failure, from what survived.
 *
 * `NotLoaded` is BLOCKED: there is nothing to read, and a panel rendering it as
 * a warning implies the document is still there. `PartiallyLoaded` is DEGRADED,
 * which is the substance of this item - a damaged file that still opened is
 * neither a success nor a failure, and reporting it as either is how a user
 * stops believing the panel. `Loaded` and `Unchanged` are INFO: the document
 * is intact and the operation simply did not happen.
 */
export function severityFor(docState: DocState): ErrorSeverity {
	switch (docState) {
		case "NotLoaded":
			return "blocked";
		case "PartiallyLoaded":
			return "degraded";
		default:
			return "info";
	}
}

/**
 * Decide how to present a registry code.
 *
 * @param code the numeric id the engine reported
 * @param docState the engine's document-survival answer. When the caller has
 * one it wins over the registry default, because it describes THIS failure
 * rather than the code in general.
 */
export function errorState(code: number, docState?: DocState): ErrorState {
	const row = REGISTRY.get(code);
	if (row === undefined) {
		// Unregistered. Surfaced rather than folded into a generic state, so
		// the number reaches a bug report instead of vanishing.
		return {
			source: "registry",
			code,
			name: "UNREGISTERED",
			severity: "blocked",
			docState: docState ?? "NotLoaded",
			action: "report",
			actionable: true,
			unknown: true,
		};
	}
	const action = recoveryFor(row);
	return {
		source: "registry",
		code: row.id,
		name: row.name,
		severity: severityFor(docState ?? row.docState),
		docState: docState ?? row.docState,
		action,
		actionable: row.retryable || action !== "retry",
		unknown: false,
	};
}

/**
 * Every registered code as a decision.
 *
 * Exists so the "every code maps" gate is a call rather than a loop written
 * twice: the test walks this and checks nothing comes back unknown.
 */
export function everyErrorState(): Array<ErrorState & { code: number }> {
	return [...REGISTRY.keys()].map((code) => errorState(code) as ErrorState & { code: number });
}

/** The registry's codes grouped by kind, for a summary row. */
export function codesByKind(): Map<ErrorKind, number[]> {
	const out = new Map<ErrorKind, number[]>();
	for (const row of REGISTRY.values()) {
		const list = out.get(row.kind);
		if (list === undefined) {
			out.set(row.kind, [row.id]);
		} else {
			list.push(row.id);
		}
	}
	return out;
}
