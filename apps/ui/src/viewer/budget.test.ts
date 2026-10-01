/**
 * SL-4.UI.13: a budget exhaustion says what it knows and admits what it does not.
 *
 * The important tests here are the negative ones. The item asks for "which
 * budget, the measured usage, split-the-file remedy"; two of those are
 * available from the wire and two numbers are not. A gate that only checked
 * the good path would let somebody "helpfully" add a usage figure later.
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

describe("what it refuses to claim", () => {
	it("reports no measured usage and no limit", () => {
		// `ResponseMessage::error` carries code/message/detail/docState, and
		// `profiles.toml` is compiled into the guest. Neither number is on the
		// wire, so neither is here.
		//
		// The real guarantee is the TYPE: `measured` and `limit` are declared
		// `null`, not `number | null`, so `tsc` rejects attaching a figure
		// anywhere outside this module. This test only pins the runtime shape.
		for (const code of [4000, 4001, 4002, 4003, 4004, 4010]) {
			const exhaustion = budgetExhaustion(code);
			expect(exhaustion?.measured).toBeNull();
			expect(exhaustion?.limit).toBeNull();
		}
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
