/**
 * The four local intake paths, end to end (SL-4.WEB.03).
 *
 * `handoff.ts` decides *what* a drop, a pick, a paste, or a `?src=` URL is;
 * this module carries each accepted document the rest of the way: it names the
 * source, plans the WASM.01 `open` request through `worker-glue.ts`, and — for
 * a `?src=` URL — pulls the bytes down with bodyless range `GET`s into local
 * memory. Nothing here hands a document to the network: the only request this
 * module can issue is `GET <url>` with a `Range` header and no body, and
 * `no-upload.ts` proves it in CI.
 *
 * Deliberately DOM-free. The shell's listeners (drop, paste, the file input,
 * `location.href`) are thin: they build an {@link Intake} from the event and
 * call {@link runIntake}. That split is why the paths can be tested in Node and
 * why no filtering logic can hide in a listener.
 *
 * The network call is resolved at call time (`deps.fetch ?? globalThis.fetch`),
 * never captured at import, so the CI recorder installed over `fetch` observes
 * the real requests instead of a stand-in.
 */

import {
	type HandoffClipboard,
	type HandoffDataTransfer,
	type HandoffFile,
	type HandoffResult,
	MAX_HANDOFF_BYTES,
	filesFromDrop,
	filesFromPaste,
	filesFromPicker,
	parseSrcParam,
} from "./handoff.js";
import {
	type BudgetSurface,
	type Delivery,
	type GlueSource,
	buildOpenRequest,
	deliveryFor,
} from "./worker-glue.js";

/** Which entry point a document arrived through. */
export type IntakePath = "drop" | "picker" | "paste" | "url";

/** One intake event, in the shape the DOM delivers it. */
export type Intake =
	| { readonly path: "drop"; readonly dataTransfer: HandoffDataTransfer }
	| { readonly path: "picker"; readonly files: ArrayLike<HandoffFile> }
	| { readonly path: "paste"; readonly clipboardData: HandoffClipboard }
	| { readonly path: "url"; readonly href: string };

/** A document resolved to a local Worker open, before any byte is moved. */
export interface PlannedOpen {
	/** Request id in the WASM.01 envelope; unique within a session. */
	readonly id: number;
	readonly path: IntakePath;
	readonly name: string;
	/** The source descriptor handed to the Worker (a handle, never raw bytes). */
	readonly source: GlueSource;
	/** How the shell must deliver the bytes before dispatching `open`. */
	readonly delivery: Delivery;
	/** The `open` request JSON (`worker-glue.ts` owns the shape). */
	readonly request: Record<string, unknown>;
}

/** A {@link PlannedOpen} after its bytes have landed in local memory. */
export interface OpenedDocument extends PlannedOpen {
	/**
	 * Bytes this path pulled over the network. Always 0 for handle sources:
	 * drop, pick, and paste hand the Worker a `Blob`/OPFS/FSA handle and the
	 * bytes never move at all.
	 */
	readonly downloaded: number;
}

/** Outcome of {@link planIntake}. */
export type PlanResult =
	| {
			readonly ok: true;
			readonly opens: readonly PlannedOpen[];
			readonly rejected: readonly string[];
	  }
	| { readonly ok: false; readonly reason: string };

/** Outcome of {@link runIntake}. */
export type RunResult =
	| {
			readonly ok: true;
			readonly opens: readonly OpenedDocument[];
			readonly rejected: readonly string[];
	  }
	| { readonly ok: false; readonly reason: string };

/** Where `http-range` bytes land: local memory, an OPFS file, worker memory. */
export interface RangeSink {
	write(chunk: Uint8Array): void | Promise<void>;
}

/** A {@link RangeSink} that keeps everything in this process. */
export interface CollectingSink extends RangeSink {
	readonly chunks: readonly Uint8Array[];
	/** Everything written so far, concatenated in order. */
	bytes(): Uint8Array;
}

/** Build an in-process sink (the shell's default before the Worker takes over). */
export function collectingSink(): CollectingSink {
	const chunks: Uint8Array[] = [];
	return {
		chunks,
		write(chunk: Uint8Array): void {
			chunks.push(chunk);
		},
		bytes(): Uint8Array {
			let total = 0;
			for (const chunk of chunks) {
				total += chunk.length;
			}
			const out = new Uint8Array(total);
			let offset = 0;
			for (const chunk of chunks) {
				out.set(chunk, offset);
				offset += chunk.length;
			}
			return out;
		},
	};
}

/** Default range request size: 1 MiB, the working unit the WASM.06 driver uses. */
export const DEFAULT_RANGE_CHUNK = 1024 * 1024;

/** Options shared by {@link planIntake} and {@link runIntake}. */
export interface IntakeOptions {
	/** First request id; ids increase per accepted document. */
	readonly idBase?: number | undefined;
	/** Budget surface for the `open` request. */
	readonly surface?: BudgetSurface | undefined;
}

/** Everything {@link runIntake} needs beyond the event itself. */
export interface IntakeDeps extends IntakeOptions {
	/**
	 * Range fetcher. Defaults to the live `globalThis.fetch`, resolved per
	 * call so the CI recorder sees the real request.
	 */
	readonly fetch?: typeof fetch | undefined;
	/** Where `http-range` bytes go. Defaults to a {@link CollectingSink}. */
	readonly sink?: RangeSink | undefined;
	/** Range request size; defaults to {@link DEFAULT_RANGE_CHUNK}. */
	readonly chunkBytes?: number | undefined;
	/** Hard cap on bytes pulled from one `?src=` URL. */
	readonly maxRangeBytes?: number | undefined;
}

