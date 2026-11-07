/**
 * Generate one static page per tool at `public/<slug>/index.html`.
 *
 * The registry (`src/tool-registry.ts`, compiled to `dist/tool-registry.js`) is
 * the only source of truth. This script reads it and writes:
 *
 *   - `public/<slug>/index.html` for every tool, from one template;
 *   - `public/pages.json`, the manifest the build compares back against the
 *     registry;
 *   - `public/sitemap.xml`, one absolute URL per generated page;
 *   - `public/robots.txt`, a pointer at the sitemap.
 *
 * ## Why directory indexes rather than rewrites
 *
 * A static host resolves `/<slug>/` to `/<slug>/index.html` with no rewrite
 * rule and no server code (plan D2). The page carries the tool's slug on
 * `<html data-tool="…">`, so `tools-page.ts` only has to read one attribute —
 * there is no second slug table and no path parsing per page.
 *
 * ## What fails the build
 *
 * A slug that is missing, duplicated, not `[a-z0-9-]+`, or collides with an
 * existing top-level path; a tool whose group or related id does not exist; a
 * duplicate `<meta name="description">` (forty-three near-identical pages are
 * doorway content, so each must describe a different job); and any generated
 * document without both `<title>` and `<h1>`.
 *
 * Run with `--check` to compare against what is on disk instead of writing.
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { SITE_ORIGIN, TOOLS, TOOL_GROUPS } from "../dist/tool-registry.js";

const pkgRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const publicRoot = join(pkgRoot, "public");

/**
 * Top-level names a slug may not take. A slug is a directory beside these, so
 * a collision would either shadow the app or be shadowed by it depending on
 * host precedence — exactly the ambiguity a generated route must not have.
 */
export const RESERVED_TOP_LEVEL = Object.freeze({
	assets: "the built UI and the tools page module",
	wasm: "the engine chunks",
	"tools.html": "the card grid",
	"index.html": "the viewer shell",
	"sw.js": "the service worker",
	"sw-register.js": "the page-side worker registrar",
	"pages.json": "the generated manifest",
	"sitemap.xml": "the generated sitemap",
	"robots.txt": "the crawler policy",
	"app-manifest.json": "the asset marker",
	"sw-manifest.json": "the worker marker",
	_headers: "host header rules",
	"vercel.json": "host routing rules",
});

const SLUG_PATTERN = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

/** Escape text for an HTML text node or a quoted attribute. */
export function escapeHtml(value) {
	return value
		.replaceAll("&", "&amp;")
		.replaceAll("<", "&lt;")
		.replaceAll(">", "&gt;")
		.replaceAll('"', "&quot;");
}

/**
 * Validate the registry before anything is written.
 *
 * Returns a list of problems rather than throwing on the first one: a build
 * log that names every bad slug is what makes a rejected commit fixable in one
 * pass. Exported so the negative cases can be driven without a browser.
 */
