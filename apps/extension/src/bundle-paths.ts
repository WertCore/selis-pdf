/**
 * SL-4.EXT.04 - what is in the shipped package, and how a reference resolves.
 *
 * The ship list is the whole contract. `tools/pack.mjs` copies exactly these
 * files out of the build, the gate scans exactly these files, and anything
 * else found in the package directory fails the build. Code becomes shippable
 * by adding a row here, which is the moment "is this remote code?" gets asked.
 *
 * See `bundle-scan.ts` for the gate itself and `BUNDLING.md` for the policy.
 */

/**
 * Directory inside `dist/` that holds the raw `tsc` output.
 *
 * A build workspace, not part of the package: `tools/pack.mjs` copies the
 * declared {@link PACKAGE_ENTRIES} out of it, and the gate fails on anything
 * else that survives beside them. Test files and the gate's own modules live
 * here, so they can never reach the store.
 */
export const BUILD_WORKSPACE = "pkg";

/**
 * SL-4.EXT.06: the build workspace holds **two** compiled trees, because the
 * extension reuses `apps/ui` verbatim (SL-4.UI.01's `PlatformAdapter` seam) and
 * `tsc` compiles both in one program.
 *
 * `rootDir` is `apps/`, so the emitted layout under `dist/pkg/` mirrors the
 * monorepo: this package's own sources under {@link OWN_BUILD_TREE}, the shared
 * UI modules under {@link SHARED_BUILD_TREE}. That is not cosmetic — the
 * relative specifiers the extension's sources write (`../../ui/src/...`) are
 * never rewritten by `tsc`, so the package has to keep the same shape for them
 * to resolve inside it. A row in {@link PACKAGE_ENTRIES} that renames either
 * tree's prefix therefore has to be accompanied by an edit to the sources that
 * import across it, and the gate fails loudly if it is not.
 */
export const OWN_BUILD_TREE = "extension";

/** Compiled `apps/ui` modules inside the build workspace, and their package path. */
export const SHARED_BUILD_TREE = "ui";

/**
 * Where a shipped file's bytes come from. Each kind is a different trust
 * question, which is why they are named rather than unified:
 *
 * - `root` — a hand-written file of this package (manifest, pages, the service
 *   worker). Not compiled, so it is not typechecked; read it in review.
 * - `build` — a module `tsc` compiled from this package's own `src/`.
 * - `shared-js` — a module `tsc` compiled from `apps/ui`, shipped because the
 *   extension *reuses* it rather than forking it (SL-4.EXT.06). It is the same
 *   source the web app runs; a change in `apps/ui` lands in the extension
 *   package on the next build, which is the point and also the risk.
 * - `shared-asset` — a non-JS file copied from `apps/ui` (the stylesheets).
 */
export type PackageSource = "root" | "build" | "shared-js" | "shared-asset";

/** One file in the shipped package: where it lands, and where it comes from. */
export interface PackageEntry {
	/** Path inside the built package (`dist/`), always with `/` separators. */
	readonly out: string;
	readonly from: PackageSource;
	/**
	 * Required for `shared-asset`: the file's path relative to this package's
	 * root, e.g. `../ui/src/viewer/page-list.css`. Assets are not compiled, so
	 * there is no build-tree path to derive and the path is spelled out.
	 */
	readonly source?: string;
}

/**
 * The complete shipped package, declared in one place.
 *
 * Adding a module means adding it here, and `bundle.test.ts` fails when a row
 * names a file the build does not produce or a packaged page references.
 *
 * ## The `shared-*` rows are the EXT.06 reuse, and they are the interesting ones
 *
 * `apps/ui` is not forked into this package. The three modules below are
 * compiled from `apps/ui/src` by this package's own `tsc` program and copied to
 * `ui/src/…`, which is exactly where the relative specifiers in this package's
 * sources point (`src/ext/adapter.ts` writes
 * `../../../ui/src/platform/errors.js`). They are the whole of what the
 * extension borrows at runtime, and the list is short on purpose:
 *
 * - `platform/errors.ts` — `AdapterError` / `ErrorCode`. The extension refuses
 *   several capabilities (clipboard read, save-in-place, deep links), and a
 *   refusal that invented its own error type would break the one rule the seam
 *   has: every rejection is a registry code with a `docState`.
 * - `viewer/surface.ts` — the `TileSurface` / `FrameClock` ports, and
 *   `SurfaceFaultError`. Pulled in at runtime by the row below.
 * - `viewer/worker-surface.ts` — `createAnimationFrameClock`, the one piece of
 *   UI.03's worker surface the extension uses verbatim, because a frame clock is
 *   a frame clock and re-deriving it here would be the second implementation
 *   the seam exists to prevent.
 *
 * Nothing else from `apps/ui` ships, and that is a decision rather than an
 * oversight. The rest of the UI is either type-only (erased at compile, so
 * costing nothing) or belongs to a viewer this package does not contain yet —
 * the virtualised page list and its stylesheet are UI.04+'s to mount, and
 * shipping them now would put a half-built viewer in a package that
 * SL-4.EXT.05 has to fit a size budget.
 *
 * ### One row ships more than the extension uses
 *
 * `worker-surface.js` also contains `createWorkerSurface`, which the extension
 * never calls: it needs an `OffscreenCanvas` transferred to a worker, and a
 * `chrome.runtime` port has no transfer list (see `src/ext/surface.ts` and
 * `REUSE.md`). It cannot be dropped without a bundler, and ADR-P0021 rules one
 * out. Shipping it is cheaper than the alternative, and the call site that would
 * make it live does not exist — which is what the reuse doc records.
 *
 * The stylesheets are `@selis/ui-kit`'s, copied verbatim (`shared-asset`): the
 * same no-bundler reason, so a shell links them, and `tokens.css` is generated
 * so it is never hand-edited (UI.14). No font is fetched — the stacks are
 * `system-ui` and friends, which is also why the CJK payload is a post-install
 * download (EXT.05).
 */
