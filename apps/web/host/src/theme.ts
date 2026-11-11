/**
 * The web's side of the theme: the storage, the system preference, and the
 * root element.
 *
 * ## Where the logic lives
 *
 * The decisions — which theme, given a stored choice and a system's
 * preference — are in `@selis/ui-kit`, in `preferences.ts`, because they are
 * the same decisions for every surface: the web app, a desktop app, an SDK
 * embedding the design system. This module is the thin web-specific half:
 * where the choice is kept (`localStorage`), where the system's answer comes
 * from (`matchMedia`), and what the attributes are written to (the root
 * element).
 *
 * ## The one thing this module knows that the kit does not
 *
 * The system's colour scheme is a LIVE thing: it changes at sunset, and
 * without a listener the page would keep the theme it started with. So the
 * system preference is watched — but **only while the reader has not chosen**
 * one. A reader who has chosen keeps their choice.
 */
import {
	type PreferenceStorage,
	type ThemeName,
	type ThemePreferences,
	applyTheme,
	hasChosenTheme,
	resolvePreferences,
	withToggledTheme,
	writeStoredPreferences,
} from "@selis/ui-kit";

export type { ThemeName, ThemePreferences };

/** The page's `localStorage`, or `null` when the browser refuses it. */
function browserStorage(): PreferenceStorage | null {
	try {
		return globalThis.localStorage ?? null;
	} catch {
		return null;
	}
}

/** The page's dark-mode media query, or `null` when there is none. */
function browserPreference(): MediaQueryList | null {
	const match = globalThis.matchMedia;
	if (typeof match !== "function") {
		return null;
	}
	try {
		return match("(prefers-color-scheme: dark)");
	} catch {
		return null;
	}
}

/** The root element, or `null` when there is no document. */
function root(): HTMLElement | null {
	return globalThis.document?.documentElement ?? null;
}

/**
 * Put the page in the right theme, and keep it there, and wire the switch.
 *
 * The reader's choice is applied immediately. The system's preference is
 * watched only while the reader has not chosen one.
 *
 * The switch is found by its id, so every surface gets one without writing
 * the wiring again: the button is `#selis-theme-toggle`, and a page without
 * one simply has nothing to wire.
 */
export function startTheme(): ThemeName {
	const element = root();
	if (element === null) {
		return "light";
	}
	const storage = browserStorage();
	const query = browserPreference();
	const preferences = resolvePreferences(storage, query?.matches === true);
	applyTheme(preferences, element);

	if (!hasChosenTheme(storage) && query !== null) {
		const listener = (event: MediaQueryListEvent): void => {
			if (hasChosenTheme(browserStorage())) {
				return;
			}
			const next = resolvePreferences(browserStorage(), event.matches);
			applyTheme(next, element);
		};
		if (typeof query.addEventListener === "function") {
			query.addEventListener("change", listener);
		} else if (typeof query.addListener === "function") {
			// Safari before 14 only has the deprecated form.
			query.addListener(listener);
		}
	}
	wireSwitch(preferences.theme);
	return preferences.theme;
}

/** The id every surface's theme switch carries. */
export const THEME_TOGGLE_ID = "selis-theme-toggle";

/**
 * Wire the page's theme switch, if it has one.
 *
 * The switch's GLYPH is chosen by the stylesheet â€” both glyphs are in the
 * markup, so the switch is right before any script has run. Only the label
 * is set here, because a title that says which theme the button lands on is
 * worth more than a glyph the reader has to guess at.
 */
function wireSwitch(theme: ThemeName): void {
	const button = globalThis.document?.getElementById(THEME_TOGGLE_ID);
	if (button === null || button === undefined) {
		return;
	}
	const next = theme === "dark" ? "light" : "dark";
	const label = `Switch to the ${next} theme`;
	button.title = label;
	button.setAttribute("aria-label", label);
	if (button.dataset.themeWired === "true") {
		return;
	}
	button.dataset.themeWired = "true";
	button.addEventListener("click", () => {
		wireSwitch(toggleTheme());
	});
}

/**
 * Switch the theme, and remember the choice.
 *
 * Returns the theme the page is now in, so a caller can label the button it
 * just pressed.
 */
export function toggleTheme(): ThemeName {
	const element = root();
	if (element === null) {
		return "light";
	}
	const storage = browserStorage();
	const query = browserPreference();
	const current = resolvePreferences(storage, query?.matches === true);
	const next = withToggledTheme(current);
	applyTheme(next, element);
	writeStoredPreferences(next, storage);
	return next.theme;
}

/**
 * The theme the page is in now.
 *
 * The attribute is the truth: it is what the stylesheet keys on, and what a
 * reader's own choice put there.
 */
export function currentTheme(): ThemeName {
	return root()?.getAttribute("data-theme") === "dark" ? "dark" : "light";
}

/** The preferences the page is in now, for a caller that wants them all. */
export function currentPreferences(): ThemePreferences {
	const query = browserPreference();
	return resolvePreferences(browserStorage(), query?.matches === true);
}
