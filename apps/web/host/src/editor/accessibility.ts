/**
 * The accessibility report: what the document says about itself.
 *
 * The engine can READ a document's tag structure but it cannot write one,
 * so this is a report and not a repair. It answers the questions a reader
 * with a screen reader - or a reader checking the file before sending it -
 * actually asks:
 *
 *   - is the document tagged at all?
 *   - does it have headings, and how many?
 *   - are its images described, and which ones are not?
 *
 * The rule this module follows is the engine's: **nothing is invented**. A
 * document with no tag tree is untagged, not "a document with no headings",
 * and an image with no alternative text is listed as one rather than
 * quietly counted as fine.
 */

import type { Structure, StructureNode } from "./engine.js";

/** What the report found. */
export interface AccessibilityReport {
	/** Whether the document carries a tag structure at all. */
	readonly tagged: boolean;
	/** How many headings the structure carries. */
	readonly headings: number;
	/** How many images the structure carries. */
	readonly images: number;
	/** How many of those images have no alternative text. */
	readonly imagesWithoutAlt: number;
	/** How many links the structure carries. */
	readonly links: number;
	/** The images that have no alternative text, named where they can be. */
	readonly undescribed: readonly string[];
}

/** The roles that count as a heading. */
const HEADING_ROLES = new Set(["H1", "H2", "H3", "H4", "H5", "H6", "H", "TITLE"]);

/** The roles that count as a link. */
const LINK_ROLES = new Set(["LINK"]);

/**
 * Walk the structure and count what a reader would need.
 *
 * The walk is depth-bounded for the same reason the engine's is: the tree
 * comes from an untrusted document.
 */
export function reportStructure(structure: Structure): AccessibilityReport {
	if (!structure.tagged || structure.root === undefined) {
		return {
			tagged: false,
			headings: 0,
			images: 0,
			imagesWithoutAlt: 0,
			links: 0,
			undescribed: [],
		};
	}
	const counts = {
		headings: 0,
		images: 0,
		imagesWithoutAlt: 0,
		links: 0,
		undescribed: [] as string[],
	};
	walk(structure.root, counts, 0);
	return { tagged: true, ...counts };
}

/** One node and its children. */
function walk(
	node: StructureNode,
	counts: {
		headings: number;
		images: number;
		imagesWithoutAlt: number;
		links: number;
		undescribed: string[];
	},
	depth: number,
): void {
	if (depth > 64) {
		return;
	}
	const role = node.role.trim().toUpperCase();
	if (HEADING_ROLES.has(role)) {
		counts.headings += 1;
	}
	if (LINK_ROLES.has(role)) {
		counts.links += 1;
	}
	if (role === "FIGURE" || role === "IMAGE") {
		counts.images += 1;
		const alt = node.alt?.trim() ?? "";
		if (alt === "") {
			counts.imagesWithoutAlt += 1;
			counts.undescribed.push(node.title?.trim() ?? "");
		}
	}
	for (const child of node.children) {
		walk(child, counts, depth + 1);
	}
}