export function validateRegistry(tools, groups = TOOL_GROUPS) {
	const problems = [];
	if (!Array.isArray(tools) || tools.length === 0) {
		return ["the registry is empty"];
	}
	const groupIds = new Set(groups.map((group) => group.id));
	const ids = new Set();
	const slugs = new Set();
	const descriptions = new Map();
	const relatedIds = new Set();
	for (const tool of tools) {
		if (typeof tool.id !== "string" || tool.id === "") {
			problems.push("a tool has no id");
			continue;
		}
		if (ids.has(tool.id)) {
			problems.push(`duplicate tool id: ${tool.id}`);
		}
		ids.add(tool.id);
		if (typeof tool.slug !== "string" || tool.slug === "") {
			problems.push(`${tool.id}: no slug`);
		} else {
			if (!SLUG_PATTERN.test(tool.slug)) {
				problems.push(`${tool.id}: slug "${tool.slug}" is not lower-case ASCII hyphenated`);
			}
			if (slugs.has(tool.slug)) {
				problems.push(`duplicate slug: ${tool.slug}`);
			}
			if (Object.hasOwn(RESERVED_TOP_LEVEL, tool.slug)) {
				problems.push(`${tool.id}: slug "${tool.slug}" collides with a top-level path`);
			}
			slugs.add(tool.slug);
		}
		if (typeof tool.group !== "string" || !groupIds.has(tool.group)) {
			problems.push(`${tool.id}: group "${tool.group}" is not in TOOL_GROUPS`);
		}
		for (const field of ["title", "blurb", "note", "verb", "mark"]) {
			if (typeof tool[field] !== "string" || tool[field].trim() === "") {
				problems.push(`${tool.id}: ${field} is empty`);
			}
		}
		if (typeof tool.blurb === "string") {
			const previous = descriptions.get(tool.blurb);
			if (previous !== undefined) {
				problems.push(
					`${tool.id}: description is identical to ${previous} — each page must describe a different job`,
				);
			}
			descriptions.set(tool.blurb, tool.id);
		}
		for (const related of tool.related ?? []) {
			relatedIds.add(`${tool.id}->${related}`);
		}
	}
	for (const pair of relatedIds) {
		const [from, to] = pair.split("->");
		if (!ids.has(to)) {
			problems.push(`${from}: related tool "${to}" does not exist`);
		}
	}
	return problems;
}

/** The header markup shared with `tools.html`. */
const HEADER = `		<header class="site-header">
			<a class="brand" href="/tools.html" aria-label="Selis Tools home">
				<span class="brand-mark" aria-hidden="true">S</span>
				<span class="brand-word">selis<span>tools</span></span>
			</a>
			<nav class="site-nav" aria-label="Primary">
				<a href="/">Viewer</a>
				<a href="/tools.html#tools">All tools</a>
				<a href="/tools.html#roadmap">Roadmap</a>
				<span class="privacy-pill"><span aria-hidden="true">●</span> No upload</span>
			</nav>
		</header>`;

/** The footer markup shared with `tools.html`. */
const FOOTER = `		<footer class="site-footer">
			<p>
				<strong>Selis Tools</strong> is part of the Selis PDF project. Local-only
				by construction, not by policy alone.
			</p>
			<p><a href="/">Viewer</a> · <a href="/tools.html">All tools</a></p>
		</footer>`;

/**
 * The shared workspace shell.
 *
 * These ids are the contract with `tools-page.ts`; every one of them is
 * required by the page script, and a generated page that omits one would throw
 * on boot. The heading is an `h1` here (it is the page's subject) while
 * `/tools.html` keeps its own `h2` inside the same shell.
 */
