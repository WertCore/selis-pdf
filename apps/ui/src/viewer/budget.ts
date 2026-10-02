/**
 * What a budget exhaustion means, and what it does not (SL-4.UI.13).
 *
 * ## The two numbers, and how they got here

The item asks for "the honest budget-exhaustion state (which budget, **the
measured usage**, split-the-file remedy)".

An earlier revision of this file typed `measured` and `limit` as `null`,
because the wire did not carry them: `ResponseMessage::error` sent `code`,
`message`, `detail` and `docState`, and nothing else. The refusal was right
but the diagnosis was wrong. The engine had BOTH numbers the whole time,
packed into the `detail` string as prose:

    "bytes limit=268435456 requested=314572800"

So the gap was a missing channel, not missing information, and refusing to
model it was refusing to read a field that already existed. `4e39b049` added
that channel: a budget failure now carries a structured `budget` object, and
this module reads it.

## Why they are `number | null` again, and still cannot lie

`null` now means "the engine did not tell us", which is a real state: a
non-budget failure has no body, and neither does `BUDGET_POISONED`, where an
EARLIER exhaustion is the reason and naming a resource would blame an
innocent budget. `null` never means "zero", and never means "we worked it
out ourselves".

A body is discarded, and the pair falls back to `null`, when it fails any of
the checks in `usageFor`. The important one is that the body's resource must
agree with the code's. A mismatch means the message came from a build whose
registry disagrees with the one this table reads, and two sources that
disagree about which budget failed is a situation to show nothing in rather
than pick a winner between.

## What IS derived rather than reported

The resource, whether retrying can possibly help (the registry knows), and a
remedy category. `usedFraction` is arithmetic on the engine's own two
numbers, never a guess.
 *
## What IS said instead
 *
 * The resource, whether retrying can possibly help (the registry knows), and a
 * remedy category. That is genuinely actionable, and every word of it is
 * derived rather than guessed.
 *
 * ## Why `BUDGET_DEPTH` is special
 *
 * It is the one budget marked `retryable: false`, and the registry is right:
 * the same file nests just as deeply the second time. So closing the document
 * and trying again is not a recovery, it is a loop - a dead end wearing a
 * button, which is the thing SL-4.UI.12 exists to stop.
 *
 * @see SL-4.UI.13
 */

import { type DocState, REGISTRY } from "./error-codes.js";
import type { ErrorState } from "./errors.js";

/** A budgeted resource, named as `selis_sandbox::Resource` names it. */
export type BudgetResource = "bytes" | "wall" | "depth" | "objects" | "pixels";

/** What a reader can actually do about it. The host supplies the words. */
export type BudgetRemedy =
	/** The document is too big overall; work on part of it. */
	| "split"
	/** The raster was the problem, not the document; render smaller. */
	| "reduce-resolution"
	/** The file is pathological rather than large. Nothing to configure. */
	| "report";

/** One budget exhaustion, as far as the wire honestly allows. */
export interface BudgetExhaustion {
	readonly code: number;
	/**
	 * The resource that ran out, or `null` for `BUDGET_POISONED`, which means
	 * an earlier exhaustion already poisoned the guard rather than a named
	 * resource being hit.
	 */
	readonly resource: BudgetResource | null;
	/** From the registry: can the same operation plausibly succeed again? */
	readonly retryable: boolean;
	readonly docState: DocState;
	readonly remedy: BudgetRemedy;
	/**
	 * What the engine reported it had used, or `null` if it did not say.
	 *
	 * `number | null` now, not `null`, because the wire carries it as of
	 * `4e39b049`. The rule that made `null` a TYPE in the first place still
	 * holds in spirit: this may only ever hold a number the ENGINE sent, never
	 * one this module worked out, defaulted, or inferred. `null` means "not
	 * reported", and rendering it as `0` would be the bug this item is about.
	 */
	readonly measured: number | null;
	/** The limit the engine measured against, or `null` if it did not say. */
	readonly limit: number | null;
	/**
	 * `measured / limit`, or `null` when either is unknown.
	 *
	 * Left UNCLAMPED on purpose. A charge that overshoots reports a fraction
	 * above 1, and that overshoot is the fact - "you asked for 300 MB of a
	 * 256 MB budget" is the useful sentence. Clamping here would hide it, and
	 * a renderer that wants a 0..1 bar should clamp at the point of drawing
	 * where the visual constraint lives.
	 */
	readonly usedFraction: number | null;
}

/**
 * A budget exhaustion as it arrives from the guest.
 *
 * Mirrors `selis_pdf_wasm::protocol::BudgetBody`. Optional everywhere except
 * `resource`, because a malformed message must not throw inside a failure
 * panel - a crash while reporting a crash shows the reader nothing at all.
 */
export interface WireBudget {
	readonly resource?: string;
	readonly measured?: number;
	readonly limit?: number;
}

/** Registry code -> resource. Mirrors `selis_sandbox::Resource::code()`. */
const RESOURCE_BY_CODE = new Map<number, BudgetResource>([
	[4000, "bytes"],
	[4001, "wall"],
	[4002, "depth"],
	[4003, "objects"],
	[4004, "pixels"],
]);

/** The codes that mean "a budget was exhausted". */
const BUDGET_CODES = new Set([4000, 4001, 4002, 4003, 4004, 4010]);

/** True when this registry code is a budget exhaustion. */
export function isBudgetCode(code: number): boolean {
	return BUDGET_CODES.has(code);
}

/**
 * The remedy for a resource.
 *
 * Keyed on the resource rather than the code, because the code is one-to-one
 * with it and a second table would be a second thing to forget to update.
 */
