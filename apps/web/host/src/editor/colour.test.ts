import { describe, expect, it } from "vitest";

import {
	CLASSIC_COLOURS,
	fromHex,
	fromHue,
	fromSquare,
	toHex,
	toHsv,
} from "./colour.js";

describe("the colour picker", () => {
	it("speaks the page's own numbers", () => {
		expect(toHex({ red: 1, green: 0, blue: 0 })).toBe("#ff0000");
		expect(toHex({ red: 0, green: 0.5, blue: 0 })).toBe("#008000");
		expect(toHex({ red: 0.05, green: 0.05, blue: 0.12 })).toBe("#0d0d1f");
	});

	it("reads a hex colour back", () => {
		expect(fromHex("#ff0000")).toEqual({ red: 1, green: 0, blue: 0 });
		expect(fromHex("ff0000")).toEqual({ red: 1, green: 0, blue: 0 });
		expect(fromHex("#f00")).toEqual({ red: 1, green: 0, blue: 0 });
		expect(fromHex("nonsense")).toBeNull();
	});

	it("picks the colour at a point of the square", () => {
		// The right of the square is the full saturation and the top is
		// the full brightness: the pure hue.
		expect(fromSquare(0, 1, 0)).toEqual({ red: 1, green: 0, blue: 0 });
		// The left of the square has no saturation at all: a grey.
		expect(fromSquare(0, 0, 0)).toEqual({ red: 1, green: 1, blue: 1 });
		// The bottom right is black.
		expect(fromSquare(0, 1, 1)).toEqual({ red: 0, green: 0, blue: 0 });
	});

	it("picks the colour on the hue strip", () => {
		expect(fromHue(0)).toEqual({ red: 1, green: 0, blue: 0 });
		expect(fromHue(120)).toEqual({ red: 0, green: 1, blue: 0 });
		expect(fromHue(240)).toEqual({ red: 0, green: 0, blue: 1 });
	});

	it("puts the mark back where the colour is", () => {
		const hsv = toHsv({ red: 0, green: 0.5, blue: 0 });
		expect(Math.round(hsv.hue)).toBe(120);
		expect(Math.round(hsv.saturation * 100)).toBe(100);
		expect(Math.round(hsv.brightness * 100)).toBe(50);
	});

	it("round-trips a colour through the square", () => {
		const colour = fromSquare(210, 0.6, 0.3);
		const hsv = toHsv(colour);
		expect(Math.round(hsv.hue)).toBe(210);
		expect(Math.round(hsv.saturation * 100)).toBe(60);
		expect(Math.round(hsv.brightness * 100)).toBe(70);
	});

	it("offers the classic four", () => {
		expect(CLASSIC_COLOURS.map((entry) => entry.name)).toEqual([
			"Black",
			"Blue",
			"Red",
			"Green",
		]);
	});
});
