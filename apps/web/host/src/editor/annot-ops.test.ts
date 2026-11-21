/**
 * The pure request builders for the two deferred annotation ops
 * (SL-5.ANNOT.02/03). No wasm and no DOM: the wire shape is pinned here so a
 * builder that shifted the page or fed a 0..255 colour to the engine's 0..1
 * colour contract would fail before it reached the guest.
 */

import { describe, expect, it } from "vitest";
import { ANNOT_OPS, inkStrokesOp, regenerateAppearancesOp } from "./operations.js";

describe("annotation op builders", () => {
	it("routes both deferred ops away from the tools chunk", () => {
		expect([...ANNOT_OPS].sort()).toEqual(["annot_ink", "annotate_appearances"]);
		const rebuild = regenerateAppearancesOp();
		expect(rebuild.op).toBe("annotate_appearances");
		expect(rebuild.options).toEqual({});
	});

	it("builds one /InkList request from every caller-supplied stroke", () => {
		const strokes = [
			[
				[10, 20],
				[30, 40],
			],
			[
				[50, 60],
				[70, 80],
			],
		] as const;
		const operation = inkStrokesOp(2, strokes, 3, [0.1, 0.2, 0.3], "two strokes", "Ada");
		expect(operation.op).toBe("annot_ink");
		expect(operation.options).toEqual({
			page: 2,
			strokes: [
				[
					[10, 20],
					[30, 40],
				],
				[
					[50, 60],
					[70, 80],
				],
			],
			width: 3,
			colour: [0.1, 0.2, 0.3],
			contents: "two strokes",
			author: "Ada",
		});
		expect(operation.label).toContain("2 strokes");
	});

	it("refuses an empty mark, a short stroke, a non-finite point and an out-of-gamut colour", () => {
		expect(() => inkStrokesOp(0, [], 2, [0, 0, 0])).toThrow(RangeError);
		expect(() => inkStrokesOp(0, [[[1, 2]]], 2, [0, 0, 0])).toThrow(RangeError);
		expect(() => inkStrokesOp(0, [[[1, Number.NaN], [2, 3]]], 2, [0, 0, 0])).toThrow(
			RangeError,
		);
		expect(() => inkStrokesOp(0, [[[1, 2], [3, 4]]], 2, [0, 0, 2])).toThrow(RangeError);
		expect(() => inkStrokesOp(0, [[[1, 2], [3, 4]]], -1, [0, 0, 0])).toThrow(RangeError);
		expect(() => inkStrokesOp(-1, [[[1, 2], [3, 4]]], 2, [0, 0, 0])).toThrow(RangeError);
	});
});
