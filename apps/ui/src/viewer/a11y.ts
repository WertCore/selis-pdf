/**
 * What the viewer exposes to assistive technology (SL-4.UI.07).
 *
 * ## The one rule everything else follows
 *
 * **This module may only report what the document actually contains.** Every
 * function here is capable of returning "I do not know", and that answer is
 * preferred over a plausible one. The reason is specific: a PDF viewer can
 * produce a confident, fluent, entirely invented structure tree, and a
 * screen-reader user cannot tell the difference between one and the real thing.
 * A blank page announced as "Blank page" is annoying; an invented heading
 * hierarchy is actively harmful, because it tells a blind reader the document
 * *means* something it does not.
 *
 * So the three ways to fabricate, all of which are conventional in this
 * industry and all of which are refused here:
 *
 *  - **Guessing headings from font size.** A big font is not a heading. If the
 *    document has no `/StructTreeRoot`, {@link structureOf} returns `null` and
 *    the shell says the document is untagged rather than inventing `/H1`s.
 *  - **Inventing alt text.** A figure with no `/Alt` gets the host's fallback
 *    word and a `needsDescription` flag - never a sentence about what the
 *    picture appears to show.
 *  - **Dropping an unmapped custom role to a bare `div`.** PDF's `/RoleMap` maps
 *    document roles onto standard ones; a role the map does not cover keeps its
 *    name and is marked {@link Semantics.unknown}, so a reader hears that there
 *    is something there rather than silence.
 *
 * ## Shape, mirroring the engine
 *
 * The inputs mirror `selis_pdf_doc::struct_tree::{StructTree, StructElement}`
 * as `selis-pdf-wasm` will expose them: a role string, an optional title, an
 * optional `/Alt`, and children. Nothing here reads a file or touches a DOM, so
 * the whole model is testable without a browser (ADR-P0044).
 *
 * @see SL-4.UI.07
 */

/**
 * A document structure role, as the PDF spec names it.
 *
 * Deliberately the spec's names rather than ARIA's, because this is what the
 * document says and the mapping to ARIA is a separate, visible step. Collapsing
 * the two is how a custom role silently acquires meaning it never had.
 */
export type StructureRole =
	| "Document"
	| "Part"
	| "Sect"
	| "Div"
	| "P"
	| "H1"
	| "H2"
	| "H3"
	| "H4"
	| "H5"
	| "H6"
	| "Figure"
	| "Table"
	| "TR"
	| "TH"
	| "TD"
	| "Link"
	| "List"
	| "LI"
	| "LBody"
	| "Lbl"
	| "Span"
	| "Quote"
	| "Note"
	| "Artifact"
	| (string & {});

/**
 * One node of the structure tree, before roles are mapped to ARIA.
 *
 * `alt` and `title` are document text, so a caller that renders them must
 * already have decided it is allowed to (ADR-P0017 forbids document bytes in
 * diagnostics and telemetry, not in the viewer's own UI).
 */
export interface StructureNode {
	readonly role: StructureRole;
	readonly title?: string;
	readonly alt?: string;
	readonly children: readonly StructureNode[];
}

/**
 * What the shell must expose for one structure node.
 *
 * Everything the DOM writer needs and nothing it has to decide - the same
 * division `error-panel.ts` and `progress.ts` use.
 */
export interface Semantics {
	/** The ARIA role, or `null` where a real element is better than a role. */
	readonly role: string | null;
	/** The HTML element to use, when it is more meaningful than a role. */
	readonly element: string;
	/** The accessible name, or `null` when the document supplies none. */
	readonly name: string | null;
	/** Heading level 1-6, or `null` for anything that is not a heading. */
	readonly level: number | null;
	/**
	 * True when this node's role came from neither the PDF spec nor `/RoleMap`.
	 *
	 * Kept visible rather than mapped to something generic: a reader is better
	 * served by an honest "Custom role: Author" than by a heading the document
	 * never claimed.
	 */
	readonly unknown: boolean;
	/**
	 * True when this is a figure with no `/Alt`.
	 *
	 * The shell must say SOMETHING - an unlabelled `role="figure"` is announced
	 * as "figure" and nothing else - but the something is a host word, and this
	 * flag is how the shell knows to also offer to edit a description.
	 */
	readonly needsDescription: boolean;
	/** Child semantics, in the document's own order. */
	readonly children: readonly Semantics[];
}

