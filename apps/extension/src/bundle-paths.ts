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

/**
 * The prefix this package's own compiled modules carry **in the shipped
 * package**, not only in the build workspace.
 *
 * This is not cosmetic and it is not a preference. `rootDir` is `apps/`, so
 * `tsc` emits `dist/pkg/extension/src/ext/adapter.js` and
 * `dist/pkg/ui/src/platform/errors.js` - it preserves the monorepo's shape
 * because the sources' relative specifiers are written against that shape and
 * `tsc` never rewrites them. For those specifiers to still resolve *inside the
 * package*, the package has to keep the same shape: the shared modules at
 * `ui/…` and this package's own at `extension/…`.
 *
 * ## What happens if this is dropped
 *
 * `dist/src/ext/adapter.js` writing `../../../ui/src/platform/errors.js`
 * resolves to `apps/extension/ui/…` - outside the package, and a 404 in the
 * browser. The bundled-only gate did **not** catch it, because
 * {@link normalisePackagePath} folds a leading `..` away and then finds
 * `ui/src/platform/errors.js` sitting happily in the ship list. `classifyRef`
 * now refuses a reference that escapes the package root, so the mistake is
 * loud rather than silent; that hardening and this prefix are the same fix
 * seen from two ends.
 */
export const OWN_PACKAGE_PREFIX = `${OWN_BUILD_TREE}/`;

/** Compiled `apps/ui` modules inside the build workspace, and their package path. */
export const SHARED_BUILD_TREE = "ui";

/**
 * Where a linked WASM artefact lives, relative to a cargo target directory.
 *
 * `cargo xtask size-check` builds every chunk with
 * `--target wasm32-unknown-unknown --release` and writes `wasm-opt -O3`
 * output next to the linked binary as `<stem>.opt.wasm`
 * (`xtask/src/size_check.rs`). Naming the same directory here is what makes
 * the bytes the packer ships the bytes `size-check` measured — a package
 * carrying an unoptimised binary while the budget is enforced on the
 * optimised one would be a budget for an artefact nobody runs.
 */
export const WASM_RELEASE_SUBDIR = "wasm32-unknown-unknown/release";

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
 * - `wasm-artifact` — a linked `.wasm` built by `cargo` for
 *   `wasm32-unknown-unknown` and optimised with `wasm-opt` (SL-4.EXT.05). It
 *   is the one row whose bytes no other row can derive, so its source is
 *   spelled out and its absence is a build failure rather than a skip.
 */
export type PackageSource = "root" | "build" | "shared-js" | "shared-asset" | "wasm-artifact";

