import { describe, expect, it } from "vitest";
import {
	MAX_PRINT_PAGES,
	MIN_PRINT_DPI,
	type PageBox,
	measuredPrintDpi,
	parsePrintScaling,
	planPrint,
	printBoxesFromOpen,
	reconcilePageBox,
} from "./print.js";

/** US Letter, the default paper. */
const LETTER: PageBox = { widthPt: 612, heightPt: 792 };
/** A4, a different aspect ratio, which is where one-axis fitting shows up. */
const A4: PageBox = { widthPt: 595.28, heightPt: 841.89 };

describe("parsePrintScaling", () => {
	it("reads the three specified names", () => {
		expect(parsePrintScaling("None")).toBe("none");
		expect(parsePrintScaling("shrinkToFit")).toBe("shrinkToFit");
		expect(parsePrintScaling("AppDefault")).toBe("appDefault");
	});

	it("accepts the lowercase spelling real producers emit", () => {
		// A viewer honouring only the spec spelling silently mis-prints these.
		expect(parsePrintScaling("appDefault")).toBe("appDefault");
	});

	it("falls back rather than throwing on anything unknown", () => {
		// A malformed entry must not stop the page printing; the spec says an
		// unknown value is ignored, and ignored means our behaviour applies.
		expect(parsePrintScaling("nonsense")).toBe("appDefault");
		expect(parsePrintScaling(null)).toBe("appDefault");
		expect(parsePrintScaling(undefined)).toBe("appDefault");
		expect(parsePrintScaling("")).toBe("appDefault");
	});

	it("is case sensitive where the spec is, so None is not noNe", () => {
		// Guards against a future case-insensitive shortcut that would make
		// "none" mean /None, which is a different instruction.
		expect(parsePrintScaling("none")).toBe("appDefault");
		expect(parsePrintScaling("NONE")).toBe("appDefault");
	});
});

describe("planPrint", () => {
	it("prints at the page's own size under /None, ignoring the paper", () => {
		const plan = planPrint(LETTER, "none", A4);
		// A4 is taller than Letter, so a viewer that honoured it would re-paper
		// the document. /None is the author saying "this page is Letter".
		expect(plan.paper).toBeNull();
		expect(plan.fitToPage).toBe(false);
		// scale is POINTS -> DEVICE pixels, so an unfitted page at 300 DPI is
		// 300/72, not 1. The fit ratio is scale / (dpi / 72), and is 1 here.
		expect(plan.scale).toBeCloseTo(MIN_PRINT_DPI / 72, 10);
	});

	it("uses the document's own size under /AppDefault", () => {
		const plan = planPrint(LETTER, "appDefault", A4);
		expect(plan.paper).toBeNull();
		expect(plan.fitToPage).toBe(false);
	});

	it("fits to the paper under /shrinkToFit, and reports that paper", () => {
		const plan = planPrint(LETTER, "shrinkToFit", A4);
		expect(plan.paper).toEqual(A4);
		expect(plan.fitToPage).toBe(true);
		// Fitted means fewer device pixels per point than an unfitted page.
		expect(plan.scale).toBeLessThan(MIN_PRINT_DPI / 72);
	});

	it("never magnifies to fill the paper, because shrinkToFit says shrink", () => {
		// A small page on big paper: filling it would upscale 3x.
		const small: PageBox = { widthPt: 200, heightPt: 300 };
		const plan = planPrint(small, "shrinkToFit", LETTER);
		expect(plan.scale).toBeCloseTo(MIN_PRINT_DPI / 72, 10);
		expect(plan.fitToPage).toBe(false);
		// The chosen sheet is still the TARGET - the user asked for it and the
		// page fits - but nothing was scaled to reach it.
		expect(plan.paper).toEqual(LETTER);
	});

	it("fits by the SMALLER relative axis, so a mismatched aspect is not stretched", () => {
		// Letter onto A4: A4 is narrower AND taller, so width is the binding
		// axis. Fitting by height instead would overflow the width.
		const plan = planPrint(LETTER, "shrinkToFit", A4);
		const expected = Math.min(A4.widthPt / LETTER.widthPt, A4.heightPt / LETTER.heightPt);
		expect(plan.scale / (MIN_PRINT_DPI / 72)).toBeCloseTo(expected, 10);
		// And the result genuinely fits inside the paper on BOTH axes.
		expect(LETTER.widthPt * (plan.scale / (MIN_PRINT_DPI / 72))).toBeLessThanOrEqual(
			A4.widthPt + 1e-9,
		);
		expect(LETTER.heightPt * (plan.scale / (MIN_PRINT_DPI / 72))).toBeLessThanOrEqual(
			A4.heightPt + 1e-9,
		);
	});

	it("rasterises at print resolution, not at screen resolution", () => {
		const plan = planPrint(LETTER, "none", null);
		expect(plan.dpi).toBe(MIN_PRINT_DPI);
		// 300 DPI is 300/72 = 4.1667 device pixels per point.
		expect(plan.scale).toBeCloseTo(300 / 72, 10);
	});

	it("keeps print resolution when fitting shrinks the page", () => {
		// Fitting must reduce the page's size on paper, NOT the number of samples
		// used to rasterise it. Those are independent, and conflating them is
		// how a fitted page ends up soft.
		const big: PageBox = { widthPt: 2000, heightPt: 3000 };
		const plan = planPrint(big, "shrinkToFit", LETTER);
		expect(plan.dpi).toBe(MIN_PRINT_DPI);
		expect(plan.scale).toBeLessThan(MIN_PRINT_DPI / 72);
	});

	it("refuses a degenerate box instead of emitting an absurd scale", () => {
		// A zero box would make fit infinite. Guarding here is what stops a
		// nonsense plan looking like a plausible number downstream.
		expect(() => planPrint({ widthPt: 0, heightPt: 792 }, "none", null)).toThrow(RangeError);
		expect(() => planPrint({ widthPt: 612, heightPt: 0 }, "none", null)).toThrow(RangeError);
		expect(() => planPrint({ widthPt: -1, heightPt: 792 }, "none", null)).toThrow(RangeError);
		expect(() => planPrint({ widthPt: NaN, heightPt: 792 }, "none", null)).toThrow(RangeError);
	});

	it("treats a null paper as the page's own size under /shrinkToFit", () => {
		// No paper chosen is not a reason to guess one.
		const plan = planPrint(LETTER, "shrinkToFit", null);
		expect(plan.paper).toBeNull();
		expect(plan.fitToPage).toBe(false);
		expect(plan.scale).toBeCloseTo(300 / 72, 10);
	});

	it("always explains itself, because a silent plan cannot be challenged", () => {
		for (const [scaling, paper] of [
			["none", null],
			["appDefault", A4],
			["shrinkToFit", A4],
			["shrinkToFit", null],
		] as const) {
			expect(planPrint(LETTER, scaling, paper).reason).toMatch(/PrintScaling/);
		}
	});
});

