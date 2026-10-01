/**
 * SL-4.UI.12: every registry code resolves to a presentation decision.
 *
 * The headline requirement is a UNIVERSAL one - every code - so the gates are
 * written to fail when COVERAGE shrinks, not merely when behaviour changes. A
 * mapping table is the easiest thing in the repo to leave incomplete without
 * anything noticing: a missing entry is not a compile error, it is a user
 * meeting an error nobody planned for.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { REGISTRY, REGISTRY_SIZE, type RegistryCode } from "./error-codes.js";
import { codesByKind, errorState, everyErrorState, recoveryFor, severityFor } from "./errors.js";

const here = dirname(fileURLToPath(import.meta.url));
const registryToml = join(here, "../../../../crates/selis-error/codes.toml");

/**
 * The registry, re-read from the engine's own file.
 *
 * Deliberately an independent parse rather than a re-export: the point is to
 * catch the committed table DRIFTING from its source, which a shared parser
 * could not do.
 */
function parseRegistry(): RegistryCode[] {
	const blocks = readFileSync(registryToml, "utf8").split("[[code]]").slice(1);
	return blocks.map((block) => {
		const field = (key: string): string => {
			const match = block.match(new RegExp(`^${key} = "([^"]*)"`, "m"));
			if (match === null) throw new Error(`codes.toml: no ${key}`);
			return match[1] as string;
		};
		const flag = (key: string): boolean => {
			const match = block.match(new RegExp(`^${key} = (true|false)`, "m"));
			if (match === null) throw new Error(`codes.toml: no ${key}`);
			return match[1] === "true";
		};
		return {
			id: Number(block.match(/^id = (-?[0-9]+)/m)?.[1] ?? Number.NaN),
			name: field("name"),
			kind: field("kind") as RegistryCode["kind"],
			retryable: flag("retryable"),
			docState: field("doc_state") as RegistryCode["docState"],
		};
	});
}

describe("the generated registry table", () => {
	const registry = parseRegistry();

	it("covers every code in codes.toml", () => {
		// THE gate for "every Code". Fails when a code is added to the registry
		// and the table is not regenerated, which is the drift generation exists
		// to prevent.
		expect(REGISTRY_SIZE).toBe(registry.length);
		for (const row of registry) {
			const got = REGISTRY.get(row.id);
			expect(got, `code ${row.id} (${row.name}) is missing from the table`).toBeDefined();
			expect(got?.name).toBe(row.name);
			expect(got?.kind).toBe(row.kind);
			expect(got?.retryable).toBe(row.retryable);
			expect(got?.docState).toBe(row.docState);
		}
	});

	it("carries no code the registry does not have", () => {
		// The other direction. A stale row is a decision about an error that
		// cannot happen, which reads as confidence the engine does not have.
		const known = new Set(registry.map((r) => r.id));
		for (const id of REGISTRY.keys()) {
			expect(known.has(id), `table has ${id}, codes.toml does not`).toBe(true);
		}
	});

	it("covers every FIELD of every row, so a hand-edit cannot slip in", () => {
		// Stricter than it looks: the tests above compare counts and ids, so an
		// edited `kind` or a flipped `retryable` would survive both. This one
		// re-derives each row and compares the whole object.
		for (const row of registry) {
			expect({ ...REGISTRY.get(row.id) }, `row ${row.id} drifted`).toEqual(row);
		}
	});

	it("holds no sentence, because the engine authors the sentence", () => {
		// The registry gives every code a `user_msg` and the engine sends it
		// back. A copy here would be a second phrase for one failure, and UI.02
		// forbids viewer prose. `strings.test.ts``s lint gate would also catch
		// this; asserting it here keeps the reason next to the code.
		const blocks = readFileSync(registryToml, "utf8").split("[[code]]").slice(1);
		expect(blocks.length).toBeGreaterThan(0);
		for (const row of everyErrorState()) {
			expect(REGISTRY.get(row.code)).not.toHaveProperty("userMsg");
		}
	});

	it("leaves a sentence for every code in the registry", () => {
		// The counterpart: the engine side is not allowed to be empty either.
		// A code with no `user_msg` would reach the viewer as a blank panel.
		for (const block of readFileSync(registryToml, "utf8").split("[[code]]").slice(1)) {
			const msg = block.match(/^user_msg = "([^"]*)"/m)?.[1] ?? "";
			expect(msg.length, "a code has an empty user_msg").toBeGreaterThan(0);
		}
	});
});

