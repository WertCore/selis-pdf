/**
 * The measurement's own arithmetic.
 *
 * The page's space is points, y up; the canvas is y down. The angle
 * therefore flips on the way in, and it is never negative: a line that goes
 * down and to the left is at 200 degrees, not at -160.
 */

import { describe, expect, it } from "vitest";
import { measureBetween, measureOnCanvas, measurementText } from "./measure.js";

describe("the distance between two points", () => {
	it("is the hypotenuse", () => {
		const measurement = measureBetween({ x: 0, y: 0 }, { x: 3, y: 4 });
		expect(measurement.points).toBe(5);
	});

	it("is the same whichever way round the points are", () => {
		const there = measureBetween({ x: 10, y: 20 }, { x: 40, y: 60 });
		const back = measureBetween({ x: 40, y: 60 }, { x: 10, y: 20 });
		expect(there.points).toBe(back.points);
	});

	it("is in millimetres as well", () => {
		// A point is 25.4/72 mm, so 72 points is 25.4 mm.
		const measurement = measureBetween({ x: 0, y: 0 }, { x: 72, y: 0 });
		expect(measurement.millimetres).toBeCloseTo(25.4, 6);
	});
});

describe("the angle of a measurement", () => {
	it("is measured from the page's horizontal", () => {
		expect(measureBetween({ x: 0, y: 0 }, { x: 10, y: 0 }).degrees).toBe(0);
		expect(measureBetween({ x: 0, y: 0 }, { x: 0, y: 10 }).degrees).toBe(90);
	});

	it("is never negative", () => {
		const measurement = measureBetween({ x: 0, y: 0 }, { x: -10, y: -10 });
		expect(measurement.degrees).toBe(225);
	});

	it("is measured counterclockwise", () => {
		const measurement = measureBetween({ x: 0, y: 0 }, { x: -10, y: 10 });
		expect(measurement.degrees).toBe(135);
	});
});

describe("a measurement taken on the canvas", () => {
	it("flips the vertical, because the canvas is y down", () => {
		// A drag to the right and DOWN the canvas is a line that goes right
		// and DOWN the page too: 315 degrees, not 45.
		const measurement = measureOnCanvas({ x: 0, y: 0 }, { x: 100, y: 100 }, 10);
		expect(measurement.degrees).toBe(315);
	});

	it("measures a straight drag downwards as it is", () => {
		// Straight down the canvas is straight down the page: 270 degrees.
		const measurement = measureOnCanvas({ x: 0, y: 0 }, { x: 0, y: 100 }, 10);
		expect(measurement.degrees).toBe(270);
		expect(measurement.points).toBe(10);
	});

	it("scales the distance by the page's own scale", () => {
		const measurement = measureOnCanvas({ x: 0, y: 0 }, { x: 300, y: 400 }, 10);
		expect(measurement.points).toBe(50);
	});

	it("measures nothing when the page has no scale", () => {
		const measurement = measureOnCanvas({ x: 0, y: 0 }, { x: 10, y: 10 }, 0);
		expect(measurement.points).toBe(0);
	});
});

describe("how a measurement is shown", () => {
	it("carries both units and the angle", () => {
		const measurement = measureBetween({ x: 0, y: 0 }, { x: 72, y: 0 });
		expect(measurementText(measurement)).toBe("72.0 pt (25.4 mm) · 0.0°");
	});
});