describe("printBoxesFromOpen", () => {
	/** An `open` reply of the shape the engine actually sends. */
	function opened(pages: number, sizes: readonly { width: number; height: number }[]) {
		return { doc: 7, pages, pageSizes: sizes };
	}

	it("reads the count and every box from one reply, with no probing", () => {
		// The walk this replaced asked the engine for page N+1 to find the end.
		// The count is a FIELD here, so the whole document costs one round trip.
		const { count, boxes } = printBoxesFromOpen(
			opened(3, [
				{ width: 612, height: 792 },
				{ width: 595.28, height: 841.89 },
				{ width: 612, height: 792 },
			]),
		);
		expect(count).toBe(3);
		expect(boxes).toHaveLength(3);
	});

	it("keeps each page's OWN box, in order", () => {
		// Index alignment is the whole contract: box i is page i. A sort or a
		// de-duplication here would silently re-paper a mixed-size document.
		const { boxes } = printBoxesFromOpen(
			opened(2, [
				{ width: 612, height: 792 },
				{ width: 841.89, height: 595.28 },
			]),
		);
		expect(boxes[0]).toEqual({ widthPt: 612, heightPt: 792 });
		expect(boxes[1]).toEqual({ widthPt: 841.89, heightPt: 595.28 });
	});

	it("refuses when the count and the boxes disagree, rather than truncating", () => {
		// The failure this prevents: a `pageSizes` array shorter than `pages`
		// sliced to its own length prints page 3 with page 1's MediaBox.
		expect(() => printBoxesFromOpen(opened(3, [{ width: 612, height: 792 }]))).toThrow(RangeError);
		expect(() => printBoxesFromOpen(opened(1, []))).toThrow(RangeError);
		expect(() => printBoxesFromOpen({ doc: 1, pages: 2 })).toThrow(RangeError);
	});

	it("refuses a reply with no usable count", () => {
		// `pages` absent, fractional, negative or a string all mean the same
		// thing here: the engine did not say how many pages there are.
		for (const pages of [undefined, null, 1.5, -1, "3", Number.NaN]) {
			expect(() => printBoxesFromOpen({ pages, pageSizes: [] })).toThrow(RangeError);
		}
		expect(() => printBoxesFromOpen(null)).toThrow(RangeError);
		expect(() => printBoxesFromOpen("nope")).toThrow(RangeError);
	});

	it("refuses a page whose box is unusable, naming the page", () => {
		// A degenerate box is refused AT THE BOUNDARY. Sliced away, it would put
		// a zero-width MediaBox on some page of the output.
		expect(() =>
			printBoxesFromOpen(
				opened(2, [
					{ width: 612, height: 792 },
					{ width: 0, height: 0 },
				]),
			),
		).toThrow(/page 1/);
		expect(() =>
			printBoxesFromOpen(opened(1, [{ width: 612, height: Number.NaN }])),
		).toThrow(RangeError);
	});

	it("accepts a genuinely empty document as zero pages", () => {
		// Not an error: a zero-page PDF is legal input, and the caller decides
		// there is nothing to print. Throwing here would misreport the document.
		expect(printBoxesFromOpen(opened(0, []))).toEqual({ count: 0, boxes: [] });
	});
});

