/**
 * The workspace's icons, as inline SVG.
 *
 * ## Why they are inline, and why they live here
 *
 * An icon font or an icon sprite would be another request the service worker
 * has to cache, and another file whose bytes nobody reviews. A path is a
 * string: the icon is drawn from the same DOM as the button that holds it, it
 * inherits `currentColor` (so dark mode is free), and there is nothing to
 * fetch.
 *
 * The set is drawn on a 24-unit grid with a 2-unit stroke, which is the
 * geometry the well-established editors use: Acrobat's ribbon and Foxit's
 * both read as a row of small, evenly-weighted marks, and the even weight is
 * what makes a row of them look like a toolbar rather than a list of
 * favourites.
 *
 * Every glyph here is decorative — the button that holds it carries the
 * accessible name — so each is marked `aria-hidden` where it is inserted.
 */

/** The names of the icons the workspace draws. */
export type IconName =
	| "open"
	| "download"
	| "undo"
	| "redo"
	| "search"
	| "close"
	| "check"
	| "chevron-left"
	| "text"
	| "text-box"
	| "image"
	| "redact"
	| "rotate-left"
	| "rotate-right"
	| "duplicate"
	| "trash"
	| "page-add"
	| "arrow-up"
	| "arrow-down"
	| "crop"
	| "resize"
	| "form"
	| "bookmark"
	| "properties"
	| "shield"
	| "watermark"
	| "page-numbers"
	| "header-footer"
	| "strip-metadata"
	| "history"
	| "zoom-in"
	| "zoom-out"
	| "fit-width"
	| "fit-page"
	| "lock"
	| "moon"
	| "sun"
	| "export"
	| "flip-horizontal"
	| "flip-vertical"
	| "nup"
	| "compress"
	| "bates";

/**
 * Each icon's path data, in the 24-unit grid.
 *
 * The paths are the shape only: the stroke, the fill rule and the accessible
 * treatment are applied once, in [`icon`], so an icon cannot be half-styled.
 */
