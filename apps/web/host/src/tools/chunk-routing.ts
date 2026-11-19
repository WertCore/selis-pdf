/**
 * The one place that decides which tools-protocol WASM module a card runs.
 *
 * Most cards run through `buildRequest` -> `runTool` and therefore load the
 * tools chunk. Three of them run the same request/response protocol but live in
 * dedicated chunks: `subset_fonts` (op 41) and `linearize` (op 43) in the
 * optimise chunk, and `compare` (op 46, two inputs, JSON report output) in the
 * compare chunk. Every other card (convert, raster, HTML, text, security, sign)
 * branches to its own loader earlier in the run function and never reaches this
 * decision; the table below is deliberately about the tools-protocol modules
 * only.
 *
 * Keeping the decision here makes it testable without a DOM: the routing test
 * walks all 51 cards and asserts that exactly these ids select their dedicated
 * chunk, so a copy/paste edit to a card cannot silently send op 46 to the
 * engine-free tools chunk.
 */

import type { ToolId } from "../tool-registry.js";
import { COMPARE_WASM_URL } from "./compare-protocol.js";
import { OPTIMIZE_WASM_URL } from "./optimize-protocol.js";
import { TOOLS_WASM_URL } from "./protocol.js";

/** The interchangeable tools-protocol modules. */
export type OpsChunk = "tools" | "optimize" | "compare";

/** The card ids whose operations are exported by the optimise chunk. */
export const OPTIMIZE_TOOL_IDS: readonly ToolId[] = ["subset_fonts", "linearize"];

/** The card id whose two-input operation is exported by the compare chunk. */
export const COMPARE_TOOL_IDS: readonly ToolId[] = ["compare"];

/** Which tools-protocol module serves `id`. */
export function opsChunkForTool(id: ToolId): OpsChunk {
	if (COMPARE_TOOL_IDS.includes(id)) return "compare";
	return OPTIMIZE_TOOL_IDS.includes(id) ? "optimize" : "tools";
}

/** The URL a card's module is fetched from, for logging and routing tests. */
export const OPS_CHUNK_URL: Readonly<Record<OpsChunk, string>> = {
	tools: TOOLS_WASM_URL,
	optimize: OPTIMIZE_WASM_URL,
	compare: COMPARE_WASM_URL,
};