/** The roles the PDF spec defines, which map without needing `/RoleMap`. */
const KNOWN_ROLES = new Set<string>([
	"Document",
	"Part",
	"Sect",
	"Div",
	"P",
	"H1",
	"H2",
	"H3",
	"H4",
	"H5",
	"H6",
	"Figure",
	"Table",
	"TR",
	"TH",
	"TD",
	"Link",
	"List",
	"LI",
	"LBody",
	"Lbl",
	"Span",
	"Quote",
	"Note",
	"Artifact",
]);

/**
 * `/RoleMap` defaults, for roles the spec names but this table does not list.
 *
 * Kept as data rather than code so the mapping is auditable in one place. A
 * document's OWN `/RoleMap` is consulted first - the DOCUMENT decides how its
 * roles map, not this module and not the viewer.
 */
const DEFAULT_ROLE_MAP: Readonly<Record<string, string>> = {
	Artifact: "Artifact",
	Document: "Document",
	Div: "Div",
	Figure: "Figure",
	H: "H1",
	Heading: "H1",
	Link: "Link",
	List: "List",
	P: "P",
	Paragraph: "P",
	Part: "Part",
	Quote: "Quote",
	Sect: "Sect",
	Section: "Sect",
	Table: "Table",
	TD: "TD",
	TH: "TH",
	TR: "TR",
};

/** ARIA role and element for each known document role. */
interface Mapped {
	readonly role: string | null;
	readonly element: string;
}

function mapKnown(role: string): Mapped | null {
	if (/^H[1-6]$/.test(role)) return { role: null, element: `h${role.slice(1)}` };
	switch (role) {
		case "Document":
			return { role: "document", element: "div" };
		case "Part":
		case "Sect":
		case "Div":
			return { role: "section", element: "section" };
		case "P":
			return { role: null, element: "p" };
		case "Figure":
			return { role: "figure", element: "figure" };
		case "Table":
			return { role: "table", element: "table" };
		case "TR":
			return { role: "row", element: "tr" };
		case "TH":
			return { role: "columnheader", element: "th" };
		case "TD":
			return { role: "cell", element: "td" };
		case "Link":
			return { role: "link", element: "a" };
		case "List":
			return { role: "list", element: "ul" };
		case "LI":
			return { role: "listitem", element: "li" };
		case "LBody":
			return { role: null, element: "div" };
		case "Lbl":
			return { role: null, element: "span" };
		case "Span":
			return { role: null, element: "span" };
		case "Quote":
			return { role: "blockquote", element: "blockquote" };
		case "Note":
			return { role: "note", element: "aside" };
		case "Artifact":
			// Presentational BY DESIGN. An artifact is content the author marked
			// as decoration, so exposing it announces exactly what the author
			// asked to be skipped.
			return { role: null, element: "span" };
		default:
			return null;
	}
}

/**
 * The heading level, and only ever from the ROLE.
 *
 * Not from nesting depth. Depth-derived levels are a well-known bug: a heading
 * inside a table cell would become an `<h3>` and appear in the document's
 * outline navigation, which is a claim about the document's organisation that
 * the document never made.
 */
function headingLevel(role: string): number | null {
	const match = /^H([1-6])$/.exec(role);
	return match === null ? null : Number(match[1]);
}

function titleOrAlt(node: StructureNode, role: string): string | null {
	if (role === "Figure") {
		const alt = node.alt?.trim();
		if (alt !== undefined && alt !== "") return alt;
		const title = node.title?.trim();
		if (title !== undefined && title !== "") return title;
		return null;
	}
	const title = node.title?.trim();
	if (title !== undefined && title !== "") return title;
	// A heading whose text is not in `/T` has its text in the marked content,
	// which this layer has not been given. Reporting no name is honest;
	// reporting an empty one is a blank heading a reader tries to hear three
	// times.
	return null;
}

/**
 * Map one structure node to what the shell must expose.
 *
 * Resolution order, and the order matters:
 *
 *  1. the document's own `/RoleMap`, because the DOCUMENT decides how its roles
 *     map - not this module, and not the viewer;
 *  2. a role this module knows;
 *  3. anything else, kept and flagged `unknown`.
 *
 * Step 3 is the one a tidy-up would delete. Deleting it is not a simplification:
 * it turns "this document uses a custom role called `Chart`" into silence, and
 * a reader is better served by an honest unknown than by a node that is not
 * there at all.
 */
