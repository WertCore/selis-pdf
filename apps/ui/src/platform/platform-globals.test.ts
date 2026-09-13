/**
 * The SL-4.UI.01 DoD gate: production code in `apps/ui` must not touch
 * platform globals — every host capability goes through the
 * `PlatformAdapter`. This is a lint-style unit test so it runs in the same
 * `pnpm -r test` pass as everything else (Biome has no restricted-globals
 * rule); it scans non-test TypeScript under `apps/ui/src`, strips comments,
 * and forbids bare references to browser/extension globals. Property keys
 * (`window:`) and member access (`adapter.window.setTitle`) are allowed —
 * those are the *port*, not the global.
 */

import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const srcRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

/** Global identifiers that must never appear bare in `apps/ui` production code. */
const FORBIDDEN_GLOBALS = [
	"window",
	"document",
	"navigator",
	"location",
	"localStorage",
	"sessionStorage",
	"indexedDB",
	"caches",
	"fetch",
	"alert",
	"confirm",
	"prompt",
	"crypto",
	"chrome",
	"browser",
	"Worker",
	"SharedArrayBuffer",
	"OffscreenCanvas",
] as const;

/** Bare-identifier usage: not a property key (`window:`), not a member (`x.window`). */
const usagePattern = (name: string): RegExp =>
	new RegExp(`(?<![\\w$."'])${name}(?![\\w$])(?!\\s*:)`, "g");

function collectTsFiles(dir: string, kind: "production" | "tests"): string[] {
	const found: string[] = [];
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		const path = join(dir, entry.name);
		if (entry.isDirectory()) {
			found.push(...collectTsFiles(path, kind));
		} else if (entry.name.endsWith(".ts")) {
			const isTest = entry.name.endsWith(".test.ts") || entry.name.endsWith(".d.ts");
			if ((kind === "tests") === isTest) {
				found.push(path);
			}
		}
	}
	return found;
}

function stripComments(source: string): string {
	return source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:"'`\\])\/\/[^\n]*/g, "$1");
}

/**
 * Drop everything that is not executable code: line/block comments and string
 * literals (single, double, template). Inside `${…}` interpolations the
 * scanner keeps skipping until the closing backtick — a deliberate blind spot
 * (a global referenced only inside a template interpolation would be missed);
 * the gate is a guardrail against accidental global access, not a parser.
 */
function stripNonCode(source: string): string {
	let out = "";
	let state: "code" | "line" | "block" | "single" | "double" | "template" = "code";
	let i = 0;
	while (i < source.length) {
		const ch = source[i];
		const next = i + 1 < source.length ? source[i + 1] : "";
		switch (state) {
			case "code":
				if (ch === "/" && next === "/") {
					state = "line";
					i += 2;
				} else if (ch === "/" && next === "*") {
					state = "block";
					i += 2;
				} else if (ch === "'") {
					state = "single";
					i += 1;
				} else if (ch === '"') {
					state = "double";
					i += 1;
				} else if (ch === "`") {
					state = "template";
					i += 1;
				} else {
					out += ch;
					i += 1;
				}
				break;
			case "line":
				if (ch === "\n") {
					state = "code";
					out += ch;
				}
				i += 1;
				break;
			case "block":
				if (ch === "*" && next === "/") {
					state = "code";
					i += 2;
				} else {
					i += 1;
				}
				break;
			case "single":
				if (ch === "\\") {
					i += 2;
				} else {
					if (ch === "'") {
						state = "code";
					}
					i += 1;
				}
				break;
			case "double":
				if (ch === "\\") {
					i += 2;
				} else {
					if (ch === '"') {
						state = "code";
					}
					i += 1;
				}
				break;
			case "template":
				if (ch === "\\") {
					i += 2;
				} else {
					if (ch === "`") {
						state = "code";
					}
					i += 1;
				}
				break;
		}
	}
	return out;
}

describe("platform-globals lint gate (SL-4.UI.01)", () => {
	it("apps/ui production code never references platform globals", () => {
		const offenders: string[] = [];
		for (const path of collectTsFiles(srcRoot, "production")) {
			const source = stripNonCode(stripComments(readFileSync(path, "utf8")));
			for (const name of FORBIDDEN_GLOBALS) {
				const pattern = usagePattern(name);
				pattern.lastIndex = 0;
				if (pattern.test(source)) {
					offenders.push(`${path.replace(srcRoot, "src")}: ${name}`);
				}
			}
		}
		expect(offenders, `forbidden platform globals: ${offenders.join("; ")}`).toEqual([]);
	});

	it("the scanner covers test files too (sanity)", () => {
		const files = collectTsFiles(srcRoot, "tests");
		expect(files.some((path) => path.endsWith("platform-globals.test.ts"))).toBe(true);
	});
});