function workspace(tool) {
	// The dropzone copy names what the tool eats, not what the app is for:
	// image tools must not say "Drop PDFs here". Mirrors selectTool in
	// tools-page.ts, so the static page and the JS-updated page agree.
	const drop =
		tool.accepts === "image"
			? {
					title: "Drop your images here",
					link: "choose images",
					label: "Choose images",
					list: "Selected images",
				}
			: tool.accepts === "text"
				? {
						title: "Drop your file here",
						link: "choose a file",
						label: "Choose a file",
						list: "Selected files",
					}
				: {
						title: "Drop PDFs here",
						link: "choose files",
						label: "Choose PDF files",
						list: "Selected PDFs",
					};
	return `			<section id="workspace" class="workspace" aria-labelledby="workspace-title">
				<a id="back-to-tools" class="back" href="/tools.html">← All tools</a>
				<div class="workspace-grid">
					<div class="workspace-main">
						<p class="eyebrow">Local workspace</p>
						<h1 id="workspace-title">${escapeHtml(tool.title)}</h1>
						<p id="workspace-blurb" class="workspace-blurb">${escapeHtml(tool.blurb)}</p>
						<p class="workspace-note">${escapeHtml(tool.note)}</p>

						<!-- Same shell as tools.html: the dropzone is a mouse/drag
						     target only and the input inside is the control; see the
						     note in tools.html about why it carries no role/tabindex. -->
						<div id="dropzone" class="dropzone">
							<span class="drop-icon" aria-hidden="true">↓</span>
							<span id="drop-title" class="drop-title">${escapeHtml(drop.title)}</span>
							<span class="drop-hint"
								>or <span id="drop-link" class="drop-link">${escapeHtml(drop.link)}</span> ·
								up to 256 MiB per run</span
							>
							<input
								id="file-input"
								class="visually-hidden"
								type="file"
								accept="application/pdf,.pdf"
								multiple
								aria-label="${escapeHtml(drop.label)}"
							/>
						</div>

						<div id="file-toolbar" class="file-toolbar" hidden>
							<button id="sort-files" class="text-button" type="button">
								Sort by name
							</button>
						</div>
						<ul id="file-list" class="file-list" aria-label="${escapeHtml(drop.list)}"></ul>
						<div id="options" class="options"></div>

						<div class="run-row">
							<button id="run-tool" class="run-button" type="button" disabled>
								Run
							</button>
							<p id="status" class="status" role="status" aria-live="polite"></p>
						</div>

						<section id="result" class="result" aria-labelledby="result-title" hidden>
							<div class="result-head">
								<div>
									<p class="eyebrow">Result</p>
									<h3 id="result-title"></h3>
									<p id="result-meta" class="result-meta"></p>
								</div>
							</div>
							<ul id="result-outputs" class="result-outputs"></ul>
							<dl id="result-rows" class="result-rows"></dl>
							<div class="result-foot">
								<p>
									Verified against the input structure after the write. The
									result is a Blob in this tab.
								</p>
								<div id="result-chain" class="result-chain" hidden>
									<label for="result-chain-file">Continue with</label>
									<select id="result-chain-file" class="control"></select>
									<label for="result-chain-tool">in</label>
									<select id="result-chain-tool" class="control"></select>
									<button id="result-chain-go" class="text-button" type="button">
										Continue
									</button>
								</div>
								<button id="result-again" class="text-button" type="button">
									Start another
								</button>
							</div>
						</section>
					</div>

					<aside class="workspace-aside" aria-labelledby="aside-title">
						<h2 id="aside-title">What stays local</h2>
						<ul>
							<li>Your PDFs are read in this tab and copied into WebAssembly memory.</li>
							<li>No request carries a document body, filename, or page text.</li>
							<li>The result is downloaded with a Blob URL created here.</li>
							<li>No watermark, no daily limit, no account.</li>
						</ul>
						<p class="aside-note">
							The first run fetches a small WebAssembly chunk from this same
							site. That request has no document bytes; after it loads, the
							operation does not touch the network.
						</p>
					</aside>
				</div>
			</section>`;
}

/** Related-tool navigation as real anchors — no `location.href` string building. */
function related(tool, byId) {
	const ids = tool.related ?? [];
	if (ids.length === 0) {
		return "";
	}
	const items = ids
		.map((id) => byId.get(id))
		.filter((other) => other !== undefined)
		.map(
			(other) =>
				`					<li><a href="/${other.slug}/">${escapeHtml(other.title)}</a> — ${escapeHtml(other.blurb)}</li>`,
		)
		.join("\n");
	return `			<section class="related" aria-labelledby="related-title">
				<h2 id="related-title">Related tools</h2>
				<ul class="related-list">
${items}
				</ul>
			</section>`;
}

/** Render one tool's document. */
export function renderToolPage(tool, byId) {
	const canonical = `${SITE_ORIGIN}/${tool.slug}/`;
	const title = `${tool.title} — Selis Tools`;
	const description = tool.blurb;
	const relatedBlock = related(tool, byId);
	return `<!doctype html>
<html lang="en" data-tool="${escapeHtml(tool.slug)}">
	<head>
		<meta charset="utf-8" />
		<meta name="viewport" content="width=device-width, initial-scale=1" />
		<meta name="theme-color" content="#f4f1e8" />
		<meta name="description" content="${escapeHtml(description)}" />
		<title>${escapeHtml(title)}</title>
		<link rel="canonical" href="${canonical}" />
		<meta property="og:type" content="website" />
		<meta property="og:title" content="${escapeHtml(title)}" />
		<meta property="og:description" content="${escapeHtml(description)}" />
		<meta property="og:url" content="${canonical}" />
		<link rel="stylesheet" href="/assets/tools.css" />
	</head>
	<body>
		<a class="skip-link" href="#main">Skip to content</a>
${HEADER}

		<main id="main">
${workspace(tool)}
${relatedBlock === "" ? "" : `\n${relatedBlock}\n`}		</main>

${FOOTER}

		<noscript>
			<p class="noscript">
				This tool needs JavaScript to read files locally and run the WebAssembly
				operation. No document is sent anywhere when JavaScript is enabled.
			</p>
		</noscript>
		<script type="module" src="/assets/host/tools/tools-page.js"></script>
	</body>
</html>
`;
}

