/**
 * SL-4.EXT.07 — every user-visible string the options page shows.
 *
 * The same shape as `apps/ui/src/viewer/strings.ts`, and deliberately so: a
 * closed key set, one English catalogue, `{name}` placeholders substituted by
 * name, and a `Partial` override a host may merge over English. The ADRs are
 * explicit that "every user-visible string goes through i18n keys from day 1 so
 * a rename is a resource change, not a code change", and an options page is the
 * one page a user reads *in order to find out what the extension does* — so the
 * copy on it is the copy most worth translating.
 *
 * It is duplicated rather than imported on purpose. `apps/ui`'s `formatMessage`
 * is module-private, and importing the viewer catalogue to reach it would put
 * the whole page-list and search string set into a package that has no page
 * list — the exact thing `REUSE.md` refuses when it declines to ship
 * `page-list.css`. SL-4.UI.11 owns the real runtime; this module is written in
 * the shape UI.11 expects, so adopting it is a change to the body of
 * `createOptionsCatalogue` and nothing else.
 *
 * ## The copy is a claim, so it is checked like one
 *
 * Two of these strings make a factual claim about what the extension does, and
 * both claims are load-bearing:
 *
 * - **Telemetry.** The switch is real (it writes the key `ext/adapter.ts`
 *   reads) and the port behind it is inert — there is no endpoint and no
 *   permission to reach one. The catalogue therefore says that nothing is
 *   recorded, in the string itself, and `options-strings.test.ts` asserts that
 *   the sentence is present rather than trusting a reviewer's eye. A page that
 *   implied data was being collected would be a false privacy disclosure, and
 *   SL-4.EXT.11 has to defend "does not collect user data" in a store review.
 * - **CJK fonts.** The payload is real and it does not fit: 11 162 268 resident
 *   bytes against this extension's 8 MiB store budget (FONT.10-F1), so a full
 *   install is refused. The sizes are placeholders filled from
 *   `ext/cjk-payload.ts` rather than typed into the sentence, and the row
 *   carries no switch — there is nothing to switch, and a control that refuses
 *   is a control that lies about why.
 */

import { ALLOWED_PERMISSIONS } from "./permissions.js";

/**
 * Every user-facing string the options page owns. The union is closed on
 * purpose: `createOptionsCatalogue` takes a `Partial`, so a host can override
 * one string, but a *new* string cannot appear without a key here.
 */
export const OPTIONS_MESSAGE_KEYS = [
	/** `<title>`; placeholders: `{product}`. */
	"options.documentTitle",
	/** The page's `h1`; placeholders: `{product}`. */
	"options.page.heading",
	/** The welcome guide's heading. */
	"options.welcome.heading",
	/** One sentence on what the extension is. */
	"options.welcome.lede",
	/** What happens when a PDF link is clicked. */
	"options.welcome.point.opens",
	/** Where rendering happens, and what is not sent anywhere. */
	"options.welcome.point.local",
	/** What this build can and cannot do to a document. */
	"options.welcome.point.readonly",
	/** `file://` documents, which this build does not open. */
	"options.welcome.localFiles",
	/** Dismisses the welcome guide. */
	"options.welcome.done",
	/** The privacy section's heading. */
	"options.privacy.heading",
	/** The privacy summary: no account, no server, no upload. */
	"options.privacy.summary",
	/** The telemetry opt-in's label. */
	"options.telemetry.label",
	/**
	 * The telemetry opt-in's help text.
	 *
	 * Says, in the product's own words, that nothing is recorded or sent. Not a
	 * nicety: see the module doc.
	 */
	"options.telemetry.help",
	/** The capability list's heading. */
	"options.permissions.heading",
	/** One line above the capability list. */
	"options.permissions.intro",
	/** `declarativeNetRequest`, in a user's words. */
	"options.permission.declarativeNetRequest",
	/** `offscreen`, in a user's words. */
	"options.permission.offscreen",
	/** `storage`, in a user's words. */
	"options.permission.storage",
	/** The sentence for holding no website access at all (the case today). */
	"options.permissions.hosts.none",
	/** One held website pattern; placeholders: `{pattern}`. */
	"options.permissions.host",
	/** The CJK section's heading. */
	"options.cjk.heading",
	/**
	 * Why the optional CJK font packs are not offered.
	 * Placeholders: `{resident}`, `{budget}`.
	 */
	"options.cjk.status",
	/** The manage section's heading. */
	"options.actions.heading",
	/** Re-opens the welcome guide, from memory. */
	"options.actions.replay",
	/** Returns every setting to its default. */
	"options.actions.reset",
	/** What "reset" actually does here, so the button is not a surprise. */
	"options.actions.reset.help",
	/** Announced after a setting is written. */
	"options.status.saved",
	/** Announced after a reset. */
	"options.status.reset",
	/** Announced when the welcome guide is re-opened. */
	"options.status.replayed",
	/** Announced when a setting could not be written. */
	"options.status.failed",
	/** The footer; placeholders: `{version}`. */
	"options.version",
] as const;

