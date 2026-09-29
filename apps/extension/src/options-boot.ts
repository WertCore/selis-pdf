/**
 * SL-4.EXT.07 — the options page's entry point.
 *
 * The counterpart to `viewer-boot.ts`: the page itself is `options.html`, which
 * carries no text, and everything a user reads arrives here through
 * `options-strings.ts`. The split is the one the viewer already uses, and for
 * the same reason — the markup is a skeleton of `data-i18n` keys, and a string
 * that lived in the HTML could not be translated without editing a shipped file.
 *
 * ## What this page is allowed to do
 *
 * It reads and writes the two stores `options-state.ts` names, and that is all.
 * In particular it does **not**:
 *
 * - build a `PlatformAdapter`, which would connect an engine port and need an
 *   offscreen document for a page that opens no document;
 * - reach `chrome.tabs`, `chrome.permissions`, or a network API. Nothing here
 *   needs one, and `options-page.test.ts` asserts the shipped page and worker
 *   name none of them, so a future "helpful" link cannot quietly widen what this
 *   extension may do.
 *
 * ## Failure is a line of text, never a blank page
 *
 * Every step is independently guarded. A store that cannot be read, a setting
 * that cannot be written, a `chrome` that is not there at all: each ends in
 * something the user can read and act on, because the one outcome this page
 * must never produce is the silent blank panel EXT.01 shipped and EXT.02 had to
 * come back and fix.
 */

import { cjkAvailability } from "./ext/cjk-payload.js";
import { createBrowserHostEnv } from "./ext/host-env.js";
import {
	type OnboardingStore,
	type SettingsStore,
	createOnboardingStore,
	planWelcome,
	readSettings,
	resetSettings,
	writeTelemetryOptIn,
} from "./options-state.js";
import {
	DEFAULT_PRODUCT_NAME,
	OPTIONS_LOCALE,
	type OptionsFormatter,
	type OptionsMessageKey,
	createOptionsCatalogue,
	createOptionsFormatter,
	formatMibibytes,
	hostAccessRows,
	permissionMessageKeys,
} from "./options-strings.js";
import { ALLOWED_HOST_PERMISSIONS, ALLOWED_OPTIONAL_HOST_PERMISSIONS } from "./permissions.js";

/** The element ids the page's skeleton declares and this module drives. */
const IDS = {
	title: "selis-title",
	version: "selis-version",
	welcome: "selis-welcome",
	welcomeHeading: "selis-welcome-heading",
	welcomeDone: "selis-welcome-done",
	telemetry: "selis-telemetry",
	capabilities: "selis-capabilities",
	hosts: "selis-hosts",
	cjkStatus: "selis-cjk-status",
	replay: "selis-replay",
	reset: "selis-reset",
	status: "selis-status",
} as const;

/** Attributes the page's skeleton uses to say "fill this from the catalogue". */
const TEXT_ATTRIBUTE = "data-i18n";

/** The `aria-label` a few elements carry, as `data-i18n-aria-label`. */
const LABEL_ATTRIBUTE = "data-i18n-aria-label";

/**
 * Everything the page talks to, as one object.
 *
 * Injected so the wiring below is a function of its collaborators rather than
 * of the browser: the same shape a Node test can build, and the reason this
 * module can be reasoned about without a DOM. The defaults are the real host
 * (`ext/host-env.ts` and `chrome.storage.local`), which is what the page uses.
 */
export interface OptionsDeps {
	readonly settings: SettingsStore;
	readonly onboarding: OnboardingStore;
	/** The product name, from `chrome.runtime.getManifest().name` where possible. */
	readonly product: string;
	/** The extension version, for the footer. */
	readonly version: string;
	/** One line to the user, in the page's own live region. */
	announce(message: string): void;
}

/**
 * Fill every `data-i18n` element's text, and every `data-i18n-aria-label`
 * element's accessible name.
 *
 * A key the catalogue does not have renders as an empty string rather than as
 * `undefined` or as the key itself: the page is better with a blank line the
 * next build fills than with a `options.welcome.…` identifier in front of a
 * user. `options-page.test.ts` checks every key in the markup against the
 * catalogue, so a typo is a test failure and not a blank.
 */
export function translate(root: Document | Element, format: OptionsFormatter): void {
	for (const element of root.querySelectorAll(`[${TEXT_ATTRIBUTE}]`)) {
		const key = element.getAttribute(TEXT_ATTRIBUTE);
		if (key !== null) {
			element.textContent = format(key as OptionsMessageKey);
		}
	}
	for (const element of root.querySelectorAll(`[${LABEL_ATTRIBUTE}]`)) {
		const key = element.getAttribute(LABEL_ATTRIBUTE);
		if (key !== null) {
			element.setAttribute("aria-label", format(key as OptionsMessageKey));
		}
	}
}