/** One file in the shipped package: where it lands, and where it comes from. */
export interface PackageEntry {
	/** Path inside the built package (`dist/`), always with `/` separators. */
	readonly out: string;
	readonly from: PackageSource;
	/**
	 * Required for `shared-asset` and `wasm-artifact`: where the bytes come
	 * from, relative to a directory this package names. Assets are not
	 * compiled, so there is no build-tree path to derive and the path is
	 * spelled out — a stylesheet is `../ui/src/viewer/page-list.css`, and a
	 * WASM artefact is a file stem inside the cargo release directory
	 * ({@link WASM_RELEASE_SUBDIR}).
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
 * ### What SL-4.EXT.03 added, and what it deliberately did not
 *
 * Four compiled modules: the offscreen document''s composition root, the
 * `EnginePort` over the engine Worker, the guest ABI, and the Worker script
 * itself. The last one is the interesting row - it is loaded by
 * `new Worker(chrome.runtime.getURL(...))` rather than by an import, so no
 * other row implies it, and a Worker script the ship list forgot would 404 in
 * the browser while every gate stayed green. `engine-host.test.ts` asserts the
 * path the code uses is a row here.
 *
 * **The core `.wasm` is on this list, and it is SL-4.EXT.05's row.** The
 * engine EXT.03 hosts is a real WASM guest; without these bytes the viewer
 * renders nothing, which is why the row exists and why the engine's own
 * "missing path" failure became a build failure instead. The bytes come from
 * the `wasm32-unknown-unknown` release build of `selis-pdf-wasm` after
 * `wasm-opt -O3` — the same artefact `cargo xtask size-check` measures, so the
 * bytes that ship and the bytes that are budgeted are the same bytes.
 *
 * The five lazy chunks are **not** rows. `jpx`/`cjk` are fetched on demand in
 * the web app; in the extension there is no origin to fetch them from
 * (`host_permissions` is `[]`), so shipping them would spend package budget on
 * a viewer session that mostly never asks. `ocr`/`convert`/`editor` are
 * `viewerAuto: false` and must never reach a viewer at all (WASM.02).
 * `size-budget.ts` fails the build if one of them appears in the package, so
 * "we did not ship it" is a checked fact rather than a claim in a comment.
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
	{ out: "extension/src/permissions.js", from: "build" },
	{ out: "extension/src/viewer-boot.js", from: "build" },
	{ out: "extension/src/ext/adapter.js", from: "build" },
	{ out: "extension/src/ext/engine-host.js", from: "build" },
	{ out: "extension/src/ext/wasm-engine.js", from: "build" },
	{ out: "extension/src/ext/wasm-guest.js", from: "build" },
	// Loaded by `new Worker(chrome.runtime.getURL(...))` from the offscreen
	// document, so it is shipped as a page would load it: a Worker script the
	// gate cannot see referenced would be a script the store upload omits.
	{ out: "extension/src/ext/wasm-worker.js", from: "build" },
	{ out: "extension/src/ext/engine-client.js", from: "build" },
	{ out: "extension/src/ext/engine-link.js", from: "build" },
	{ out: "extension/src/ext/engine-protocol.js", from: "build" },
	// SL-4.EXT.05: the CJK payload store. Nothing calls it yet — the transport
	// that would fill it is WASM.07's row and SL-3.FONT.10 has produced no
	// payload — so this is shipped dead code, deliberately and cheaply (~5 KB).
	// Shipping it is what makes "the payload is not a bundled asset" a
	// structural fact: the store the extension will use is here, and the
	// bytes it will hold are not. See `REUSE.md`.
	{ out: "extension/src/ext/cjk-payload.js", from: "build" },
	{ out: "extension/src/ext/host-env.js", from: "build" },
	{ out: "extension/src/ext/offscreen-engine.js", from: "build" },
	{ out: "extension/src/ext/surface.js", from: "build" },
	{ out: "extension/src/ext/viewer-session.js", from: "build" },
	{ out: "ui/src/platform/errors.js", from: "shared-js" },
	{ out: "ui/src/viewer/surface.js", from: "shared-js" },
	{ out: "ui/src/viewer/worker-surface.js", from: "shared-js" },
	// SL-4.EXT.05: the core WASM chunk. `wasm-worker.test.ts` already asserts
	// this path agrees with the WASM.02 manifest; `size-budget.test.ts` asserts
	// it agrees with the row below, so the ship list and the code that reads
	// it cannot disagree about where the engine lives.
	{ out: "wasm/selis_pdf_wasm.wasm", from: "wasm-artifact", source: "selis_pdf_wasm.opt.wasm" },
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

/**
 * The path of a package entry's bytes inside the build workspace, or `null`.
 *
 * For both compiled kinds that is simply `entry.out`, because the build
 * workspace mirrors the package (see {@link OWN_PACKAGE_PREFIX}): `tsc` emits
 * `dist/pkg/extension/…` and `dist/pkg/ui/…` from `rootDir: ".."`, and the
 * package copies those same paths. Deriving a prefix here would double it.
 */
export function buildSourceOf(entry: PackageEntry): string | null {
	switch (entry.from) {
		case "build":
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

/**
 * Normalise a package path: `\` folds to `/`, `.` and `..` segments resolve.
 *
 * A `..` that would step above the package root is **kept as `..`** rather than
 * folded away. Folding it was a real hole: a module at `src/ext/adapter.js`
 * importing `../../../ui/src/platform/errors.js` normalises to
 * `ui/src/platform/errors.js`, which *is* on the ship list, so the reference
 * passed — while in the browser it resolves above the package and 404s. The
 * caller turns a surviving `..` into an `unresolved-local-ref` finding.
 */
export function normalisePackagePath(path: string): string {
	const segments: string[] = [];
	for (const segment of path.replaceAll("\\", "/").split("/")) {
		if (segment === "" || segment === ".") {
			continue;
		}
		if (segment === "..") {
			if (segments.length === 0 || segments[segments.length - 1] === "..") {
				// Escapes the package. Preserve it so the escape is visible.
				segments.push("..");
				continue;
			}
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
	// A surviving `..` means the reference climbs out of the package. It can
	// never be a packaged file, so it is unresolved whatever the ship list
	// says - see `normalisePackagePath`.
	if (resolved === "" || resolved.startsWith("../")) {
		return { kind: "unresolved", value };
	}
	return shipped.has(resolved)
		? { kind: "packaged", path: resolved }
		: { kind: "unresolved", value };
}
