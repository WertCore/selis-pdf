/**
 * A dependency-free static server for the built app.
 *
 * The deployment root is `public/`, and two host behaviours matter for the
 * checks this serves:
 *
 *   1. **Directory indexes.** `/<slug>/` resolves to `/<slug>/index.html`.
 *      That is the one host requirement plan 36's routes were designed around,
 *      and a server that 404s the trailing-slash form would make the generated
 *      pages untestable locally.
 *   2. **Real 404s.** An unknown path is 404, never the tools page with 200.
 *      A soft-404 on forty-three generated URLs is the crawl-budget problem
 *      the sitemap exists to avoid.
 *
 * COOP/COEP are sent on every response so `crossOriginIsolated` is true on
 * `http://localhost`, which is a secure context and therefore enough to
 * register the service worker exactly as production does. No document byte
 * ever leaves this process; it only reads files the build already wrote.
 *
 * Usage:
 *
 *   pnpm --filter @selis/web-host build
 *   pnpm --filter @selis/web-host serve
 *
 * Then open the printed URL. `SELIS_PORT` and `SELIS_HOST` override the
 * defaults (4173, 127.0.0.1).
 */

import { createReadStream, existsSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { dirname, extname, join, normalize, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const pkgRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const publicRoot = join(pkgRoot, "public");
const port = Number.parseInt(process.env.SELIS_PORT ?? "4173", 10);
const host = process.env.SELIS_HOST ?? "127.0.0.1";

if (!existsSync(join(publicRoot, "index.html"))) {
	console.error(
		"serve: public/index.html is missing. Build the app first:\n\n" +
			"  pnpm --filter @selis/web-host build\n",
	);
	process.exit(1);
}

/** The content types the app serves. Anything else is octet-stream. */
const CONTENT_TYPES = new Map([
	[".html", "text/html; charset=utf-8"],
	[".css", "text/css; charset=utf-8"],
	[".js", "text/javascript; charset=utf-8"],
	[".mjs", "text/javascript; charset=utf-8"],
	[".json", "application/json; charset=utf-8"],
	[".wasm", "application/wasm"],
	[".svg", "image/svg+xml"],
	[".png", "image/png"],
	[".jpg", "image/jpeg"],
	[".jpeg", "image/jpeg"],
	[".webp", "image/webp"],
	[".woff2", "font/woff2"],
	[".ttf", "font/ttf"],
	[".xml", "application/xml; charset=utf-8"],
	[".txt", "text/plain; charset=utf-8"],
]);

/**
 * Map a URL path to a file, applying directory-index resolution.
 *
 * The traversal check runs on the resolved absolute path, not on the raw
 * string: `%2e%2e` is decoded by `decodeURIComponent` before this is called,
 * and a resolved path outside `public/` is refused.
 */
function resolveFile(requestPath) {
	let pathname;
	try {
		pathname = decodeURIComponent(requestPath.split("?")[0].split("#")[0]);
	} catch {
		return null;
	}
	const relative = normalize(pathname).replace(/^[\\/]+/, "");
	const target = resolve(publicRoot, relative);
	if (target !== publicRoot && !target.startsWith(publicRoot + sep)) {
		return null;
	}
	if (existsSync(target) && statSync(target).isDirectory()) {
		const index = join(target, "index.html");
		return existsSync(index) ? index : null;
	}
	return existsSync(target) && statSync(target).isFile() ? target : null;
}

const server = createServer((request, response) => {
	const file = resolveFile(request.url ?? "/");
	const common = {
		"Cross-Origin-Opener-Policy": "same-origin",
		"Cross-Origin-Embedder-Policy": "require-corp",
		"Cross-Origin-Resource-Policy": "same-origin",
		"X-Content-Type-Options": "nosniff",
		// A dev server must never hand the browser a cached module behind the
		// one that was just rebuilt; the service worker's own versioning is
		// what makes offline behaviour cacheable.
		"Cache-Control": "no-cache",
	};
	if (file === null) {
		const body =
			'<!doctype html><html lang="en"><meta charset="utf-8">' +
			"<title>Not found — Selis Tools</title><h1>404</h1>" +
			"<p>That path is not a page on this site.</p>" +
			'<p><a href="/">All tools</a></p>';
		response.writeHead(404, {
			...common,
			"Content-Type": "text/html; charset=utf-8",
			"Content-Length": Buffer.byteLength(body),
		});
		response.end(request.method === "HEAD" ? undefined : body);
		return;
	}
	const stats = statSync(file);
	response.writeHead(200, {
		...common,
		"Content-Type": CONTENT_TYPES.get(extname(file).toLowerCase()) ?? "application/octet-stream",
		"Content-Length": stats.size,
		"Last-Modified": stats.mtime.toUTCString(),
	});
	if (request.method === "HEAD") {
		response.end();
		return;
	}
	const stream = createReadStream(file);
	stream.on("error", () => response.destroy());
	stream.pipe(response);
});

server.listen(port, host, () => {
	console.log(`serve: Selis Tools at http://${host}:${port}/`);
	console.log(`serve: one generated page, e.g. http://${host}:${port}/merge-pdf/`);
	console.log("serve: press Ctrl+C to stop");
});

for (const signal of ["SIGINT", "SIGTERM"]) {
	process.on(signal, () => {
		server.close(() => process.exit(0));
	});
}
