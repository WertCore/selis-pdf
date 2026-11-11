/**
 * Remembering the reader's preferences, and choosing the defaults.
 *
 * ## Where this belongs, and why
 *
 * The theming module next door says what a preference set MEANS: which
 * attribute values it becomes. This module says how a preference set is
 * **remembered** and how the default is **chosen** — and that is the half
 * every surface needs and none of them should write again:
 *
 * - the web app, which keeps the choice in `localStorage` and the default in
 *   the operating system's colour scheme;
 * - a desktop app, which keeps it in the user's settings file and the default
 *   in the desktop's own theme;
 * - an SDK embedding the design system, which keeps it wherever the host
 *   keeps it.
 *
 * So the logic is here, DOM-free and platform-free: the storage is a
 * two-method interface, and the system preference is a boolean. A shell
 * supplies both; nothing here reaches for a global.
 *
 * ## The rule about the default
 *
 * A reader who has chosen keeps their choice. A reader who has NOT chosen
 * gets the system's preference, and keeps getting it: the theme follows the
 * operating system as it changes, until the reader says otherwise. That is
 * why the stored value is read separately from the chosen default, and why
 * [`readPreferences`] can be told which of the two it is looking at.
 */
import {
	type ContrastPreference,
	type DensityPreference,
	type MotionPreference,
	type ThemeName,
	type ThemePreferences,
	defaultPreferences,
} from "./theming.js";

/** Where the theme is remembered. */
export const THEME_STORAGE_KEY = "selis-theme";

/** Where the contrast preference is remembered. */
export const CONTRAST_STORAGE_KEY = "selis-contrast";

/** Where the density preference is remembered. */
export const DENSITY_STORAGE_KEY = "selis-density";

/** Where the motion preference is remembered. */
export const MOTION_STORAGE_KEY = "selis-motion";

/** A place to keep a preference. Narrowed so a shell can supply its own. */
export interface PreferenceStorage {
	getItem(key: string): string | null;
	setItem(key: string, value: string): void;
}

/**
 * Read the stored preferences.
 *
 * A stored value that is not a valid preference is treated as no value at
 * all: a stale key must not leave a surface in a state nobody can name. A
 * storage that refuses to be read is the same thing.
 */
export function readStoredPreferences(
	storage: PreferenceStorage | null | undefined,
): Partial<ThemePreferences> {
	const stored: Partial<ThemePreferences> = {};
	if (storage === null || storage === undefined) {
		return stored;
	}
	const read = (key: string): string | null => {
		try {
			return storage.getItem(key);
		} catch {
			return null;
		}
	};
	const theme = read(THEME_STORAGE_KEY);
	if (theme === "light" || theme === "dark") {
		stored.theme = theme;
	}
	const contrast = read(CONTRAST_STORAGE_KEY);
	if (contrast === "normal" || contrast === "high") {
		stored.contrast = contrast;
	}
	const density = read(DENSITY_STORAGE_KEY);
	if (density === "comfortable" || density === "compact") {
		stored.density = density;
	}
	const motion = read(MOTION_STORAGE_KEY);
	if (motion === "system" || motion === "reduced") {
		stored.motion = motion;
	}
	return stored;
}

/**
 * Remember the preferences.
 *
 * A storage that refuses the write is not an error: the preference still
 * applies to this session, and the reader is not nagged about it.
 */
export function writeStoredPreferences(
	preferences: ThemePreferences,
	storage: PreferenceStorage | null | undefined,
): void {
	if (storage === null || storage === undefined) {
		return;
	}
	const write = (key: string, value: string): void => {
		try {
			storage.setItem(key, value);
		} catch {
			// The choice simply does not outlive the session.
		}
	};
	write(THEME_STORAGE_KEY, preferences.theme);
	write(CONTRAST_STORAGE_KEY, preferences.contrast);
	write(DENSITY_STORAGE_KEY, preferences.density);
	write(MOTION_STORAGE_KEY, preferences.motion);
}

/**
 * The preferences to start with.
 *
 * The reader's own choices win; whatever they have not chosen follows the
 * system's colour scheme and the design system's defaults.
 *
 * @param storage where the reader's choices are kept
 * @param systemDark whether the system asks for a dark theme
 */
export function resolvePreferences(
	storage: PreferenceStorage | null | undefined,
	systemDark: boolean,
): ThemePreferences {
	const stored = readStoredPreferences(storage);
	return {
		theme: stored.theme ?? (systemDark ? "dark" : "light"),
		contrast: stored.contrast ?? defaultPreferences.contrast,
		density: stored.density ?? defaultPreferences.density,
		motion: stored.motion ?? defaultPreferences.motion,
	};
}

/**
 * Whether the reader has chosen a theme of their own.
 *
 * A surface that follows the system watches the system's colour scheme only
 * while this is false: a reader who has chosen is not overridden by their
 * operating system changing at sunset.
 */
export function hasChosenTheme(storage: PreferenceStorage | null | undefined): boolean {
	return readStoredPreferences(storage).theme !== undefined;
}

/** The preferences with the theme switched. */
export function withToggledTheme(preferences: ThemePreferences): ThemePreferences {
	const theme: ThemeName = preferences.theme === "dark" ? "light" : "dark";
	return { ...preferences, theme };
}

/** The preferences with the contrast switched. */
export function withToggledContrast(preferences: ThemePreferences): ThemePreferences {
	const contrast: ContrastPreference = preferences.contrast === "high" ? "normal" : "high";
	return { ...preferences, contrast };
}

/** The preferences with the density switched. */
export function withToggledDensity(preferences: ThemePreferences): ThemePreferences {
	const density: DensityPreference = preferences.density === "compact" ? "comfortable" : "compact";
	return { ...preferences, density };
}

/** The preferences with the motion switched. */
export function withToggledMotion(preferences: ThemePreferences): ThemePreferences {
	const motion: MotionPreference = preferences.motion === "reduced" ? "system" : "reduced";
	return { ...preferences, motion };
}
