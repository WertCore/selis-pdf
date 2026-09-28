/**
 * SL-4.EXT.03 - the WASM.01 guest ABI, as JavaScript sees it.
 *
 * ## What this file is
 *
 * `crates/selis-pdf-wasm` exports a C ABI over guest linear memory
 * (ADR-P0041, ADR-P0042): everything crosses as a `(ptr, len)` pair into a
 * buffer the *guest* allocated, and the host copies out of it. This module is
 * the other end of that contract, and it is deliberately the only place in
 * the extension that knows a pointer exists.
 *
 * The split matters for the same reason `apps/ui` is forbidden from touching
 * platform globals: everything above this file speaks bytes and JSON, so the
 * ABI can be tested - and a mistake in it caught - without a browser. It is
 * also the one place where a wrong offset is a memory-safety bug rather than a
 * wrong answer, so every length is checked against what the guest wrote
 * before a single byte is read out of its memory.
 *
 * ## The ownership rules, which are the whole of the ABI
 *
 * 1. **The guest allocates; the host frees.** `selis_input_alloc` returns a
 *    pointer into the guest's heap; the host copies bytes in and hands the
 *    pair straight back to `selis_dispatch`. The guest never sees a host
 *    pointer, so there is no host memory to alias.
 * 2. **Every allocation is freed exactly once**, including on the failure
 *    paths. A leaked buffer in a long-lived offscreen document is a leak that
 *    only shows up as a later document failing a budget it should have
 *    passed.
 * 3. **The view is re-derived after every call.** A wasm memory can grow on
 *    any call, which detaches every typed array over `memory.buffer`.
 *    Caching one view across a dispatch is the classic way to read a detached
 *    buffer and get zeros.
 *
 * `xtask wasm-protocol` drives the same exports from wasmtime against the
 * real guest, so both ends of this contract are checked against the spec
 * rather than only against each other.
 */

/** The exports of the core guest this module drives. */
export interface GuestExports {
	readonly memory: WebAssembly.Memory;
	selis_input_alloc(len: number): number;
	selis_free(ptr: number, len: number): void;
	selis_dispatch(
		reqPtr: number,
		reqLen: number,
		payloadPtr: number,
		payloadLen: number,
		outPtr: number,
	): number;
}

/** A response as the guest writes it (`protocol.rs`'s `ResponseMessage`). */
export interface GuestResponse {
	readonly v: number;
	readonly id: number;
	readonly ok?: boolean;
	readonly value?: unknown;
	readonly code?: number;
	readonly message?: string;
	readonly detail?: string;
	readonly docState?: string;
}

/** One dispatch: the response, plus the attachment if the op sent one. */
export interface GuestReply {
	readonly response: GuestResponse;
	readonly attachment: Uint8Array;
}

/** The guest's copy-out words: `[response_len, payload_ptr, payload_len]`. */
const OUT_WORDS = 3;

/**
 * `selis_input_alloc`'s own bound; a claim above it returns null.
 *
 * The same 64 MiB the Rust side enforces (SL-4.WASM.01's inline-source
 * bound), checked here too because a claim above it must never be written:
 * a refusal is the only safe answer.
 */
const MAX_ALLOC_BYTES = 64 * 1024 * 1024;

/** A live guest. */
export interface WasmGuest {
	/** Dispatch one request, with an optional binary attachment. */
	send(request: unknown, attachment?: Uint8Array): GuestReply;
}

/** Raised when the guest's answer does not obey the ABI it documents. */
export class GuestAbiError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "GuestAbiError";
	}
}

