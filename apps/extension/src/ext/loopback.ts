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

import { type EngineMessage, type PortLink, replyRejection } from "./engine-link.js";
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

type Handler = (message: EngineReply) => void;

/**
 * Two links wired to each other, with the JSON hop the real port performs.
 */
export function createLoopbackLink(): LoopbackLink {
	let clientHandler: Handler | null = null;
	let hostHandler: Handler | null = null;

	/** Deliver `message` to whoever is listening on the far side, asynchronously. */
	const cross = (read: () => Handler | null) => {
		return (message: EngineMessage): void => {
			const target = read();
			if (target === null) {
				return;
			}
			queueMicrotask(() => {
				// The JSON hop is the point, not a detail: see the module doc.
				target(JSON.parse(JSON.stringify(message)) as EngineReply);
			});
		};
	};

	const make = (
		deliver: (message: EngineMessage) => void,
		attach: (handler: Handler) => () => void,
		state: { rejected: number; delivered: number },
	): PortLink => {
		const listeners = new Set<Handler>();
		let off: (() => void) | null = null;
		let closed = false;

		const link: PortLink = {
			post: deliver,
			subscribe(listener) {
				if (closed) {
					return () => {};
				}
				listeners.add(listener);
				if (off === null) {
					off = attach((message) => {
						if (replyRejection(message) !== null) {
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
			close() {
				if (closed) {
					return;
				}
				closed = true;
				listeners.clear();
				off?.();
				off = null;
			},
			get diagnostics() {
				return state;
			},
		};
		return link;
	};

	const clientState = { rejected: 0, delivered: 0 };
	const hostState = { rejected: 0, delivered: 0 };

	return {
		client: make(
			cross(() => hostHandler),
			(handler) => {
				hostHandler = handler;
				return () => {
					hostHandler = null;
				};
			},
			clientState,
		),
		host: make(
			cross(() => clientHandler),
			(handler) => {
				clientHandler = handler;
				return () => {
					clientHandler = null;
				};
			},
			hostState,
		),
		clientRejections: () => clientState.rejected,
	};
}