export const PACKAGE_ENTRIES: readonly PackageEntry[] = [
	{ out: "manifest.json", from: "root" },
	{ out: "viewer.html", from: "root" },
	{ out: "offscreen.html", from: "root" },
	{ out: "service-worker.js", from: "root" },
	{ out: "offscreen.js", from: "root" },
	{ out: "src/permissions.js", from: "build" },
	{ out: "src/viewer-boot.js", from: "build" },
	{ out: "src/ext/adapter.js", from: "build" },
	{ out: "src/ext/engine-client.js", from: "build" },
	{ out: "src/ext/engine-link.js", from: "build" },
	{ out: "src/ext/engine-protocol.js", from: "build" },
	{ out: "src/ext/host-env.js", from: "build" },
	{ out: "src/ext/offscreen-engine.js", from: "build" },
	{ out: "src/ext/surface.js", from: "build" },
	{ out: "src/ext/viewer-session.js", from: "build" },
	{ out: "ui/src/platform/errors.js", from: "shared-js" },
	{ out: "ui/src/viewer/surface.js", from: "shared-js" },
	{ out: "ui/src/viewer/worker-surface.js", from: "shared-js" },
	{
		out: "ui-kit/css/tokens.css",
		from: "shared-asset",
		source: "../../packages/ui-kit/css/tokens.css",
	},
	{
		out: "ui-kit/css/base.css",
		from: "shared-asset",
		source: "../../packages/ui-kit/css/base.css",
	},
];

/** The path of a package entry's bytes inside the build workspace, or `null`. */
export function buildSourceOf(entry: PackageEntry): string | null {
	switch (entry.from) {
		case "build":
			return `${OWN_BUILD_TREE}/${entry.out}`;
		case "shared-js":
			return entry.out;
		default:
			return null;
	}
}

/** Every path in the shipped package, in declaration order. */
export const SHIPPED_FILES: readonly string[] = PACKAGE_ENTRIES.map((entry) => entry.out);

/** The ship list as a lookup set. */
export function shippedSet(paths: readonly string[] = SHIPPED_FILES): Set<string> {
	return new Set(paths);
}

/**
 * Schemes a packaged file may legitimately spell out, as bare scheme names.
 *
 * No trailing colon: `classifyRef` matches `^([a-zA-Z][a-zA-Z0-9+.-]*):` and
 * looks the captured name up here. Entries written as `"data:"` never matched,
 * which silently classified every `data:` and `blob:` reference as remote — a
 * false positive on exactly the URLs an extension legitimately produces at
 * runtime.
 */
const LOCAL_SCHEMES: ReadonlySet<string> = new Set([
	"blob",
	"data",
	"filesystem",
	"chrome-extension",
	"moz-extension",
	"extension",
	"about",
	"chrome",
]);

/** How a reference written inside a packaged file resolves. */
export type RefClass =
	/** It resolves to a file that is in the package. */
	| { readonly kind: "packaged"; readonly path: string }
	/** A local scheme (`blob:`, `data:`, ...): created at runtime, not shipped. */
	| { readonly kind: "scheme-local" }
	/** Off-package: remote, protocol-relative, or a scheme we do not own. */
	| { readonly kind: "remote"; readonly value: string }
	/** Package-relative, but no such file ships. */
	| { readonly kind: "unresolved"; readonly value: string };

/** 1-based line number of `index` in `text`. */
export function lineOf(text: string, index: number): number {
	let line = 1;
	const limit = Math.min(Math.max(index, 0), text.length);
	for (let i = 0; i < limit; i += 1) {
		if (text.charCodeAt(i) === 10) {
			line += 1;
		}
	}
	return line;
}

/** Normalise a package path: `\` folds to `/`, `.` and `..` segments resolve. */
export function normalisePackagePath(path: string): string {
	const segments: string[] = [];
	for (const segment of path.replaceAll("\\", "/").split("/")) {
		if (segment === "" || segment === ".") {
			continue;
		}
		if (segment === "..") {
			segments.pop();
			continue;
		}
		segments.push(segment);
	}
	return segments.join("/");
}

/** Directory part of a package path (`""` for a root-level file). */
export function dirOf(path: string): string {
	const index = path.lastIndexOf("/");
	return index < 0 ? "" : path.slice(0, index);
}

/**
 * Classify `ref`, written inside `fromFile`, against the shipped file set.
 *
 * A scheme the package does not own counts as remote even when it is not
 * http(s): it cannot be a packaged file, and "unrecognised" is exactly the
 * state in which a CDN reference hides. `blob:`/`data:` stay local even when
 * they embed the creating origin (`blob:https://example.com/uuid`).
 */
export function classifyRef(fromFile: string, ref: string, shipped: ReadonlySet<string>): RefClass {
	const value = ref.trim();
	if (value.startsWith("//")) {
		return { kind: "remote", value };
	}
	const scheme = /^([a-zA-Z][a-zA-Z0-9+.-]*):/.exec(value)?.[1];
	if (scheme !== undefined) {
		return LOCAL_SCHEMES.has(scheme.toLowerCase())
			? { kind: "scheme-local" }
			: { kind: "remote", value };
	}
	if (value.startsWith("#")) {
		return { kind: "scheme-local" };
	}
	const resolved = normalisePackagePath(
		value.startsWith("/") ? value : `${dirOf(fromFile)}/${value}`,
	);
	return shipped.has(resolved)
		? { kind: "packaged", path: resolved }
		: { kind: "unresolved", value };
}
