/**
 * The request builders for first-class add-content / object-edit ops
 * (TEXTEDIT.05/06/07/08).
 *
 * These are pure functions, so the exact wire shape is pinned without a wasm
 * binary or a DOM: the engine expects a zero-based page, PDF user-space rects,
 * 0..255 colours, and a stable object id. A builder that shifted the page or
 * flipped a coordinate would place content somewhere the reader did not point.
 */

import { describe, expect, it } from "vitest";
import {
	addBackgroundOp,
	addBatesOp,
	addHeaderFooterOp,
	addImageOp,
	addLinkOp,
	addShapeOp,
	addTextBoxOp,
	addWatermarkOp,
	deleteObjectOp,
	engineColor,
	extractImageOp,
	listObjectsOp,
	listVectorsOp,
	moveObjectOp,
	replaceImageOp,
	resizeObjectOp,
	userRectArray,
} from "./operations.js";

const RECT = { x: 10, y: 20, width: 100, height: 40 };

describe("engine colour and rectangle encoding", () => {
	it("scales 0..1 floats to the engine's 0..255 components", () => {
		expect(engineColor([0, 0.5, 1])).toEqual([0, 128, 255]);
	});

	it("refuses an out-of-gamut component", () => {
		expect(() => engineColor([1.2, 0, 0])).toThrow(RangeError);
	});

	it("expands a user rectangle to four corners", () => {
		expect(userRectArray(RECT)).toEqual([10, 20, 110, 60]);
	});
});

describe("add-content op builders", () => {
	it("builds a first-class text-box request, not a stamp", () => {
		const operation = addTextBoxOp(1, RECT, "Hello", 14, [0, 0, 0]);
		expect(operation.op).toBe("add_text_box");
		expect(operation.options).toEqual({
			page: 1,
			rect: [10, 20, 110, 60],
			text: "Hello",
			font: "Helvetica",
			font_size: 14,
			color: [0, 0, 0],
		});
	});

	it("builds an image request whose bytes ride as a second input", () => {
		const operation = addImageOp(0, RECT, 30);
		expect(operation.op).toBe("add_image");
		expect(operation.options).toEqual({ page: 0, rect: [10, 20, 110, 60], angle_degrees: 30 });
	});

	it("builds shape, link and background requests", () => {
		expect(addShapeOp(0, RECT, "ellipse", null, [0, 0, 1]).op).toBe("add_shape");
		expect(addBackgroundOp(0, RECT, [1, 1, 1]).op).toBe("add_background");
		const link = addLinkOp(0, RECT, { uri: "https://example.com" });
		expect(link.op).toBe("add_link");
		expect(link.options.uri).toBe("https://example.com");
		const internal = addLinkOp(0, RECT, { page: 3 });
		expect(internal.options.target_page).toBe(3);
	});

	it("builds the page-furniture ops", () => {
		expect(addHeaderFooterOp(0, "Title", true, "center", 10).op).toBe("add_header_footer");
		expect(addWatermarkOp(0, "DRAFT", 48, 0.2).op).toBe("add_watermark");
		expect(addBatesOp("CASE-000001", 7).options.start).toBe(7);
		expect(() => addBatesOp("NO-DIGITS", 1)).toThrow(RangeError);
	});

	it("uses a zero-based page everywhere", () => {
		expect(listObjectsOp(4).options.page).toBe(4);
		expect(listVectorsOp(4).options.page).toBe(4);
		expect(() => listObjectsOp(-1)).toThrow(RangeError);
	});
});

describe("object-edit op builders", () => {
	const id = { name: "Im1", op_index: 3 };

	it("moves, resizes, deletes, replaces and extracts by the reported id", () => {
		expect(moveObjectOp(2, id, 5, 7).options).toMatchObject({ page: 2, id, dx: 5, dy: 7 });
		expect(resizeObjectOp(2, id, RECT).options.rect).toEqual([10, 20, 110, 60]);
		expect(deleteObjectOp(2, id).op).toBe("delete_object");
		expect(replaceImageOp(2, id).op).toBe("replace_image");
		expect(extractImageOp(2, id).op).toBe("extract_image");
	});

	it("refuses a malformed id or a non-finite move", () => {
		expect(() => moveObjectOp(0, { name: "", op_index: -1 }, 1, 1)).toThrow(RangeError);
		expect(() => moveObjectOp(0, id, Number.NaN, 1)).toThrow(RangeError);
	});
});
