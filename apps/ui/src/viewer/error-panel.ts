/**
 * What a failure panel contains, decided without touching the DOM (SL-4.UI.12).
 *
 * ## Why this is separate from `errors.ts`
 *
 * `errors.ts` answers "how bad is this and what do we offer". This answers "what
 * does the panel say", which needs the sentence the engine sent and the chrome
 * around it. Separating them keeps the interesting question - which parts of a
 * failure are visible, and which are not - testable without a document.
 *
 * ## The rule about what is shown
 *
 * The engine's own `message` is the sentence: the registry authors it once, in
 * `codes.toml`, and the engine sends it back (ADR-P0034 has the shell localise
 * BY CODE, which is a data change on top of this). This module does not rewrite
 * it, paraphrase it, or replace it with a generic phrase.
 *
 * The numeric code is shown too, but quietly. A user who reports "it said
 * something went wrong" gives support nothing; a user who reports "it said 1001"
 * gives them a link to the registry. That is the whole reason the code is not
 * hidden behind a details toggle nobody opens.
 *
 * ## The refusal
 *
 * `docState` is reported, never inferred. A failure that left the document
 * untouched says so, because "the operation failed" and "your document is
 * unaffected" are different facts and conflating them is how a user loses trust
 * in a panel.
 *
 * @see SL-4.UI.12
 */

import type { DocState } from "../platform/errors.js";
import { type ErrorSeverity, type ErrorState, errorState, shellFailure } from "./errors.js";

/** The chrome a host supplies; the viewer owns no sentences of its own (UI.02). */
export interface FailureStrings {
	/** Label for the action button. */
	actionLabel(action: ErrorState["action"]): string;
	/** The sentence describing what happened to the document. */
	docStateLabel(docState: DocState): string;
	/**
	 * The sentence for a code the registry does not know. Called only for
	 * `unknown` states, and only because there is no registry message to show.
	 */
	unknownCode(code: number): string;
}

/** One panel: everything the DOM layer needs, and nothing it has to decide. */
export interface FailurePanel {
	/** The registry id, or `null` for a failure that never reached the registry. */
	readonly code: number | null;
	readonly name: string;
	readonly source: "registry" | "shell";
	readonly severity: ErrorSeverity;
	/** The sentence, from the engine. Never reworded. */
	readonly message: string;
	readonly docState: DocState;
	/** Second line: what happened to the document. Always present. */
	readonly docStateLabel: string;
	/** The button. Always present, because a dead end is the bug we are fixing. */
	readonly actionLabel: string;
	readonly action: ErrorState["action"];
	readonly actionable: boolean;
	readonly unknown: boolean;
	/**
	 * Whether the code is rendered in the panel body.
	 *
	 * True only for an unregistered code. For a known one the message alone is
	 * what a reader needs, and the number would be noise; for an unknown one it
	 * is the only thing that lets anyone find the bug.
	 */
	readonly showCode: boolean;
}

/**
 * Build the panel for a failure the engine reported.
 *
 * @param state the decision from `errorState`
 * @param engineMessage the `message` field off the wire. Required for a known
 * code: without it there is nothing honest to show, and silently substituting a
 * generic phrase would hide the fact that the shell dropped a field.
 */
export function failurePanel(
	state: ErrorState,
	engineMessage: string | undefined,
	strings: FailureStrings,
): FailurePanel {
	return {
		code: state.code,
		name: state.name,
		source: state.source,
		severity: state.severity,
		// For an unknown code there is no registry sentence, so the catalogue
		// supplies one that still carries the number.
		message: engineMessage ?? (state.unknown ? strings.unknownCode(state.code ?? 0) : ""),
		docState: state.docState,
		docStateLabel: strings.docStateLabel(state.docState),
		actionLabel: strings.actionLabel(state.action),
		action: state.action,
		actionable: state.actionable,
		unknown: state.unknown,
		showCode: state.unknown && state.code !== null,
	};
}

/**
 * Build the panel straight from a wire failure.
 *
 * This takes the wire shape and nothing else, and derives the decision itself.
 * An earlier version also took a pre-built `ErrorState`, which was a trap: the
 * caller could pair one failure's code with another failure's document state,
 * and the panel would confidently describe a document that never existed. The
 * engine's `docState` is the answer to "what happened to MY document", so it
 * belongs here and wins over the registry default.
 *
 * @param wire one engine failure: `{code, message, docState}`
 */
export function failurePanelFromWire(
	wire: { code?: number | null; message?: string | null; docState?: string | null },
	strings: FailureStrings,
): FailurePanel {
	const docState = isDocState(wire.docState) ? wire.docState : undefined;
	// No code at all means the failure never reached the registry: a refused
	// allocation, a module that would not import. That is a different thing
	// from a code the registry does not have, and conflating them once made
	// this render "unrecognised error code -1" for a C-ABI open refusal - a
	// bug report about a number the platform never issued.
	if (typeof wire.code !== "number") {
		return failurePanel(shellFailure(), wire.message ?? undefined, strings);
	}
	return failurePanel(errorState(wire.code, docState), wire.message ?? undefined, strings);
}

function isDocState(value: string | null | undefined): value is DocState {
	return (
		value === "NotLoaded" ||
		value === "Loaded" ||
		value === "PartiallyLoaded" ||
		value === "Unchanged" ||
		value === "Modified"
	);
}
