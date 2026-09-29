/**
 * SL-4.EXT.09 — every user-visible string the viewer page shows.
 *
 * ## Why this module exists, when `options-strings.ts` already does this
 *
 * Because this is the **shared** seam and that one is the extension's own. UI.11
 * split the runtime so that `apps/ui/src/i18n/message.ts` and `runtime.ts` know
 * nothing about the viewer, and `apps/ui/src/i18n/i18n-boundary.test.ts` names
 * this package as the consumer that was waiting for exactly those two files: "the
 * extension then needs two ship rows and one import, and gets a runtime rather
 * than a copy." So this module registers a catalogue against *that* runtime and
 * adds the two rows.
 *
 * `options-strings.ts` still carries its own hand-rolled formatter. That is
 * EXT.07's recorded debt, not this task's, and it is not extended here: its
 * public surface (`OPTIONS_MESSAGE_KEYS`, `createOptionsCatalogue`,
 * `createOptionsFormatter`, `formatMessage`) is pinned by `options-strings.test.ts`
 * and by a shipped page, and a rewrite of it belongs in the task that owns it.
 * What this task declines to do is *add a third* implementation, which is why
 * the strings below are registered against the shared runtime rather than
 * hand-rolled. `REUSE.md` records the remainder.
 *
 * ## The keys are closed, and the closure is the test
 *
 * `VIEWER_MESSAGE_KEYS` is a union in the way `PAGE_LIST_MESSAGE_KEYS` is, and
 * `viewer-page.test.ts` reads the *shipped* `viewer.html` and fails on any
 * `data-i18n` key this list does not contain. So a string cannot appear in the
 * markup without a key, and a key cannot appear without English text.
 *
 * ## The local-file copy is a set of claims, and each one is checkable
 *
 * The whole of SL-4.EXT.09's user-facing surface is in the `viewer.local.*`
 * namespace, and four of those sentences make factual claims a reviewer or a
 * support answer will rest on:
 *
 * - **"your browser can still open it"** — true, and the reason the denial state
 *   is not a dead end: with the toggle off nothing intercepts the local file, and
 *   Chrome's own viewer handles it. A page implying the user had lost access to
 *   their own document would be a false and alarming claim.
 * - **"This needs no permission at all"** — true and load-bearing: the
 *   file-picker route is `files.pickOpen`, a transient `<input type="file">`, and
 *   it needs no MV3 permission. It is offered *first*, because for "open a local
 *   PDF" it is the whole answer; the toggle only buys the other thing.
 * - **"Selis asks for nothing until you do"** — true because `file:///` is in
 *   `optional_host_permissions`, `host_permissions` is still `[]`, and the
 *   extension calls no permission API at all.
 * - **"never uploaded"** — ADR-P0016, and the store review defends it.
 *   `viewer-page.test.ts` asserts the sentence is present rather than trusting a
 *   reader's eye.
 *
 * `{toggle}` is filled from `local-files.ts`'s `FILE_ACCESS_TOGGLE` rather than
 * typed here, because it is a string the *browser* owns: the page has to name the
 * switch exactly as Chrome names it, or the user is looking for something that is
 * not there.
 */

import type { Catalogue, MessageKey, MessageRuntime } from "../../ui/src/i18n/runtime.js";
import { SOURCE_LOCALE, createFormatter, createMessageRuntime } from "../../ui/src/i18n/runtime.js";

/**
 * Every string the viewer page owns. The union is closed: a host may override
 * one, but a *new* string cannot appear without a key here.
 */
export const VIEWER_MESSAGE_KEYS = [
	/** While the engine is being brought up. */
	"viewer.status.starting",
	/** A document is open. Countable; placeholders `{name}` and `{pages}`. */
	"viewer.status.opened",
	/** The document server refused. Placeholders: `{status}`. */
	"viewer.status.serverError",
	/** The `?src=` value is not a URL. */
	"viewer.status.invalidSource",
	/** A scheme this extension does not open. Placeholders: `{scheme}`. */
	"viewer.status.unsupportedScheme",
	/** Anything the engine or the transport rejected. Placeholders: `{reason}`. */
	"viewer.status.failure",

	/** The heading of every local-file state, including "nothing chosen yet". */
	"viewer.local.heading",
	/** Why the browser's own switch decides this, in plain words. */
	"viewer.local.lede",
	/** The live status line when the toggle is on. */
	"viewer.local.status.on",
	/** The live status line when the toggle is off. */
	"viewer.local.status.off",
	/** The live status line when the browser refused to answer. */
	"viewer.local.status.unknown",
	/** The written steps, for a browser that did not open the settings page. Placeholders: `{toggle}`. */
	"viewer.local.steps",
	/** The link that opens the browser's extension details page. */
	"viewer.local.openSettings",
	/** The permission-free route: the file picker. */
	"viewer.local.choose",
	/** Why the permission-free route is available, and what it costs. */
	"viewer.local.chooseHelp",
	/** Re-read the grant after the user has been to the settings page. */
	"viewer.local.check",
	/** The picker was dismissed. Said, so "nothing happened" is not unambiguous. */
	"viewer.local.nothingChosen",
	/** The grant is off. The denial state, in words. */
	"viewer.local.denied",
	/** The grant is on and the read still failed. A different cause, different words. */
	"viewer.local.unreadable",
	/** The browser would not say. Not a refusal, and said as such. */
	"viewer.local.unchecked",
	/** ADR-P0016, for the local route specifically. */
	"viewer.local.privacy",
	/** The `file://` address field's label. */
	"viewer.local.addressLabel",
	/** The `file://` address field's help text. */
	"viewer.local.addressHelp",
	/** The address field's submit button. */
	"viewer.local.addressSubmit",
	/** The address field refused what was typed into it. Placeholders: `{value}`. */
	"viewer.local.addressInvalid",
] as const;

