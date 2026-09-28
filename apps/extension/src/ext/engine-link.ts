/**
 * SL-4.EXT.06 — the link half of the engine transport.
 *
 * `chrome.runtime` ports are symmetric, so both ends are the same object seen
 * from opposite sides: something that posts {@link EngineRequest}s and hands
 * back {@link EngineReply}s. Naming that object separately from either end is
 * what lets the whole transport be tested in Node against a loopback pair
 * (`loopback.ts`), and it is the seam EXT.03's offscreen host and this task's
 * viewer page both attach to.
 *
 * ## `replyRejection` is a trust boundary, not a type guard for convenience
 *
 * A port delivers `unknown`. Anything in the extension can post on it — another
 * extension page, a compromised offscreen document, a bug. So every reply is
 * *validated* before it reaches a correlation table, and a message that fails
 * validation is counted and dropped rather than parsed optimistically. Same
 * reasoning as the base64 decoder in `engine-protocol.ts`: a decoder that
 * guesses is a decoder with a memory-safety story.
 *
 * The loopback deliberately lives in its own module so it never reaches the
 * ship list — `PACKAGE_ENTRIES` has no row for it, and the gate fails on any
 * file that is in the package without one.
 */

import type { EngineReply, EngineRequest } from "./engine-protocol.js";
import type { HostPort } from "./host-env.js";

/**
 * Anything this link carries. A port is bidirectional: requests go one way and
 * replies the other, so one `post` serves both ends.
 */
export type EngineMessage = EngineRequest | EngineReply;

/** One direction-pair of the transport, independent of what is on the far end. */
export interface EngineLink {
	/** Send a message. Fire-and-forget; the other direction arrives via {@link subscribe}. */
	post(message: EngineMessage): void;
	/** Observe replies. Returns an unsubscribe. */
	subscribe(listener: (reply: EngineReply) => void): () => void;
	/** Close the link. Idempotent. */
	close(): void;
}

/**
 * Why a message arriving on the wire was refused.
 *
 * `version` is separate from `shape` because they fail for different reasons and
 * the fix is different: a bad shape is a bug in one of our halves, while a good
 * shape at the wrong version is a stale build talking to a new one, which is
 * exactly what `EngineReply.v` exists to make loud.
 */
export type ReplyRejection = "not-an-object" | "version" | "shape";

/**
 * Validate one message off the wire, or say why it was refused.
 */
export function replyRejection(message: unknown): ReplyRejection | null {
	if (typeof message !== "object" || message === null) {
		return "not-an-object";
	}
	const candidate = message as Partial<EngineReply>;
	if (candidate.v !== 1) {
		return "version";
	}
	if (typeof candidate.id !== "number" || !Number.isInteger(candidate.id)) {
		return "shape";
	}
	if (typeof candidate.ok !== "boolean") {
		return "shape";
	}
	if (candidate.ok) {
		return candidate.value === undefined ? "shape" : null;
	}
	return isWireError(candidate.error) ? null : "shape";
}

/** Narrow `unknown` to a validated {@link EngineReply}. */
export function isEngineReply(message: unknown): message is EngineReply {
	return replyRejection(message) === null;
}

function isWireError(value: unknown): boolean {
	if (typeof value !== "object" || value === null) {
		return false;
	}
	const error = value as Record<string, unknown>;
	return (
		typeof error.code === "number" &&
		typeof error.message === "string" &&
		typeof error.docState === "string" &&
		typeof error.retryable === "boolean"
	);
}

/** Counters the link keeps, so a dropped message is visible rather than silent. */
export interface LinkDiagnostics {
	/** Messages that arrived and were refused by {@link replyRejection}. */
	readonly rejected: number;
	/** Messages delivered to subscribers. */
	readonly delivered: number;
}

/** A link over a real `chrome.runtime` port. */
export type PortLink = EngineLink & { readonly diagnostics: LinkDiagnostics };

/** Wrap a {@link HostPort} as an {@link EngineLink}. */
export function createPortLink(port: HostPort): PortLink {
	const listeners = new Set<(reply: EngineReply) => void>();
	let closed = false;
	let rejected = 0;
	let delivered = 0;

	const offMessage = port.onMessage((message) => {
		if (replyRejection(message) !== null) {
			rejected += 1;
			return;
		}
		delivered += 1;
		for (const listener of listeners) {
			listener(message as EngineReply);
		}
	});

	// A dead far end is not an error the caller can retry, so it surfaces as a
	// normal close: the page's open document has already been released by the
	// time this fires, and rethrowing would produce an unhandled rejection in a
	// module the page has finished with.
	port.onDisconnect(() => {
		closed = true;
		listeners.clear();
	});

	return {
		post(message) {
			if (closed) {
				return;
			}
			port.postMessage(message);
		},
		subscribe(listener) {
			if (closed) {
				return () => {};
			}
			listeners.add(listener);
			return () => {
				listeners.delete(listener);
			};
		},
		close() {
			if (closed) {
				return;
			}
			closed = true;
			listeners.clear();
			offMessage();
			port.disconnect();
		},
		diagnostics: {
			get rejected() {
				return rejected;
			},
			get delivered() {
				return delivered;
			},
		},
	};
}