const PATHS: Readonly<Record<IconName, string>> = {
	open: "M3 7.5A1.5 1.5 0 0 1 4.5 6h4.05a1.5 1.5 0 0 1 1.06.44l1.28 1.28H19.5A1.5 1.5 0 0 1 21 9.22V17a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 17zM3 10h18",
	download: "M12 3.5v11m0 0 4-4m-4 4-4-4M4.5 17v1.5A1.5 1.5 0 0 0 6 20h12a1.5 1.5 0 0 0 1.5-1.5V17",
	undo: "M9 8.5 5 12l4 3.5M5 12h8.5a5.5 5.5 0 0 1 0 11H12",
	redo: "m15 8.5 4 3.5-4 3.5M19 12h-8.5a5.5 5.5 0 0 0 0 11H12",
	search: "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14m5 12 4 4",
	close: "m6 6 12 12M18 6 6 18",
	check: "m5 12.5 4.5 4.5L19 7.5",
	"chevron-left": "m14.5 5-7 7 7 7",
	text: "M5 6.5V5h14v1.5M12 5v14M9 19h6",
	"text-box": "M4 5.5h16v13H4zM8.5 9h7M12 9v5.5",
	image: "M4 5.5h16v13H4zM4 15l4.5-4.5 4 4 3-3L20 15M15.5 9.5h.01",
	redact: "M4 5.5h16v13H4zM7.5 12h9M9 9.6 15 14.4M15 9.6 9 14.4",
	"rotate-left": "M4.5 9.5A8 8 0 1 1 4 12M4.5 4.5v5h5",
	"rotate-right": "M19.5 9.5A8 8 0 1 0 20 12M19.5 4.5v5h-5",
	duplicate: "M8.5 8.5h9v11h-9zM15.5 8.5v-3h-9v11h3",
	trash:
		"M4.5 7h15M9.5 7V5.5A1.5 1.5 0 0 1 11 4h2a1.5 1.5 0 0 1 1.5 1.5V7M6.5 7l.8 11.6A1.5 1.5 0 0 0 8.8 20h6.4a1.5 1.5 0 0 0 1.5-1.4L17.5 7",
	"page-add": "M6 3.5h7l5 5v12H6zM13 3.5v5h5M12 11v6M9 14h6",
	"arrow-up": "M12 19V6m0 0-5.5 5.5M12 6l5.5 5.5",
	"arrow-down": "M12 5v13m0 0 5.5-5.5M12 18l-5.5-5.5",
	crop: "M6.5 2.5v15h15M2.5 6.5h15v15",
	resize: "M4.5 9.5v-5h5M19.5 14.5v5h-5M4.5 4.5 10 10m9.5 9.5L14 14",
	form: "M6 3.5h12v17H6zM9 8h6M9 12h6M9 16h4",
	bookmark: "M7 4.5h10v16l-5-3.5-5 3.5z",
	properties: "M5 7h14M5 12h14M5 17h14M9 5.5v3M15 10.5v3M11 15.5v3",
	shield: "M12 3.5 19 6v6c0 4.2-2.9 7.4-7 8.5-4.1-1.1-7-4.3-7-8.5V6zM9 12l2.2 2.2L15.5 10",
	watermark: "M4 5.5h16v13H4zM7.5 15 16.5 8.5",
	"page-numbers": "M4.5 7.5h3M4.5 12h3M4.5 16.5h3M10 7.5h9.5M10 12h9.5M10 16.5h9.5",
	"header-footer": "M4 4.5h16v15H4zM4 9h16M4 15h16",
	"strip-metadata": "m14.5 3.5 6 6M4 20l6.5-6.5M9.5 5.5 15 11M5 19h14",
	history: "M12 4.5a7.5 7.5 0 1 0 7.4 8.6M12 4.5V8m0 0 3 2.5M12 8l-3 2.5M4.6 12.4 3 13.6l1 1.7",
	"zoom-in": "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14m5 12 4 4M11 8.5v5M8.5 11h5",
	"zoom-out": "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14m5 12 4 4M8.5 11h5",
	"fit-width": "M3.5 6.5h17M3.5 17.5h17M9 9.5 6 12l3 2.5M15 9.5l3 2.5-3 2.5",
	"fit-page": "M6 3.5h12v17H6zM3 6.5v11M21 6.5v11",
	lock: "M7 10.5V8a5 5 0 0 1 10 0v2.5M5.5 10.5h13V20h-13zM12 14v2.5",
	moon: "M20 14.2A8.2 8.2 0 0 1 9.8 4a8.5 8.5 0 1 0 10.2 10.2",
	sun: "M12 7.5a4.5 4.5 0 1 0 0 9 4.5 4.5 0 0 0 0-9M12 2.5v2M12 19.5v2M2.5 12h2M19.5 12h2M5.2 5.2l1.4 1.4M17.4 17.4l1.4 1.4M18.8 5.2l-1.4 1.4M6.6 17.4l-1.4 1.4",
	export:
		"M12 15.5V3.5m0 0L8 7.5m4-4 4 4M4.5 14.5v4A1.5 1.5 0 0 0 6 20h12a1.5 1.5 0 0 0 1.5-1.5v-4",
	"flip-horizontal": "M12 3.5v17M9 7 4.5 12 9 17M15 7l4.5 5L15 17",
	"flip-vertical": "M3.5 12h17M7 9 12 4.5 17 9M7 15l5 4.5 5-4.5",
	nup: "M4 5.5h7v6H4zM13 5.5h7v6h-7zM4 13.5h7v6H4zM13 13.5h7v6h-7z",
	compress: "M4 8.5h16M4 12h16M4 15.5h16M9 4.5 12 7.5 15 4.5M9 19.5l3-3 3 3",
	bates: "M6 4.5h12v15H6zM9 8h6M9 11.5h6M9 15h3",
};

/**
 * Build one icon.
 *
 * The glyph is decorative: the button around it carries the name a screen
 * reader reads, so the SVG itself is hidden from the accessibility tree.
 */
export function icon(name: IconName): SVGElement {
	const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
	svg.setAttribute("viewBox", "0 0 24 24");
	svg.setAttribute("fill", "none");
	svg.setAttribute("stroke", "currentColor");
	svg.setAttribute("stroke-width", "1.75");
	svg.setAttribute("stroke-linecap", "round");
	svg.setAttribute("stroke-linejoin", "round");
	svg.setAttribute("aria-hidden", "true");
	svg.setAttribute("focusable", "false");
	svg.classList.add("selis-editor-icon");
	const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
	path.setAttribute("d", PATHS[name]);
	svg.appendChild(path);
	return svg;
}
