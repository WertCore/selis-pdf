/**
 * Local, typed feature flags and staged rollout (SL-4.SHIP.02).
 *
 * There is no remote config service and this module never fetches anything: a
 * flag's state comes only from the typed defaults, an explicit `set`, or an
 * explicit local override map (e.g. a per-install config file or a reader's
 * own settings). That keeps the product offline-first and stops a flag service
 * from becoming a telemetry channel (ADR-P0017). Unknown flag names and
 * non-boolean values are refused rather than silently ignored.
 *
 * "Staged rollout" is a local, deterministic bucketing helper over a stable
 * subject key. It is explicit — a caller must supply a percentage — and it is
 * reproducible, so a given subject is always in or out for a given flag. An
 * explicit override always wins over a rollout bucket.
 */

/** The closed set of flags the host understands. Add a flag here, not ad hoc. */
export const FEATURE_FLAGS = [
	"tools.extensionPanel",
	"ui.noUploadIndicator",
	"viewer.editDeepLink",
] as const;

/** A single feature flag. */
export type FeatureFlag = (typeof FEATURE_FLAGS)[number];

/** The complete on/off state of every flag. */
export type FlagState = Readonly<Record<FeatureFlag, boolean>>;

/**
 * The shipped default: everything off. A new flag is dark until someone
 * explicitly turns it on, which is what "staged" means here.
 */
export const DEFAULT_FLAGS: FlagState = {
	"tools.extensionPanel": false,
	"ui.noUploadIndicator": false,
	"viewer.editDeepLink": false,
};

function isFeatureFlag(value: string): value is FeatureFlag {
	return (FEATURE_FLAGS as readonly string[]).includes(value);
}

/** Mutable shape used while parsing; `FlagState` itself stays readonly. */
type MutableFlagOverrides = { -readonly [K in FeatureFlag]?: boolean };

/** Result of parsing a local override map. */
export interface FlagOverrideParse {
	readonly flags: Partial<FlagState>;
	/** Names that were not a known flag or not a boolean; never applied. */
	readonly rejected: readonly string[];
}

/**
 * Parse an untyped local source (a config object, query string map, etc.).
 * Unknown names and non-booleans are refused, so a typo cannot silently enable
 * an unrelated flag or disable the no-upload indicator.
 */
export function parseFlagOverrides(raw: Readonly<Record<string, unknown>>): FlagOverrideParse {
	const flags: MutableFlagOverrides = {};
	const rejected: string[] = [];
	for (const [name, value] of Object.entries(raw)) {
		if (!isFeatureFlag(name) || typeof value !== "boolean") {
			rejected.push(name);
			continue;
		}
		flags[name] = value;
	}
	return { flags, rejected };
}

/** The flag store's read/write surface. */
export interface FeatureFlagStore {
	isEnabled(flag: FeatureFlag): boolean;
	/** Flags currently on, in canonical order. */
	enabled(): readonly FeatureFlag[];
	snapshot(): FlagState;
	/** Explicit, local opt-in or opt-out. */
	set(flag: FeatureFlag, value: boolean): void;
}

/** An in-memory store. It persists nowhere and phones home to nobody. */
export class LocalFeatureFlags implements FeatureFlagStore {
	private state: Record<FeatureFlag, boolean> = { ...DEFAULT_FLAGS };

	isEnabled(flag: FeatureFlag): boolean {
		return this.state[flag] === true;
	}

	enabled(): readonly FeatureFlag[] {
		return FEATURE_FLAGS.filter((flag) => this.state[flag]);
	}

	snapshot(): FlagState {
		return { ...this.state };
	}

	set(flag: FeatureFlag, value: boolean): void {
		this.state[flag] = value;
	}
}

/** Create a store, optionally with explicitly typed overrides applied. */
export function createFeatureFlags(overrides?: Partial<FlagState>): FeatureFlagStore {
	const store = new LocalFeatureFlags();
	if (overrides !== undefined) {
		for (const flag of FEATURE_FLAGS) {
			const value = overrides[flag];
			if (value !== undefined) {
				store.set(flag, value);
			}
		}
	}
	return store;
}

/** Apply a parsed override map to a store; returns any rejected names. */
export function applyFlagOverrides(
	store: FeatureFlagStore,
	raw: Readonly<Record<string, unknown>>,
): readonly string[] {
	const parsed = parseFlagOverrides(raw);
	for (const flag of FEATURE_FLAGS) {
		const value = parsed.flags[flag];
		if (value !== undefined) {
			store.set(flag, value);
		}
	}
	return parsed.rejected;
}

/** One rollout decision, supplied explicitly by the host's local config. */
export interface RolloutConfig {
	readonly flag: FeatureFlag;
	/** A stable per-install or per-document key; no document bytes. */
	readonly subject: string;
	/** 0 = nobody, 100 = everybody; values outside are refused (off). */
	readonly percentage: number;
}

/** FNV-1a over `flag\0subject`, folded into a stable 0..99 bucket. */
export function rolloutBucket(flag: string, subject: string): number {
	const key = `${flag}\u0000${subject}`;
	let hash = 0x811c9dc5;
	for (let i = 0; i < key.length; i += 1) {
		hash ^= key.charCodeAt(i);
		hash = Math.imul(hash, 0x01000193);
	}
	return (hash >>> 0) % 100;
}

/**
 * Decide a staged rollout locally. Explicit opt-in wins; otherwise a finite
 * percentage is compared with the stable bucket. A missing, invalid, or
 * non-finite percentage fails closed (off).
 */
export function isRolloutEnabled(store: FeatureFlagStore, config: RolloutConfig): boolean {
	if (store.isEnabled(config.flag)) {
		return true;
	}
	const pct = config.percentage;
	if (!Number.isFinite(pct) || pct <= 0) {
		return false;
	}
	if (pct >= 100) {
		return true;
	}
	return rolloutBucket(config.flag, config.subject) < pct;
}
