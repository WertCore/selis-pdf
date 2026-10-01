/**
 * What a budget exhaustion means, and what it does not (SL-4.UI.13).
 *
 * ## The Do asks for two numbers this layer cannot supply
 *
 * The item asks for "the honest budget-exhaustion state (which budget, **the
 * measured usage**, split-the-file remedy)". Two of those three are available;
 * one is not, and the third number is not either:
 *
 *  - **WHICH budget** - yes. Each budget failure has its own registry code
 *    (`BUDGET_BYTES`, `BUDGET_WALL`, ...), so the resource is known.
 *  - **The MEASURED usage** - **no.** `selis_sandbox::BudgetGuard` tracks a
 *    `Usage`, but `ResponseMessage::error` carries only `code`, `message`,
 *    `detail` and `docState`. The string `usage` appears nowhere in the WASM
 *    crate. The wire does not have it.
 *  - **The LIMIT** - **also no**, which is not obvious. `profiles.toml` is
 *    compiled into a Rust `const fn` by `selis-sandbox/build.rs`, so the
 *    numbers exist in the guest and are not reachable from JS at all. The
 *    shell can ask for `{surface: "viewer"}` but cannot read back what that
 *    surface allows.
 *
 * So `measured` and `limit` are typed `null` below. Not "null for now" -
 * `null` as a TYPE, so no caller can assign a number to them and no UI can
 * render one by accident. Filling them in needs a protocol change that carries
 * the resource, the limit and the usage on a budget failure; until then a panel
 * that shows "used 300 MB of 256 MB" would be inventing both figures.
 *
 * ## What IS said instead
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
	 * Always `null`: the wire does not carry it.
	 *
	 * Typed `null`, not `number | null`, on purpose. `number | null` invites
	 * exactly the bug this item is about - a caller that finds no measurement
	 * and substitutes a plausible one. This cannot be assigned a value at all.
	 */
	readonly measured: null;
	/** Always `null`: `profiles.toml` is compiled into the guest, not exposed. */
	readonly limit: null;
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
 * Read a budget exhaustion out of a registry row.
 *
 * Returns `null` for anything that is not a budget failure, so a caller can
 * branch on the answer rather than on a name.
 */
export function budgetExhaustion(code: number): BudgetExhaustion | null {
	const row = REGISTRY.get(code);
	if (row === undefined || !isBudgetCode(code)) return null;
	const resource = RESOURCE_BY_CODE.get(code) ?? null;
	return {
		code,
		resource,
		retryable: row.retryable,
		docState: row.docState,
		remedy: remedyFor(resource, row.retryable),
		// Never a number. See the type.
		measured: null,
		limit: null,
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
