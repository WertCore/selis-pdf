/**
 * SL-4.WEB.02 - copy the app's ESM modules and the WASM engine into `public/`.
 *
 * The DoD is "open a local PDF, view, search, print" with no network, and two
 * things were needed for that which `public/` did not have:
 *
 *   1. The engine. `apps/web/host/public/` shipped NO `.wasm` at all, so there
 *      was nothing to open a PDF with even once a viewer existed.
 *   2. The viewer. `@selis/ui` was imported zero times across the host, so the
 *      shipped UI (UI.02-UI.05) was unreachable from any host.
 *
 * ## Why this is a COPY and not a bundle
 *
 * The obvious move is a bundler, and the absence of one looked like the real
 * blocker. It is not. Every shipped `.js` under `apps/ui/dist` and
 * `packages/ui-kit/dist` is already valid browser ESM (`tsc` emits
 * `module: ESNext`), and the ONLY bare specifier in the whole closure is
 * `import { VERSION } from "@selis/ui-kit"` in `apps/ui/dist/index.js`.
 * `@selis/ui-kit` is a workspace package whose own dist has no bare imports,
 * so mapping it to `/assets/ui-kit/` is a rename, not a resolution.
 *
 * So the page is served as the same bundle-free, same-origin tree the service
 * worker already is. That is not a compromise - it is the rule ADR-P0016 and
 * ADR-P0028 ask for, applied consistently:
 *
 *   - Nothing new is vendored, so no supply-chain surface is added.
 *   - Every shipped byte is the reviewed `tsc` output, byte for byte. A
 *     bundler emits code no human read, which is the same objection
 *     `tools/pack.mjs` and `place-sw.mjs` already record for their outputs.
 *   - The import walk below is the declaration. A file not reachable from an
 *     entry cannot appear in `public/`, so the shipped page cannot quietly
 *     grow a dependency the review did not see.
 *
 * The cost is honest and worth stating: the browser fetches the module graph
 * as ~40 separate requests rather than one.
 *
 * ## Offline: what is precached, and what is deliberately not
 *
 * `sw-policy.ts`'s `PRECACHE_PATHS` is `/`, `/index.html`, `/assets/style.css`,
 * `/assets/boot.js`. This script does NOT change that, and the reason matters:
 * `addAll` is atomic, so a precache list naming a file the build did not emit
 * fails the whole install and leaves the app online-only forever with no error
 * anywhere. Adding 40 module files would make every one of them a way to break
 * offline by editing a file.
 *
 * The viewer modules are served from `/app/` and the engine from `/wasm/`,
 * which are prefixes the runtime cache already accepts, so both populate on
 * first use and are then available offline - the same on-demand path WEB.02's
 * caching-policy note already chose for the WASM ("fetched once on demand into
 * the runtime cache"). First load needs a network; subsequent loads do not.
 *
 * Run through `pnpm --filter @selis/web-host build` (after `tsc`).
 */

import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// The deployment root is `public/` (that is what `public/_headers` and
// `vercel.json` describe).
const pkgRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const publicRoot = join(pkgRoot, "public");
// This package is nested (`apps/web/host`), so the workspace root is three
// levels up, not two. Deriving it from this file's own location rather than
// `process.cwd()` is deliberate: the build must not depend on where it was
// invoked from.
const workspaceRoot = resolve(pkgRoot, "../../..");

/**
 * The ESM roots copied into `public/`, and the prefix each lands under.
 *
 * `ui` is the viewer. `ui-kit` is the design system (`VERSION` and the CSS
 * generators); it gets a separate prefix rather than being flattened into the
 * viewer's tree so the mapping stays a bijection - two packages cannot
 * collide on one filename, and a reviewer can see which package a served file
 * came from.
 *
 * Both land under `/assets/`, and that is not a naming preference - it is what
 * makes the app work offline. `sw-policy.ts`'s `RUNTIME_PATH_PREFIXES` is
 * exactly `["/assets/", "/wasm/"]`, and a path outside those prefixes is
 * refused by the worker rather than cached. Serving the viewer from `/app/`
 * would have produced an app that loads once online and then 503s offline,
 * which is the exact failure WEB.02 exists to prevent. The policy's own
 * comment already describes `/assets/` as "the built UI", so this is the
 * prefix it was written for.
 */
