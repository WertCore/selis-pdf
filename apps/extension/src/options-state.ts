/**
 * SL-4.EXT.07 — the options page's state: the settings it owns, and the
 * first-run flag that decides whether the welcome guide is shown.
 *
 * ## Two stores, and why there are two
 *
 * | Store | Holds | Why that one and not the other |
 * |---|---|---|
 * | `localStorage` on the extension origin ({@link HostStorage}) | the telemetry opt-in | It is the store the `PlatformAdapter`'s `storage` and `telemetry` ports already read and write (`ext/adapter.ts`), so there is one key name and one definition of "opted in". The options page writes the same key the viewer reads, and `ext/adapter.ts` imports the key from here rather than repeating it. |
 * | `chrome.storage.local` | the welcome-guide flag | An MV3 service worker has no `localStorage` — it is a `Window` API, and a `ServiceWorkerGlobalScope` is not a `Window` — and the worker is the only context that hears `chrome.runtime.onInstalled`. A flag the worker cannot read cannot gate anything, so this one lives in the store the `storage` permission (SL-4.EXT.05) already bought. |
 *
 * The options page does **not** build a `PlatformAdapter` to reach the first
 * store. `createExtensionAdapter` takes an `EngineLink` and hands back an
 * engine port, so an options page that opens no document would be paying for an
 * offscreen document it never uses. It takes the same {@link HostStorage} the
 * adapter's ports are built from, which is the seam, not a second one.
 *
 * ## The first-run rule, stated once
 *
 * {@link planFirstRun} is the whole trigger, and it is a pure function so that
 * both ways of getting it wrong are testable:
 *
 * - **Never nag.** The guide is offered on a *fresh install* only
 *   (`reason === "install"`), and only until it has been seen. An update, a
 *   browser upgrade, a shared-module update, and any reason this code does not
 *   recognise all decline — a user who has read the guide once must not meet it
 *   again on every version bump.
 * - **Never skip silently.** A flag that is missing, empty, or corrupt counts
 *   as *not seen*, so the guide is shown rather than lost. The failure
 *   direction is deliberate: an extra read of a page the user can close is
 *   recoverable, a user who never learns what the extension does is not.
 * - **Seen is recorded when the guide is rendered, not when it is dismissed.**
 *   The guide states facts and changes no setting, so there is no consent to
 *   withhold and no reason to make dismissal load-bearing. Marking it on render
 *   is what removes the nag loop entirely — including the case where the user
 *   closes the tab without reading it, which is the one that would otherwise
 *   reappear on every visit. "Show the welcome guide again" re-opens it
 *   deliberately, from memory, without rewriting stored state.
 */

import type { HostStorage } from "./ext/host-env.js";

/* ── Settings: the store the adapter's ports already use ───────────────────── */

/**
 * The settings store, named so this module never has to say `localStorage`.
 *
 * A type-only alias of the adapter's own storage port, so the options page and
 * the viewer provably write the same bytes. The import is erased at compile
 * time, which matters twice over: the service worker imports this module, and
 * it must not drag a DOM-facing module into a context that has no DOM.
 */
export type SettingsStore = HostStorage;

/**
 * Every settings key this extension owns, in one place.
 *
 * `telemetryOptIn` is the key `ext/adapter.ts` reads and writes; it is declared
 * here and imported *there* (not the other way round) because the options page
 * is what a user opens in order to change it, and a key only one of the two ends
 * knows about is a setting that silently resets.
 */
export const SETTINGS_KEYS = {
	/** `"1"` when the user has opted in to usage data; anything else is off. */
	telemetryOptIn: "selis.telemetry.enabled",
} as const;

/** Everything the options page can change. */
export interface Settings {
	/**
	 * Has the user opted in to usage data?
	 *
	 * Off unless the stored value is exactly {@link TELEMETRY_ON}. ADR-P0017:
	 * opt-in, and the default is the absence of a decision, never a decision to
	 * opt in.
	 */
	readonly telemetryOptIn: boolean;
}

/** What a user who has changed nothing has. */
export const DEFAULT_SETTINGS: Settings = { telemetryOptIn: false };

/** The one stored value that means "opted in". */
export const TELEMETRY_ON = "1";

/** Written when the user opts out, so the key is never absent afterwards. */
const TELEMETRY_OFF = "0";

/**
 * Is a stored opt-in flag on?
 *
 * Strictly `"1"`, and nothing else. A permissive parse (`"true"`, `1`, any
 * non-empty string) turns a value written by some future build, or a corrupted
 * one, into consent the user never gave — and the whole point of ADR-P0017 is
 * that the answer to "may I collect?" is only ever yes when it was written as
 * yes.
 */
export function isTelemetryOptIn(stored: string | null): boolean {
	return stored === TELEMETRY_ON;
}

/** Read every setting, filling in the default for whatever is not stored. */
export async function readSettings(store: SettingsStore): Promise<Settings> {
	const stored = await store.get(SETTINGS_KEYS.telemetryOptIn);
	return { ...DEFAULT_SETTINGS, telemetryOptIn: isTelemetryOptIn(stored) };
}