/** A key into {@link OPTIONS_MESSAGE_KEYS}. */
export type OptionsMessageKey = (typeof OPTIONS_MESSAGE_KEYS)[number];

/** A set of templates keyed by message key. */
export type OptionsCatalogue = Readonly<Record<OptionsMessageKey, string>>;

/** Values a `{placeholder}` can take. No document data ever passes through here. */
export type OptionsMessageValues = Readonly<Record<string, string | number>>;

/**
 * The locale this catalogue is written in.
 *
 * One locale ships (English). It is declared rather than assumed so that the
 * page's `lang` attribute is set from the catalogue rather than left as a
 * literal in the markup, and so a second locale is a value here and a table
 * there rather than a fork.
 */
export const OPTIONS_LOCALE = "en";

/**
 * English catalogue. Placeholders: `{product}`, `{pattern}`, `{resident}`,
 * `{budget}`, `{version}`.
 *
 * Three sentences are worded the way they are, and the wording is the point:
 *
 * - `options.welcome.point.local` says the document is **fetched with the
 *   browser's own credentials** and rendered here, rather than saying "nothing
 *   leaves your device". The first is what happens; the second is a claim a
 *   network trace would contradict, and this product's privacy claim has to
 *   survive a store review (SL-4.EXT.11).
 * - `options.telemetry.help` says the switch records nothing, in the string
 *   itself, because the port behind it is inert (ADR-P0017).
 * - `options.cjk.status` gives the two measured sizes and says a complete
 *   install is refused, rather than calling the feature "coming soon".
 */
export const EN_OPTIONS_CATALOGUE: OptionsCatalogue = {
	"options.documentTitle": "{product} — Options",
	"options.page.heading": "{product} options",
	"options.welcome.heading": "Welcome",
	"options.welcome.lede":
		"Selis opens PDFs in a viewer that runs on this device. Here is what it does, and what it deliberately does not do.",
	"options.welcome.point.opens":
		"Clicking a PDF link on the web opens it here instead of your browser's built-in viewer. Everything else on the page is left alone.",
	"options.welcome.point.local":
		"The file is fetched using your browser's own login and rendered on this device. It is never uploaded: Selis has no server to upload it to.",
	"options.welcome.point.readonly":
		"This version can open, search and print a PDF. It cannot edit, sign, or save one back yet.",
	"options.welcome.localFiles":
		"PDFs stored in a folder on this computer are not opened by the extension. That is a separate permission flow, and it has not shipped yet.",
	"options.welcome.done": "Start viewing PDFs",
	"options.privacy.heading": "Privacy",
	"options.privacy.summary":
		"Selis has no account, no analytics and no server. It cannot read the pages you visit, and it cannot send a document anywhere.",
	"options.telemetry.label": "Share anonymous usage data",
	"options.telemetry.help":
		"Off, and this version has nowhere to send it: there is no analytics endpoint and no permission to reach one, so nothing is recorded or transmitted either way. The answer is kept so that a version which does collect something has to ask you first.",
	"options.permissions.heading": "What the browser has allowed",
	"options.permissions.intro":
		"These three capabilities are the whole of what the browser has granted Selis. Everything else it would need, it has refused.",
	"options.permission.declarativeNetRequest":
		"Redirect links to PDFs into the built-in viewer. The rules are evaluated by your browser; Selis never sees the request.",
	"options.permission.offscreen":
		"Run the PDF engine in a hidden Selis page, so an open document survives you switching tabs.",
	"options.permission.storage": "Keep your settings, and any optional font packs, on this device.",
	"options.permissions.hosts.none":
		"No website access at all: Selis can neither read nor change any site you visit, and it never runs a script inside one.",
	"options.permissions.host": "Website access: {pattern}",
	"options.cjk.heading": "Chinese, Japanese and Korean text",
	"options.cjk.status":
		"The optional font packs are not available in this version. The complete set needs {resident} on your device, more than the {budget} Selis keeps there, so a full install is refused and there is nothing to switch on. Latin text is unaffected.",
	"options.actions.heading": "Settings",
	"options.actions.replay": "Show the welcome guide",
	"options.actions.reset": "Reset all settings",
	"options.actions.reset.help":
		"Puts usage data back to off. Nothing else is stored on this device, so there is nothing else to clear.",
	"options.status.saved": "Saved.",
	"options.status.reset": "Settings reset.",
	"options.status.replayed": "Welcome guide shown.",
	"options.status.failed": "That change could not be saved on this device.",
	"options.version": "Version {version}",
};

