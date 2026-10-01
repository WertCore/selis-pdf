/**
 * SL-4.UI.13: the rules that keep a progress bar honest.
 *
 * Each `describe` below names one of the three properties of the engine's
 * slot that the model exists to enforce, and each was chosen because getting
 * it wrong produces a bar that is confidently wrong - which is worse than no
 * bar at all.
 */
import { describe, expect, it } from "vitest";
import {
	type OperationState,
	type SlotReport,
	applyReport,
	beginOperation,
	confirmCancelled,
	fail,
	finish,
	idle,
	requestCancel,
} from "./progress.js";

/** A slot poll, with sensible defaults so each test states only what matters. */
const slot = (over: Partial<SlotReport> = {}): SlotReport => ({
	request: 7,
	stage: 1,
	fractionBp: 0,
	...over,
});

describe("the request id decides which operation a report belongs to", () => {
	it("ignores a report from a different request", () => {
		// The core safety property. A slow operation leaves its slot contents
		// behind; without the id the next poll paints a confident 90% for a job
		// that has not started.
		const started = beginOperation(7);
		const afterOwn = applyReport(started, slot({ fractionBp: 9000 }));
		const afterStale = applyReport(afterOwn, slot({ request: 6, fractionBp: 9000 }));

		expect(afterStale).toEqual(afterOwn);
		expect(afterStale.fraction).toBe(0.9);
	});

	it("does not let a stale report change the stage either", () => {
		const state = applyReport(beginOperation(7), slot({ stage: 3, fractionBp: 1000 }));
		expect(applyReport(state, slot({ request: 8, stage: 99 })).stage).toBe(3);
	});

	it("follows a report for the request it is tracking", () => {
		const state = applyReport(beginOperation(7), slot({ fractionBp: 2500 }));
		expect(state.fraction).toBe(0.25);
		expect(state.stage).toBe(1);
	});
});

describe("progress only ever moves forward", () => {
	it("holds the higher value when the engine reports a smaller one", () => {
		// The engine reports, it does not animate. A bar that runs backwards
		// teaches the reader that the number cannot be trusted.
		const state = applyReport(beginOperation(7), slot({ fractionBp: 6000 }));
		expect(applyReport(state, slot({ fractionBp: 2000 })).fraction).toBe(0.6);
	});

	it("is happy to repeat the same value", () => {
		const state = applyReport(beginOperation(7), slot({ fractionBp: 4000 }));
		expect(applyReport(state, slot({ fractionBp: 4000 })).fraction).toBe(0.4);
	});

	it("clamps above one rather than showing a bar past its end", () => {
		const state = applyReport(beginOperation(7), slot({ fractionBp: 25_000 }));
		expect(state.fraction).toBe(1);
	});

	it("clamps a negative reading to zero", () => {
		const state = applyReport(beginOperation(7), slot({ fractionBp: -100 }));
		expect(state.fraction).toBe(0);
	});
});

describe("progress starts unknown, not at zero", () => {
	it("reports no fraction until something is measured", () => {
		// Zero is a claim - "nothing done". For the first moments of an
		// operation the honest answer is that nothing has been measured yet.
		expect(beginOperation(7).fraction).toBeNull();
	});

	it("finishes without inventing a number if none was ever reported", () => {
		// Otherwise a fast operation jumps from "unknown" to "complete", which
		// is a bar that appears to have run.
		expect(finish(beginOperation(7)).fraction).toBeNull();
	});

	it("completes to one once progress WAS measured", () => {
		const started = applyReport(beginOperation(7), slot({ fractionBp: 3000 }));
		expect(finish(started).fraction).toBe(1);
	});

	it("starts at no fraction rather than zero when idle", () => {
		expect(idle().fraction).toBeNull();
	});
});

describe("cancelling is a request, not a fact", () => {
	it("moves to cancelling, never straight to cancelled", () => {
		// Writing the cancel flag asks the engine to stop; it may finish first.
		// Claiming "Cancelled" for a job that ran to completion is a lie.
		const state = requestCancel(beginOperation(7));
		expect(state.phase).toBe("cancelling");
		expect(state.phase).not.toBe("cancelled");
		expect(state.awaitingCancel).toBe(true);
	});

	it("only reaches cancelled when the engine says so", () => {
		const state = confirmCancelled(requestCancel(beginOperation(7)));
		expect(state.phase).toBe("cancelled");
		expect(state.awaitingCancel).toBe(false);
		expect(state.busy).toBe(false);
	});

	it("does not report a cancelled operation as complete", () => {
		// A full bar for work that stopped early is a small lie a reader notices.
		const started = applyReport(beginOperation(7), slot({ fractionBp: 3000 }));
		expect(confirmCancelled(requestCancel(started)).fraction).toBe(0.3);
	});

	it("stays cancelling across further progress reports", () => {
		// The engine may keep reporting while it winds down. That is not a
		// reason to go back to "running".
		const state = applyReport(requestCancel(beginOperation(7)), slot({ fractionBp: 5000 }));
		expect(state.phase).toBe("cancelling");
	});

	it("ignores a cancel for an operation that is not running", () => {
		expect(requestCancel(idle()).phase).toBe("idle");
	});

	it("is idempotent", () => {
		const once = requestCancel(beginOperation(7));
		expect(requestCancel(once)).toEqual(once);
	});
});

describe("terminal phases are terminal", () => {
	const finished = (): OperationState => finish(beginOperation(7));

	it("ignores a late poll after the operation finished", () => {
		// The slot still holds the old contents. A late poll must not resurrect
		// a bar for work that is over.
		expect(applyReport(finished(), slot({ fractionBp: 5000 }))).toEqual(finished());
	});

	it("ignores a cancel after the operation finished", () => {
		expect(requestCancel(finished()).phase).toBe("done");
	});

	it("keeps a failure from becoming a completion", () => {
		const state = fail(applyReport(beginOperation(7), slot({ fractionBp: 7000 })));
		expect(state.phase).toBe("failed");
		expect(state.busy).toBe(false);
		// Progress stops where it was. Finishing it would report success.
		expect(state.fraction).toBe(0.7);
	});
});

describe("the whole shape of a long operation", () => {
	it("runs, is asked to stop, then is confirmed stopped", () => {
		let state = beginOperation(7);
		state = applyReport(state, slot({ fractionBp: 2000 }));
		expect(state.phase).toBe("running");
		expect(state.busy).toBe(true);

		state = requestCancel(state);
		expect(state.busy).toBe(true);

		state = confirmCancelled(state);
		expect(state.phase).toBe("cancelled");
		expect(state.busy).toBe(false);
	});

	it("reports itself busy while running and not while cancelling-only", () => {
		const running = beginOperation(7);
		expect(running.busy).toBe(true);
		expect(running.awaitingCancel).toBe(false);
		expect(requestCancel(running).busy).toBe(true);
		expect(idle().busy).toBe(false);
	});

	it("tracks the request it is following", () => {
		expect(beginOperation(42).request).toBe(42);
		expect(idle().request).toBeNull();
	});
});
