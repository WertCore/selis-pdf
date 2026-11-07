/**
 * The tools registry: one list that the page, the static-page generator, the
 * card links, the canonical URLs and the service worker all read.
 *
 * This module is deliberately DOM-free. `tools-page.ts` imports it to render
 * the grid and the workspace; `tools/build-pages.mjs` imports the compiled
 * `dist/tool-registry.js` to write one static page per tool; `sw-policy.ts`
 * imports it to precache those same pages. There is no second table to drift:
 * a slug added here is a page, a card link and a cached navigation in the same
 * commit, and the tests in `tool-registry.test.ts` fail if one of those views
 * could disagree.
 *
 * A slug is public and frozen. The in-app `id` (`clear_permissions`) is not the
 * slug (`clear-pdf-permissions`); renaming a slug after indexing costs a
 * redirect this static host cannot serve, so the list is checked once, in one
 * place, and the checks are part of the build.
 */

/** Every card the page can open: a tools-chunk op or a lazy-chunk tool. */
export type ToolId =
	| "merge"
	| "mix"
	| "split"
	| "rotate"
	| "crop"
	| "flip"
	| "page_labels"
	| "resize"
	| "nup"
	| "delete"
	| "reorder"
	| "metadata_report"
	| "fingerprint"
	| "privacy"
	| "metadata"
	| "strip_metadata"
	| "page_numbers"
	| "watermark"
	| "image_watermark"
	| "outline"
	| "header_footer"
	| "bates"
	| "compress"
	| "attachments"
	| "attach"
	| "detach"
	| "repair"
	| "batch"
	| "image_to_pdf"
	| "convert"
	| "png"
	| "jpg"
	| "tiff"
	| "webp"
	| "html"
	| "text"
	| "zip"
	| "fill_forms"
	| "unlock"
	| "protect"
	| "clear_permissions"
	| "permissions"
	| "sign"
	| "certificate"
	| "workflow";

/**
 * The sections the card grid is organised into. Forty-three cards in one flat
 * list is not a menu, it is a wall: the groups are named after what a reader
 * has come to *do*, in the order the operations build on each other — arrange
 * the pages, change what they are, mark them up, then decide who may open
 * them. `TOOL_GROUPS` is the display order; a card with no group is a bug, so
 * the type makes the field required rather than optional.
 */
export type ToolGroup =
	| "organise"
	| "convert"
	| "stamp"
	| "metadata"
	| "security"
	| "attach"
	| "repair"
	| "bundle";

export interface ToolGroupDef {
	readonly id: ToolGroup;
	readonly label: string;
	readonly note: string;
}

export const TOOL_GROUPS: readonly ToolGroupDef[] = [
	{ id: "organise", label: "Organise pages", note: "Put pages in the order you want them." },
	{
		id: "convert",
		label: "Convert",
		note: "Change a PDF into another format, or another format into a PDF.",
	},
	{
		id: "stamp",
		label: "Stamp & annotate",
		note: "Write numbers, marks and bookmarks onto the pages.",
	},
	{
		id: "metadata",
		label: "Metadata",
		note: "Read the document’s own description, edit it, or remove it.",
	},
	{
		id: "security",
		label: "Security & signatures",
		note: "Passwords, permissions, forms and signatures.",
	},
	{ id: "attach", label: "Attachments", note: "Files carried inside the document." },
	{ id: "repair", label: "Optimise & repair", note: "Make it smaller, or make it open again." },
	{
		id: "bundle",
		label: "Batch & archive",
		note: "Run one tool over many files, or collect them into a ZIP.",
	},
];

export interface ToolDef {
	readonly id: ToolId;
	/** The frozen public path segment: `/merge-pdf/`. */
	readonly slug: string;
	readonly group: ToolGroup;
	readonly mark: string;
	readonly title: string;
	readonly blurb: string;
	/**
	 * The honest one-line note the generated page carries below its lead.
	 * Distinct from the option note inside the workspace: this one explains
	 * the operation to a reader (and a search result), the option note
	 * explains a control.
	 */
	readonly note: string;
	readonly verb: string;
	readonly multiple: boolean;
	/** The fewest files a run needs; defaults to 2 for multi-file tools. */
	readonly minimum?: number;
	/**
	 * The input containers this tool accepts; defaults to PDF. `pdf-image`
	 * takes exactly one PDF plus one PNG/JPEG watermark image.
	 */
	readonly accepts?: "pdf" | "image" | "text" | "pdf-image";
	/** The tools a reader is most likely to confuse this one with. */
	readonly related?: readonly ToolId[];
}

