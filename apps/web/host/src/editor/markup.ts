/**
 * The markup: the marks a reader leaves on a document.
 *
 * The mark is a **standard PDF annotation** with a generated appearance
 * stream, written by the engine's annotation crate, so every other reader
 * sees it too (ADR-P0026). The editor only chooses where it goes and what
 * colour it is.
 */

/** The kinds of markup the editor offers. */
export type MarkupKind = "highlight" | "underline" | "strikeout" | "note";

/** The options the engine's annotate operation takes. */
export interface AnnotateOptions {
	/** The 0-based page the mark goes on. */
	readonly page: number;
	/** The kind of markup. */
	readonly kind: MarkupKind;
	/** The mark's rectangle, in the page's own user space. */
	readonly rect: readonly [number, number, number, number];
	/** The text lines the mark covers, as rectangles in user space. */
	readonly quads: readonly (readonly [number, number, number, number])[];
	/** The mark's colour, red, green and blue between 0 and 1. */
	readonly colour: readonly [number, number, number];
	/** The reader's comment. */
	readonly contents?: string;
	/** Who left the mark. */
	readonly author?: string;
}

/** One annotation, as the engine's report describes it. */
export interface PageAnnotation {
	/** The PDF subtype. */
	readonly subtype: string;
	/** The annotation's rectangle, in user space. */
	readonly rect: readonly [number, number, number, number];
	/** The annotation's colour. */
	readonly colour: readonly [number, number, number];
	/** The comment it carries, when it carries one. */
	readonly contents: string;
	/** Who left it, when it says. */
	readonly author: string;
	/** How many text lines it covers. */
	readonly quads: number;
}

/** The report the engine's annotations operation returns. */
export interface AnnotationsReport {
	/** The page the report is about. */
	readonly page: number;
	/** The annotations the page carries. */
	readonly annotations: readonly PageAnnotation[];
}

/** The colour each kind of markup wears. */
export const MARKUP_COLOURS: Readonly<Record<MarkupKind, readonly [number, number, number]>> = {
	highlight: [1, 0.92, 0.23],
	underline: [0.05, 0.05, 0.12],
	strikeout: [0.05, 0.05, 0.12],
	note: [1, 0.92, 0.23],
};

/**
 * Read the annotations out of the engine's report.
 *
 * The report is JSON the engine wrote, so it is parsed rather than trusted:
 * a row that is not the shape it claims is a row this reader cannot draw,
 * and it is dropped rather than shown broken.
 */
export function readAnnotationsReport(json: string): AnnotationsReport {
	let parsed: unknown;
	try {
		parsed = JSON.parse(json);
	} catch {
		return { page: 0, annotations: [] };
	}
	if (typeof parsed !== "object" || parsed === null) {
		return { page: 0, annotations: [] };
	}
	const record = parsed as Record<string, unknown>;
	const rows = Array.isArray(record.annotations) ? record.annotations : [];
	const annotations: PageAnnotation[] = [];
	for (const row of rows) {
		if (typeof row !== "object" || row === null) {
			continue;
		}
		const entry = row as Record<string, unknown>;
		const subtype = typeof entry.subtype === "string" ? entry.subtype : "";
		if (subtype === "") {
			continue;
		}
		annotations.push({
			subtype,
			rect: readRect(entry.rect),
			colour: readColour(entry.colour),
			contents: typeof entry.contents === "string" ? entry.contents : "",
			author: typeof entry.author === "string" ? entry.author : "",
			quads: typeof entry.quads === "number" ? entry.quads : 0,
		});
	}
	const page = typeof record.page === "number" ? record.page : 0;
	return { page, annotations };
}

/** A rectangle, or the origin when the report does not carry one. */
function readRect(value: unknown): [number, number, number, number] {
	if (!Array.isArray(value) || value.length < 4) {
		return [0, 0, 0, 0];
	}
	const out: [number, number, number, number] = [0, 0, 0, 0];
	for (let index = 0; index < 4; index += 1) {
		const part = value[index];
		out[index] = typeof part === "number" && Number.isFinite(part) ? part : 0;
	}
	return out;
}

/** A colour, or black when the report does not carry one. */
function readColour(value: unknown): [number, number, number] {
	if (!Array.isArray(value) || value.length < 3) {
		return [0, 0, 0];
	}
	const out: [number, number, number] = [0, 0, 0];
	for (let index = 0; index < 3; index += 1) {
		const part = value[index];
		out[index] = typeof part === "number" && Number.isFinite(part) ? part : 0;
	}
	return out;
}

/**
 * The label a mark is shown with.
 *
 * The subtype is the engine's own name for it, and the ones this editor
 * writes are the ones every reader knows.
 */
export function markupLabel(subtype: string): string {
	switch (subtype) {
		case "Highlight":
			return "Highlight";
		case "Underline":
			return "Underline";
		case "StrikeOut":
			return "Strike out";
		case "Text":
			return "Note";
		case "Ink":
			return "Drawn mark";
		case "Square":
			return "Box";
		default:
			return subtype === "" ? "Mark" : subtype;
	}
}
