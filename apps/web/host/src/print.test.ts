import { describe, expect, it } from "vitest";
import { MIN_PRINT_DPI, type PageBox, parsePrintScaling, planPrint } from "./print.js";

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
