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

import { ENGINE_PROTOCOL, type EngineReply, type EngineRequest } from "./engine-protocol.js";
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
	/**
	 * Observe this end's incoming messages. Returns an unsubscribe.
	 *
	 * What arrives is named by `createPortLink`'s direction: replies for the
	 * viewer page, requests for the offscreen host.
	 */
	subscribe(listener: (reply: EngineReply) => void): () => void;
	/**
	 * The far end going away: the page closed, or the service worker was killed.
	 *
	 * Not decoration. The offscreen host keeps every open document in a table
	 * precisely so it can release them here, when the page that owned them
	 * no longer exists to ask. Returns an unsubscribe.
	 */
	onDisconnect(handler: () => void): () => void;
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

/** Request operations the host acts on. Anything else is refused by name. */
const KNOWN_OPS: ReadonlySet<string> = new Set([
	"open",
	"close",
	"render",
	"text",
	"textLayer",
	"search",
	"outline",
	"pageLabels",
	"destinations",
	"pageLinks",
	"cancel",
]);

/** Narrow `unknown` to an {@link EngineRequest} this host will act on. */
export function isEngineRequest(message: unknown): message is EngineRequest {
	if (typeof message !== "object" || message === null) {
		return false;
	}
	const candidate = message as Partial<EngineRequest>;
	if (candidate.v !== ENGINE_PROTOCOL) {
		return false;
	}
	if (typeof candidate.op !== "string" || !KNOWN_OPS.has(candidate.op)) {
		return false;
	}
	return typeof candidate.id === "number" && Number.isInteger(candidate.id);
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
	/** Messages that arrived and were refused by this end's validator. */
	readonly rejected: number;
	/** Messages delivered to subscribers. */
	readonly delivered: number;
}

/** A link over a real `chrome.runtime` port. */
export type PortLink = EngineLink & { readonly diagnostics: LinkDiagnostics };

/**
 * The slice of a real `chrome.runtime.Port` this package reads.
 *
 * Chrome's port exposes `onMessage` and `onDisconnect` as **events**
 * (`addListener` / `removeListener`), not as methods that take a listener and
 * return an unsubscribe. {@link HostPort} is deliberately the method shape,
 * because a loopback can implement that in a few lines and every Node test
 * drives it; the browser end has to be adapted at the boundary.
 */
export interface BrowserPort {
	postMessage(message: unknown): void;
	disconnect(): void;
	onMessage: {
		addListener(listener: (message: unknown) => void): void;
		removeListener(listener: (message: unknown) => void): void;
	};
	onDisconnect: {
		addListener(listener: () => void): void;
		removeListener(listener: () => void): void;
	};
}

/**
 * Wrap a real port as a {@link HostPort}.
 *
 * This is the only place the two shapes meet. Both ends of the engine link go
 * through it - the viewer page's `connect()` and the offscreen document's
 * `onConnect` - so there is one definition of "what a port is" for the real
 * browser, and `engine-link.ts`'s transport stays testable without `chrome`.
 *
 * Reading `port.onMessage(...)` as a function is exactly what the SL-4.UI.06
 * browser check caught: every mock had the method shape and the real port had
 * the event one.
 */
export function hostPortOf(port: BrowserPort): HostPort {
	return {
		postMessage(message) {
			port.postMessage(message);
		},
		disconnect() {
			port.disconnect();
		},
		onMessage(listener) {
			const handler = (message: unknown): void => {
				listener(message);
			};
			port.onMessage.addListener(handler);
			return () => {
				port.onMessage.removeListener(handler);
			};
		},
		onDisconnect(listener) {
			const handler = (): void => {
				listener();
			};
			port.onDisconnect.addListener(handler);
			return () => {
				port.onDisconnect.removeListener(handler);
			};
		},
	};
}

/**
 * Wrap a {@link HostPort} as an {@link EngineLink}.
 *
 * `receives` names which half this is, and it is not cosmetic. A
 * `chrome.runtime` port is bidirectional, so one wrapper serves both ends - but
 * each end must validate what *it* receives: the viewer page receives replies,
 * and the offscreen host receives requests. Feeding the host half through
 * {@link replyRejection} drops every request it is sent, and the page waits
 * forever for an answer that was never asked for; the SL-4.UI.06 browser check
 * found exactly that, because the loopback pair had always supplied the right
 * validator per side.
 */
export function createPortLink(
	port: HostPort,
	receives: "replies" | "requests" = "replies",
): PortLink {
	const listeners = new Set<(reply: EngineReply) => void>();
	let closed = false;
	let rejected = 0;
	let delivered = 0;
	const accepts =
		receives === "replies"
			? (message: unknown): boolean => replyRejection(message) === null
			: isEngineRequest;

	const offMessage = port.onMessage((message) => {
		if (!accepts(message)) {
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
	// module the page has finished with. Handlers registered through
	// `onDisconnect` are the ones that *do* have cleanup to do, so they run.
	const disconnects = new Set<() => void>();
	port.onDisconnect(() => {
		if (closed) {
			return;
		}
		closed = true;
		listeners.clear();
		for (const handler of disconnects) {
			handler();
		}
		disconnects.clear();
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
		onDisconnect(handler) {
			if (closed) {
				handler();
				return () => {};
			}
			disconnects.add(handler);
			return () => {
				disconnects.delete(handler);
			};
		},
		close() {
			if (closed) {
				return;
			}
			closed = true;
			listeners.clear();
			disconnects.clear();
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
