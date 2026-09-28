/**
 * SL-4.EXT.06 — the wire between the viewer page and the offscreen engine host.
 *
 * ## Why a wire vocabulary at all
 *
 * The extension reuses `apps/ui` verbatim, and `apps/ui` reaches the engine
 * through `PlatformAdapter.engine` — an in-process interface. In MV3 the engine
 * is not in the viewer's process: it lives in the offscreen document
 * (`offscreen.html`), because a service worker is killed aggressively and
 * document bytes must never transit it (24-BINDINGS-SPEC §5). So the engine
 * port here is a **transport**, and this module is its protocol. It is pure data
 * plus a codec: no `chrome.*`, no `fetch`, no DOM, so both ends and the tests
 * can be exercised under Vitest without a browser. SL-4.EXT.03 writes the
 * server half's engine lifecycle against this same file.
 *
 * ## The constraint that shapes the encoding: it is JSON, not structured clone
 *
 * `chrome.runtime` port messaging serialises with JSON, not the structured
 * clone algorithm. Three consequences are load-bearing, and each one is a
 * decision rather than an accident:
 *
 * 1. **No `ImageBitmap`, no `ArrayBuffer`, no transfer lists.** UI.03's worker
 *    path moves both by *ownership*; nothing can be moved by ownership across
 *    this link. Rendered tiles therefore travel as base64 text, and the
 *    viewer's `TileSurface` has to be a main-thread one
 *    (`takesOwnership: false` — see `surface.ts` and `REUSE.md`).
 * 2. **Document bytes travel as base64 too**, which costs ~33 % on the way out
 *    and another decode on the way in. `apps/web/host` gets them for free
 *    because a `Worker` port is a structured clone. This is the price of the
 *    offscreen host and it is the strongest argument for EXT.03 re-examining the
 *    arrangement; the numbers are in `REUSE.md`.
 * 3. **A blob handle cannot cross.** `File`/`Blob`/`FileSystemFileHandle` are
 *    not serialisable, so the page reads the file and sends bytes. The wire
 *    source is therefore always inline, and the descriptor *kind* is carried
 *    only as information for the host's diagnostics.
 *
 * The alternative was ruled out rather than not considered: the service worker
 * cannot instantiate the WASM engine (no DOM, and MV3 blocks it there), and a
 * `Worker` spawned by the viewer page dies with the page — which is exactly the
 * lifetime the `offscreen` permission was justified for (PERMISSIONS.md,
 * SL-4.EXT.01). See `REUSE.md` for the full argument.
 */

/** Protocol version, so a stale host fails loudly instead of mis-rendering. */
export const ENGINE_PROTOCOL = 1;

/** Name both ends connect with; the second is the reply direction's listener. */
export const ENGINE_PORT_NAMES = {
	/** The viewer page connects with this; the offscreen host answers on it. */
	request: "selis.engine.v1",
	/**
	 * The offscreen host connects with this purely to *listen*. Chrome delivers
	 * a connection to every extension context holding an `onConnect` listener,
	 * the service worker included — and that one must never see a port which
	 * can carry document bytes, so it deliberately has no listener
	 * (`service-worker.js`).
	 */
	accept: "selis.engine.host.v1",
} as const;

/**
 * The viewer page's one message to the service worker, asking it to make sure
 * an offscreen document exists.
 *
 * It lives here because it is the extension's internal protocol vocabulary and
 * both ends import this module: the page sends it, and the worker — which
 * imports nothing from `host-env.ts`, having no DOM — matches on it.
 *
 * **It carries a verb and nothing else.** That is the whole reason the service
 * worker may be involved at all: `chrome.offscreen.createDocument` may only be
 * called from the worker, so asking for a document is unavoidable, and a
 * message that carried document bytes would not be (24-BINDINGS-SPEC §5).
 */
export const ENSURE_ENGINE_HOST = "selis/ensure-engine-host";

/** Budget surface names, matching the Rust `SurfaceName` enum (`worker-glue.ts`). */
export type WireSurface = "thumbnail" | "viewer" | "editor" | "batch" | "server";

/** The UI's `DocState` vocabulary, as it crosses the wire. */
export type WireDocState = "NotLoaded" | "Loaded" | "PartiallyLoaded" | "Unchanged" | "Modified";

/**
 * A document source, always inline bytes.
 *
 * Both supported UI descriptors (`bytes`, `blob`) collapse to this: the page
 * reads the blob and sends the same thing. The descriptors the extension
 * refuses (`opfs`, `fsa`, `file`) are refused *before* a request is built — see
 * `adapter.ts` — so this type never has to represent them and the host never
 * has to guess whether an OPFS path is a path or a document name.
 */
