/**
 * The error-code registry, GENERATED - do not edit by hand.
 *
 * Source of truth: `crates/selis-error/codes.toml` (03-CONVENTIONS.md section 3,
 * SL-0.ERR.01). Update:
 *
 *   1. edit `codes.toml`
 *   2. `UPDATE_ERROR_CODES=1 pnpm --filter @selis/ui test`
 *   3. review the diff
 *
 * ## Why this is generated rather than hand-written
 *
 * SL-4.UI.12 says **every** `Code` maps to a user-facing state with a recovery
 * action. The registry has 83 of them and grows. A hand-written table
 * would cover the codes someone remembered, and the gap would be invisible: a
 * missing entry is not a compile error, it is a user staring at a dead end on an
 * error nobody wrote a state for.
 *
 * So the table is derived from the same file the engine uses, and a test
 * re-reads that file and fails if the committed table has drifted. The registry
 * is the single source of truth in both languages, exactly as UI.10 made the
 * token files the source of truth for the shipped CSS.
 *
 * ## What is deliberately NOT here: `user_msg`
 *
 * The registry authors one English sentence per code, and the engine sends it
 * back inside the error. Copying it into this table would put prose in the
 * viewer, which is what UI.02 forbids: the viewer owns no sentences of its own,
 * so the same failure would be phrased two ways depending on which layer
 * answered. The sentence the user reads is the one the engine produced.
 *
 * What IS here is everything the viewer must decide FOR ITSELF - the severity
 * a failure is shown at, and the recovery action offered - none of which comes
 * back over the wire. That is the part the engine cannot tell us, because it
 * describes how to present a failure rather than what went wrong.
 *
 * @generated
 * @see SL-4.UI.12
 */

import type { DocState } from "../platform/errors.js";

/**
 * `DocState` is NOT redefined here. The adapter seam already owns it
 * (`platform/errors.ts`), and redeclaring an identical type would give the package two
 * names for one vocabulary and make the barrel ambiguous. It is re-exported so the
 * generated table still reads as self-contained.
 */
export type { DocState };

/** The registry's `kind`, which decides how a failure is presented. */
export type ErrorKind =
	| "Malformed"
	| "Invalid"
	| "Unsupported"
	| "Budget"
	| "Auth"
	| "Policy"
	| "Io"
	| "Internal"
	| "Cancelled";

/** One row of the registry: the facts that are not prose. */
export interface RegistryCode {
	readonly id: number;
	readonly name: string;
	readonly kind: ErrorKind;
	readonly retryable: boolean;
	readonly docState: DocState;
}

/**
 * Every registered code, keyed by numeric id.
 *
 * A `Map` rather than an object literal: the ids are numbers, and an object
 * would coerce them to strings and turn a typo into a silent `undefined`.
 */
