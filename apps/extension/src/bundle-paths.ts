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

/** One file in the shipped package: where it lands, and where it comes from. */
export interface PackageEntry {
	/** Path inside the built package (`dist/`), always with `/` separators. */
	readonly out: string;
	/**
	 * `root` = a hand-written file copied from the package source directory;
	 * `build` = a compiled module copied from {@link BUILD_WORKSPACE} under
	 * its basename (`src/permissions.js` <- `permissions.js`).
	 */
	readonly from: "root" | "build";
}

/**
 * The complete shipped package, declared in one place.
 *
 * Adding a module means adding it here, and `bundle.test.ts` fails when a row
 * names a file the build does not produce or a packaged page references.
 */
export const PACKAGE_ENTRIES: readonly PackageEntry[] = [
	{ out: "manifest.json", from: "root" },
	{ out: "viewer.html", from: "root" },
	{ out: "offscreen.html", from: "root" },
	{ out: "service-worker.js", from: "root" },
	{ out: "offscreen.js", from: "root" },
	{ out: "src/permissions.js", from: "build" },
	{ out: "src/viewer-boot.js", from: "build" },
];

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