/** Record (or withdraw) the telemetry opt-in. */
export async function writeTelemetryOptIn(store: SettingsStore, enabled: boolean): Promise<void> {
	await store.set(SETTINGS_KEYS.telemetryOptIn, enabled ? TELEMETRY_ON : TELEMETRY_OFF);
}

/**
 * Put every setting back to its default.
 *
 * Keys are removed rather than written as "0", so a setting this build has
 * since stopped owning cannot be left behind holding a value no code reads.
 */
export async function resetSettings(store: SettingsStore): Promise<void> {
	for (const key of Object.values(SETTINGS_KEYS)) {
		await store.remove(key);
	}
}

/* ── The first-run flag ────────────────────────────────────────────────────── */

/** Where the welcome-guide flag lives in `chrome.storage.local`. */
export const ONBOARDING_KEY = "selis.onboarding.v1";

/** The only stored value that means "the guide has been shown". */
export const ONBOARDING_SEEN = "seen";

/** What the flag says. */
export interface OnboardingState {
	readonly seen: boolean;
}

/** A user who has never been shown the guide. */
export const DEFAULT_ONBOARDING: OnboardingState = { seen: false };

/**
 * Has the guide been shown, according to a raw stored value?
 *
 * Anything that is not exactly {@link ONBOARDING_SEEN} counts as "not seen" —
 * `undefined`, `null`, `""`, a value written by an older build, a boolean, an
 * object. The alternative (treat anything present as seen) loses the guide
 * permanently the first time a value is written by something other than this
 * function, and the user has no way to get it back.
 */
export function onboardingSeenFrom(stored: unknown): boolean {
	return stored === ONBOARDING_SEEN;
}

/** The `chrome.runtime.onInstalled` reasons this code gives a name to. */
export type InstallReason = "install" | "update" | "chrome_update" | "shared_module_update";

/**
 * Why the guide was or was not offered. A code, not a sentence.
 *
 * A machine-readable outcome rather than prose, so that the service worker's log
 * line is not an untranslated user-facing string wearing a console's clothes.
 */
export type FirstRunOutcome =
	/** A fresh install, and the guide has not been seen. Show it. */
	| "fresh-install"
	/** A fresh install, but the guide was already shown. Do not nag. */
	| "already-seen"
	/** Not a fresh install (update, browser upgrade, unknown). Do not nag. */
	| "not-first-run";

/** The decision, and the code that explains it. */
export interface FirstRunPlan {
	readonly show: boolean;
	readonly outcome: FirstRunOutcome;
}

/**
 * Should this install event offer the welcome guide?
 *
 * The entire trigger, as a pure function of what the browser reported and what
 * the flag says. `reason` is taken as a plain `string` rather than
 * {@link InstallReason} because Chrome is the one producing it: an unrecognised
 * reason is a browser that has learned something new since this was written,
 * and the correct response to that is to *decline*, not to guess.
 */
export function planFirstRun(input: { reason: string; seen: boolean }): FirstRunPlan {
	if (input.reason !== "install") {
		return { show: false, outcome: "not-first-run" };
	}
	return input.seen
		? { show: false, outcome: "already-seen" }
		: { show: true, outcome: "fresh-install" };
}

/* ── The `chrome.storage.local` binding ───────────────────────────────────── */

/** Reading and writing the flag, with nothing browser-specific in the type. */
export interface OnboardingStore {
	/** What the flag says. Rejects when the store is unavailable. */
	read(): Promise<OnboardingState>;
	/** Record that the guide has been shown. Rejects when it cannot be recorded. */
	markSeen(): Promise<void>;
}

/** The `chrome.storage.StorageArea` calls this module makes. */
interface ChromeStorageArea {
	get(key: string): Promise<Record<string, unknown>>;
	set(items: Record<string, unknown>): Promise<void>;
}

/**
 * The flag, in `chrome.storage.local`.
 *
 * `chrome` is read inside the factory and never at module scope, so this file
 * stays importable from the plain Node the test suite runs in — and so a context
 * without the API (a test, a port that has not exposed it) gets an explicit
 * failure rather than a `TypeError` at import time.
 *
 * The unavailable case **rejects** rather than resolving to a default. A default
 * would have to be either "seen" (the guide is lost, silently) or "not seen"
 * (the guide reappears on every visit, forever) — and the caller can do better
 * than both, because it knows whether it is a page that can simply show the
 * guide once and carry on.
 */
export function createOnboardingStore(area?: ChromeStorageArea): OnboardingStore {
	const unavailable = (): Promise<never> =>
		Promise.reject(
			new Error(
				"chrome.storage.local is unavailable, so the welcome-guide flag cannot be read or written",
			),
		);
	const storage =
		area ??
		(globalThis as { chrome?: { storage?: { local?: ChromeStorageArea } } }).chrome?.storage?.local;
	if (storage === undefined) {
		return { read: unavailable, markSeen: unavailable };
	}
	return {
		async read() {
			const record = await storage.get(ONBOARDING_KEY);
			return { seen: onboardingSeenFrom(record[ONBOARDING_KEY]) };
		},
		async markSeen() {
			await storage.set({ [ONBOARDING_KEY]: ONBOARDING_SEEN });
		},
	};
}
