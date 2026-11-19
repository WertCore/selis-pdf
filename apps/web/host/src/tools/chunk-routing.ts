/**
 * The one place that decides which tools-protocol WASM module a card runs.
 *
 * Most cards run through `buildRequest` -> `runTool` and therefore load the
 * tools chunk. Two of them — `subset_fonts` (op 41) and `linearize` (op 43) —
 * run the same request/response protocol but live in the dedicated optimise
 * chunk, and `redact_patterns` (op 47) lives in its own chunk because the
 * pattern scan needs the engine's text ladder. Every other card (convert,
 * raster, HTML, text, security, sign) branches to its own loader earlier in
 * the run function and never reaches this decision; the table below is only
 * about the interchangeable tools-protocol modules.
 *
 * Keeping the decision here makes it testable without a DOM: the routing test
 * walks every card and asserts that exactly these three ids select their own
 * module, so a copy/paste edit to a card cannot silently send op 47 to the
 * tools chunk. That matters for redaction in particular: the tools chunk
 * cannot dispatch op 47 at all, and the redact chunk refuses every other op.
 */

import type { ToolId } from "../tool-registry.js";
import { OPTIMIZE_WASM_URL } from "./optimize-protocol.js";
import { TOOLS_WASM_URL } from "./protocol.js";
import { REDACT_PATTERNS_WASM_URL } from "./redact-patterns-protocol.js";

/** The interchangeable tools-protocol modules. */
export type OpsChunk = "tools" | "optimize" | "redact_patterns";

/** The card ids whose operations are exported by the optimise chunk. */
export const OPTIMIZE_TOOL_IDS: readonly ToolId[] = ["subset_fonts", "linearize"];

/** The card whose scan/apply operations are exported by the redact chunk. */
export const REDACT_PATTERNS_TOOL_IDS: readonly ToolId[] = ["redact_patterns"];

/** Which tools-protocol module serves `id`. */
export function opsChunkForTool(id: ToolId): OpsChunk {
	if (REDACT_PATTERNS_TOOL_IDS.includes(id)) return "redact_patterns";
	return OPTIMIZE_TOOL_IDS.includes(id) ? "optimize" : "tools";
}

/** The URL a card's module is fetched from, for logging and routing tests. */
export const OPS_CHUNK_URL: Readonly<Record<OpsChunk, string>> = {
	tools: TOOLS_WASM_URL,
	optimize: OPTIMIZE_WASM_URL,
	redact_patterns: REDACT_PATTERNS_WASM_URL,
};
