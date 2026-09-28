/**
 * SL-4.EXT.06 — a connected {@link EngineLink} pair, for tests only.
 *
 * **This module is not on `PACKAGE_ENTRIES` and must not be.** The gate fails on
 * any file in the package without a ship-list row, so the loopback cannot reach
 * a store upload by being next to the code that uses it. Nothing in
 * `src/ext/*.ts` imports it; only `*.test.ts` does.
 *
 * ## Why the messages go through a JSON round trip
 *
 * A loopback that passed objects by reference would hide exactly the class of
 * bug this transport exists to survive: an `ArrayBuffer` or a typed array that
 * quietly works in Node and silently base64s in Chrome, where the port is a JSON
 * channel. Every message here is serialised and parsed exactly once, so a test
 * that passes on the loopback is testing the encoding rather than a shortcut
 * around it. `engine-transport.test.ts` asserts the hop is real by planting a
 * value that does not survive it.
 */

import {
	type EngineMessage,
	type PortLink,
	isEngineReply,
	isEngineRequest,
} from "./engine-link.js";
import type { EngineReply } from "./engine-protocol.js";

/** A connected link pair, one per end. */
export interface LoopbackLink {
	/** The side the viewer page's engine port speaks. */
	readonly client: PortLink;
	/** The side the offscreen host's engine answers on. */
	readonly host: PortLink;
	/** Replies the client received that {@link replyRejection} refused. */
	readonly clientRejections: () => number;
}

/** Which direction a link receives, and therefore which validator it applies. */
type Validator = (message: unknown) => boolean;

type Handler = (message: EngineReply) => void;

/**
 * Two links wired to each other, with the JSON hop the real port performs.
 */
/** One end of the loopback: its liveness and the cleanup waiting on it. */
interface Side {
	open: boolean;
	readonly disconnects: Set<() => void>;
}

export function createLoopbackLink(): LoopbackLink {
	/**
	 * Two independent channels, one per direction.
	 *
	 * `toHost` is written by the **host** link's `subscribe` and read by the
	 * **client** link's `post`; `toClient` is the mirror image. Naming them is
	 * the whole point: an earlier version of this file wired `post` and
	 * `subscribe` to the same slot on each side, so every message was delivered
	 * back to its own sender, every request was silently dropped, and the
	 * transport test hung rather than failing. A loopback that cannot be wrong
	 * about direction is not a loopback, it is a mirror.
	 */
	let toHost: Handler | null = null;
	let toClient: Handler | null = null;

	/**
	 * Liveness and pending cleanup, per side.
	 *
	 * Not decoration. A real `chrome.runtime` port fires `onDisconnect` on the
	 * far end when one side closes, and the offscreen host's whole teardown
	 * story - "close every document still open, because the page that owned
	 * them is gone" - hangs off it. A loopback that only delivered messages
	 * would let that code ship untested.
	 */
	const client: Side = { open: true, disconnects: new Set() };
	const host: Side = { open: true, disconnects: new Set() };

	/** The far end has gone: mark it dead and run whatever it was waiting on. */
	const drop = (side: Side): void => {
		if (!side.open) {
			return;
		}
		side.open = false;
		for (const handler of [...side.disconnects]) {
			handler();
		}
		side.disconnects.clear();
	};

	/** Post `message` to whoever is listening in the other direction, asynchronously. */
	const cross = (read: () => Handler | null, far: Side) => {
		return (message: EngineMessage): void => {
			if (!far.open) {
				// A real port throws here. Dropping is the kinder behaviour for a
				// fixture, and the diagnostics counter records that it happened.
				return;
			}
			const target = read();
			if (target === null) {
				throw new Error(
					"loopback: nothing is listening in that direction - the far side has not subscribed yet",
				);
			}
			queueMicrotask(() => {
				// The JSON hop is the point, not a detail: see the module doc.
				target(JSON.parse(JSON.stringify(message)) as EngineReply);
			});
		};
	};

	const make = (
		self: Side,
		deliver: (message: EngineMessage) => void,
		attach: (handler: Handler) => () => void,
		state: { rejected: number; delivered: number },
		validate: Validator,
		onFarClose: () => void,
	): PortLink => {
		const listeners = new Set<Handler>();
		let off: (() => void) | null = null;

		return {
			post: deliver,
			subscribe(listener) {
				if (!self.open) {
					return () => {};
				}
				listeners.add(listener);
				if (off === null) {
					off = attach((message: EngineReply) => {
						if (!self.open || !validate(message)) {
							state.rejected += 1;
							return;
						}
						state.delivered += 1;
						for (const each of listeners) {
							each(message);
						}
					});
				}
				return () => {
					listeners.delete(listener);
					if (listeners.size === 0 && off !== null) {
						off();
						off = null;
					}
				};
			},
			onDisconnect(handler) {
				if (!self.open) {
					handler();
					return () => {};
				}
				self.disconnects.add(handler);
				return () => {
					self.disconnects.delete(handler);
				};
			},
			close() {
				if (!self.open) {
					return;
				}
				// Tear this side down first, then tell the far end. The order
				// matters for the host: its own disconnect handler must be able
				// to unsubscribe from a link that is already quiet.
				listeners.clear();
				off?.();
				off = null;
				drop(self);
				onFarClose();
			},
			get diagnostics() {
				return state;
			},
		};
	};

	const clientState = { rejected: 0, delivered: 0 };
	const hostState = { rejected: 0, delivered: 0 };

	return {
		// The client posts requests, which arrive at the host's subscribers.
		client: make(
			client,
			cross(() => toHost, host),
			(handler) => {
				toClient = handler;
				return () => {
					toClient = null;
				};
			},
			clientState,
			// The viewer side receives replies.
			isEngineReply,
			() => drop(host),
		),
		// The host posts replies, which arrive at the client's subscribers.
		host: make(
			host,
			cross(() => toClient, client),
			(handler) => {
				toHost = handler;
				return () => {
					toHost = null;
				};
			},
			hostState,
			// The host side receives requests. Using the wrong validator here is
			// not a typo a test catches - it silently drops every message and
			// hangs the viewer, which is exactly how this was found.
			isEngineRequest,
			() => drop(client),
		),
		clientRejections: () => clientState.rejected,
	};
}
