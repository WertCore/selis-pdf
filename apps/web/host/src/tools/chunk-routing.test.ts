import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { TOOLS } from "../tool-registry.js";
import {
	COMPARE_TOOL_IDS,
	OPS_CHUNK_URL,
	OPTIMIZE_TOOL_IDS,
	opsChunkForTool,
} from "./chunk-routing.js";
import { COMPARE_WASM_URL } from "./compare-protocol.js";
import { OPTIMIZE_WASM_URL } from "./optimize-protocol.js";
import { TOOLS_WASM_URL } from "./protocol.js";

describe("tools-protocol chunk routing", () => {
	it("routes exactly the subset-fonts and linearize cards to the optimise chunk", () => {
		const routed = TOOLS.filter((tool) => opsChunkForTool(tool.id) === "optimize").map(
			(tool) => tool.id,
		);
		expect(routed).toEqual(["subset_fonts", "linearize"]);
		expect([...OPTIMIZE_TOOL_IDS]).toEqual(routed);
	});

	it("routes exactly the compare card to the dedicated compare chunk", () => {
		const routed = TOOLS.filter((tool) => opsChunkForTool(tool.id) === "compare").map(
			(tool) => tool.id,
		);
		expect(routed).toEqual(["compare"]);
		expect([...COMPARE_TOOL_IDS]).toEqual(routed);
	});

	it("keeps every other card on the tools module", () => {
		for (const tool of TOOLS) {
			if (opsChunkForTool(tool.id) !== "tools") continue;
			expect(opsChunkForTool(tool.id), tool.slug).toBe("tools");
		}
		expect(TOOLS.filter((tool) => opsChunkForTool(tool.id) === "tools").length).toBe(
			TOOLS.length - OPTIMIZE_TOOL_IDS.length - COMPARE_TOOL_IDS.length,
		);
	});

	it("points each module at its own distinct same-origin URL", () => {
		expect(OPS_CHUNK_URL.tools).toBe(TOOLS_WASM_URL);
		expect(OPS_CHUNK_URL.optimize).toBe(OPTIMIZE_WASM_URL);
		expect(OPS_CHUNK_URL.compare).toBe(COMPARE_WASM_URL);
		const urls = Object.values(OPS_CHUNK_URL);
		expect(new Set(urls).size).toBe(urls.length);
		for (const url of urls) {
			expect(url.startsWith("/wasm/")).toBe(true);
		}
	});

	it("wires the decision into the page's run path, not just the test table", () => {
		// The pure function above is only useful if the click handler actually
		// calls it and runs the returned module. Pin that coupling here so a
		// future edit cannot leave a test-green page that still fetches the old
		// chunk for op 46.
		const page = readFileSync(new URL("./tools-page.ts", import.meta.url), "utf8");
		expect(page).toContain("const chunk = opsChunkForTool(tool.id)");
		expect(page).toContain("runOptimize(optimize, request)");
		expect(page).toContain("loadOptimize()");
		expect(page).toContain("runCompare(compareWasm, request)");
		expect(page).toContain("loadCompare()");
	});
});
