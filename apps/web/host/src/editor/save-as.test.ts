/**
 * Writing an exported file where the reader chose (pdf-plan/43).
 *
 * The bug these tests exist for is quiet: a file's LENGTH and a file's BYTES
 * are both numbers to the careless eye, and `new Uint8Array(length)` builds an
 * array of that many zeros. The file is written, the right size, and empty —
 * and nothing says so.
 *
 * So the write is checked against the bytes that were handed in, and the
 * exported file's two number-ish fields are checked for being two different
 * things.
 */
import { describe, expect, it } from "vitest";
import { type FileHandleLike, writeToHandle } from "./save-as.js";

/** A handle that records what was written into it. */
function recordingHandle(): { handle: FileHandleLike; written: Uint8Array[]; closed: number } {
	const written: Uint8Array[] = [];
	let closed = 0;
	const handle: FileHandleLike = {
		createWritable: async () => ({
			write: (data: Uint8Array) => {
				written.push(data);
			},
			close: () => {
				closed += 1;
			},
		}),
	};
	return { handle, written, closed: 0 };
}

describe("writing an exported file where the reader chose", () => {
	it("writes the bytes, and not the file's length", async () => {
		const { handle, written } = recordingHandle();
		const content = new Uint8Array([0x50, 0x4b, 0x03, 0x04, 0x00, 0x01]);
		await writeToHandle(handle, content);
		expect(written).toHaveLength(1);
		expect([...(written[0] ?? [])]).toEqual([...content]);
		expect(written[0]?.byteLength).toBe(6);
	});

	it("closes the stream even when the write fails", async () => {
		let closed = 0;
		const handle: FileHandleLike = {
			createWritable: async () => ({
				write: () => {
					throw new Error("the disk is full");
				},
				close: () => {
					closed += 1;
				},
			}),
		};
		await expect(writeToHandle(handle, new Uint8Array([1]))).rejects.toThrow("the disk is full");
		expect(closed).toBe(1);
	});
});
