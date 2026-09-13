/**
 * Adapter error model. The UI never invents an error taxonomy: every failure
 * carries the numeric code from the `selis-error` registry
 * (`crates/selis-error/codes.toml`, 03-CONVENTIONS.md §3) plus `docState`, so
 * the caller can always answer *"what happened to the user's document?"*
 * without reading the source. Mirrors the bindings rule (24-BINDINGS-SPEC §1).
 */

/** What happened to the document — same vocabulary as `codes.toml` `doc_state`. */
export type DocState = "NotLoaded" | "Loaded" | "PartiallyLoaded" | "Unchanged" | "Modified";

/**
 * Registry codes this seam names. These are **not** new codes — they are the
 * ids already registered in `crates/selis-error/codes.toml`; the constants
 * exist so the TS layer does not sprinkle magic numbers. If an adapter needs
 * a code that is not registered yet, add it to `codes.toml` first
 * (`xtask check-codes` enforces the registry), never here.
 */
export const ErrorCode = {
	/** The operation's allocation budget was exhausted. */
	BudgetBytes: 4000,
	/** The operation's wall-clock deadline expired. */
	BudgetWall: 4001,
	/** The nesting-depth budget was exhausted. */
	BudgetDepth: 4002,
	/** The indirect-object resolution budget was exhausted. */
	BudgetObjects: 4003,
	/** The rasterised-sample budget was exhausted (tile too large). */
	BudgetPixels: 4004,
	/** The caller's cancel signal fired. */
	Cancelled: 4020,
	/** A source read failed (missing descriptor, unreadable source). */
	IoReadFailed: 5000,
	/** A handle the adapter issued is unknown, stale, or already closed. */
	BindingBadHandle: 6000,
	/** A request argument is invalid (page out of range, malformed options). */
	BindingBadArgument: 6001,
} as const;

/** One registry code value. */
export type ErrorCodeValue = (typeof ErrorCode)[keyof typeof ErrorCode];

/** The typed error every adapter port rejects with. */
export class AdapterError extends Error {
	/** Registry code (see {@link ErrorCode}). */
	readonly code: number;
	/** Document survival answer for this failure. */
	readonly docState: DocState;
	/** Whether retrying the same operation can plausibly succeed. */
	readonly retryable: boolean;

	constructor(options: {
		code: number;
		message: string;
		docState: DocState;
		retryable: boolean;
	}) {
		super(`[selis:${options.code}] ${options.message}`);
		this.name = "AdapterError";
		this.code = options.code;
		this.docState = options.docState;
		this.retryable = options.retryable;
	}

	/** A cancelled operation never changes the document (registry 4020). */
	static cancelled(message = "The operation was cancelled."): AdapterError {
		return new AdapterError({
			code: ErrorCode.Cancelled,
			message,
			docState: "Unchanged",
			retryable: true,
		});
	}

	/** The user's document is untouched by this failure. */
	static badHandle(message: string, docState: DocState = "Unchanged"): AdapterError {
		return new AdapterError({
			code: ErrorCode.BindingBadHandle,
			message,
			docState,
			retryable: false,
		});
	}

	/** The request itself is invalid; the document is untouched. */
	static badArgument(message: string): AdapterError {
		return new AdapterError({
			code: ErrorCode.BindingBadArgument,
			message,
			docState: "Unchanged",
			retryable: false,
		});
	}

	/** The source could not be read; nothing was loaded. */
	static sourceUnreadable(message: string, docState: DocState = "NotLoaded"): AdapterError {
		return new AdapterError({
			code: ErrorCode.IoReadFailed,
			message,
			docState,
			retryable: true,
		});
	}
}

/** Narrow any unknown rejection to an {@link AdapterError}. */
export function isAdapterError(error: unknown): error is AdapterError {
	return error instanceof AdapterError;
}