/** One key of the viewer catalogue. */
export type ViewerMessageKey = (typeof VIEWER_MESSAGE_KEYS)[number];

import { FILE_ACCESS_TOGGLE } from "./local-files.js";

/**
 * The English catalogue.
 *
 * The two `PluralMessage`s are not decoration. "1 page" and "2 pages" are
 * different sentences in every language this extension will ever be translated
 * into, and the alternative — picking a key in the boot and stringing `"s"` onto
 * a noun — is an English plural rule with a locale parameter on it, which is the
 * exact thing `message.ts`'s `PluralMessage` exists to stop.
 */
export const EN_VIEWER_CATALOGUE: Catalogue = {
	"viewer.status.starting": "Starting the Selis engine…",
	"viewer.status.opened": {
		count: "pages",
		one: "Opened {name} — {pages} page.",
		other: "Opened {name} — {pages} pages.",
	},
	"viewer.status.serverError": "The document server answered {status}.",
	"viewer.status.invalidSource": "The document address is not a valid URL.",
	"viewer.status.unsupportedScheme": "Selis cannot open a {scheme} document.",
	"viewer.status.failure": "The document could not be opened: {reason}",

	"viewer.local.heading": "Opening a PDF stored on this computer",
	"viewer.local.lede":
		"Selis cannot read a file on your disk until your browser lets it. You switch that on yourself, in your browser's extension settings, and Selis asks for nothing until you do.",
	"viewer.local.status.on": "Local file access is on, so Selis can open this file.",
	"viewer.local.status.off": "Local file access is off, so Selis has not tried to open this file.",
	"viewer.local.status.unknown":
		"This browser would not tell Selis whether local file access is on, so Selis has not tried to open this file.",
	"viewer.local.steps":
		"To switch it on: right-click the Selis icon in your browser's toolbar, choose “Manage extension”, and turn on “{toggle}”.",
	"viewer.local.openSettings": "Open Selis's extension settings",
	"viewer.local.choose": "Choose a PDF on this device instead",
	"viewer.local.chooseHelp":
		"This needs no permission at all. Your browser hands Selis the one file you pick, and Selis reads it here without uploading it.",
	"viewer.local.check": "Check again",
	"viewer.local.nothingChosen": "No file was chosen, so nothing has been opened.",
	"viewer.local.denied":
		"Selis will not read files stored on this computer. Nothing has been changed: your PDF has not moved, and your browser can still open it. You can carry on now by choosing the file yourself.",
	"viewer.local.unreadable":
		"Local file access is on, but this file could not be read. It may have been moved or renamed since this page was opened, or another program may be holding it open.",
	"viewer.local.unchecked":
		"Selis could not check whether this browser allows reading local files, so it did not try. The steps above work either way.",
	"viewer.local.privacy": "A local file is read on this device and never uploaded.",
	"viewer.local.addressLabel": "Or a local file's address",
	"viewer.local.addressHelp":
		"Paste a file:///… address. Selis will say so if this browser is not allowed to read it yet.",
	"viewer.local.addressSubmit": "Open",
	"viewer.local.addressInvalid": "“{value}” is not a file:///… address.",
};

/** How a host supplies a different locale, or overrides individual strings. */
export interface ViewerRuntimeOptions {
	readonly locale?: string | undefined;
	readonly catalogues?: Readonly<Record<string, Catalogue>> | undefined;
	readonly overrides?: Catalogue | undefined;
}

/**
 * The runtime the viewer page resolves against.
 *
 * `declaredKeys` is what makes a **stale** key detectable: a translation left
 * behind by a rename is reported rather than sitting in a file until somebody
 * reads it. `strict` is off, because a shipped shell that throws on a missing
 * message is a viewer that fails to boot over a copy change, and the gap is
 * recorded either way (`onGap`).
 */
export function createViewerRuntime(options: ViewerRuntimeOptions = {}): MessageRuntime {
	const runtime = createMessageRuntime({
		locale: options.locale ?? VIEWER_LOCALE,
		catalogues: { [SOURCE_LOCALE]: EN_VIEWER_CATALOGUE, ...options.catalogues },
		declaredKeys: VIEWER_MESSAGE_KEYS,
		onGap: (gap) => {
			console.warn(`selis: viewer message gap - ${gap.locale} ${gap.kind} ${gap.key}`);
		},
	});
	return options.overrides === undefined ? runtime : runtime.withOverrides(options.overrides);
}

/** A `key -> text` function bound to the viewer's runtime. */
export function createViewerFormatter(options: ViewerRuntimeOptions = {}) {
	return createFormatter(createViewerRuntime(options));
}

/**
 * The viewer's formatter, ready to use, built once per module.
 *
 * Once, because `createMessageRuntime` validates the whole catalogue at
 * construction: a malformed template should be a load-time failure in the page,
 * not a surprise on the first sentence. Not built at import scope for the reason
 * every accessor in this package is lazy — a module that reads a global while
 * being imported cannot be imported by a Node test, and `bundle.test.ts` imports
 * the gate's own modules from a plain Node process.
 */
let bound: ((key: MessageKey, values?: Record<string, string | number>) => string) | null = null;

export function viewerText(
	key: ViewerMessageKey,
	values?: Record<string, string | number>,
): string {
	if (bound === null) {
		bound = createViewerFormatter();
	}
	return bound(key, values);
}

/** The browser's own name for the switch, for `{toggle}`. */
export const TOGGLE_NAME = FILE_ACCESS_TOGGLE;

/** The locale this package ships: English, resolved as a source catalogue like any other. */
export const VIEWER_LOCALE = SOURCE_LOCALE;