export function semanticsOf(
	node: StructureNode,
	roleMap: Readonly<Record<string, string>> = {},
): Semantics {
	const mappedRole = roleMap[node.role] ?? DEFAULT_ROLE_MAP[node.role] ?? node.role;
	const known = KNOWN_ROLES.has(mappedRole);
	const shape = known ? mapKnown(mappedRole) : null;
	const alt = node.alt?.trim() ?? null;

	return {
		role: shape?.role ?? (known ? null : "group"),
		element: shape?.element ?? "div",
		name: titleOrAlt(node, mappedRole),
		level: headingLevel(mappedRole),
		unknown: !known,
		// Only a FIGURE needs a description, and only when the document gave it
		// none. Flagging every nameless node would be noise the reader has to
		// listen past.
		needsDescription: mappedRole === "Figure" && (alt === null || alt === ""),
		children: node.children.map((child) => semanticsOf(child, roleMap)),
	};
}

/**
 * The heading outline a screen reader's rotor would show.
 *
 * Flattened, in document order, because that is what a reader navigating by
 * heading consumes. A document with no headings returns `[]`, which is a TRUE
 * answer - not an error, and not a reason to invent any.
 */
export function outlineOf(roots: readonly StructureNode[]): Semantics[] {
	const found: Semantics[] = [];
	const walk = (nodes: readonly StructureNode[]): void => {
		for (const node of nodes) {
			const semantics = semanticsOf(node);
			if (semantics.level !== null) found.push(semantics);
			// Artifacts are skipped, and so is their subtree: an artifact is
			// content the author marked as decoration, and a heading inside one is
			// decoration too.
			if (node.role !== "Artifact") walk(node.children);
		}
	};
	walk(roots);
	return found;
}

/**
 * The structure for a document, or `null` when it has none.
 *
 * `null` is the answer for an UNTAGGED document, and it is the point of this
 * module. The Do asks for the structure tree to be exposed to AT; an untagged
 * document has no structure tree to expose, and the honest rendering of that is
 * "this document has no structure information" - not a heading hierarchy
 * reconstructed from font sizes, which is the convention this module exists to
 * refuse.
 */
export function structureOf(
	roots: readonly StructureNode[] | null | undefined,
): StructureNode[] | null {
	if (roots === null || roots === undefined) return null;
	if (!Array.isArray(roots)) return null;
	return [...roots];
}

/**
 * True when the document carries structure a reader can navigate.
 *
 * Used by the shell to choose between "here is your document's outline" and
 * "this document is untagged". An empty array of roots is FALSE: a document can
 * have a `/StructTreeRoot` with nothing navigable in it, and that is still a
 * document a reader cannot navigate by heading.
 */
export function hasStructure(roots: readonly StructureNode[] | null | undefined): boolean {
	const structure = structureOf(roots);
	if (structure === null) return false;
	return outlineOf(structure).length > 0;
}

/**
 * A one-line, honest statement of what this document offers a reader.
 *
 * The shell needs one sentence to put in a status line, and the temptation is
 * to write "12 headings, 3 figures" out of a count that was never measured.
 * Every number here comes from the structure, and a document with no structure
 * gets the untagged sentence instead of a count of zero.
 */
export function structureSummary(roots: readonly StructureNode[] | null | undefined): {
	tagged: boolean;
	headings: number;
	figures: number;
	needsDescription: number;
} {
	const structure = structureOf(roots);
	if (structure === null) return { tagged: false, headings: 0, figures: 0, needsDescription: 0 };
	let headings = 0;
	let figures = 0;
	let needsDescription = 0;
	const walk = (nodes: readonly StructureNode[]): void => {
		for (const node of nodes) {
			const semantics = semanticsOf(node);
			if (semantics.level !== null) headings += 1;
			if (semantics.element === "figure") figures += 1;
			if (semantics.needsDescription) needsDescription += 1;
			if (node.role !== "Artifact") walk(node.children);
		}
	};
	walk(structure);
	return { tagged: true, headings, figures, needsDescription };
}
