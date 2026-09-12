/**
 * Theming: how user preferences become the data attributes the generated CSS
 * keys on. Deliberately DOM-free — `applyTheme` takes the element to update,
 * so tests and non-browser hosts can drive it without platform globals.
 */

export type ThemeName = "light" | "dark";
export type ContrastPreference = "normal" | "high";
export type DensityPreference = "comfortable" | "compact";
export type MotionPreference = "system" | "reduced";

export interface ThemePreferences {
	theme: ThemeName;
	contrast: ContrastPreference;
	density: DensityPreference;
	motion: MotionPreference;
}

/** Default preference set (light, normal contrast, comfortable, system motion). */
export const defaultPreferences: ThemePreferences = {
	theme: "light",
	contrast: "normal",
	density: "comfortable",
	motion: "system",
};

/**
 * The attribute values for a preference set. `comfortable` density and
 * `system` motion emit no attribute: the generated CSS defaults to them.
 */
export function themeAttributeValues(prefs: ThemePreferences): Record<string, string> {
	const attrs: Record<string, string> = {
		"data-theme": prefs.theme,
	};
	if (prefs.contrast === "high") {
		attrs["data-contrast"] = "high";
	}
	if (prefs.density !== "comfortable") {
		attrs["data-density"] = prefs.density;
	}
	if (prefs.motion === "reduced") {
		attrs["data-motion"] = "reduced";
	}
	return attrs;
}

/** Minimal element surface `applyTheme` needs (keeps tests DOM-free). */
export interface AttributeElement {
	setAttribute(qualifiedName: string, value: string): void;
}

/** Apply the preference set to an element (typically the document root). */
export function applyTheme(prefs: ThemePreferences, element: AttributeElement): void {
	for (const [name, value] of Object.entries(themeAttributeValues(prefs))) {
		element.setAttribute(name, value);
	}
}
