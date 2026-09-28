/**
 * SL-4.EXT.03 - the guest ABI's pointer discipline.
 *
 * There is no `.wasm` in this repository, so these tests drive
 * `createWasmGuest` against a hand-written set of exports backed by a real
 * `WebAssembly.Memory`. That is the right level for this module: what it does
 * is arithmetic - allocate, copy in, read three words, read two buffers, free
 * five buffers - and arithmetic over a fake heap is exactly what a real guest
 * would be checked against by `cargo xtask wasm-protocol`.
 *
 * The assertions are the ones a memory-safety bug would violate: every
 * allocation is freed exactly once, a length the guest reports is validated
 * against its memory before anything is read, and a response that is not a
 * protocol message is refused rather than interpreted.
 */

import { describe, expect, it } from "vitest";
import { type GuestExports, GuestAbiError, createWasmGuest } from "./wasm-guest.js";

/** A bump allocator over real linear memory, recording every free. */
function createFakeGuest(answer: (request: unknown, payload: Uint8Array) => {
	response: string;
	attachment?: Uint8Array;
	/** Deliberately lie about the out-words, to prove they are checked. */
	report?: { responseLen?: number; payloadPtr?: number; payloadLen?: number };
	nullDispatch?: boolean;
}) {
	const memory = new WebAssembly.Memory({ initial: 1 });
	let next = 16;
	const live = new Map<number, number>();
	const freed: number[] = [];
	const view = (): Uint8Array => new Uint8Array(memory.buffer);
	const alloc = (len: number): number => {
		const ptr = next;
		next += Math.max(len, 1) + 8;
		live.set(ptr, len);
		return ptr;
	};
	const exports: GuestExports = {
		memory,
		selis_input_alloc: alloc,
		selis_free: (ptr) => {
			freed.push(ptr);
			live.delete(ptr);
		},
		selis_dispatch: (reqPtr, reqLen, payloadPtr, payloadLen, outPtr) => {
			const bytes = view();
			const request = JSON.parse(new TextDecoder().decode(bytes.subarray(reqPtr, reqPtr + reqLen)));
			const reply = answer(request, bytes.subarray(payloadPtr, payloadPtr + payloadLen));

			// A null dispatch is the guest's "I could not allocate" answer: it
			// has produced no response buffer, so there is none to free.
			const responsePtr = reply.nullDispatch === true ? 0 : alloc(reply.response.length);
			bytes.set(new TextEncoder().encode(reply.response), responsePtr);
			let attachmentPtr = 0;
			let attachmentLen = 0;
			if (reply.attachment !== undefined) {
				attachmentPtr = alloc(reply.attachment.byteLength);
				attachmentLen = reply.attachment.byteLength;
				bytes.set(reply.attachment, attachmentPtr);
			}
			const report = reply.report ?? {};
			const words = new DataView(memory.buffer);
			words.setUint32(outPtr, report.responseLen ?? reply.response.length, true);
			words.setUint32(outPtr + 4, report.payloadPtr ?? attachmentPtr, true);
			words.setUint32(outPtr + 8, report.payloadLen ?? attachmentLen, true);
			return reply.nullDispatch === true ? 0 : responsePtr;
		},
	};
	return { guest: createWasmGuest(exports), freed, live, exports };
}

describe("the guest ABI (SL-4.EXT.03)", () => {
	it("returns the parsed response and its attachment", () => {
		const attachment = new Uint8Array([1, 2, 3, 4]);
		const { guest } = createFakeGuest(() => ({
			response: JSON.stringify({ v: 1, id: 1, ok: true, value: { doc: 3 } }),
			attachment,
		}));
		const reply = guest.send({ v: 1, id: 1, op: "open" });
		expect(reply.response.value).toEqual({ doc: 3 });
		expect(Array.from(reply.attachment)).toEqual([1, 2, 3, 4]);
	});

	it("frees every allocation exactly once, on success and on failure", () => {
		const ok = createFakeGuest(() => ({
			response: JSON.stringify({ v: 1, id: 1, ok: true, value: {} }),
		}));
		ok.guest.send({ v: 1, id: 1, op: "page" });
		// Request, out-words, response: three allocations, three frees, and
		// nothing left behind. A leak here is invisible until a long-lived
		// offscreen document hits a budget it should have passed.
		expect(ok.freed).toHaveLength(3);
		expect(ok.live.size).toBe(0);

		const bad = createFakeGuest(() => ({
			response: JSON.stringify({ v: 1, id: 1, ok: true, value: {} }),
			nullDispatch: true,
		}));
		expect(() => bad.guest.send({ v: 1, id: 1, op: "page" })).toThrow(GuestAbiError);
		expect(bad.live.size).toBe(0);
	});

	it("refuses a length the guest reported outside its own memory", () => {
		const { guest } = createFakeGuest(() => ({
			response: JSON.stringify({ v: 1, id: 1, ok: true, value: {} }),
			// A length that runs off the end of a 64 KiB page.
			report: { responseLen: 1_000_000 },
		}));
		expect(() => guest.send({ v: 1, id: 1, op: "page" })).toThrow(/outside its/);
	});

	it("refuses a response that is not a protocol message", () => {
		for (const body of ["not json", "[]", '{"v":1}', '{"v":1,"id":1}']) {
			const { guest } = createFakeGuest(() => ({ response: body }));
			expect(() => guest.send({ v: 1, id: 1, op: "page" }), body).toThrow(GuestAbiError);
		}
	});

	it("refuses an attachment above the protocol's 64 MiB bound without writing it", () => {
		// A claim the guest would refuse has to be refused here too: the
		// alternative is writing past the end of a buffer the guest sized for
		// something else.
		const { guest, live } = createFakeGuest(() => ({
			response: JSON.stringify({ v: 1, id: 1, ok: true, value: {} }),
		}));
		const tooBig = new Uint8Array(64 * 1024 * 1024 + 1);
		expect(() => guest.send({ v: 1, id: 1, op: "open" }, tooBig)).toThrow(/64 MiB/);
		expect(live.size).toBe(0);
	});
});