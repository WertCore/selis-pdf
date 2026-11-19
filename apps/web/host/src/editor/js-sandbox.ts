/**
 * Browser form-script runner (SL-5.JS.01/02/03, ADR-P0020).
 *
 * Uses the browser's OWN engine in a fresh sandboxed Worker. No WASM engine is
 * compiled, so there are no new dependencies and no wasm bytes. The Worker is
 * created only after the consent/origin gate allows the document, and is
 * terminated on a hard deadline — an infinite loop cannot outlive that timer.
 *
 * The postMessage protocol carries only the script text, field values and an
 * event descriptor in, and only values out. No document bytes, URLs or host
 * handles cross the boundary.
 */

import {
	type DocumentIdentity,
	type ScriptAuthorization,
	authorizeScripting,
	denyAllAuthorization,
} from "./js-policy.js";
import {
	REMOVED_GLOBALS,
	type SandboxField,
	type SandboxRequest,
	buildWorkerBootstrap,
} from "./js-worker.js";

/** A field visible to the script. */
export type FormField = SandboxField;

/** The event a script handles. */
export interface FormScriptEvent {
	readonly type: "calculate" | "validate" | "format";
	readonly targetName: string;
	readonly value?: string;
}

/** The caller's request. No document bytes or URLs are accepted. */
export interface FormScriptRequest {
	readonly script: string;
	readonly event: FormScriptEvent;
	readonly fields: readonly FormField[];
}

/** Limits for one run. `deadlineMs` is the hard Worker termination deadline. */
export interface JsBudget {
	readonly deadlineMs: number;
	readonly maxScriptBytes: number;
	readonly maxFields: number;
	readonly maxStringBytes: number;
}

/** A conservative viewer default. */
export const DEFAULT_JS_BUDGET: JsBudget = {
	deadlineMs: 50,
	maxScriptBytes: 65_536,
	maxFields: 512,
	maxStringBytes: 65_536,
};

/** Typed failures mirror the Rust codes (kebab-case over the wire). */
export type JsFailureCode =
	| "js-consent-required"
	| "js-api-refused"
	| "js-script-rejected"
	| "js-instruction-budget"
	| "js-memory-budget"
	| "js-runtime-error"
	| "js-worker-error";

/** The result of one run. */
export type FormScriptOutcome =
	| {
			readonly ok: true;
			readonly eventValue: string;
			readonly eventRc: boolean;
			readonly updates: readonly FormField[];
	  }
	| { readonly ok: false; readonly code: JsFailureCode };

/** The minimum Worker surface we use — injectable so tests can supply a mock. */
export interface WorkerLike {
	onmessage: ((event: { readonly data: unknown }) => void) | null;
	onerror: ((event: unknown) => void) | null;
	postMessage(message: unknown): void;
	terminate(): void;
}

/** Creates a fresh worker from the bootstrap source. */
export type WorkerFactory = (bootstrapSource: string) => WorkerLike;

/** Timers are injected so tests can fire the deadline deterministically. */
export interface Timers {
	setTimeout(fn: () => void, ms: number): unknown;
	clearTimeout(handle: unknown): void;
}

/** All policy and infrastructure knobs for one run. */
export interface RunOptions {
	readonly budget?: JsBudget;
	readonly authorization?: ScriptAuthorization;
	readonly document?: DocumentIdentity;
	readonly workerFactory?: WorkerFactory;
	readonly timers?: Timers;
}

/** The default factory: a blob-URL Worker in the browser's own realm. */
export const browserWorkerFactory: WorkerFactory = (source: string): WorkerLike => {
	const url = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
	const worker = new Worker(url);
	URL.revokeObjectURL(url);
	return worker as unknown as WorkerLike;
};

const systemTimers: Timers = {
	setTimeout: (fn: () => void, ms: number): unknown => globalThis.setTimeout(fn, ms),
	clearTimeout: (handle: unknown): void => {
		globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>);
	},
};

/** Acrobat APIs outside the calculation/format whitelist (mirror of Rust). */
export const FORBIDDEN_ACROBAT_APIS: readonly string[] = [
	"app",
	"launchURL",
	"Net",
	"Collab",
	"submitForm",
	"exportDataObject",
	"importDataObject",
	"getURL",
	"require",
	"process",
	"WebAssembly",
	"fetch",
	"XMLHttpRequest",
	"WebSocket",
	"indexedDB",
	"importScripts",
	"postMessage",
	"Worker",
];

/** Find the first forbidden identifier as a whole token, if any. */
export function findForbiddenApi(script: string): string | null {
	let i = 0;
	while (i < script.length) {
		const ch = script.charAt(i);
		if (/[A-Za-z0-9_$]/.test(ch)) {
			const start = i;
			while (i < script.length && /[A-Za-z0-9_$]/.test(script.charAt(i))) {
				i++;
			}
			const token = script.slice(start, i);
			if (FORBIDDEN_ACROBAT_APIS.includes(token)) {
				return token;
			}
		} else {
			i++;
		}
	}
	return null;
}