/** Build a {@link WasmGuest} over an already-instantiated module. */
export function createWasmGuest(exports: GuestExports): WasmGuest {
	/** A fresh view of guest memory. Never cached - see the module docs. */
	const view = (): Uint8Array => new Uint8Array(exports.memory.buffer);

	/** Copy `bytes` into a guest buffer; the caller frees it with `free`. */
	const copyIn = (bytes: Uint8Array): { ptr: number; len: number } => {
		if (bytes.byteLength > MAX_ALLOC_BYTES) {
			throw new GuestAbiError(
				`a ${bytes.byteLength} byte buffer is above the protocol's 64 MiB bound`,
			);
		}
		const ptr = exports.selis_input_alloc(bytes.byteLength);
		if (ptr === 0) {
			throw new GuestAbiError(`the guest refused an allocation of ${bytes.byteLength} bytes`);
		}
		view().set(bytes, ptr);
		return { ptr, len: bytes.byteLength };
	};

	const free = (buffer: { ptr: number; len: number } | null): void => {
		if (buffer !== null && buffer.ptr !== 0) {
			exports.selis_free(buffer.ptr, buffer.len);
		}
	};

	/** Read `len` bytes at `ptr`, refusing anything outside guest memory. */
	const readBytes = (ptr: number, len: number): Uint8Array => {
		const bytes = view();
		if (ptr === 0 || len === 0) {
			return new Uint8Array(0);
		}
		if (ptr < 0 || len < 0 || ptr + len > bytes.byteLength) {
			throw new GuestAbiError(
				`the guest reported (${ptr}, ${len}), outside its ${bytes.byteLength} byte memory`,
			);
		}
		return bytes.slice(ptr, ptr + len);
	};

	const readU32 = (ptr: number): number => {
		const bytes = view();
		if (ptr < 0 || ptr + 4 > bytes.byteLength) {
			throw new GuestAbiError(`the guest wrote a word at ${ptr}, outside its memory`);
		}
		// A DataView rather than four indexed reads: the bytes are `number |
		// undefined` under `noUncheckedIndexedAccess`, and a shift on an
		// undefined would be a silent zero in a word that decides a length.
		return new DataView(bytes.buffer, bytes.byteOffset + ptr, 4).getUint32(0, true);
	};

	return {
		send(request, attachment) {
			// Everything this method allocates is freed in one `finally`, which
			// includes the allocations that happen *before* the dispatch: an
			// over-sized attachment throws out of `copyIn`, and a `finally`
			// that only wrapped the dispatch would leak the request.
			const req = copyIn(new TextEncoder().encode(JSON.stringify(request)));
			let payload: { ptr: number; len: number } | null = null;
			let out: { ptr: number; len: number } | null = null;
			let responsePtr = 0;
			let responseLen = 0;
			let replyPtr = 0;
			let replyLen = 0;
			try {
				payload = attachment === undefined ? null : copyIn(attachment);
				out = copyIn(new Uint8Array(OUT_WORDS * 4));
				responsePtr = exports.selis_dispatch(
					req.ptr,
					req.len,
					payload?.ptr ?? 0,
					payload?.len ?? 0,
					out.ptr,
				);
				responseLen = readU32(out.ptr);
				replyPtr = readU32(out.ptr + 4);
				replyLen = readU32(out.ptr + 8);
				// The copies are read *before* their buffers are freed, because
				// the response and the attachment are guest allocations too.
				const responseBytes = readBytes(responsePtr, responseLen);
				const replyBytes = readBytes(replyPtr, replyLen);
				if (responsePtr === 0) {
					throw new GuestAbiError(
						"selis_dispatch returned null - the guest produced no response buffer",
					);
				}
				if (responseLen === 0) {
					throw new GuestAbiError("the guest returned a zero-length response");
				}
				return { response: parseResponse(responseBytes), attachment: replyBytes };
			} finally {
				// Every buffer this call allocated, freed once, whether the
				// call returned, threw, or never dispatched.
				free(req);
				free(payload);
				free(out);
				free({ ptr: responsePtr, len: responseLen });
				free({ ptr: replyPtr, len: replyLen });
			}
		},
	};
}

/**
 * Parse a response, refusing anything that is not a protocol message.
 *
 * A guest that answered with a truncated buffer would otherwise be read as
 * "no value" and turned into a blank page somewhere further along, which is
 * the class of failure this whole layer exists to make loud.
 */
function parseResponse(bytes: Uint8Array): GuestResponse {
	let parsed: unknown;
	try {
		parsed = JSON.parse(new TextDecoder().decode(bytes));
	} catch (error) {
		throw new GuestAbiError(
			`the guest's response is not JSON (${
				error instanceof Error ? error.message : "unparseable"
			})`,
		);
	}
	if (typeof parsed !== "object" || parsed === null) {
		throw new GuestAbiError("the guest's response is not an object");
	}
	const response = parsed as GuestResponse;
	if (typeof response.v !== "number" || typeof response.id !== "number") {
		throw new GuestAbiError("the guest's response carries no version and id");
	}
	if (typeof response.ok !== "boolean") {
		throw new GuestAbiError(`the guest's response for id ${response.id} is neither ok nor failed`);
	}
	return response;
}