/**
 * Substitute `{name}` placeholders. An unknown placeholder is left in place
 * rather than silently blanked, copied from `apps/ui/src/viewer/strings.ts` and
 * for the same reason: a catalogue with a typo should look wrong in the UI,
 * where somebody can see it, not read as an empty label to a screen reader.
 */
export function formatMessage(template: string, values: OptionsMessageValues): string {
	return template.replace(/\{(\w+)\}/g, (whole, name: string) =>
		name in values ? String(values[name]) : whole,
	);
}

/**
 * Merge a partial catalogue over English.
 *
 * Merged rather than replaced, and shallowly: a translator overriding one string
 * must not have to restate the other thirty, and a key missing from the
 * override falls back to English rather than rendering as `undefined`.
 */
export function createOptionsCatalogue(
	overrides: Partial<OptionsCatalogue> = {},
): OptionsCatalogue {
	return { ...EN_OPTIONS_CATALOGUE, ...overrides };
}

/** A bound formatter over one catalogue: the page's whole i18n seam. */
export type OptionsFormatter = (key: OptionsMessageKey, values?: OptionsMessageValues) => string;

/** Bind a catalogue into a `key -> text` function, for the page's DOM fill. */
export function createOptionsFormatter(catalogue: OptionsCatalogue): OptionsFormatter {
	return (key, values = {}) => formatMessage(catalogue[key], values);
}

/**
 * The sentence a permission's name is explained by.
 *
 * A map rather than a catalogue lookup by string-building, so a permission with
 * no entry is a **test failure** and not a row reading `undefined`: adding a
 * permission to the manifest is a sign-off event (`PERMISSIONS.md`,
 * `manifest.test.ts`), and a new capability arriving with no sentence for it is
 * exactly the case this map makes loud. The keys are checked against
 * `ALLOWED_PERMISSIONS` rather than the other way round, so dropping a
 * permission does not leave a stale explanation behind.
 */
export const PERMISSION_MESSAGE_KEYS: Readonly<Record<string, OptionsMessageKey>> = {
	declarativeNetRequest: "options.permission.declarativeNetRequest",
	offscreen: "options.permission.offscreen",
	storage: "options.permission.storage",
};

/**
 * The keys the capability list renders, in the order given.
 *
 * A permission with no entry in {@link PERMISSION_MESSAGE_KEYS} yields `null`
 * rather than being skipped: the page then holds a key it cannot fill, which the
 * catalogue test fails on, instead of a silently shorter list.
 */
export function permissionMessageKeys(
	permissions: readonly string[] = ALLOWED_PERMISSIONS,
): readonly (OptionsMessageKey | null)[] {
	return permissions.map((permission) => PERMISSION_MESSAGE_KEYS[permission] ?? null);
}

/** One row of the website-access list. */
export interface HostAccessRow {
	readonly pattern: string;
	readonly key: OptionsMessageKey;
}

/**
 * How the page describes the extension's website access.
 *
 * The empty case is its own string and not a formatting of the general one: "no
 * website access at all" is the sentence that makes ADR-P0016's privacy claim
 * checkable by a reader, and a version of it built by joining a pattern list
 * would be the sentence that quietly stops being true the day a pattern is
 * added. A future task that adds a host permission has to delete a line here on
 * purpose.
 */
export function hostAccessRows(hosts: readonly string[]): readonly HostAccessRow[] {
	if (hosts.length === 0) {
		return [{ pattern: "", key: "options.permissions.hosts.none" }];
	}
	return hosts.map((pattern) => ({ pattern, key: "options.permissions.host" }));
}

const MEBIBYTE = 1024 * 1024;

/**
 * A byte count as the page shows it: mebibytes, one decimal.
 *
 * MiB rather than MB because every number it formats is compared against a
 * budget stated in MiB (`SIZE.md`'s 8 MiB package budget, `cjk-payload.ts`'s
 * 8 MiB storage budget), and a page that said "8.4 MB" beside a limit written
 * as "8 MiB" invites a support question about a difference that is only
 * notation. Not locale-formatted yet, deliberately: `Intl` output belongs to the
 * runtime SL-4.UI.11 brings, and a hand-rolled grouping separator here would be
 * English-only formatting in the one place a translator cannot reach it.
 */
export function formatMibibytes(bytes: number): string {
	return `${(bytes / MEBIBYTE).toFixed(1)} MiB`;
}

/**
 * The product name to use when the manifest cannot be read.
 *
 * The page prefers `chrome.runtime.getManifest().name`, because that is the one
 * definition of the product's name, and duplicating it in a catalogue is how a
 * rename ends up applied to one and not the other. The fallback covers the
 * contexts with no `chrome` at all, and `options-strings.test.ts` asserts it
 * still equals `manifest.json`'s `name` so it cannot rot into a second name.
 */
export const DEFAULT_PRODUCT_NAME = "Selis PDF Viewer";