function validRequest(request: FormScriptRequest, budget: JsBudget): boolean {
	if (
		request.script.length === 0 ||
		request.script.length > budget.maxScriptBytes ||
		request.script.includes("\0") ||
		request.fields.length > budget.maxFields ||
		request.event.targetName.length > budget.maxStringBytes ||
		(request.event.value ?? "").length > budget.maxStringBytes
	) {
		return false;
	}
	const seen = new Set<string>();
	for (const field of request.fields) {
		if (
			field.name.length === 0 ||
			field.name.length > budget.maxStringBytes ||
			field.value.length > budget.maxStringBytes ||
			field.name.includes("\0") ||
			field.value.includes("\0") ||
			seen.has(field.name)
		) {
			return false;
		}
		seen.add(field.name);
	}
	return true;
}

function toSandboxRequest(request: FormScriptRequest): SandboxRequest {
	return {
		script: request.script,
		event: {
			type: request.event.type,
			targetName: request.event.targetName,
			value: request.event.value ?? "",
		},
		fields: request.fields.map((field) => ({ name: field.name, value: field.value })),
	};
}

function parseResponse(data: unknown): FormScriptOutcome {
	if (typeof data !== "object" || data === null) {
		return { ok: false, code: "js-worker-error" };
	}
	const record = data as Record<string, unknown>;
	if (record.ok === true) {
		const eventValue = typeof record.eventValue === "string" ? record.eventValue : "";
		const eventRc = typeof record.eventRc === "boolean" ? record.eventRc : true;
		const updates: FormField[] = [];
		if (Array.isArray(record.updates)) {
			for (const entry of record.updates) {
				if (
					typeof entry === "object" &&
					entry !== null &&
					typeof (entry as Record<string, unknown>).name === "string" &&
					typeof (entry as Record<string, unknown>).value === "string"
				) {
					const row = entry as Record<string, unknown>;
					updates.push({ name: row.name as string, value: row.value as string });
				}
			}
		}
		return { ok: true, eventValue, eventRc, updates };
	}
	if (record.code === "js-script-rejected" || record.code === "js-runtime-error") {
		return { ok: false, code: record.code };
	}
	return { ok: false, code: "js-worker-error" };
}

/**
 * Run one form script in a sandboxed Worker, gated by consent/origin policy.
 *
 * A denied document never creates a Worker. A script that outlives the deadline
 * has its Worker terminated and returns `js-instruction-budget`.
 */
export async function runFormScript(
	request: FormScriptRequest,
	options: RunOptions = {},
): Promise<FormScriptOutcome> {
	// Default is deny-all: no policy, no document, no run.
	const auth = options.authorization ?? denyAllAuthorization();
	const doc = options.document;
	if (doc === undefined || authorizeScripting(auth, doc).kind === "deny") {
		return { ok: false, code: "js-consent-required" };
	}

	const budget = options.budget ?? DEFAULT_JS_BUDGET;
	if (!validRequest(request, budget)) {
		return { ok: false, code: "js-script-rejected" };
	}
	// Defence in depth: the realm itself has no network capability; this gives
	// a typed refusal before a Worker is even created.
	if (findForbiddenApi(request.script) !== null) {
		return { ok: false, code: "js-api-refused" };
	}

	const factory = options.workerFactory ?? browserWorkerFactory;
	const timers = options.timers ?? systemTimers;
	let worker: WorkerLike;
	try {
		worker = factory(buildWorkerBootstrap());
	} catch {
		return { ok: false, code: "js-worker-error" };
	}

	return await new Promise<FormScriptOutcome>((resolve) => {
		let settled = false;
		const timer: { handle?: unknown } = {};
		const finish = (outcome: FormScriptOutcome): void => {
			if (settled) {
				return;
			}
			settled = true;
			timers.clearTimeout(timer.handle);
			worker.terminate();
			resolve(outcome);
		};

		timer.handle = timers.setTimeout(() => {
			finish({ ok: false, code: "js-instruction-budget" });
		}, budget.deadlineMs);

		worker.onmessage = (event) => {
			finish(parseResponse(event.data));
		};
		worker.onerror = () => {
			finish({ ok: false, code: "js-runtime-error" });
		};
		try {
			worker.postMessage(toSandboxRequest(request));
		} catch {
			finish({ ok: false, code: "js-worker-error" });
		}
	});
}

/** The capability names the worker bootstrap removes (re-exported for audits). */
export { REMOVED_GLOBALS };
