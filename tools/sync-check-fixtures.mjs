/**
 * Refresh the base64 fixtures embedded in the browser check page.
 *
 * The check page carries its PDFs as base64 literals rather than reading them
 * from disk: the page is served from a scratch directory by the harness, and a
 * file it has to fetch is a file the fetch can fail on. So when
 * `tools/make-tagged-fixture.mjs` regenerates a fixture, this script puts the
 * new bytes into the page.
 *
 * Run: node tools/sync-check-fixtures.mjs
 */
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const pagePath = join(repoRoot, "xtask", "browser", "app-shell", "index.html");

/** The page's literals, and the fixture each one is the bytes of. */
const FIXTURES = [
	{ name: "TAGGED_PDF_B64", path: join(repoRoot, "corpus", "fixtures", "tagged_structure.pdf") },
];

let page = readFileSync(pagePath, "utf8");
for (const fixture of FIXTURES) {
	const encoded = readFileSync(fixture.path).toString("base64");
	const pattern = new RegExp(`(var ${fixture.name} =\\s*\\n\\s*")[^"]*(")`, "m");
	if (!pattern.test(page)) {
		throw new Error(`the page has no ${fixture.name} literal to replace`);
	}
	page = page.replace(pattern, `$1${encoded}$2`);
	console.log(`sync-check-fixtures: ${fixture.name} <- ${fixture.path}`);
}
writeFileSync(pagePath, page, "utf8");