export const TOOLS: readonly ToolDef[] = [
	{
		id: "merge",
		slug: "merge-pdf",
		group: "organise",
		mark: "⇄",
		title: "Merge PDF",
		blurb: "Combine several PDFs into one, in the order you choose.",
		note: "Merge whole documents, in the order you pick them. To interleave pages from two documents instead, use Mix pages.",
		verb: "Merge PDFs",
		multiple: true,
		related: ["mix"],
	},
	{
		id: "mix",
		slug: "mix-pdf-pages",
		group: "organise",
		mark: "⇆",
		title: "Mix pages",
		blurb: "Interleave pages from two or more PDFs, round-robin, into one document.",
		note: "Mix interleaves pages — page 1 of each file, then page 2 — where Merge appends whole documents. Use Mix for two scans of the same document.",
		verb: "Mix pages",
		multiple: true,
		related: ["merge"],
	},
	{
		id: "split",
		slug: "split-pdf",
		group: "organise",
		mark: "✂",
		title: "Split PDF",
		blurb: "Split by range, every N pages, in half, by size, by bookmarks, or extract pages.",
		note: "One document in; the pieces you asked for out, each verified before it downloads.",
		verb: "Split PDF",
		multiple: false,
	},
	{
		id: "rotate",
		slug: "rotate-pdf",
		group: "organise",
		mark: "↻",
		title: "Rotate PDF",
		blurb: "Turn all pages or a selection by 90, 180, or 270 degrees.",
		note: "Rotation is written into the page tree, so every reader shows the same orientation.",
		verb: "Rotate PDF",
		multiple: false,
	},
	{
		id: "crop",
		slug: "crop-pdf",
		group: "organise",
		mark: "⌗",
		title: "Crop PDF",
		blurb: "Trim page edges by points, measured in the page's displayed orientation.",
		note: "Margins are measured on the page as displayed, so a rotated page crops where you expect.",
		verb: "Crop PDF",
		multiple: false,
	},
	{
		id: "flip",
		slug: "flip-pdf",
		group: "organise",
		mark: "⇋",
		title: "Flip PDF",
		blurb: "Mirror page content horizontally or vertically.",
		note: "Pages are mirrored in place; text selection and links keep working.",
		verb: "Flip PDF",
		multiple: false,
	},
	{
		id: "page_labels",
		slug: "page-labels",
		group: "organise",
		mark: "L",
		title: "Page labels",
		blurb: "Set the numbering readers show: decimal, roman, letters, or a prefix.",
		note: "Labels change what a reader's page counter shows, not the document's page order.",
		verb: "Apply page labels",
		multiple: false,
	},
	{
		id: "resize",
		slug: "resize-pdf-pages",
		group: "organise",
		mark: "⤢",
		title: "Resize pages",
		blurb: "Scale pages to A4, Letter, or a custom size in points.",
		note: "Choose a standard size or exact points; content is scaled, not re-rendered.",
		verb: "Resize pages",
		multiple: false,
	},
	{
		id: "nup",
		slug: "n-up-pdf",
		group: "organise",
		mark: "▦",
		title: "N-up pages",
		blurb: "Place 2, 4, 6, or 9 pages on each sheet.",
		note: "Several pages are placed on each sheet without rasterising them.",
		verb: "Place pages",
		multiple: false,
	},
	{
		id: "delete",
		slug: "delete-pdf-pages",
		group: "organise",
		mark: "⌫",
		title: "Delete pages",
		blurb: "Remove the pages you no longer need.",
		note: "Removed pages leave the file; the remaining pages keep their content and links.",
		verb: "Delete pages",
		multiple: false,
	},
	{
		id: "reorder",
		slug: "organize-pdf-pages",
		group: "organise",
		mark: "⇅",
		title: "Organize pages",
		blurb: "Reorder pages, duplicate pages, or insert blank pages.",
		note: "Reorder, duplicate or insert blanks — the document is rebuilt and checked.",
		verb: "Organize pages",
		multiple: false,
	},
	{
		id: "metadata_report",
		slug: "read-pdf-metadata",
		group: "metadata",
		mark: "ℹ",
		title: "Read metadata",
		blurb: "Export the Info dictionary, XMP packet, and structure as JSON.",
		note: "A read-only report; the document itself is not modified.",
		verb: "Read metadata",
		multiple: false,
	},
	{
		id: "fingerprint",
		slug: "pdf-fingerprint",
		group: "metadata",
		mark: "❖",
		title: "Fingerprint",
		blurb: "Report identifiers, authoring trails, fonts, and image hashes as JSON.",
		note: "A read-only report; the document itself is not modified. Two files from the same source share a fingerprint.",
		verb: "Fingerprint",
		multiple: false,
	},
	{
		id: "privacy",
		slug: "pdf-privacy-scan",
		group: "metadata",
		mark: "⚠",
		title: "Privacy scan",
		blurb: "Find revisions, metadata trails, hidden layers, embedded files, JavaScript, and external links.",
		note: "A read-only report; nothing is changed or removed. Review each risk yourself before sharing the file.",
		verb: "Scan for risks",
		multiple: false,
	},
	{
		id: "metadata",
		slug: "edit-pdf-metadata",
		group: "metadata",
		mark: "Aa",
		title: "Edit metadata",
		blurb: "Set the title, author, subject, and keywords.",
		note: "Only the four Info fields change; the rest of the document is copied as-is.",
		verb: "Save metadata",
		multiple: false,
	},
	{
		id: "strip_metadata",
		slug: "remove-pdf-metadata",
		group: "metadata",
		mark: "×",
		title: "Remove metadata",
		blurb: "Strip the Info dictionary and the XMP packet from the document.",
		note: "Removes the Info dictionary and the XMP packet, and says what that costs before you run it.",
		verb: "Remove metadata",
		multiple: false,
	},
	{
		id: "page_numbers",
		slug: "add-page-numbers",
		group: "stamp",
		mark: "1",
		title: "Add page numbers",
		blurb: "Stamp page numbers on every page or a selection.",
		note: "Numbers are drawn as a text overlay, so they stay sharp at any zoom.",
		verb: "Add page numbers",
		multiple: false,
	},
	{
		id: "watermark",
		slug: "add-watermark",
		group: "stamp",
		mark: "W",
		title: "Watermark PDF",
		blurb: "Add a translucent diagonal or horizontal text watermark.",
		note: "Text is drawn over the page with transparency; the underlying content is untouched.",
		verb: "Add watermark",
		multiple: false,
	},
	{
		id: "image_watermark",
		slug: "add-image-watermark",
		group: "stamp",
		mark: "IW",
		title: "Image watermark",
		blurb: "Stamp a PNG or JPEG logo or seal across the pages you choose.",
		note: "A PNG or JPEG is stamped across the pages you select.",
		verb: "Add image watermark",
		multiple: true,
		minimum: 2,
		accepts: "pdf-image",
	},
	{
		id: "outline",
		slug: "edit-pdf-bookmarks",
		group: "stamp",
		mark: "BM",
		title: "Edit bookmarks",
		blurb: "Add, rename, retarget, or remove the document's bookmarks.",
		note: "Bookmarks are document structure, not page content; readers show them in their sidebar.",
		verb: "Save bookmarks",
		multiple: false,
	},
	{
		id: "header_footer",
		slug: "add-header-footer",
		group: "stamp",
		mark: "HF",
		title: "Header & footer",
		blurb: "Add header and footer lines with page placeholders.",
		note: "Use {page} and {pages} in any slot; they are filled per page.",
		verb: "Add header & footer",
		multiple: false,
	},
	{
		id: "bates",
		slug: "bates-numbering",
		group: "stamp",
		mark: "B",
		title: "Bates numbering",
		blurb: "Apply sequential legal-style numbers with a prefix and padding.",
		note: "A prefix, a start number and zero padding, advancing only over the pages you chose.",
		verb: "Add Bates numbers",
		multiple: false,
	},
	{
		id: "compress",
		slug: "compress-pdf",
		group: "repair",
		mark: "Z",
		title: "Compress PDF",
		blurb:
			"Compress PDF in three lossy levels with an optional target size. Images re-encoded, text kept. No watermark, no daily limit — files never leave your device.",
		note: "Choose Less, Recommended or Extreme and an optional target size in KiB. Images are re-encoded; text and structure stay intact, and the result is never larger than the input.",
		verb: "Compress PDF",
		multiple: false,
	},
	{
		id: "attachments",
		slug: "extract-pdf-attachments",
		group: "attach",
		mark: "A",
		title: "Extract attachments",
		blurb: "Download the files embedded inside a PDF, individually or as a ZIP.",
		note: "Each embedded file downloads as itself; two or more also arrive as one ZIP.",
		verb: "Extract attachments",
		multiple: false,
	},
	{
		id: "attach",
		slug: "embed-files-in-pdf",
		group: "attach",
		mark: "＋",
		title: "Embed files",
		blurb: "Attach files inside a PDF so they travel with it.",
		note: "The first file is the PDF; every later file is stored inside it.",
		verb: "Embed files",
		multiple: true,
	},
	{
		id: "detach",
		slug: "remove-pdf-attachments",
		group: "attach",
		mark: "－",
		title: "Remove attachments",
		blurb: "Delete every file embedded inside a PDF.",
		note: "Every embedded file is removed and the superseded objects are pruned.",
		verb: "Remove attachments",
		multiple: false,
	},
	{
		id: "repair",
		slug: "repair-pdf",
		group: "repair",
		mark: "R",
		title: "Repair PDF",
		blurb: "Recover the pages of a damaged PDF into a clean, consistent file.",
		note: "Tries the strict parser, the last intact revision, then a rebuilt index, and reports which worked.",
		verb: "Repair PDF",
		multiple: false,
	},
	{
		id: "batch",
		slug: "batch-pdf",
		group: "bundle",
		mark: "≡",
		title: "Batch queue",
		blurb: "Run one tool over several PDFs; each file gets its own verified result.",
		note: "One operation over many files; a file the engine refuses is reported without stopping the queue.",
		verb: "Run the queue",
		multiple: true,
		minimum: 2,
		accepts: "pdf",
	},
	{
		id: "image_to_pdf",
		slug: "image-to-pdf",
		group: "convert",
		mark: "▣",
		title: "Image to PDF",
		blurb: "Turn PNG and JPEG images into a PDF, one page per image.",
		note: "PNG and JPEG become one page each, scaled to fit the page size you choose.",
		verb: "Create PDF",
		multiple: true,
		minimum: 1,
		accepts: "image",
	},
	{
		id: "convert",
		slug: "convert-to-pdf",
		group: "convert",
		mark: "▾",
		title: "Convert to PDF",
		blurb: "Turn Markdown, HTML, CSV, LaTeX, SVG, EPUB, Word, PowerPoint or Excel into a PDF.",
		note: "Markdown, HTML, CSV, LaTeX, SVG, EPUB, Word, PowerPoint and Excel are rendered locally.",
		verb: "Convert to PDF",
		multiple: false,
		accepts: "text",
	},
	{
		id: "png",
		slug: "pdf-to-png",
		group: "convert",
		mark: "▧",
		title: "PDF to PNG",
		blurb: "Render every page, or a selection, to lossless PNG images.",
		note: "Lossless page renders at the DPI you choose; several pages arrive as one ZIP.",
		verb: "Render PNGs",
		multiple: false,
		accepts: "pdf",
		related: ["jpg"],
	},
	{
		id: "jpg",
		slug: "pdf-to-jpg",
		group: "convert",
		mark: "▨",
		title: "PDF to JPG",
		blurb: "Render every page, or a selection, to compact JPEG images.",
		note: "Baseline JPEG at the quality you choose; several pages arrive as one ZIP.",
		verb: "Render JPGs",
		multiple: false,
		accepts: "pdf",
		related: ["png"],
	},
	{
		id: "tiff",
		slug: "pdf-to-tiff",
		group: "convert",
		mark: "▤",
		title: "PDF to TIFF",
		blurb: "Render every page, or a selection, into one lossless multi-page TIFF.",
		note: "One lossless multi-page TIFF, whatever the page count.",
		verb: "Render TIFF",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "webp",
		slug: "pdf-to-webp",
		group: "convert",
		mark: "▥",
		title: "PDF to WebP",
		blurb: "Render every page, or a selection, to lossless WebP images.",
		note: "Lossless WebP page renders; several pages arrive as one ZIP.",
		verb: "Render WebPs",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "html",
		slug: "pdf-to-html",
		group: "convert",
		mark: "◩",
		title: "PDF to HTML",
		blurb: "Write every page, or a selection, as one positioned HTML document.",
		note: "One self-contained HTML document with every recovered span positioned on its page.",
		verb: "Export HTML",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "text",
		slug: "extract-text-from-pdf",
		group: "convert",
		mark: "¶",
		title: "Extract text",
		blurb:
			"Recover the document's text, its tables, a reading-order article, a Word document, a PowerPoint deck, or an Excel workbook.",
		note: "Text, tables, an article, Word, PowerPoint or Excel — recovered on this device.",
		verb: "Extract text",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "zip",
		slug: "zip-files",
		group: "bundle",
		mark: "▣",
		title: "ZIP files",
		blurb: "Bundle several files into one deterministic ZIP.",
		note: "A deterministic archive of the files you picked, built on this device.",
		verb: "Create ZIP",
		multiple: true,
		minimum: 2,
		accepts: "pdf",
	},
	{
		id: "fill_forms",
		slug: "fill-pdf-forms",
		group: "security",
		mark: "☑",
		title: "Fill forms",
		blurb: "Read an AcroForm, fill its fields, and save the filled PDF.",
		note: "The form is read here, the values are written into the PDF, and the result is verified.",
		verb: "Fill form",
		multiple: false,
		accepts: "pdf",
	},
	{
		id: "unlock",
		slug: "unlock-pdf",
		group: "security",
		mark: "◇",
		title: "Remove password",
		blurb: "Open a protected PDF with its password and save it without one.",
		note: "The password is used in this tab to decrypt the file; it is never sent anywhere.",
		verb: "Remove password",
		multiple: false,
		accepts: "pdf",
		related: ["protect"],
	},
	{
		id: "protect",
		slug: "protect-pdf",
		group: "security",
		mark: "◆",
		title: "Protect with password",
		blurb: "Encrypt a PDF with AES-256 and choose what a reader may do with it.",
		note: "AES-256 with separate user and owner passwords, chosen here and never stored.",
		verb: "Protect PDF",
		multiple: false,
		accepts: "pdf",
		related: ["unlock"],
	},
	{
		id: "clear_permissions",
		slug: "clear-pdf-permissions",
		group: "security",
		mark: "◈",
		title: "Clear permissions",
		blurb: "Lift the print, copy, modify and annotate restrictions, with the owner password.",
		note: "The owner password lifts the restrictions; the document's content is not re-encrypted.",
		verb: "Clear permissions",
		multiple: false,
		accepts: "pdf",
		related: ["permissions"],
	},
	{
		id: "permissions",
		slug: "set-pdf-permissions",
		group: "security",
		mark: "◉",
		title: "Set permissions",
		blurb: "Write the permission bits a document should carry, guarded by an owner password.",
		note: "The permission bits are written and read back from the output before it downloads.",
		verb: "Set permissions",
		multiple: false,
		accepts: "pdf",
		related: ["clear_permissions"],
	},
	{
		id: "sign",
		slug: "sign-pdf",
		group: "security",
		mark: "✍",
		title: "Sign PDF",
		blurb: "Draw a typed or freehand stamp, or sign with a key that never leaves this device.",
		note: "Sign with a key on this device, or add a visual stamp; earlier signatures stay intact.",
		verb: "Sign PDF",
		multiple: false,
		accepts: "pdf",
		related: ["certificate"],
	},
	{
		id: "certificate",
		slug: "validate-pdf-signatures",
		group: "security",
		mark: "◊",
		title: "Certificate validation",
		blurb: "Read every signature and say whether its chain ends at a certificate you trust.",
		note: "Every signature is checked against the bytes it covers and the anchors you supply.",
		verb: "Validate signatures",
		multiple: false,
		accepts: "pdf",
		related: ["sign"],
	},
	{
		id: "workflow",
		slug: "pdf-approval-workflow",
		group: "security",
		mark: "⤳",
		title: "Approval workflow",
		blurb: "Record an approval decision on the document itself, with no server in the loop.",
		note: "An approval decision is written into the document itself; there is no server queue.",
		verb: "Record approval",
		multiple: false,
		accepts: "pdf",
	},
];

