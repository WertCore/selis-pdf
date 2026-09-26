/**
 * Canonical security headers for the Selis web deployment (SL-4.WEB.01).
 *
 * Single source of truth for every static-hosting config in this package:
 * `public/_headers` (Netlify / Cloudflare Pages file syntax) and
 * `vercel.json` (Vercel header config) are generated from these builders, and
 * `headers.test.ts` fails the build when any provider file drifts from them.
 *
 * What the task requires, and where it lives here:
 * - Cross-origin isolation for threads (WASM.03): `COOP: same-origin` +
 *   `COEP: require-corp`. When both are present the page is
 *   `crossOriginIsolated` and `SharedArrayBuffer` unlocks multi-threaded
 *   rasterisation; when they are absent (extension, some embeds) the app
 *   falls back to the single-threaded engine path — see `isolation.ts`. The
 *   DoD "works with isolation disabled" test lives there, not here.
 * - Strict CSP with `wasm-unsafe-eval` only (no `unsafe-inline`,
 *   no `unsafe-eval`, no remote hosts): WASM needs compilation permission
 *   without full eval, and ADR-P0016 forbids third-party scripts on the
 *   document-handling path.
 * - SRI on every asset: enforced by `sri.ts` + `public/index.html` (every
 *   `<script>`/`<link rel="stylesheet">` carries `integrity` + `crossorigin`).
 * - securityheaders.sh A+: every header that scanner grades is emitted here
 *   (`CSP`, `HSTS`, `X-Frame-Options` + `frame-ancestors`, `X-Content-Type`,
 *   `Referrer-Policy`, `Permissions-Policy`, `COOP`, `COEP`, `CORP`).
 *
 * `connect-src 'self'` is deliberate and locked: remote `?src=` range fetches
 * (SL-4.WASM.06) must be allow-listed per deployment by editing
 * `EXTRA_CONNECT_SRC` in the hosting config review — never by widening this
 * default. The locked default keeps ADR-P0016's "never upload" claim
 * enforceable: the only network the viewer needs out of the box is itself.
 */

/** Response-header map: header name -> value. */
export type HeaderMap = Readonly<Record<string, string>>;

/**
 * Content-Security-Policy for the document-handling origin.
 *
 * Directives, with the reason each exists:
 * - `default-src 'self'`: closed by default; every other directive narrows
 *   from here, nothing widens to a remote origin.
 * - `script-src 'self' 'wasm-unsafe-eval'`: local scripts only, plus the
 *   narrow WASM-compile permission. No `unsafe-inline`, no `unsafe-eval`,
 *   no `https:` hosts — a build check (`sri.test.ts`) fails on any remote
 *   `<script src>` in the bundle.
 * - `worker-src 'self'`: the engine lives in a same-origin Worker
 *   (24-BINDINGS-SPEC §2); no blob:/remote workers.
 * - `style-src 'self'`: no inline styles, no remote stylesheets.
 * - `img-src 'self' blob: data:`: rendered tiles arrive as `blob:`/`data:`
 *   bitmaps; document images are decoded locally, never hot-linked.
 * - `font-src 'self'`: fonts ship with the app (lazy CJK chunks included);
 *   no remote font CDN.
 * - `connect-src 'self'`: locked default (see module doc). Range fetches to
 *   the app origin work; remote `?src=` origins need an explicit, reviewed
 *   allow-list entry per deployment.
 * - `media-src 'self' blob:`: same tile-bitmap reasoning as `img-src`.
 * - `object-src 'none'`, `base-uri 'self'`, `form-action 'none'`,
 *   `frame-ancestors 'none'`: no plugins, no base hijack, no forms, no
 *   embedding — the viewer is a top-level page, never a frame.
 * - `upgrade-insecure-requests`: http degrades to https, never the reverse.
 */
export function buildContentSecurityPolicy(): string {
	return [
		"default-src 'self'",
		"script-src 'self' 'wasm-unsafe-eval'",
		"worker-src 'self'",
		"style-src 'self'",
		"img-src 'self' blob: data:",
		"font-src 'self'",
		"connect-src 'self'",
		"media-src 'self' blob:",
		"object-src 'none'",
		"base-uri 'self'",
		"form-action 'none'",
		"frame-ancestors 'none'",
		"upgrade-insecure-requests",
	].join("; ");
}

/**
 * Full security-header set for the document-handling origin. Values are
 * fixed strings (no per-request templating) so static hosts can serve them
 * verbatim from `_headers` / `vercel.json`.
 */
export function buildSecurityHeaders(): HeaderMap {
	return {
		"Content-Security-Policy": buildContentSecurityPolicy(),
		"Cross-Origin-Opener-Policy": "same-origin",
		"Cross-Origin-Embedder-Policy": "require-corp",
		"Cross-Origin-Resource-Policy": "same-origin",
		"Strict-Transport-Security": "max-age=63072000; includeSubDomains; preload",
		"X-Content-Type-Options": "nosniff",
		"X-Frame-Options": "DENY",
		"Referrer-Policy": "no-referrer",
		"Permissions-Policy":
			"camera=(), microphone=(), geolocation=(), payment=(), usb=(), magnetometer=(), gyroscope=(), accelerometer=()",
		"Origin-Agent-Cluster": "?1",
		"X-Permitted-Cross-Domain-Policies": "none",
	};
}

/**
 * Headers that must be present for `crossOriginIsolated` to be `true`
 * (the threaded WASM.03 path). The app MUST NOT require them — see
 * `isolation.ts` — but the deployment MUST send them so the fast path
 * is available where the embedder allows it.
 */
export const ISOLATION_HEADERS: Readonly<Record<string, string>> = {
	"Cross-Origin-Opener-Policy": "same-origin",
	"Cross-Origin-Embedder-Policy": "require-corp",
};

/** Header names securityheaders.sh grades — all must be in {@link buildSecurityHeaders}. */
export const GRADED_HEADERS: readonly string[] = [
	"Content-Security-Policy",
	"Strict-Transport-Security",
	"X-Frame-Options",
	"X-Content-Type-Options",
	"Referrer-Policy",
	"Permissions-Policy",
	"Cross-Origin-Opener-Policy",
	"Cross-Origin-Embedder-Policy",
	"Cross-Origin-Resource-Policy",
];