export const REGISTRY: ReadonlyMap<number, RegistryCode> = new Map([
	[1000, { id: 1000, name: "NOT_A_PDF", kind: "Malformed", retryable: false, docState: "NotLoaded" }],
	[1001, { id: 1001, name: "LEX_UNEXPECTED_BYTE", kind: "Malformed", retryable: false, docState: "NotLoaded" }],
	[1002, { id: 1002, name: "LEX_UNTERMINATED_STRING", kind: "Malformed", retryable: false, docState: "NotLoaded" }],
	[1003, { id: 1003, name: "LEX_NUMBER_OVERFLOW", kind: "Malformed", retryable: false, docState: "NotLoaded" }],
	[1010, { id: 1010, name: "OBJ_UNEXPECTED", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1011, { id: 1011, name: "OBJ_MISSING", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1012, { id: 1012, name: "OBJ_CYCLE", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1013, { id: 1013, name: "OBJ_GENERATION_MISMATCH", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1100, { id: 1100, name: "XREF_NOT_FOUND", kind: "Malformed", retryable: false, docState: "NotLoaded" }],
	[1101, { id: 1101, name: "XREF_MALFORMED", kind: "Malformed", retryable: false, docState: "NotLoaded" }],
	[1102, { id: 1102, name: "XREF_PREV_CYCLE", kind: "Malformed", retryable: false, docState: "NotLoaded" }],
	[1103, { id: 1103, name: "TRAILER_MISSING_ROOT", kind: "Malformed", retryable: false, docState: "NotLoaded" }],
	[1204, { id: 1204, name: "XREF_UNRECOVERABLE", kind: "Malformed", retryable: false, docState: "NotLoaded" }],
	[1205, { id: 1205, name: "OBJSTM_MALFORMED", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1206, { id: 1206, name: "OBJSTM_NESTED", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1300, { id: 1300, name: "PAGE_TREE_MALFORMED", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1301, { id: 1301, name: "PAGE_OUT_OF_RANGE", kind: "Invalid", retryable: false, docState: "Loaded" }],
	[1302, { id: 1302, name: "STREAM_LENGTH_MISMATCH", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1500, { id: 1500, name: "FILTER_UNKNOWN", kind: "Unsupported", retryable: false, docState: "PartiallyLoaded" }],
	[1501, { id: 1501, name: "FILTER_PARAM_INVALID", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1502, { id: 1502, name: "FILTER_CHAIN_TOO_LONG", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1510, { id: 1510, name: "FLATE_CORRUPT", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1511, { id: 1511, name: "PREDICTOR_INVALID", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1520, { id: 1520, name: "LZW_CORRUPT", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1530, { id: 1530, name: "ASCII_CORRUPT", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1540, { id: 1540, name: "RUNLENGTH_CORRUPT", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1545, { id: 1545, name: "DCT_CORRUPT", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1546, { id: 1546, name: "FAX_CORRUPT", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1547, { id: 1547, name: "JBIG2_CORRUPT", kind: "Malformed", retryable: false, docState: "PartiallyLoaded" }],
	[1548, { id: 1548, name: "JBIG2_UNSUPPORTED", kind: "Unsupported", retryable: false, docState: "PartiallyLoaded" }],
	[1550, { id: 1550, name: "IMAGE_UNSUPPORTED", kind: "Unsupported", retryable: false, docState: "Loaded" }],
	[1560, { id: 1560, name: "FUNCTION_INPUT_COUNT", kind: "Invalid", retryable: false, docState: "PartiallyLoaded" }],
	[1561, { id: 1561, name: "FUNCTION_BUDGET", kind: "Budget", retryable: false, docState: "PartiallyLoaded" }],
	[1551, { id: 1551, name: "IMAGE_MALFORMED", kind: "Malformed", retryable: false, docState: "Unchanged" }],
	[1800, { id: 1800, name: "ENCRYPTED", kind: "Auth", retryable: true, docState: "NotLoaded" }],
	[1801, { id: 1801, name: "WRONG_PASSWORD", kind: "Auth", retryable: true, docState: "NotLoaded" }],
	[1802, { id: 1802, name: "ENCRYPT_UNSUPPORTED", kind: "Unsupported", retryable: false, docState: "NotLoaded" }],
	[1803, { id: 1803, name: "ENCRYPT_MALFORMED", kind: "Malformed", retryable: false, docState: "NotLoaded" }],
	[1804, { id: 1804, name: "PERMISSION_DENIED", kind: "Policy", retryable: false, docState: "Unchanged" }],
	[1805, { id: 1805, name: "PERMS_TAMPERED", kind: "Malformed", retryable: false, docState: "NotLoaded" }],
	[1806, { id: 1806, name: "ALREADY_ENCRYPTED", kind: "Invalid", retryable: false, docState: "NotLoaded" }],
	[1807, { id: 1807, name: "RECIPIENT_NO_MATCH", kind: "Auth", retryable: true, docState: "NotLoaded" }],
	[1808, { id: 1808, name: "PERMISSION_DENIED_BY_CMS", kind: "Policy", retryable: false, docState: "Unchanged" }],
	[2000, { id: 2000, name: "SHAPE_FONT", kind: "Malformed", retryable: false, docState: "Unchanged" }],
	[2300, { id: 2300, name: "SMASK_MALFORMED", kind: "Malformed", retryable: false, docState: "Unchanged" }],
	[2301, { id: 2301, name: "PATTERN_MALFORMED", kind: "Malformed", retryable: false, docState: "Unchanged" }],
	[2800, { id: 2800, name: "WRITE_FAILED", kind: "Io", retryable: true, docState: "Unchanged" }],
	[2801, { id: 2801, name: "PREFIX_VIOLATION", kind: "Internal", retryable: false, docState: "Unchanged" }],
	[2802, { id: 2802, name: "VERIFY_FAILED", kind: "Internal", retryable: false, docState: "Unchanged" }],
	[2803, { id: 2803, name: "MUTATION_INVALID", kind: "Invalid", retryable: false, docState: "Unchanged" }],
	[2804, { id: 2804, name: "SOURCE_CHANGED", kind: "Io", retryable: false, docState: "Unchanged" }],
	[2805, { id: 2805, name: "NOTHING_TO_WRITE", kind: "Invalid", retryable: false, docState: "Unchanged" }],
	[2806, { id: 2806, name: "MERGE_CONFLICT", kind: "Invalid", retryable: false, docState: "Unchanged" }],
	[2807, { id: 2807, name: "SIGNATURE_WOULD_BREAK", kind: "Policy", retryable: false, docState: "Unchanged" }],
	[3600, { id: 3600, name: "CONFORMANCE_RULE_UNEVALUATED", kind: "Unsupported", retryable: false, docState: "Loaded" }],
	[4000, { id: 4000, name: "BUDGET_BYTES", kind: "Budget", retryable: true, docState: "PartiallyLoaded" }],
	[4001, { id: 4001, name: "BUDGET_WALL", kind: "Budget", retryable: true, docState: "PartiallyLoaded" }],
	[4002, { id: 4002, name: "BUDGET_DEPTH", kind: "Budget", retryable: false, docState: "PartiallyLoaded" }],
	[4003, { id: 4003, name: "BUDGET_OBJECTS", kind: "Budget", retryable: true, docState: "PartiallyLoaded" }],
	[4004, { id: 4004, name: "BUDGET_PIXELS", kind: "Budget", retryable: true, docState: "Loaded" }],
	[4010, { id: 4010, name: "BUDGET_POISONED", kind: "Budget", retryable: false, docState: "PartiallyLoaded" }],
	[4020, { id: 4020, name: "CANCELLED", kind: "Cancelled", retryable: true, docState: "Unchanged" }],
	[4100, { id: 4100, name: "INTERNAL_PANIC", kind: "Internal", retryable: false, docState: "Unchanged" }],
	[4101, { id: 4101, name: "INTERNAL_INVARIANT", kind: "Internal", retryable: false, docState: "Unchanged" }],
	[4300, { id: 4300, name: "FEATURE_NOT_ENTITLED", kind: "Policy", retryable: false, docState: "Unchanged" }],
	[4301, { id: 4301, name: "ACTIVE_CONTENT_BLOCKED", kind: "Policy", retryable: false, docState: "Loaded" }],
	[4302, { id: 4302, name: "CLOUD_CONSENT_REQUIRED", kind: "Policy", retryable: false, docState: "Unchanged" }],
	[5000, { id: 5000, name: "IO_READ_FAILED", kind: "Io", retryable: true, docState: "NotLoaded" }],
	[5001, { id: 5001, name: "IO_UNEXPECTED_EOF", kind: "Io", retryable: false, docState: "PartiallyLoaded" }],
	[5002, { id: 5002, name: "IO_PENDING", kind: "Io", retryable: true, docState: "PartiallyLoaded" }],
	[5003, { id: 5003, name: "IO_NOT_RANDOM_ACCESS", kind: "Io", retryable: true, docState: "PartiallyLoaded" }],
	[5004, { id: 5004, name: "SINK_FINISHED", kind: "Internal", retryable: false, docState: "Unchanged" }],
	[5005, { id: 5005, name: "SOURCE_TOO_LARGE", kind: "Budget", retryable: false, docState: "NotLoaded" }],
	[6000, { id: 6000, name: "BINDING_BAD_HANDLE", kind: "Invalid", retryable: false, docState: "Unchanged" }],
	[6001, { id: 6001, name: "BINDING_BAD_ARGUMENT", kind: "Invalid", retryable: false, docState: "Unchanged" }],
	[6010, { id: 6010, name: "SANDBOX_MEMORY_CAP", kind: "Budget", retryable: true, docState: "Loaded" }],
	[6011, { id: 6011, name: "SANDBOX_FUEL", kind: "Budget", retryable: true, docState: "Loaded" }],
	[6012, { id: 6012, name: "SANDBOX_TRAP", kind: "Internal", retryable: false, docState: "Loaded" }],
	[6013, { id: 6013, name: "SANDBOX_IMPORT_DENIED", kind: "Invalid", retryable: false, docState: "Loaded" }],
	[6014, { id: 6014, name: "SANDBOX_PROTOCOL", kind: "Invalid", retryable: false, docState: "Loaded" }],
	[6015, { id: 6015, name: "SANDBOX_MODULE_ERROR", kind: "Malformed", retryable: false, docState: "Loaded" }],
	[6016, { id: 6016, name: "SANDBOX_HOST_ERROR", kind: "Internal", retryable: false, docState: "Loaded" }],
	[6017, { id: 6017, name: "BINDING_UNSUPPORTED_OP", kind: "Unsupported", retryable: false, docState: "Unchanged" }],
]);

/** The registry's code count, asserted against `codes.toml` by the sync test. */
export const REGISTRY_SIZE = 83;