export const TOOL_BY_ID: ReadonlyMap<ToolId, ToolDef> = new Map(
	TOOLS.map((tool) => [tool.id, tool]),
);

export const TOOL_BY_SLUG: ReadonlyMap<string, ToolDef> = new Map(
	TOOLS.map((tool) => [tool.slug, tool]),
);

/** The slug of every generated page, in display order. */
export const TOOL_PAGE_SLUGS: readonly string[] = TOOLS.map((tool) => tool.slug);

/**
 * The one place the deployed origin is written.
 *
 * Canonical links, Open Graph URLs and the sitemap all have to agree, and a
 * host name copied into a template is exactly the sort of value that drifts
 * after the first move. Every absolute URL in a generated page is built from
 * this constant.
 */
export const SITE_ORIGIN = "https://pdf.selis.com";

/**
 * The operation count in a heading, as a word.
 *
 * Past twenty the spelled form stops being something a reader parses at a
 * glance, so the numeral takes over — and any number without a word falls back
 * to the numeral rather than throwing, because a heading is not worth a failed
 * page load.
 */
export function spellOut(count: number): string {
	const words = [
		"zero",
		"one",
		"two",
		"three",
		"four",
		"five",
		"six",
		"seven",
		"eight",
		"nine",
		"ten",
		"eleven",
		"twelve",
		"thirteen",
		"fourteen",
		"fifteen",
		"sixteen",
		"seventeen",
		"eighteen",
		"nineteen",
		"twenty",
	];
	if (count >= 0 && count < words.length) {
		return words[count] ?? String(count);
	}
	return String(count);
}
