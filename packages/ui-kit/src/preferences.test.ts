/**
 * The preferences the reader's choices become (pdf-plan/41, slice 8).
 *
 * The decisions here are shared by every surface — the web app, a desktop
 * app, an SDK embedding the design system — so they are tested on their own,
 * with the storage and the system preference supplied as arguments.
 */
import { describe, expect, it } from "vitest";
import {
	CONTRAST_STORAGE_KEY,
	DENSITY_STORAGE_KEY,
	MOTION_STORAGE_KEY,
	type PreferenceStorage,
	THEME_STORAGE_KEY,
	hasChosenTheme,
	readStoredPreferences,
	resolvePreferences,
	withToggledContrast,
	withToggledDensity,
	withToggledMotion,
	withToggledTheme,
	writeStoredPreferences,
} from "./preferences.js";
import { defaultPreferences } from "./theming.js";

/** A storage that keeps what it is given. */
function memoryStorage(initial: Record<string, string> = {}): PreferenceStorage & {
	readonly values: Map<string, string>;
} {
	const values = new Map(Object.entries(initial));
	return {
		values,
		getItem: (key) => values.get(key) ?? null,
		setItem: (key, value) => {
			values.set(key, value);
		},
	};
}

/** A storage that refuses every operation, like a browser in private mode. */
function refusingStorage(): PreferenceStorage {
	return {
		getItem: () => {
			throw new Error("no");
		},
		setItem: () => {
			throw new Error("no");
		},
	};
}

describe("the reader's preferences", () => {
	it("follows the system until the reader chooses", () => {
		expect(resolvePreferences(memoryStorage(), true).theme).toBe("dark");
		expect(resolvePreferences(memoryStorage(), false).theme).toBe("light");
		expect(resolvePreferences(null, true).theme).toBe("dark");
	});

	it("keeps the reader's choice whatever the system says", () => {
		const storage = memoryStorage({ [THEME_STORAGE_KEY]: "light" });
		expect(resolvePreferences(storage, true).theme).toBe("light");
		expect(hasChosenTheme(storage)).toBe(true);
	});

	it("treats a stale key as no choice at all", () => {
		// A value that is not a theme name must not leave a surface in a state
		// nobody can name.
		const storage = memoryStorage({ [THEME_STORAGE_KEY]: "chartreuse" });
		expect(readStoredPreferences(storage).theme).toBeUndefined();
		expect(resolvePreferences(storage, true).theme).toBe("dark");
		expect(hasChosenTheme(storage)).toBe(false);
	});

	it("survives a storage that refuses to be read or written", () => {
		expect(readStoredPreferences(refusingStorage())).toEqual({});
		expect(resolvePreferences(refusingStorage(), true).theme).toBe("dark");
		expect(() => writeStoredPreferences(defaultPreferences, refusingStorage())).not.toThrow();
		expect(() => writeStoredPreferences(defaultPreferences, null)).not.toThrow();
	});

	it("remembers every preference it is given", () => {
		const storage = memoryStorage();
		writeStoredPreferences(
			{ theme: "dark", contrast: "high", density: "compact", motion: "reduced" },
			storage,
		);
		expect(storage.values.get(THEME_STORAGE_KEY)).toBe("dark");
		expect(storage.values.get(CONTRAST_STORAGE_KEY)).toBe("high");
		expect(storage.values.get(DENSITY_STORAGE_KEY)).toBe("compact");
		expect(storage.values.get(MOTION_STORAGE_KEY)).toBe("reduced");
		expect(readStoredPreferences(storage)).toEqual({
			theme: "dark",
			contrast: "high",
			density: "compact",
			motion: "reduced",
		});
	});

	it("switches each preference on its own, and leaves the rest alone", () => {
		const preferences = defaultPreferences;
		expect(withToggledTheme(preferences).theme).toBe("dark");
		expect(withToggledTheme(preferences).contrast).toBe(preferences.contrast);
		expect(withToggledTheme({ ...preferences, theme: "dark" }).theme).toBe("light");
		expect(withToggledContrast(preferences).contrast).toBe("high");
		expect(withToggledDensity(preferences).density).toBe("compact");
		expect(withToggledMotion(preferences).motion).toBe("reduced");
	});
});