/**
 * The rows of the capability list, as the elements that hold them.
 *
 * Built rather than declared in the HTML, because the list is a fact about the
 * manifest: writing it out by hand would be a second copy of the permission
 * set, and the copy would be the one a reviewer reads.
 */
function renderCapabilities(list: Element, format: OptionsFormatter): void {
	list.replaceChildren();
	for (const key of permissionMessageKeys()) {
		const item = list.ownerDocument.createElement("li");
		item.className = "selis-fact";
		// A permission with no sentence cannot happen without the catalogue test
		// failing first; the guard is here so that if it somehow does, the row is
		// visibly incomplete rather than silently short.
		item.textContent = key === null ? "" : format(key);
		list.append(item);
	}
}

/**
 * The rows describing website access: none granted today, one per granted pattern
 * if that changes, and one per *declared but ungranted* optional pattern.
 *
 * The optional list is here (SL-4.EXT.09) because this page is where a reader
 * goes to find out what the browser has allowed, and a declared pattern the
 * browser has not allowed is part of that answer. It gets its own sentence rather
 * than the granted one — see `hostAccessRows` — so the page never claims access it
 * does not have.
 */
function renderHostAccess(list: Element, format: OptionsFormatter): void {
	list.replaceChildren();
	for (const row of hostAccessRows(ALLOWED_HOST_PERMISSIONS, ALLOWED_OPTIONAL_HOST_PERMISSIONS)) {
		const item = list.ownerDocument.createElement("li");
		item.className = "selis-fact";
		item.textContent =
			row.pattern === "" ? format(row.key) : format(row.key, { pattern: row.pattern });
		list.append(item);
	}
}

/**
 * Bring the page up.
 *
 * Ordered so the user meets the guide first and the rest of the page behind it:
 * the translations, then the welcome panel's visibility, then the settings, then
 * the two lists, then the CJK sentence. Every store call is guarded by its
 * caller, because an options page that never finishes loading is
 * indistinguishable from a broken one.
 */
export async function bootOptions(root: Element, deps: OptionsDeps): Promise<void> {
	const format = createOptionsFormatter(createOptionsCatalogue());
	translate(root.ownerDocument, format);
	root.ownerDocument.documentElement.lang = OPTIONS_LOCALE;

	// The three strings that take a value, filled here rather than by
	// `translate`. A templated key reached through `data-i18n` would render as
	// `{product}` in front of a user — `formatMessage` leaves an unfilled
	// placeholder visible on purpose, so the mistake shows rather than hides,
	// and this is where it is avoided.
	root.ownerDocument.title = format("options.documentTitle", { product: deps.product });
	setText(root, IDS.title, format("options.page.heading", { product: deps.product }));
	setText(root, IDS.version, format("options.version", { version: deps.version }));

	const announce = (key: OptionsMessageKey): void => deps.announce(format(key));

	await mountWelcome(root, deps, announce);
	await mountTelemetry(root, deps, announce);
	renderCapabilities(requireElement(root, IDS.capabilities), format);
	renderHostAccess(requireElement(root, IDS.hosts), format);
	renderCjkStatus(root, format);
	mountActions(root, deps, announce);
}

/** Put one resolved string into the element the skeleton gives it an id for. */
function setText(root: Element, id: string, text: string): void {
	const element = root.ownerDocument.getElementById(id);
	if (element !== null) {
		element.textContent = text;
	}
}

/**
 * Wire the welcome guide, and show it when it has not been seen.
 *
 * The recording is the anti-nag mechanism and it happens *here*, on render, not
 * on dismissal — see the module doc in `options-state.ts`. A store that cannot
 * be read leaves the guide visible for this page load, which is the safe
 * direction: a user who has just been told what the extension does has been
 * told it. A store that cannot be written only means the next visit shows it
 * again, and the next visit is a page the user opened deliberately.
 *
 * **Replay is wired before the flag is read, and unconditionally.** That is the
 * whole reason this function does not return early: a user who has already seen
 * the guide is exactly the user who presses "Show the welcome guide", and
 * wiring the button only on the first-run path left it dead for everyone else.
 */