const ESM_ROOTS = [
	{ from: join(workspaceRoot, "apps/ui/dist"), to: "assets/ui" },
	{ from: join(workspaceRoot, "packages/ui-kit/dist"), to: "assets/ui-kit" },
	// The web host's OWN modules, for the same reason and under the same prefix:
	// `print.js` and `print-pdf.js` are the print path's decision and assembly
	// layers (UI.08), and `boot.js` imports them exactly as it imports the viewer
	// modules. Keeping them as host TypeScript rather than inlining them into
	// boot.js is what keeps them unit-testable - the print plan has 14 tests and
	// the writer 13, none of which could exist if the logic lived in one long
	// script.
	//
	// The entry is `page.js`, NOT `index.js`. The host's index is the service
	// worker's source generator; walking from it would drag the whole worker
	// (sw, headers, no-upload, worker-glue) onto the page origin for no reason.
	// Because the walk follows IMPORTS, `page.ts` re-exporting only the print
	// modules ships exactly those two and nothing else - and test files, maps
	// and declarations are excluded automatically because nothing imports them.
	{ from: join(workspaceRoot, "apps/web/host/dist"), to: "assets/host", entry: "page.js" },
];

/** The engine filename inside a cargo target dir, optimised first. */
const WASM_OPT = "wasm32-unknown-unknown/release/selis_pdf_wasm.opt.wasm";
/** The unoptimised build, so a developer who only ran `cargo build` still gets a working app. */
const WASM_RAW = "wasm32-unknown-unknown/release/selis_pdf_wasm.wasm";

/**
 * Where the engine is looked for. `CARGO_TARGET_DIR` wins because that is what
 * cargo itself honours, and a redirected build (a CI job, or a contributor with
 * a small source disk) must not be told to look in the default place.
 */
function targetDirs() {
	const dirs = [];
	const fromEnv = process.env.CARGO_TARGET_DIR;
	if (fromEnv) dirs.push(fromEnv);
	dirs.push(join(workspaceRoot, "target"));
	return dirs;
}

/** The first engine that exists, optimised preferred over raw. */
function findEngine() {
	for (const dir of targetDirs()) {
		for (const name of [WASM_OPT, WASM_RAW]) {
			const candidate = join(dir, name);
			if (existsSync(candidate)) return candidate;
		}
	}
	return null;
}

/**
 * `import ... from "./x.js"` specifiers, relative only.
 *
 * Matching needs care in two ways, and both were found by running this rather
 * than by reading it. A JSDoc comment reading
 * `/** Refusal: document JavaScript. *\/` contains the word `import`, and
 * `viewer/strings.js` is a TRANSLATION TABLE whose values legitimately contain
 * the words `import` and `javascript`. A regex that looks for
 * `import` followed by any quoted run reads those as module specifiers and
 * dies on prose.
 *
 * So comments are blanked to spaces first (same width, so offsets survive),
 * and then the match must begin at a STATEMENT BOUNDARY - the start of a line,
 * or a `;`/`}` before it. `import:` inside an object literal and `"import"`
 * inside a string value both fail that test, while a real `import ... from "x"`
 * and `export ... from "x"` pass.
 */
function localImports(source) {
	const code = source
		.replace(/\/\*[\s\S]*?\*\//g, (m) => " ".repeat(m.length))
		.replace(/\/\/[^\n]*/g, (m) => " ".repeat(m.length));
	const found = [];
	// `from "x"`, or a bare side-effect `import "x"`. Anything else is not an
	// import: `export const VERSION = "0.1.0"` also starts with `export`, and a
	// keyword-only match reads the string on that line as a specifier. Requiring
	// `from` (or a side-effect import with nothing between) is what separates
	// them, and it is the same distinction a bundler makes.
	const pattern =
		/(?:^|[;}])\s*(?:import|export)\s*(?:[\w${},\s*]+?\s+from\s+)?["']([^"'\n]+)["']/gm;
	for (const match of code.matchAll(pattern)) {
		const specifier = match[1];
		if (specifier.startsWith("./") || specifier.startsWith("../")) {
			found.push(specifier);
		} else if (specifier === "@selis/ui-kit") {
			// The one bare specifier in the shipped UI. See the header.
			found.push(specifier);
		} else if (!specifier.startsWith(".")) {
			console.error(
				`place-assets: a page module imports "${specifier}". The page is served ` +
					"bundle-free and same-origin (ADR-P0016, ADR-P0028); no bare or remote " +
					"specifier may reach public/.",
			);
			process.exit(1);
		}
	}
	return found;
}
/**
 * The one specifier rewrite this script performs.
 *
 * `"@selis/ui-kit"` is a workspace package name. The browser has no resolver
 * for it: served as-is, the page fails with `TypeError: Failed to resolve
 * module specifier "@selis/ui-kit"`. Since the page is served unbundled, that
 * specifier must become a real URL, and the mapping is declared right here
 * beside ESM_ROOTS.
 *
 * This is the ONLY mutation place-assets.mjs makes. Everything else is copied
 * byte for byte, and `rewrites` counts the substitutions so the build log
 * states how many were needed — a copy that silently rewrote nothing while a
 * bare specifier shipped would be the worst outcome, so the count is reported
 * rather than assumed.
 */
