/**
 * The one place that decides which tools-protocol WASM module a card runs.
 *
 * Most cards run through `buildRequest` -> `runTool` and therefore load the
 * tools chunk. Two of them — `subset_fonts` (op 41) and `linearize` (op 43) —
 * run the same request/response protocol but live in the dedicated optimise
 * chunk, so they load that module instead. Every other card (convert, raster,
 * HTML, text, security, sign) branches to its own loader earlier in the run
 * function and never reaches this decision; the table below is deliberately
 * about the two interchangable tools-protocol modules only.
 *
 * Keeping the decision here makes it testable without a DOM: the routing test
 * walks all 50 cards and asserts that exactly these two ids select `optimize`,
 * so a copy/paste edit to a card cannot silently send op 41 to the tools chunk.
 */

import type { ToolId } from "../tool-registry.js";
import { OPTIMIZE_WASM_URL } from "./optimize-protocol.js";
import { TOOLS_WASM_URL } from "./protocol.js";

/** The two interchangable tools-protocol modules. */
export type OpsChunk = "tools" | "optimize";

/** The card ids whose operations are exported by the optimise chunk. */
export const OPTIMIZE_TOOL_IDS: readonly ToolId[] = ["subset_fonts", "linearize"];

/** Which tools-protocol module serves `id`. */
export function opsChunkForTool(id: ToolId): OpsChunk {
	return OPTIMIZE_TOOL_IDS.includes(id) ? "optimize" : "tools";
}

/** The URL a card's module is fetched from, for logging and routing tests. */
export const OPS_CHUNK_URL: Readonly<Record<OpsChunk, string>> = {
	tools: TOOLS_WASM_URL,
	optimize: OPTIMIZE_WASM_URL,
};