/**
 * Reject a document that would not carry the two things a crawler and a reader
 * need. The template always emits both, so this only fires if the template
 * changes — which is exactly when the check is worth having.
 */
export function assertPageShape(page, slug) {
	if (!page.includes("<title>") || !page.includes("<h1")) {
		throw new Error(`${slug}: the generated page lacks a <title> and an <h1>`);
	}
}

/** Render the manifest the build compares back against the registry. */
export function renderPagesJson(tools) {
	return `${JSON.stringify(
		{
			source: "src/tool-registry.ts",
			origin: SITE_ORIGIN,
			pages: tools.map((tool) => ({
				slug: tool.slug,
				title: tool.title,
				description: tool.blurb,
				id: tool.id,
			})),
		},
		null,
		2,
	)}\n`;
}

/** Render `sitemap.xml` from the same list the pages were generated from. */
export function renderSitemap(tools) {
	const urls = tools
		.map((tool) => `	<url><loc>${SITE_ORIGIN}/${tool.slug}/</loc></url>`)
		.join("\n");
	return `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
${urls}
</urlset>
`;
}

/** `robots.txt`: crawlers may fetch the app; the sitemap is the discovery path. */
export function renderRobots() {
	return `User-agent: *
Allow: /

Sitemap: ${SITE_ORIGIN}/sitemap.xml
`;
}

/**
 * Render everything without touching disk. Exported so `--check` and the unit
 * tests share one definition of "the files the build should produce".
 */
export function renderOutputs(tools = TOOLS, groups = TOOL_GROUPS) {
	const problems = validateRegistry(tools, groups);
	if (problems.length > 0) {
		throw new Error(`the tool registry is not publishable:\n  ${problems.join("\n  ")}`);
	}
	const byId = new Map(tools.map((tool) => [tool.id, tool]));
	const files = new Map();
	for (const tool of tools) {
		const page = renderToolPage(tool, byId);
		assertPageShape(page, tool.slug);
		files.set(`${tool.slug}/index.html`, page);
	}
	files.set("pages.json", renderPagesJson(tools));
	files.set("sitemap.xml", renderSitemap(tools));
	files.set("robots.txt", renderRobots());
	return files;
}

function main() {
	const check = process.argv.includes("--check");
	const files = renderOutputs();
	const drifted = [];
	for (const [relative, content] of files) {
		const target = join(publicRoot, relative);
		if (check) {
			const current = existsSync(target) ? readFileSync(target, "utf8") : null;
			if (current !== content) {
				drifted.push(relative);
			}
			continue;
		}
		mkdirSync(dirname(target), { recursive: true });
		writeFileSync(target, content, "utf8");
	}
	if (check) {
		if (drifted.length > 0) {
			console.error(
				`build-pages: ${drifted.length} generated file(s) are stale; run \`pnpm --filter @selis/web-host build\`:\n  ${drifted.join("\n  ")}`,
			);
			process.exit(1);
		}
		console.log(`build-pages: ${files.size} generated file(s) are current`);
		return;
	}
	console.log(
		`build-pages: wrote ${TOOLS.length} tool pages, pages.json, sitemap.xml and robots.txt`,
	);
}

// Importing this module in a test must not write to `public/`.
if (process.argv[1] !== undefined && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
	main();
}