const SPECIFIER_MAP = {
	"@selis/ui-kit": "assets/ui-kit/index.js",
};

/**
 * Copy one file into `public/`, rewriting declared bare specifiers.
 *
 * `to` is asserted to stay inside `public/`. That is not defensive padding: an
 * earlier version of this walk computed a served name as
 * `relative(root.from, from)` for a file that lived in ANOTHER package, which
 * produced `../../../packages/ui-kit/dist/index.js`. `join` resolved it, the
 * copy succeeded, and the files landed in `apps/web/host/packages/` - outside
 * the deployment root, where the browser would never find them and `git status`
 * would show them as new source. Every served path is now carried explicitly and
 * checked here rather than derived by subtraction.
 *
 * @returns the number of specifiers rewritten in this file.
 */
function place(from, to) {
	const served = relative(publicRoot, to);
	if (served.startsWith("..") || isAbsolute(served)) {
		console.error(
			`place-assets: refusing to write ${served} - a served path escaped public/.`,
		);
		process.exit(1);
	}
	mkdirSync(dirname(to), { recursive: true });

	const original = readFileSync(from, "utf8");
	let rewritten = original;
	let count = 0;
	// The rewritten specifier is RELATIVE to the file being written, not an
	// absolute URL. That is not a style choice: with an absolute
	// `/assets/ui-kit/index.js` the module fails to load in a real engine while
	// the origin answers the very same path with 200 and the right MIME type —
	// measured, and reproducible, by importing the barrel from a page both with
	// and without the specifier rewritten as relative. A relative specifier is
	// also what makes the tree deployable under a subpath, which an absolute one
	// silently is not.
	const servedDir = dirname(served);
	for (const [bare, target] of Object.entries(SPECIFIER_MAP)) {
		// Match the bare specifier only as a whole quoted module specifier, so
		// a mention in prose or a longer name (`@selis/ui-kit-extra`) is not
		// rewritten by accident.
		const pattern = new RegExp(`(["'])${bare.replace("/", "\\/")}\\1`, "g");
		rewritten = rewritten.replace(pattern, (_match, quote) => {
			count++;
			const relativeTarget = relative(servedDir, target).split("\\").join("/");
			const specifier = relativeTarget.startsWith(".") ? relativeTarget : `./${relativeTarget}`;
			return `${quote}${specifier}${quote}`;
		});
	}

	if (count === 0) {
		// Nothing to rewrite: the byte-for-byte path, which is the common one.
		writeFileSync(to, original);
		return 0;
	}
	writeFileSync(to, rewritten);
	return count;
}

// ── the viewer, as a declared ESM closure ──────────────────────────────────

const modules = [];
/** How many bare specifiers were rewritten across the whole closure. */
let rewrites = 0;
// Derived from ESM_ROOTS rather than hardcoded, so changing a prefix above
// cannot leave this pointing at a root that no longer exists (which silently
// degrades to "no bare specifier found" instead of failing).
const kit = ESM_ROOTS.find((r) => r.from === join(workspaceRoot, "packages/ui-kit/dist"));

