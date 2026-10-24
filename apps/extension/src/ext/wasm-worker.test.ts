/**
 * SL-4.EXT.03 - the four imports the engine artifact declares.
 *
 * The engine is a `wasm-bindgen --target web` build: it imports four generated
 * shims rather than host functions. Instantiating it with `{}` is what the
 * SL-4.UI.06 browser check found first - the viewer never got past "Starting
 * the Selis engine..." and the only evidence was `Import #0
 * "__wbindgen_placeholder__"` in the status line.
 *
 * These tests pin the closed list without needing the 4 MB artifact: the same
 * closure the compile step hands to `WebAssembly.instantiate` is testable on
 * its own, which is the level the bug lived at.
 */

import { describe, expect, it } from "vitest";
import { engineImports } from "./wasm-worker.js";

const DECLARED = [
	{ module: "__wbindgen_placeholder__", name: "__wbindgen_describe", kind: "function" },
	{
		module: "__wbindgen_placeholder__",
		name: "__wbg___wbindgen_throw_bb96b2010945f0bc",
		kind: "function",
	},
	{
		module: "__wbindgen_externref_xform__",
		name: "__wbindgen_externref_table_set_null",
		kind: "function",
	},
	{
		module: "__wbindgen_externref_xform__",
		name: "__wbindgen_externref_table_grow",
		kind: "function",
	},
] as const;

/** One import as a callable, for the assertions below. */
function callable(
	imports: WebAssembly.Imports,
	mod: string,
	name: string,
): (...args: unknown[]) => unknown {
	return imports[mod]?.[name] as (...args: unknown[]) => unknown;
}

describe("engineImports", () => {
	it("supplies every import the artifact declares", () => {
		const imports = engineImports(DECLARED);
		expect(Object.keys(imports).sort()).toEqual([
			"__wbindgen_externref_xform__",
			"__wbindgen_placeholder__",
		]);
		for (const entry of DECLARED) {
			expect(typeof callable(imports, entry.module, entry.name)).toBe("function");
		}
	});

	it("throws the wasm-bindgen throw shim rather than swallowing it", () => {
		const imports = engineImports(DECLARED);
		const thrown = callable(
			imports,
			"__wbindgen_placeholder__",
			"__wbg___wbindgen_throw_bb96b2010945f0bc",
		);
		expect(() => thrown()).toThrow(/wasm-bindgen shim/);
	});

	it("grows the externref table by answering 0", () => {
		const imports = engineImports(DECLARED);
		const grow = callable(
			imports,
			"__wbindgen_externref_xform__",
			"__wbindgen_externref_table_grow",
		);
		expect(grow(1)).toBe(0);
	});

	it("refuses an import outside the four it knows", () => {
		// A wasm-bindgen upgrade that adds a shim must fail here, loudly,
		// rather than be stubbed and surface as a null dereference later.
		expect(() =>
			engineImports([
				{ module: "__wbindgen_placeholder__", name: "__wbg_crypto_1", kind: "function" },
			]),
		).toThrow(/unexpected engine import __wbindgen_placeholder__::__wbg_crypto_1/);
		expect(() => engineImports([{ module: "env", name: "abort", kind: "function" }])).toThrow(
			/unexpected engine import module env/,
		);
		expect(() =>
			engineImports([
				{ module: "__wbindgen_placeholder__", name: "__wbindgen_describe", kind: "global" },
			]),
		).toThrow(/unexpected engine import __wbindgen_placeholder__::__wbindgen_describe \(global\)/);
	});
});
