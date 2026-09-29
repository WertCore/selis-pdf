/**
 * SL-4.UI.11 — the gate that keeps the runtime *shippable*.
 *
 * `apps/extension` reuses `apps/ui` by relative path, and every module it takes
 * is a row in its ship list (`bundle-paths.ts`, `REUSE.md`). EXT.07 recorded
 * that it wrote its own copy of the i18n seam rather than import the viewer's,
 * for one reason: `apps/ui`'s `formatMessage` was module-private, and reaching
 * it would have put the whole page-list and search string set into a package
 * that has no page list — the trade `REUSE.md` already refused once when it
 * declined to ship `page-list.css`.
 *
 * UI.11 removes that reason by splitting the runtime in two: `message.ts` and
 * `runtime.ts` know nothing about the viewer, and `viewer/strings.ts` is what
 * registers the catalogues. The extension then needs two ship rows and one
 * import, and gets a runtime rather than a copy.
 *
 * **That is only true while this file passes.** One convenient import — the
 * viewer's `PageFitMode`, the ui-kit's `VERSION`, a shared constant — and the
 * runtime starts dragging the viewer into a package with no viewer, which is
 * the exact regression `REUSE.md` documents. So the constraint is a test rather
 * than a review comment, in the same idiom as
 * `../platform/platform-globals.test.ts` and `viewer/strings.test.ts`.
 *
 * ## What is checked, and what is not
 *
 * Checked: the two files' import specifiers, transitively within `i18n/`, and
 * the absence of any bare or parent specifier. That is the whole closure that
 * matters, because the shipped rows are exactly these two files.
 *
 * Not checked: the compiled byte size, which the extension's own size gate
 * (`SIZE.md`, 1 MB for everything but the engine) already bounds, and which
 * would fail there rather than here for a reason no reader of this file could
 * act on.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));

/** The two modules the extension's ship list would name. */
const SHIPPABLE = ["message.ts", "runtime.ts"] as const;

/** Every relative specifier one file imports, comments and strings removed. */
function importsOf(file: string): string[] {
	const source = readFileSync(join(here, file), "utf8")
		.replace(/\/\*[\s\S]*?\*\//g, "")
		.replace(/(^|[^:"'`\\])\/\/[^\n]*/g, "$1");
	return [...source.matchAll(/(?:from|import)\s*"([^"]+)"/g)].map((match) => match[1] ?? "");
}

describe("the i18n runtime boundary (SL-4.UI.11)", () => {
	it("imports nothing outside i18n/, so it can ship without the viewer", () => {
		const offenders: string[] = [];
		for (const file of SHIPPABLE) {
			for (const specifier of importsOf(file)) {
				// A relative specifier that climbs out of this directory, or a bare
				// one at all, drags a module the extension would then have to ship.
				if (!specifier.startsWith("./") || specifier.includes("../")) {
					offenders.push(`${file}: ${specifier}`);
				}
			}
		}
		expect(offenders, `runtime imports to keep out: ${offenders.join("; ")}`).toEqual([]);
	});

	it("keeps message.ts free of even the runtime", () => {
		// The pure half has to stay pure: a caller that wants formatting and
		// nothing else — a DOM fill, the options page — takes this one file, and
		// the moment it needs the runtime to format, the split has been undone
		// without anybody deciding to undo it.
		expect(importsOf("message.ts")).toEqual([]);
	});

	it("has the runtime depend on exactly the message module", () => {
		expect(importsOf("runtime.ts")).toEqual(["./message.js"]);
	});

	it("keeps the viewer registration on the viewer's side of the line", () => {
		// The other half of the same property: the catalogues are the viewer's, so
		// they live in the viewer, and `i18n/` never reaches back for them.
		const source = readFileSync(join(here, "..", "viewer", "strings.ts"), "utf8");
		const specifiers = [...source.matchAll(/(?:from|import)\s*"([^"]+)"/g)].map(
			(match) => match[1] ?? "",
		);
		expect(specifiers).toContain("../i18n/runtime.js");
		expect(specifiers).toContain("../i18n/pseudo-locale.js");
	});
});