for (const root of ESM_ROOTS) {
	if (!existsSync(root.from)) {
		console.error(
			`place-assets: ${relative(workspaceRoot, root.from)} does not exist - build the ` +
				"workspace packages first (`pnpm -r build`).",
		);
		process.exit(1);
	}
	const entry = join(root.from, root.entry ?? "index.js");
	if (!existsSync(entry)) {
		console.error(
			`place-assets: no ${root.entry ?? "index.js"} in ${relative(workspaceRoot, root.from)} - run ` +
				"`tsc -p tsconfig.json` there first.",
		);
		process.exit(1);
	}

	// The walk follows IMPORTS from the entry, so a file nothing imports cannot
	// ship. Test output is excluded because vitest is a bare specifier.
	//
	// Each queue entry is `{ from, served }`: an absolute SOURCE path and the
	// path it is SERVED at, carried together rather than derived. Both earlier
	// bugs came from deriving one from the other:
	//
	//  - Keying on the specifier text made `./strings.js` mean "next to the
	//    package root" instead of "next to the importing file".
	//  - Deriving the served name as `relative(root.from, from)` was wrong for
	//    a file in another package: `@selis/ui-kit` lives outside this root, so
	//    the name came out as `../../../packages/ui-kit/dist/index.js` and the
	//    copies escaped `public/` entirely (see `place`).
	const queue = [{ from: entry, served: "index.js" }];
	const seen = new Set();
	while (queue.length > 0) {
		const { from, served } = queue.pop();
		if (seen.has(served)) continue;
		seen.add(served);

		if (!existsSync(from)) {
			console.error(`place-assets: the page imports ${served}, which tsc did not emit.`);
			process.exit(1);
		}
		// Never ship test output: vitest is a bare specifier.
		if (from.endsWith(".test.js")) continue;

		// `relative` yields OS separators; a served path is a URL path, so it is
		// normalised here rather than at every consumer. `place` needs the
		// native form to build a real path, so it takes `served` and joins it.
		const servedUrl = served.split("\\").join("/");
		rewrites += place(from, join(publicRoot, root.to, served));
		modules.push(`${root.to}/${servedUrl}`);

		// Each dependency resolves against THIS file's directory.
		const dir = dirname(from);
		for (const dependency of localImports(readFileSync(from, "utf8"))) {
			if (dependency === "@selis/ui-kit") {
				// The one bare specifier: it maps into the OTHER root, so it is
				// served under that root's prefix, not this one.
				queue.push({ from: join(kit.from, "index.js"), served: "index.js" });
			} else {
				const target = resolve(dir, dependency);
				queue.push({ from: target, served: relative(root.from, target) });
			}
		}
	}
}

/**
 * A missing engine is a BUILD FAILURE naming the command that makes it, never
 * a skip and never a pass. The extension already learned this the hard way
 * (EXT.03 shipped a package whose viewer rendered nothing with every gate
 * green), and it applies with more force here: a web app with no engine looks
 * like a web app that is merely empty, and "the app loaded" is exactly what
 * the existing browser checks assert.
 */
const engine = findEngine();
if (engine === null) {
	console.error(
		[
			"place-assets: no WASM engine found - the app cannot open a PDF without one, so",
			"this is a build failure rather than a skip. Build it first:",
			"",
			"  cargo build -p selis-pdf-wasm --target wasm32-unknown-unknown --release",
			"  wasm-opt -O3 <target>/wasm32-unknown-unknown/release/selis_pdf_wasm.wasm \\",
			"            -o <target>/wasm32-unknown-unknown/release/selis_pdf_wasm.opt.wasm",
			"",
			"(`cargo xtask size-check` does both steps. Set CARGO_TARGET_DIR if the build is",
			"redirected; this script reads it.)",
		].join("\n"),
	);
	process.exit(1);
}

const engineOut = join(publicRoot, "wasm/selis_pdf_wasm.wasm");
mkdirSync(dirname(engineOut), { recursive: true });
copyFileSync(engine, engineOut);
const engineBytes = readFileSync(engineOut).length;

// A marker so a deployed root can be checked for the app's parts without
// guessing whether the build ran. Mirrors place-sw.mjs's sw-manifest.json.
writeFileSync(
	join(publicRoot, "app-manifest.json"),
	`${JSON.stringify(
		{
			entry: "/assets/boot.js",
			modules: modules.sort(),
			// Recorded rather than implied: a build where this is 0 while a bare
			// specifier still shipped would be the failure worth catching.
			specifierRewrites: rewrites,
			engine: "/wasm/selis_pdf_wasm.wasm",
			engineBytes,
		},
		null,
		2,
	)}\n`,
	"utf8",
);

console.log(
	`place-assets: copied ${modules.length} modules + the engine ` +
		`(${engineBytes} B, from ${relative(workspaceRoot, engine)}) into public/, ` +
		`rewriting ${rewrites} bare specifier(s)`,
);