describe("reconcilePageBox", () => {
	it("leaves an upright page alone and measures its scale", () => {
		const got = reconcilePageBox(LETTER, 2550, 3300);
		expect(got.rotated).toBe(false);
		expect(got.box).toEqual(LETTER);
		expect(got.scale).toBeCloseTo(300 / 72, 10);
	});

	it("transposes a page the engine rendered turned a quarter turn", () => {
		// THE bug this exists for. `open` reports 612x792 because that is the
		// un-rotated MediaBox; `render` returns 792x612 because /Rotate 90 is
		// applied. Printing the declared box would scale a landscape scan onto
		// portrait paper and squash it.
		const got = reconcilePageBox(LETTER, 3300, 2550);
		expect(got.rotated).toBe(true);
		expect(got.box).toEqual({ widthPt: 792, heightPt: 612 });
		// And the scale is measured from the RASTER, so it is still 300 DPI.
		expect(got.scale).toBeCloseTo(300 / 72, 10);
	});

	it("distinguishes a transposed page from a differently-shaped one", () => {
		// A4 onto Letter is not a rotation. If the aspect test were "did the
		// numbers change", this would be reported as a 90-degree turn.
		const got = reconcilePageBox(A4, 2480, 3508);
		expect(got.rotated).toBe(false);
		expect(got.box).toEqual(A4);
	});

	it("needs no correction for a square page, which matches both pairings", () => {
		// A 600x600 box: transposing it is a no-op, so the ambiguity is harmless
		// and the page must still be measured at its true scale.
		const square: PageBox = { widthPt: 600, heightPt: 600 };
		const got = reconcilePageBox(square, 2500, 2500);
		expect(got.scale).toBeCloseTo(2500 / 600, 10);
		expect(got.box.widthPt).toBe(600);
		expect(got.box.heightPt).toBe(600);
	});

	it("tolerates the rounding the engine does to a whole-pixel canvas", () => {
		// 612pt at 300 DPI is 2550 exactly, but a box that does not divide evenly
		// rounds, and a strict equality test would call that a rotation.
		const awkward: PageBox = { widthPt: 611.5, heightPt: 792.3 };
		const scale = 300 / 72;
		const got = reconcilePageBox(awkward, Math.round(611.5 * scale), Math.round(792.3 * scale));
		expect(got.rotated).toBe(false);
	});

	it("reports the measured scale when the raster matches NEITHER box", () => {
		// A broken render must produce a visibly wrong reading, not a plausible
		// one. 2550 px across a 612pt page is 300 DPI whatever the height says,
		// and the height here says nothing consistent - so no transposition is
		// invented and the caller sees a scale it can question.
		const got = reconcilePageBox(LETTER, 2550, 999);
		expect(got.rotated).toBe(false);
		expect(got.scale).toBeCloseTo(300 / 72, 10);
	});

	it("refuses inputs it cannot measure against", () => {
		expect(() => reconcilePageBox({ widthPt: 0, heightPt: 792 }, 2550, 3300)).toThrow(RangeError);
		expect(() => reconcilePageBox(LETTER, 0, 3300)).toThrow(RangeError);
		expect(() => reconcilePageBox(LETTER, 2550, -1)).toThrow(RangeError);
		// A fractional canvas is not a pixel count.
		expect(() => reconcilePageBox(LETTER, 2550.5, 3300)).toThrow(RangeError);
	});
});

describe("measuredPrintDpi", () => {
	it("derives the DPI from the raster and the printed box", () => {
		expect(measuredPrintDpi(2550, LETTER)).toBe(300);
		expect(measuredPrintDpi(1275, LETTER)).toBe(150);
	});

	it("measures against the box the page is PRINTED at, not the declared one", () => {
		// The rotation case. Measuring 3300 against the un-rotated 612pt box
		// gives 388 DPI, which is a number no printer and no plan produced.
		const turned = reconcilePageBox(LETTER, 3300, 2550).box;
		expect(measuredPrintDpi(3300, LETTER)).not.toBe(300);
		expect(measuredPrintDpi(3300, turned)).toBe(300);
	});

	it("refuses to divide by a zero-width page", () => {
		expect(() => measuredPrintDpi(2550, { widthPt: 0, heightPt: 792 })).toThrow(RangeError);
	});
});

describe("MAX_PRINT_PAGES", () => {
	it("is a bound a caller can enforce BEFORE rendering anything", () => {
		// It is a count, not a byte total, precisely so the refusal happens
		// before a single 25 MB raster exists. 40 pages is about 1 GB.
		expect(MAX_PRINT_PAGES).toBeGreaterThan(0);
		expect(Number.isInteger(MAX_PRINT_PAGES)).toBe(true);
		expect(MAX_PRINT_PAGES).toBeLessThanOrEqual(64);
	});
});
