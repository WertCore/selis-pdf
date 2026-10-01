/**
 * The state of a long operation in the tab (SL-4.UI.13).
 *
 * ## Where the numbers come from, and why this model is shaped around them
 *
 * The engine publishes progress through `selis_progress_slot()`: twelve
 * consecutive bytes holding a **request id**, a **stage code**, and a
 * **fraction in basis points**. That is the whole input. Three properties of
 * that input drive every decision below, and none of them are obvious until
 * you have watched them go wrong:
 *
 *  1. **The fraction is a snapshot, not a stream.** The host polls the slot
 *     between operations. It can observe the same value twice, and it can
 *     observe a SMALLER value for a LATER poll - the engine is reporting, not
 *     animating. So progress here is **monotonic**: it only ever moves
 *     forward. A progress bar that runs backwards is worse than no bar, because
 *     it teaches the reader that the number is not to be trusted.
 *
 *  2. **The request id is what makes the rest safe.** A slow operation
 *     followed by a fast one leaves the slow one's slot contents behind. Without
 *     the id, the next poll reads the PREVIOUS operation's progress and paints
 *     a confident 90% for a job that has not started. So a report whose id is
 *     not the one the shell is tracking is **dropped**, not shown.
 *
 *  3. **Cancellation is a request, not a fact.** Writing the cancel flag asks
 *     the engine to stop; the operation may finish before it notices. So
 *     cancelling moves to `cancelling`, and only the engine's own `CANCELLED`
 *     reply - or the caller saying the op is done - reaches `cancelled`. The UI
 *     must never claim a cancellation that did not happen.
 *
 * ## No prose, no DOM
 *
 * Like `error-panel.ts`, this decides and returns; the host paints and supplies
 * the words (ADR-P0044, UI.02). Everything here is pure and testable without a
 * browser.
 *
 * @see SL-4.UI.13
 */

/** One poll of `selis_progress_slot()`, already decoded. */
export interface SlotReport {
	/** The engine request id this report belongs to. */
	readonly request: number;
	/** The engine's stage code. Meaning is the engine's; the shell passes it on. */
	readonly stage: number;
	/** Progress in basis points, 0-10000. The engine clamps. */
	readonly fractionBp: number;
}

/** What the shell is doing. */
export type Phase =
	/** Nothing running. */
	| "idle"
	/** An operation is in flight. */
	| "running"
	/** A cancel has been asked for; the engine has not answered. */
	| "cancelling"
	/** Finished on its own. */
	| "done"
	/** The engine reported a failure. */
	| "failed"
	/** The engine reported `CANCELLED`. */
	| "cancelled";

/** Everything the panel needs, and nothing it has to decide. */
export interface OperationState {
	readonly phase: Phase;
	/** 0..1, or `null` when there is nothing to show. */
	readonly fraction: number | null;
	/** The engine's stage code, `null` before the first report. */
	readonly stage: number | null;
	/** The request this state is tracking; `null` when idle. */
	readonly request: number | null;
	/** True while an operation is in flight (running or cancelling). */
	readonly busy: boolean;
	/** True when a cancel has been asked for but not yet confirmed. */
	readonly awaitingCancel: boolean;
}

/** How often progress advances are published, in basis points. */
const BP_PER_UNIT = 10_000;

/**
 * Begin tracking an operation.
 *
 * Progress starts at `null` rather than `0`. Zero is a claim - "nothing done" -
 * and for the first moments of an operation the honest answer is that nothing
 * has been measured yet. A bar that starts empty and springs to life reads
 * better than one that sits at zero implying work is already underway.
 */
export function beginOperation(request: number): OperationState {
	return {
		phase: "running",
		fraction: null,
		stage: null,
		request,
		busy: true,
		awaitingCancel: false,
	};
}

/** Nothing running. */
export function idle(): OperationState {
	return {
		phase: "idle",
		fraction: null,
		stage: null,
		request: null,
		busy: false,
		awaitingCancel: false,
	};
}

/**
 * Fold one slot poll into the current state.
 *
 * Dropped, deliberately, when:
 *  - the report belongs to a different request (rule 2 above);
 *  - the operation is already terminal, so a late poll cannot resurrect a bar.
 *
 * Clamped, deliberately, when the fraction would go backwards (rule 1).
 */
export function applyReport(state: OperationState, report: SlotReport): OperationState {
	if (report.request !== state.request) return state;
	if (!state.busy) return state;
	const fraction = Math.min(1, Math.max(0, report.fractionBp / BP_PER_UNIT));
	// Monotonic. The engine reports, it does not animate.
	const next = state.fraction === null ? fraction : Math.max(state.fraction, fraction);
	return {
		...state,
		phase: state.phase === "cancelling" ? "cancelling" : "running",
		fraction: next,
		stage: report.stage,
	};
}

/**
 * Ask the operation to stop.
 *
 * Moves to `cancelling`, NOT `cancelled` (rule 3). Only
 * {@link confirmCancelled} or {@link finish} may reach the terminal phases, so
 * the shell cannot show "Cancelled" for a job the engine never stopped.
 */
export function requestCancel(state: OperationState): OperationState {
	if (!state.busy) return state;
	if (state.phase === "cancelling") return state;
	return { ...state, phase: "cancelling", awaitingCancel: true };
}

/**
 * The engine answered `CANCELLED`, or the shell observed the operation stop.
 *
 * The only way to reach `cancelled`.
 */
export function confirmCancelled(state: OperationState): OperationState {
	if (!state.busy) return state;
	return {
		...state,
		phase: "cancelled",
		busy: false,
		awaitingCancel: false,
		// Cancelled is not 100%. Reporting a full bar for work that stopped
		// early is a small lie that a reader will notice.
		fraction: state.fraction,
	};
}

/**
 * The operation finished, whether or not a fraction was ever reported.
 *
 * `fraction` is forced to 1 only when progress had actually been measured. A
 * fast operation that never reported would otherwise jump from "unknown" to
 * "complete", which is a bar that appears to have run.
 */
export function finish(state: OperationState): OperationState {
	if (!state.busy) return state;
	return {
		...state,
		phase: "done",
		busy: false,
		awaitingCancel: false,
		fraction: state.fraction === null ? null : 1,
	};
}

/** The operation failed. Progress stops where it was; it does not complete. */
export function fail(state: OperationState): OperationState {
	if (!state.busy) return state;
	return { ...state, phase: "failed", busy: false, awaitingCancel: false };
}