export interface WireSource {
	readonly kind: "inline";
	/** File name for titles and diagnostics. Never a path, never a URL. */
	readonly name: string;
	/** Base64 of the document bytes ({@link encodeBase64}). */
	readonly bytes64: string;
}

/** A rectangle in PDF user space, for a tiled render. */
export interface WireRect {
	readonly x: number;
	readonly y: number;
	readonly width: number;
	readonly height: number;
}

/** Viewer page → offscreen host. Every request carries the id it is answered by. */
export type EngineRequest =
	| {
			readonly v: 1;
			readonly id: number;
			readonly op: "open";
			readonly src: WireSource;
			readonly surface: WireSurface;
	  }
	| { readonly v: 1; readonly id: number; readonly op: "close"; readonly doc: string }
	| {
			readonly v: 1;
			readonly id: number;
			readonly op: "render";
			readonly doc: string;
			readonly page: number;
			readonly scale: number;
			readonly rect?: WireRect;
	  }
	/**
	 * Per-character geometry for one page (SL-4.UI.04, SL-4.EXT.03).
	 *
	 * A separate op from `text` rather than a `format` on it, and the reason
	 * is the payload: `text` answers with a UTF-8 attachment that travels as
	 * one base64 string, while a text layer is per-character rectangles whose
	 * whole value is their precision. One op meaning two wire shapes would
	 * have forced the client's reply validator - which checks every field of
	 * a layer, because a subtly wrong quad still type-checks - to guess
	 * which shape arrived.
	 */
	| {
			readonly v: 1;
			readonly id: number;
			readonly op: "textLayer";
			readonly doc: string;
			readonly page: number;
		}

	| {
			readonly v: 1;
			readonly id: number;
			readonly op: "text";
			readonly doc: string;
			readonly page: number;
	  }
	| {
			readonly v: 1;
			readonly id: number;
			readonly op: "search";
			readonly doc: string;
			readonly query: string;
			readonly caseSensitive: boolean;
			readonly wholeWord: boolean;
			readonly fromPage: number;
	  }
	/**
	 * Cancellation, as its own message rather than a flag on the request: the
	 * request is already in flight and may already be answered. Fire-and-forget,
	 * and a host may ignore it — the client's own rejection is what the UI
	 * sees, so a host that ignores `cancel` wastes work but never hangs the
	 * reader. That property is what makes cancelling *prompt* here rather than
	 * best-effort.
	 */
	| { readonly v: 1; readonly op: "cancel"; readonly id: number };

/** An engine error as it crosses the wire: a registry code and a `docState`. */
export interface WireError {
	readonly code: number;
	readonly message: string;
	readonly docState: WireDocState;
	readonly retryable: boolean;
}

/** An open document's metadata. Mirrors `DocHandle`, with a host-issued id. */
export interface WireDoc {
	readonly doc: string;
	readonly pageCount: number;
	readonly pageSizes: readonly { readonly width: number; readonly height: number }[];
}

/** A rendered tile: RGBA8, base64. */
export interface WireTile {
	readonly page: number;
	readonly width: number;
	readonly height: number;
	readonly format: "rgba8";
	readonly bytes64: string;
}

/** One page of extracted text, in engine reading order. */
export interface WirePageText {
	readonly page: number;
	readonly text: string;
}

/**
 * A page's text layer as the engine host sends it (SL-4.UI.04).
 *
 * Structurally the same as the UI's `PageTextLayer`; named separately because it
 * is untrusted JSON arriving over a port, and every field is validated on the
 * way in rather than cast.
 */
export interface WirePageTextLayer {
	readonly page: number;
	readonly width: number;
	readonly height: number;
	readonly text: string;
	readonly lowConfidence?: boolean;
	readonly lines: readonly {
		readonly text: string;
		readonly rect: { x: number; y: number; width: number; height: number };
		readonly direction: "ltr" | "rtl";
		readonly chars: readonly {
			readonly rect: { x: number; y: number; width: number; height: number };
			readonly advance: number;
			readonly inked?: boolean;
		}[];
	}[];
}

/** One match, as a character range within a page's text. */
export interface WireMatch {
	readonly page: number;
	readonly start: number;
	readonly end: number;
}

/**
 * One progressive slice of a search. `search` is a *stream*: several replies
 * share the request's `id` and the last carries `done: true`. That is why `id`
 * is not a one-shot correlation token, and why a stream needs an explicit
 * terminator rather than the channel going quiet.
 *
 * No `page` field: the UI's `SearchBatch` does not carry one, and every match
 * already names its page. A batch-level page would be a field only the wire
 * believed in.
 */
export interface WireSearchBatch {
	readonly matches: readonly WireMatch[];
	readonly progress: number;
	readonly done: boolean;
}

