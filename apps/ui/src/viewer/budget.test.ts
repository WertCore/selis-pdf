/**
 * SL-4.UI.13: a budget exhaustion says what it knows and admits what it does not.
 *
 * The item asks for "which budget, the measured usage, split-the-file remedy".
 *
 * The measured usage used to be unavailable: the wire carried only
 * code/message/detail/docState, and this file REFUSED to model the two missing
 * numbers, typing them `null` so `tsc` would reject a fabricated figure
 * anywhere. That refusal was correct and its diagnosis was wrong — the engine
 * had both numbers the whole time, inside a `detail` string. `4e39b049` added
 * the structured channel and this file now reads it.
 *
 * So most of these tests are negative again, for the same reason as before and
 * one more: a gate that only checked the good path would let somebody
 * "helpfully" accept a mismatched, NaN, or poisoned body later. The type is no
 * longer the guarantee — these tests are.
 */
import { describe, expect, it } from "vitest";
import { budgetAction, budgetErrorState, budgetExhaustion, isBudgetCode } from "./budget.js";
import { REGISTRY } from "./error-codes.js";

describe("which budget ran out", () => {
	it("names the resource for every budget code that names one", () => {
		// Mirrors `selis_sandbox::Resource::code()`. A mismatch would name the
		// wrong budget to the reader, which is worse than naming none.
		expect(budgetExhaustion(4000)?.resource).toBe("bytes");
		expect(budgetExhaustion(4001)?.resource).toBe("wall");
		expect(budgetExhaustion(4002)?.resource).toBe("depth");
		expect(budgetExhaustion(4003)?.resource).toBe("objects");
		expect(budgetExhaustion(4004)?.resource).toBe("pixels");
	});

	it("gives `BUDGET_POISONED` no resource, because none is named", () => {
		// It means an earlier exhaustion poisoned the guard. Attributing it to
		// bytes - the most common - would be a guess.
		expect(isBudgetCode(4010)).toBe(true);
		expect(budgetExhaustion(4010)?.resource).toBeNull();
	});

	it("ignores codes that are not budget failures", () => {
		for (const code of [1000, 4020, 5000, 6017, 999_999]) {
			expect(isBudgetCode(code)).toBe(false);
			expect(budgetExhaustion(code)).toBeNull();
		}
	});
});

describe("the measured usage and the limit", () => {
	const bytes = { resource: "bytes", measured: 314_572_800, limit: 268_435_456 };

	it("reports both figures when the guest sends a body that agrees", () => {
		const e = budgetExhaustion(4000, bytes);
		expect(e?.measured).toBe(314_572_800);
		expect(e?.limit).toBe(268_435_456);
		expect(e?.usedFraction).toBeCloseTo(314_572_800 / 268_435_456, 10);
	});

	it("reports null figures when the guest sends no body at all", () => {
		// The real, common case: a non-budget failure, or an older guest.
		const e = budgetExhaustion(4000);
		expect(e?.measured).toBeNull();
		expect(e?.limit).toBeNull();
		expect(e?.usedFraction).toBeNull();
	});

	it("reports an OVERSHOOT above 1 rather than clamping it", () => {
		// "You asked for 300 MB of a 256 MB budget" is the useful sentence.
		// Clamping here would render it as exactly at the limit, which is a
		// different and less useful claim.
		const e = budgetExhaustion(4000, bytes);
		expect(e?.usedFraction ?? 0).toBeGreaterThan(1);
	});
});