async function mountWelcome(
	root: Element,
	deps: OptionsDeps,
	announce: (key: OptionsMessageKey) => void,
): Promise<void> {
	const panel = root.ownerDocument.getElementById(IDS.welcome);
	if (panel === null) {
		return;
	}
	const heading = root.ownerDocument.getElementById(IDS.welcomeHeading);
	// Replay is deliberately not a stored-state change. Rewriting the flag to
	// "unseen" would let any later visit re-arm the first-run path, and that
	// flag is the one piece of state deciding whether a user is ever told what
	// this extension does.
	requireElement(root, IDS.replay).addEventListener("click", () => {
		panel.removeAttribute("hidden");
		heading?.focus();
		announce("options.status.replayed");
	});

	let seen = false;
	try {
		seen = (await deps.onboarding.read()).seen;
	} catch {
		seen = false;
	}
	if (!planWelcome(seen).show) {
		return;
	}
	panel.removeAttribute("hidden");
	if (planWelcome(seen).record) {
		try {
			await deps.onboarding.markSeen();
		} catch {
			// Nothing to do: the guide is on screen, which is the point of it.
		}
	}
	// Focus the heading rather than the first button: a screen reader then
	// announces where the user has arrived instead of a button's label, and the
	// guide's own text is what a first run needs read.
	heading?.focus();
	requireElement(root, IDS.welcomeDone).addEventListener("click", () => {
		panel.setAttribute("hidden", "");
		announce("options.status.saved");
	});
}

/** The one switch: the telemetry opt-in, reflecting whatever is stored. */
async function mountTelemetry(
	root: Element,
	deps: OptionsDeps,
	announce: (key: OptionsMessageKey) => void,
): Promise<void> {
	// `querySelector` rather than `getElementById`: the id is in the markup, but
	// only this one element type has a `checked`, and a typed query says so
	// instead of asserting it.
	const input = root.ownerDocument.querySelector<HTMLInputElement>(`#${IDS.telemetry}`);
	if (input === null) {
		return;
	}
	let enabled = false;
	try {
		enabled = (await readSettings(deps.settings)).telemetryOptIn;
	} catch {
		enabled = false;
	}
	input.checked = enabled;
	input.addEventListener("change", () => {
		void writeTelemetryOptIn(deps.settings, input.checked).then(
			() => announce("options.status.saved"),
			() => announce("options.status.failed"),
		);
	});
}

/** The CJK row: the measured refusal, and no control to switch on. */
function renderCjkStatus(root: Element, format: OptionsFormatter): void {
	const target = root.ownerDocument.getElementById(IDS.cjkStatus);
	if (target === null) {
		return;
	}
	const availability = cjkAvailability();
	target.textContent = format("options.cjk.status", {
		resident: formatMibibytes(availability.residentBytes),
		budget: formatMibibytes(availability.budgetBytes),
	});
}

/**
 * Reset: every setting back to its default, and a line saying so.
 *
 * No confirmation dialog. The only setting is a consent flag whose default is
 * the safe side, the help text under the button says exactly what happens, and a
 * modal asking "are you sure?" about turning something *off* teaches users to
 * click through dialogs. A second setting worth a confirmation will want one.
 */
function mountActions(
	root: Element,
	deps: OptionsDeps,
	announce: (key: OptionsMessageKey) => void,
): void {
	requireElement(root, IDS.reset).addEventListener("click", () => {
		void resetSettings(deps.settings).then(
			() => {
				const input = root.ownerDocument.querySelector<HTMLInputElement>(`#${IDS.telemetry}`);
				if (input !== null) {
					input.checked = false;
				}
				announce("options.status.reset");
			},
			() => announce("options.status.failed"),
		);
	});
}

/**
 * The dependencies the page actually runs with.
 *
 * Every one is looked up lazily and defensively. A missing `chrome` — a test, a
 * port that has not exposed it — yields empty manifest fields rather than a
 * throw, so the page still renders its text and its explanations: the parts that
 * are just as true without a browser as with one.
 */
export function browserDeps(): OptionsDeps {
	const api = (
		globalThis as {
			chrome?: { runtime?: { getManifest?: () => { name?: string; version?: string } } };
		}
	).chrome?.runtime;
	let manifest: { name?: string; version?: string } = {};
	try {
		manifest = api?.getManifest?.() ?? {};
	} catch {
		manifest = {};
	}
	return {
		settings: createBrowserHostEnv().storage,
		onboarding: createOnboardingStore(),
		product: manifest.name ?? DEFAULT_PRODUCT_NAME,
		version: manifest.version ?? "0.0.0",
		announce: (message) => {
			const status = globalThis.document?.getElementById(IDS.status);
			if (status !== null && status !== undefined) {
				status.textContent = message;
			}
		},
	};
}

/** An element the page's skeleton must have, or the page is broken. */
function requireElement(root: Element, id: string): Element {
	const element = root.ownerDocument.getElementById(id);
	if (element === null) {
		throw new Error(`options.html is missing #${id}`);
	}
	return element;
}

/** Run the options page when this module is loaded as its entry point. */
if (typeof document !== "undefined") {
	const mount = document.getElementById("selis-options");
	if (mount !== null) {
		void bootOptions(mount, browserDeps());
	}
}