/**
 * The reply to a request with no result value of its own (`close`).
 *
 * It exists because `replyRejection` refuses an `ok` reply carrying no `value`:
 * a missing value and a dropped reply are indistinguishable to the client, and
 * the point of an explicit terminator is that they never have to be.
 */
export interface WireAck {
	readonly done: true;
}

/** Offscreen host → viewer page. */
export interface EngineReply {
	readonly v: 1;
	readonly id: number;
	readonly ok: boolean;
	/** Present when `ok`. */
	readonly value?: WireDoc | WireTile | WirePageText | WirePageTextLayer | WireSearchBatch | WireAck;
	/** Present when not `ok`. */
	readonly error?: WireError;
}

// ─────────────────────────────────────────────────────────────────────────────
// Base64.
//
// Hand-rolled rather than `btoa`/`atob` for two reasons, both testable: this
// module has to behave identically in the Node test process and in the
// offscreen document's window scope, and the decoder is a **trust boundary**
// (it turns a string off the wire into an allocation), so it is written to
// reject rather than to guess. `engine-protocol.test.ts` pins the round trip
// over every byte value and every length up to 1 000, plus the rejection cases.
// ─────────────────────────────────────────────────────────────────────────────

const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PAD = "=";
const PAD_CODE = 61;

/** Per-character reverse map; `-1` marks a character outside the alphabet. */
const DECODE: Int8Array = (() => {
	const table = new Int8Array(128).fill(-1);
	for (let i = 0; i < ALPHABET.length; i += 1) {
		table[ALPHABET.charCodeAt(i)] = i;
	}
	return table;
})();

/** Characters buffered before the encoder flushes a block. */
const BLOCK_CHARS = 8192;

/** Encode bytes as standard base64 with `=` padding. */
export function encodeBase64(bytes: Uint8Array): string {
	const blocks: string[] = [];
	let current = "";
	const length = bytes.length;
	for (let i = 0; i < length; i += 3) {
		const remaining = length - i;
		const b0 = bytes[i] ?? 0;
		const b1 = remaining > 1 ? (bytes[i + 1] ?? 0) : undefined;
		const b2 = remaining > 2 ? (bytes[i + 2] ?? 0) : undefined;
		current += ALPHABET[b0 >> 2];
		current += ALPHABET[((b0 & 0x03) << 4) | ((b1 ?? 0) >> 4)];
		current += b1 === undefined ? PAD : ALPHABET[((b1 & 0x0f) << 2) | ((b2 ?? 0) >> 6)];
		current += b2 === undefined ? PAD : ALPHABET[b2 & 0x3f];
		if (current.length >= BLOCK_CHARS) {
			blocks.push(current);
			current = "";
		}
	}
	blocks.push(current);
	return blocks.join("");
}

/** Decode standard base64 into bytes, or throw with the reason it was refused. */
export function decodeBase64(text: string): Uint8Array {
	const length = text.length;
	if (length % 4 !== 0) {
		throw new Error(`base64 length ${length} is not a multiple of 4`);
	}
	if (length === 0) {
		return new Uint8Array(0);
	}
	let padding = 0;
	if (text.charCodeAt(length - 1) === PAD_CODE) {
		padding = text.charCodeAt(length - 2) === PAD_CODE ? 2 : 1;
	}
	const bytes = new Uint8Array((length / 4) * 3 - padding);
	let out = 0;
	for (let i = 0; i < length; i += 4) {
		const last = i + 4 === length;
		let value = 0;
		for (let offset = 0; offset < 4; offset += 1) {
			const code = text.charCodeAt(i + offset);
			let six: number;
			if (code === PAD_CODE) {
				// Padding is legal only in the final quad's last two positions.
				if (!last || offset < 2) {
					throw new Error(`base64 padding at offset ${i + offset}`);
				}
				six = 0;
			} else {
				six = code < 128 ? (DECODE[code] ?? -1) : -1;
				if (six < 0) {
					throw new Error(`base64 character at offset ${i + offset} is not in the alphabet`);
				}
			}
			value = (value << 6) | six;
		}
		bytes[out] = (value >> 16) & 0xff;
		if (out + 1 < bytes.length) {
			bytes[out + 1] = (value >> 8) & 0xff;
		}
		if (out + 2 < bytes.length) {
			bytes[out + 2] = value & 0xff;
		}
		out += 3;
	}
	return bytes;
}

/** Why {@link decodeBase64} refused a string. */
export type Base64ErrorReason = "bad-length" | "bad-padding" | "bad-character";

/** Narrow an unknown throw from {@link decodeBase64} to a named reason. */
export function base64ErrorReason(error: unknown): Base64ErrorReason {
	const message = error instanceof Error ? error.message : "";
	if (message.includes("not a multiple of 4")) {
		return "bad-length";
	}
	if (message.includes("padding")) {
		return "bad-padding";
	}
	return "bad-character";
}