describe("errorState", () => {
	it("resolves EVERY registered code to a state with an action", () => {
		// The Do, sentence by sentence. Walked over the generated table so a new
		// code is covered by construction rather than by remembering.
		const states = everyErrorState();
		expect(states.length).toBe(REGISTRY_SIZE);
		for (const state of states) {
			expect(state.unknown, `code ${state.code} is unmapped`).toBe(false);
			expect(state.action, `code ${state.code} has no recovery action`).toBeTruthy();
			expect(state.docState, `code ${state.code} has no document state`).toBeTruthy();
			expect(state.severity, `code ${state.code} has no severity`).toBeTruthy();
		}
	});

	it("carries no message of its own", () => {
		// The engine authors one sentence per code; the viewer renders it. A
		// message here would be a second phrasing that can drift.
		for (const state of everyErrorState()) {
			expect(state).not.toHaveProperty("message");
		}
	});

	it("surfaces an unregistered code instead of folding it into a generic", () => {
		// A user reporting "it said something went wrong" gives us nothing. The
		// number has to survive to a bug report for this to ever get fixed.
		const state = errorState(999_999);
		expect(state.unknown).toBe(true);
		expect(state.code).toBe(999_999);
		expect(state.action).toBe("report");
		expect(state.severity).toBe("blocked");
	});

	it("prefers the caller's docState over the registry default", () => {
		// The engine's answer describes THIS failure; the registry default
		// describes the code in general. A file that half-opened must not be
		// reported as cleanly loaded because its usual code says so.
		const partial = errorState(1000, "PartiallyLoaded");
		expect(partial.docState).toBe("PartiallyLoaded");
		expect(partial.severity).toBe("degraded");
	});

	it("never offers a retry the registry says cannot help", () => {
		// A retry control on a non-retryable failure is a dead end wearing a
		// button, which is worse than no button because it looks like progress.
		for (const row of REGISTRY.values()) {
			if (row.retryable) continue;
			const state = errorState(row.id);
			expect(state.action, `code ${row.id} (${row.name}) offers retry`).not.toBe("retry");
			expect(state.actionable, `code ${row.id} claims retry is actionable`).toBe(true);
		}
	});
});

describe("severityFor", () => {
	it("blocks on a document that did not load", () => {
		expect(severityFor("NotLoaded")).toBe("blocked");
	});

	it("calls a partial load DEGRADED, which is the point of the item", () => {
		// A damaged file that still opened is neither a success nor a failure.
		// Reporting it as either is how a user stops trusting the panel.
		expect(severityFor("PartiallyLoaded")).toBe("degraded");
	});

	it("is informational when the document is intact", () => {
		expect(severityFor("Loaded")).toBe("info");
		expect(severityFor("Unchanged")).toBe("info");
		expect(severityFor("Modified")).toBe("info");
	});
});

describe("recoveryFor", () => {
	it("offers a different file when the DOCUMENT is what is wrong", () => {
		for (const row of REGISTRY.values()) {
			if (["Malformed", "Invalid", "Unsupported"].includes(row.kind)) {
				expect(recoveryFor(row), `${row.name} should offer another file`).toBe("open-another");
			}
		}
	});

	it("offers access for a failure the user can grant", () => {
		for (const row of REGISTRY.values()) {
			if (row.kind === "Auth" || row.kind === "Policy") {
				expect(recoveryFor(row), `${row.name} should offer access`).toBe("grant-access");
			}
		}
	});

	it("offers to free memory for a budget failure", () => {
		for (const row of REGISTRY.values()) {
			if (row.kind === "Budget") {
				expect(recoveryFor(row), `${row.name} should offer close-and-retry`).toBe(
					"close-and-retry",
				);
			}
		}
	});

	it("never leaves a code with nothing to do", () => {
		for (const state of everyErrorState()) {
			expect(state.action, `code ${state.code} is a dead end`).toBeTruthy();
		}
	});
});

describe("codesByKind", () => {
	it("groups every code exactly once", () => {
		let total = 0;
		for (const list of codesByKind().values()) total += list.length;
		expect(total).toBe(REGISTRY_SIZE);
	});
});