/** The three file-shaped paths share one filter; only the event differs. */
type FileIntake = Extract<Intake, { readonly path: "drop" | "picker" | "paste" }>;

function filesFor(intake: FileIntake): HandoffResult {
	switch (intake.path) {
		case "drop":
			return filesFromDrop(intake.dataTransfer);
		case "picker":
			return filesFromPicker(intake.files);
		case "paste":
			return filesFromPaste(intake.clipboardData);
	}
}

/** A display name for a `?src=` document, from the URL's last path segment. */
function nameFromUrl(url: string): string {
	const withoutQuery = url.split("?")[0] ?? url;
	const segments = withoutQuery.split("/");
	const last = segments[segments.length - 1] ?? "";
	return last === "" ? "document" : last;
}

function planned(
	id: number,
	path: IntakePath,
	name: string,
	source: GlueSource,
	surface: BudgetSurface,
): PlannedOpen {
	return {
		id,
		path,
		name,
		source,
		delivery: deliveryFor(source),
		request: buildOpenRequest(id, source, surface),
	};
}

/**
 * Resolve an intake event to local Worker opens, moving no bytes and touching
 * no network. This is the whole of the "all local" claim for drop, pick, and
 * paste: their sources are handles the Worker reads itself.
 */
export function planIntake(intake: Intake, options: IntakeOptions = {}): PlanResult {
	const surface = options.surface ?? "viewer";
	const idBase = options.idBase ?? 1;
	if (intake.path === "url") {
		const opening = parseSrcParam(intake.href);
		if (!opening.ok) {
			return { ok: false, reason: opening.reason };
		}
		if (opening.source.kind !== "http-range") {
			return { ok: false, reason: `unsupported ?src= source "${opening.source.kind}"` };
		}
		const source: GlueSource = { kind: "http-range", url: opening.source.url };
		return {
			ok: true,
			opens: [planned(idBase, "url", nameFromUrl(source.url), source, surface)],
			rejected: [],
		};
	}
	const result = filesFor(intake);
	const opens = result.accepted.map((file, index) => {
		const id = idBase + index;
		// A handle, never bytes: the Worker reads the Blob with `FileReaderSync`
		// (WASM.05 `BlobSource`), so the main thread never parses a document.
		const source: GlueSource = { kind: "blob", sourceId: `b${id}` };
		return planned(id, intake.path, file.name, source, surface);
	});
	return { ok: true, opens, rejected: result.rejected };
}

/** Live `fetch`, or null where there is none (SSR, a locked-down test env). */
function liveFetch(): typeof fetch | null {
	const scope = globalThis as unknown as { fetch?: typeof fetch };
	return typeof scope.fetch === "function" ? scope.fetch : null;
}

function describeError(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

interface Downloaded {
	readonly ok: true;
	readonly bytes: number;
}

interface DownloadFailed {
	readonly ok: false;
	readonly reason: string;
}

/**
 * Pull a `?src=` document into `sink` with bodyless range `GET`s.
 *
 * Every request here is `GET` with a `Range` header and no body — the document
 * travels inbound, into this process. There is deliberately no retry against a
 * server-side renderer and no "upload the file so we can parse it" fallback: a
 * silent fallback is exactly what would make the local-first claim false
 * (ADR-P0016). A failure is reported, not papered over.
 */
async function downloadRange(url: string, deps: IntakeDeps): Promise<Downloaded | DownloadFailed> {
	const doFetch = deps.fetch ?? liveFetch();
	if (doFetch === null) {
		return { ok: false, reason: "no fetch available to open ?src=" };
	}
	const sink = deps.sink ?? collectingSink();
	const chunkBytes = deps.chunkBytes ?? DEFAULT_RANGE_CHUNK;
	const maxBytes = deps.maxRangeBytes ?? MAX_HANDOFF_BYTES;
	let received = 0;
	let start = 0;
	for (;;) {
		let response: Response;
		try {
			response = await doFetch(url, {
				method: "GET",
				headers: { Range: `bytes=${start}-${start + chunkBytes - 1}` },
			});
		} catch (error) {
			return { ok: false, reason: `range fetch failed: ${describeError(error)}` };
		}
		if (!response.ok) {
			return { ok: false, reason: `range fetch returned HTTP ${response.status}` };
		}
		const chunk = new Uint8Array(await response.arrayBuffer());
		if (chunk.length === 0) {
			break;
		}
		if (received + chunk.length > maxBytes) {
			return { ok: false, reason: `?src= document exceeds the ${maxBytes} byte cap` };
		}
		received += chunk.length;
		await sink.write(chunk);
		// HTTP 200 (not 206) means the origin ignored the Range header and sent
		// the whole document; there is nothing left to ask for. A short chunk
		// likewise means the end of the document.
		if (response.status !== 206 || chunk.length < chunkBytes) {
			break;
		}
		start += chunk.length;
	}
	return { ok: true, bytes: received };
}

/**
 * Run one intake event: plan the local opens, then move the bytes each one
 * needs — which is to say, none, except for a `?src=` URL.
 */
export async function runIntake(intake: Intake, deps: IntakeDeps = {}): Promise<RunResult> {
	const plan = planIntake(intake, { idBase: deps.idBase, surface: deps.surface });
	if (!plan.ok) {
		return plan;
	}
	const opened: OpenedDocument[] = [];
	for (const open of plan.opens) {
		if (open.source.kind !== "http-range") {
			opened.push({ ...open, downloaded: 0 });
			continue;
		}
		const download = await downloadRange(open.source.url, deps);
		if (!download.ok) {
			return { ok: false, reason: download.reason };
		}
		opened.push({ ...open, downloaded: download.bytes });
	}
	return { ok: true, opens: opened, rejected: plan.rejected };
}
