import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { TOOLS } from "../tool-registry.js";
import {
	OPS_CHUNK_URL,
	OPTIMIZE_TOOL_IDS,
	REDACT_PATTERNS_TOOL_IDS,
	opsChunkForTool,
} from "./chunk-routing.js";
import { OPTIMIZE_WASM_URL } from "./optimize-protocol.js";
import { TOOLS_WASM_URL } from "./protocol.js";
import { REDACT_PATTERNS_WASM_URL } from "./redact-patterns-protocol.js";

describe("tools-protocol chunk routing", () => {
	it("routes exactly the subset-fonts and linearize cards to the optimise chunk", () => {
		const routed = TOOLS.filter((tool) => opsChunkForTool(tool.id) === "optimize").map(
			(tool) => tool.id,
		);
		expect(routed).toEqual(["subset_fonts", "linearize"]);
		expect([...OPTIMIZE_TOOL_IDS]).toEqual(routed);
	});

	it("routes exactly the redact-patterns card to its own chunk", () => {
		const routed = TOOLS.filter((tool) => opsChunkForTool(tool.id) === "redact_patterns").map(
			(tool) => tool.id,
		);
		expect(routed).toEqual(["redact_patterns"]);
		expect([...REDACT_PATTERNS_TOOL_IDS]).toEqual(routed);
	});

	it("keeps every other card on the tools module", () => {
		for (const tool of TOOLS) {
			if (OPTIMIZE_TOOL_IDS.includes(tool.id)) continue;
			if (REDACT_PATTERNS_TOOL_IDS.includes(tool.id)) continue;
			expect(opsChunkForTool(tool.id), tool.slug).toBe("tools");
		}
	});

	it("points each module at its own distinct same-origin URL", () => {
		expect(OPS_CHUNK_URL.tools).toBe(TOOLS_WASM_URL);
		expect(OPS_CHUNK_URL.optimize).toBe(OPTIMIZE_WASM_URL);
		expect(OPS_CHUNK_URL.redact_patterns).toBe(REDACT_PATTERNS_WASM_URL);
		const urls = Object.values(OPS_CHUNK_URL);
		expect(new Set(urls).size).toBe(urls.length);
		for (const url of urls) expect(url.startsWith("/wasm/")).toBe(true);
	});

	it("wires the decision into the page's run path, not just the test table", () => {
		// The pure function above is only useful if the click handler actually
		// calls it and runs the returned module. Pin that coupling here so a
		// future edit cannot leave a test-green page that still fetches the old
		// chunk for ops 41, 43 and 47.
		const page = readFileSync(new URL("./tools-page.ts", import.meta.url), "utf8");
		expect(page).toContain("const chunk = opsChunkForTool(tool.id)");
		expect(page).toContain("runOptimize(optimize, request)");
		expect(page).toContain("loadOptimize()");
		expect(page).toContain('chunk === "redact_patterns"');
		expect(page).toContain("runRedactPatterns(");
		expect(page).toContain("loadRedactPatterns()");
	});
});