function remedyFor(resource: BudgetResource | null, retryable: boolean): BudgetRemedy {
	switch (resource) {
		case "pixels":
			// The document is fine; the RASTER was the problem. Splitting it
			// would not help, and the honest advice is a smaller render.
			return "reduce-resolution";
		case "depth":
			// Pathological nesting. Not retryable, and not a size problem.
			return "report";
		case "wall":
			// Slow rather than big. A second attempt on the same machine is
			// unlikely to be faster, but this is the one budget where retrying
			// is the registry's own answer, so it is not called unrecoverable.
			return retryable ? "split" : "report";
		case "bytes":
		case "objects":
			// The document is genuinely too big for this surface.
			return "split";
		default:
			// `BUDGET_POISONED`, or an unmapped budget code.
			return "report";
	}
}

/**
 * Validate a wire budget body against the code it arrived with.
 *
 * Returns `null` for anything untrustworthy, and the CALLER then reports
 * `measured`/`limit` as `null`. Failing closed is the whole point: the panel
 * that is supposed to explain an exhaustion is the worst possible place to
 * print a number that was not checked.
 *
 * The rejections, and why each earns its keep:
 *
 * - **No body, or a missing field.** Nothing to show.
 * - **Resource disagrees with the code.** The guest and the shell disagree
 *   about which budget failed. Picking either one would be a coin toss
 *   presented as fact, so show neither.
 * - **`BUDGET_POISONED`.** Never named, whatever the body claims. Poisoning
 *   means an earlier exhaustion was the cause; attributing it to the resource
 *   that tripped the assertion blames an innocent budget.
 * - **Non-finite, negative or fractional.** Not a byte count and not a
 *   duration. `NaN` reaching a template literal renders "NaN MB used", which
 *   is worse than nothing.
 * - **A limit of zero.** The fraction would be a division by zero; the engine
 *   cannot exhaust a zero budget, so this is a malformed message, not a fact.
 */
export function usageFor(
	code: number,
	body: WireBudget | undefined,
): { measured: number; limit: number } | null {
	const resource = RESOURCE_BY_CODE.get(code);
	if (resource === undefined || resource === null) return null;
	if (body === undefined || body === null) return null;

	const { measured, limit } = body;
	if (typeof measured !== "number" || typeof limit !== "number") return null;
	if (!Number.isFinite(measured) || !Number.isFinite(limit)) return null;
	if (measured < 0 || limit <= 0) return null;
	if (!Number.isInteger(measured) || !Number.isInteger(limit)) return null;

	// The cross-check. `body.resource` must name the very budget this code is.
	// A missing or different name is a mismatch, not a shortcut.
	if (body.resource !== resource) return null;

	return { measured, limit };
}

/**
 * Read a budget exhaustion out of a registry row.
 *
 * `body` is the optional `budget` object from the failure message. Omit it
 * and the exhaustion is returned with `measured`/`limit` as `null` - the
 * honest "not reported", not a zero.
 *
 * Returns `null` for anything that is not a budget failure, so a caller can
 * branch on the answer rather than on a name.
 */
export function budgetExhaustion(code: number, body?: WireBudget): BudgetExhaustion | null {
	const row = REGISTRY.get(code);
	if (row === undefined || !isBudgetCode(code)) return null;
	const resource = RESOURCE_BY_CODE.get(code) ?? null;
	const usage = usageFor(code, body);
	return {
		code,
		resource,
		retryable: row.retryable,
		docState: row.docState,
		remedy: remedyFor(resource, row.retryable),
		measured: usage === null ? null : usage.measured,
		limit: usage === null ? null : usage.limit,
		usedFraction: usage === null ? null : usage.measured / usage.limit,
	};
}

/**
 * The recovery action a budget failure should offer, or `null` if the code is
 * not a budget failure.
 *
 * Takes the CODE rather than a `BudgetExhaustion`, so a caller does not have to
 * handle the "not a budget failure" null itself before asking the only question
 * it has.
 *
 * A non-retryable budget must NOT be offered "try again": the same document
 * exhausts the same budget every time, so the button promises progress and
 * delivers a loop. `BUDGET_DEPTH` is the case the registry marks that way.
 */
export function budgetAction(code: number): "close-and-retry" | "report" | null {
	const exhaustion = budgetExhaustion(code);
	if (exhaustion === null) return null;
	return exhaustion.retryable ? "close-and-retry" : "report";
}

/**
 * Re-derive the presentation for a budget failure.
 *
 * Kept separate from `errorState` rather than folded into it: budget failures
 * are the one case where the generic "close and retry" answer is wrong, and a
 * caller that forgets to consult this is exactly the dead end above.
 *
 * Deliberately does NOT hang an `exhaustion` off the returned `ErrorState`.
 * That interface is the generic one every failure flows through; bolting a
 * budget-only field onto it couples the two exactly as tightly as folding this
 * function into `errorState` would, and a reader wanting the numbers would
 * have to know which of the two shapes it is holding. A caller that wants the
 * figures asks {@link budgetExhaustion} for them, with the same `body`.
 */
export function budgetErrorState(code: number): ErrorState | null {
	const exhaustion = budgetExhaustion(code);
	if (exhaustion === null) return null;
	return {
		code,
		name: REGISTRY.get(code)?.name ?? "BUDGET",
		source: "registry",
		severity: exhaustion.docState === "NotLoaded" ? "blocked" : "degraded",
		docState: exhaustion.docState,
		action: budgetAction(code) ?? "report",
		actionable: true,
		unknown: false,
	};
}