describe("what it refuses to claim", () => {
	it("ignores a body whose resource disagrees with the code", () => {
		// BUDGET_BYTES carrying a "pixels" body means the guest and the shell
		// disagree about which budget failed. Showing either number would be a
		// coin toss presented as fact.
		const e = budgetExhaustion(4000, { resource: "pixels", measured: 5, limit: 4 });
		expect(e?.measured).toBeNull();
		expect(e?.limit).toBeNull();
	});

	it("ignores a body with no resource name", () => {
		// Present-but-unnamed is not a shortcut around the cross-check.
		const e = budgetExhaustion(4000, { measured: 5, limit: 4 });
		expect(e?.measured).toBeNull();
	});

	it("never accepts a body for BUDGET_POISONED", () => {
		// Poisoning means an EARLIER exhaustion was the cause. Whatever resource
		// the message names, the honest report is that no budget is identified.
		// 4010 is deliberately absent from RESOURCE_BY_CODE.
		for (const resource of ["bytes", "wall", "depth", "objects", "pixels"]) {
			const e = budgetExhaustion(4010, { resource, measured: 5, limit: 4 });
			expect(e?.measured).toBeNull();
			expect(e?.limit).toBeNull();
		}
	});

	it("refuses numbers that are not byte counts or durations", () => {
		// Each of these would render as something absurd but not obviously so:
		// "NaN MB used", "-1 bytes used", a bar pinned to Infinity.
		const bad = [
			{ resource: "bytes", measured: Number.NaN, limit: 10 },
			{ resource: "bytes", measured: 10, limit: Number.POSITIVE_INFINITY },
			{ resource: "bytes", measured: -1, limit: 10 },
			{ resource: "bytes", measured: 10, limit: 0 },
			{ resource: "bytes", measured: 1.5, limit: 10 },
			{ resource: "bytes", measured: 10, limit: -5 },
		];
		for (const body of bad) {
			const e = budgetExhaustion(4000, body);
			expect(e?.measured, JSON.stringify(body)).toBeNull();
			expect(e?.usedFraction, JSON.stringify(body)).toBeNull();
		}
	});

	it("does not throw on a malformed body", () => {
		// A failure panel that throws while reporting a failure shows nothing
		// at all, which is strictly worse than showing less.
		expect(() => budgetExhaustion(4000, {} as never)).not.toThrow();
		expect(() => budgetExhaustion(4000, { resource: 7 } as never)).not.toThrow();
	});
});

describe("the remedy", () => {
	it("suggests splitting for budgets that mean the document is too big", () => {
		expect(budgetExhaustion(4000)?.remedy).toBe("split");
		expect(budgetExhaustion(4003)?.remedy).toBe("split");
	});

	it("suggests a smaller render for the pixel budget", () => {
		// The document is fine; the RASTER was the problem. Splitting it would
		// not help, and the honest advice is a smaller render.
		expect(budgetExhaustion(4004)?.remedy).toBe("reduce-resolution");
	});

	it("reports rather than offering a fix for pathological nesting", () => {
		// Not retryable, and not a size problem. There is nothing to configure.
		expect(budgetExhaustion(4002)?.remedy).toBe("report");
	});

	it("reports for a poisoned guard, which names nothing", () => {
		expect(budgetExhaustion(4010)?.remedy).toBe("report");
	});
});

describe("the action a budget failure offers", () => {
	it("offers close-and-retry only where the registry says retrying can help", () => {
		for (const code of [4000, 4001, 4003, 4004]) {
			expect(budgetAction(code)).toBe("close-and-retry");
		}
	});

	it("never offers retry for the one budget that cannot succeed on a retry", () => {
		// `BUDGET_DEPTH` is `retryable: false` and the registry is right: the
		// same file nests just as deeply the second time. A button here
		// promises progress and delivers a loop.
		const depth = budgetExhaustion(4002);
		expect(depth?.retryable).toBe(false);
		expect(budgetAction(4002)).toBe("report");
	});

	it("never offers retry for a poisoned guard either", () => {
		expect(budgetAction(4010)).toBe("report");
	});
});

describe("the presentation it derives", () => {
	it("is degraded, never blocked, because a budget keeps the document", () => {
		// Every budget code's `doc_state` is PartiallyLoaded or Loaded. Calling
		// it blocked would claim the reader lost a document they still have.
		for (const code of [4000, 4001, 4002, 4003, 4004, 4010]) {
			expect(budgetErrorState(code)?.severity).toBe("degraded");
		}
	});

	it("keeps the registry's own document state", () => {
		// `BUDGET_PIXELS` is Loaded, not PartiallyLoaded: the document is
		// entirely intact and only the raster did not finish.
		expect(budgetErrorState(4004)?.docState).toBe("Loaded");
		expect(budgetErrorState(4000)?.docState).toBe("PartiallyLoaded");
	});

	it("is null for anything that is not a budget failure", () => {
		expect(budgetErrorState(1000)).toBeNull();
		expect(budgetErrorState(4020)).toBeNull();
	});

	it("agrees with the registry on every budget code", () => {
		for (const code of [4000, 4001, 4002, 4003, 4004, 4010]) {
			expect(budgetErrorState(code)?.name).toBe(REGISTRY.get(code)?.name);
		}
	});
});
