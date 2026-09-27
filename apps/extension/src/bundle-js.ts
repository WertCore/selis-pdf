/**
 * SL-4.EXT.04 - the lexical layer: what a shipped module is allowed to say.
 *
 * A shipped module may contain remote URLs only as *data* it matches on. This
 * module finds every remote URL in a built `.js` file, exempts the two shapes
 * that are provably not fetches, and reports the rest. No parser dependency
 * exists in this repository (and none may be added), so the scan is lexical;
 * `BUNDLING.md` records exactly what that does and does not prove.
 *
 * Blank-then-scan is the whole trick, and it is a three-stage pipeline: a
 * single lexical pass produces two masks, and every later stage reads whichever
 * mask it needs. Comments are blanked in both, because a JSDoc example is
 * prose. String *bodies* are blanked only in `code`, because a sink is a call
 * site while a specifier is data. Regex literal *bodies* are blanked in both,
 * because a regex is neither prose nor code nor a fetch target.
 *
 * The masks are length-preserving throughout, so every offset is simultaneously
 * valid in the source, in `text` and in `code` — which is what lets a match
 * found in one mask be quoted from the other.
 */

import { classifyRef, lineOf } from "./bundle-paths.js";

/** A remote URL, with the scheme introducing it, anywhere in a value. */
const REMOTE_URL = /(?:https?|wss?|ftps?):\/\/[^\s"'`\\)<>\]]*/g;

/**
 * A local scheme immediately in front of the match, e.g. the `blob:` in
 * `blob:https://example.com/550e8400`.
 *
 * That object URL embeds the origin that created it, but the bytes it names were
 * made in this process. Flagging it would be the single noisiest false positive
 * the gate could have, so the scheme is checked rather than the host. Matched
 * against a bounded window rather than the whole prefix, so the scan stays
 * linear in the file size.
 */
const LOCAL_SCHEME_PREFIX =
	/(?:^|[^A-Za-z0-9])(?:blob|data|filesystem|chrome-extension|moz-extension|about|chrome):$/i;

/** Longest scheme in {@link LOCAL_SCHEME_PREFIX}, as a look-back window. */
const SCHEME_PREFIX_WINDOW = 20;

/** `export const NAME =` (the `export` is optional so a rollup keeps working). */
const CONST_DECLARATION = /(?:^|[\n;])\s*(?:export\s+)?const\s+([A-Za-z_$][\w$]*)\s*=/g;

/**
 * Dynamic-code sinks. Each is a store-policy rejection on its own.
 *
 * Every pattern runs against the `code` mask, so a *string* that happens to
 * contain `eval(` is not a finding (a comment or a UI message about the ban is
 * not a violation) while a real call site is. The string-bodied timer is the
 * reason `code` keeps its quote characters: with them, `setTimeout("...")` still
 * reads as "a literal reached a timer" after the body has been blanked.
 */
const DYNAMIC_CODE_SINKS: ReadonlyArray<readonly [RegExp, string]> = [
	[/\beval\s*\(/g, "eval() - MV3 forbids it outright (ADR-P0028)"],
	[/\bnew\s+Function\s*\(/g, "new Function() is eval with better branding"],
	[/\bFunction\s*\(\s*["'`]/g, "Function() without `new` is the same dynamic code"],
	[/\bimportScripts\s*\(/g, "importScripts() is remote-code-shaped in a classic worker"],
	[/\b(?:setTimeout|setInterval)\s*\(\s*["'`]/g, "string-bodied timer - code as a string is eval"],
	[/\bdocument\s*\.\s*write(?:ln)?\s*\(/g, "document.write() can inject a script"],
];

/**
 * Import/export specifiers, static and dynamic.
 *
 * Group 1 is the keyword prefix, group 3 the specifier. Matching runs on the
 * `text` mask (the specifier has to be readable) and every hit is then confirmed
 * against the `code` mask, whose group 1 is identical for real code and blanked
 * for a string that merely *reads* like an import.
 */
const IMPORT_SPECIFIER =
	/(\bfrom\s*|\bimport\s*\(\s*|\bimport\s+|\brequire\s*\(\s*|\brequire\s+)(["'`])([^"'`\n]*)\2/g;

/** A dynamic `import(...)` whose argument is not a string literal. */
const COMPUTED_DYNAMIC_IMPORT = /\bimport\s*\(\s*(?!["'`])/g;

/**
 * Exported constants in shipped modules that may carry remote URLs *as data*.
 *
 * Each name is reviewed on its own, and the exemption is conditional on the
 * initializer being pure data (see {@link dataExportSpans}). Adding a name is a
 * statement that those URLs are matched, never fetched.
 */
export const DATA_ONLY_EXPORTS: ReadonlySet<string> = new Set([
	// 38 real-world PDF-serving URLs the ruleset must recognise (SL-4.EXT.02).
	"PATTERN_MATRIX",
	// Host patterns this extension refuses (`http://*/*`, `*://*/*`, ...).
	"DENIED_HOST_PATTERNS",
]);

/** One remote URL the gate considered, with the verdict it reached. */
export interface RemoteLiteral {
	readonly value: string;
	readonly line: number;
	/** Empty when the literal is a violation, else the exempting export name. */
	readonly exemptBy: string;
}

/** One finding in a shipped module. */
export interface JsViolation {
	readonly line: number;
	readonly class:
		| "remote-url-literal"
		| "remote-import"
		| "unresolved-import"
		| "node-builtin-import"
		| "unresolvable-dynamic-import"
		| "dynamic-code";
	readonly detail: string;
}

/** The two length-preserving views of one module, produced by {@link maskJs}. */
export interface JsMask {
	/** Comments blanked; string and regex contents intact. For data scans. */
	readonly text: string;
	/**
	 * Comments blanked; string and regex *bodies* blanked, quote characters kept.
	 * For call-shape scans.
	 */
	readonly code: string;
}

/**
 * Tokens after which a `/` opens a regular expression rather than dividing.
 *
 * This is the one heuristic the scanner cannot avoid without a full parser, and
 * it is the classic one: a `/` is a regex opener exactly where a value may
 * start, and a divisor exactly where one has just ended. Getting it wrong in
 * the *other* direction is the dangerous one — a regex mis-read as division
 * leaves its body visible, and a body is where a crafted `/'/` would sit to
 * hide an `eval(` from a comment stripper.
 */
const REGEX_ALLOWED_BEFORE = new Set([
	"(",
	",",
	"=",
	":",
	"[",
	"!",
	"&",
	"|",
	"?",
	"{",
	"}",
	";",
	"+",
	"-",
	"*",
	"%",
	"~",
	"^",
	"<",
	">",
	"\n",
]);

/** Keywords after which a `/` opens a regex rather than dividing. */
const REGEX_ALLOWED_AFTER_KEYWORD = new Set([
	"return",
	"typeof",
	"instanceof",
	"in",
	"of",
	"new",
	"delete",
	"void",
	"case",
	"do",
	"else",
	"yield",
	"await",
	"throw",
]);

/**
 * Whether the `/` at `i` opens a regex literal, given what came before.
 *
 * Reads the mask in place rather than a joined copy: the look-back is bounded
 * by the longest keyword in {@link REGEX_ALLOWED_AFTER_KEYWORD}, so this stays
 * O(1) per slash instead of re-joining the file at every candidate.
 */
function opensRegex(code: string[], i: number): boolean {
	const wordish = (char: string): boolean => /[A-Za-z0-9_$]/.test(char);
	const isWordChar = (char: string): boolean => /[\w$]/.test(char);
	let j = i - 1;
	while (j >= 0 && /\s/.test(code[j] ?? "")) {
		j -= 1;
	}
	if (j < 0) {
		return true;
	}
	const char = code[j] ?? "";
	if (REGEX_ALLOWED_BEFORE.has(char)) {
		return true;
	}
	if (wordish(char)) {
		let start = j;
		while (start >= 0 && isWordChar(code[start] ?? "")) {
			start -= 1;
		}
		return REGEX_ALLOWED_AFTER_KEYWORD.has(code.slice(start + 1, j + 1).join(""));
	}
	return false;
}

/** Blank `from`/`to` (exclusive) in `out`, keeping newlines so lines still line up. */
function blank(out: string[], from: number, to: number): void {
	for (let i = from; i < to && i < out.length; i += 1) {
		if (out[i] !== "\n") {
			out[i] = " ";
		}
	}
}

/**
 * One lexical pass producing the `text` and `code` masks.
 *
 * This replaces the previous comment-only stripper, which had a real hole: it
 * skipped string literals by scanning for the next matching quote, so a regex
 * literal containing a quote (`const q = /'/;`) put the scanner into
 * "unterminated string" mode and it never looked at the rest of the file. An
 * `eval(` planted after such a regex was invisible. The test suite pins that
 * case; `BUNDLING.md` records the limits that remain.
 *
 * Newlines are never blanked, so a reported line number is the file's real line
 * in both masks.
 */
export function maskJs(source: string): JsMask {
	const text = source.split("");
	const code = source.split("");
	let i = 0;
	while (i < source.length) {
		const char = source.charAt(i);
		const next = source.charAt(i + 1);
		if (char === "/" && next === "/") {
			let end = i;
			while (end < source.length && source.charAt(end) !== "\n") {
				end += 1;
			}
			blank(text, i, end);
			blank(code, i, end);
			i = end;
			continue;
		}
		if (char === "/" && next === "*") {
			let end = i + 2;
			while (
				end < source.length &&
				!(source.charAt(end) === "*" && source.charAt(end + 1) === "/")
			) {
				end += 1;
			}
			end = Math.min(end + 2, source.length);
			blank(text, i, end);
			blank(code, i, end);
			i = end;
			continue;
		}
		if (char === "'" || char === '"' || char === "`") {
			const quote = char;
			const bodyStart = i + 1;
			let j = bodyStart;
			while (j < source.length) {
				const inner = source.charAt(j);
				if (inner === "\\") {
					j += 2;
					continue;
				}
				j += 1;
				// A raw newline ends an unterminated literal rather than
				// swallowing the rest of the file; a blanked-out body must never
				// be able to hide code.
				if (inner === quote || inner === "\n") {
					break;
				}
			}
			// `close` is the first index *past* the literal, `bodyEnd` the first
			// index of it. Resuming at `bodyEnd` would put the scanner back on
			// the closing quote, which reads as a fresh opening quote: the scan
			// would then run to the next quote in the file and blank everything
			// in between, hiding any `eval(` that followed the first string.
			const close = Math.min(j, source.length);
			blank(code, bodyStart, Math.max(close - 1, bodyStart));
			i = close;
			continue;
		}
		if (char === "/" && opensRegex(code, i)) {
			let end = i + 1;
			let inClass = false;
			while (end < source.length) {
				const inner = source.charAt(end);
				if (inner === "\\") {
					end += 2;
					continue;
				}
				if (inner === "[") {
					inClass = true;
				} else if (inner === "]") {
					inClass = false;
				} else if (inner === "/" && !inClass) {
					end += 1;
					break;
				} else if (inner === "\n") {
					break;
				}
				end += 1;
			}
			// A regex is neither prose nor a fetch target: blank it in both
			// masks so `^https?://` in a DNR pattern is not a URL finding and
			// `/\/\//` cannot open a phantom comment.
			blank(text, i, end);
			blank(code, i, end);
			i = end;
			continue;
		}
		i += 1;
	}
	return { text: text.join(""), code: code.join("") };
}

/** Comments blanked, everything else intact. Retained as the named contract. */
export function stripJsComments(source: string): string {
	return maskJs(source).text;
}

/**
 * Blank the string literal that follows a `regexFilter` key.
 *
 * A DNR `regexFilter` is a *match pattern* for navigations the browser makes,
 * not something the extension fetches: `"^https?://...\\.pdf$"` is the single
 * most legitimate remote-looking string in the whole package, and it changes
 * whenever the ruleset is retuned. Blanking it keeps the gate from flagging the
 * ruleset that exists precisely to keep the extension off the network.
 *
 * The alternation is inside the capture group on purpose. Written the obvious
 * way — prefix, then `('...'|"..."|`...`)`, with the alternation *outside* the
 * group — the second branch matches every double-quoted string in the file, so
 * the scan silently sees no string content at all and a real `https://` in a
 * module is never reported. That bug shipped here once; the planted-violation
 * tests are what caught it.
 */
export function blankRegexFilters(masked: string): string {
	return masked.replace(
		/(regexFilter\s*:\s*)('(?:[^'\\\n]|\\.)*'|"(?:[^"\\\n]|\\.)*"|`(?:[^`\\]|\\.)*`)/g,
		(match) => " ".repeat(match.length),
	);
}

/** An inclusive-exclusive `[start, end)` range of a module's source. */
interface Span {
	readonly start: number;
	readonly end: number;
}

/**
 * The end of a statement starting at `from`: a `;`, or a newline at depth 0.
 *
 * Runs on the `code` mask, where string and regex bodies are already blanked.
 * That matters for correctness, not tidiness: an unbalanced `(` inside a URL
 * string would otherwise leave the depth counter permanently raised and the
 * span would run to the end of the file — silently exempting every remote URL
 * after it.
 */
function endOfStatement(code: string, from: number): number {
	let depth = 0;
	let i = from;
	while (i < code.length) {
		const char = code.charAt(i);
		if (char === "(" || char === "[" || char === "{") {
			depth += 1;
		} else if (char === ")" || char === "]" || char === "}") {
			if (depth === 0) {
				return i;
			}
			depth -= 1;
		} else if ((char === ";" || char === "\n") && depth === 0) {
			return i;
		}
		i += 1;
	}
	return code.length;
}

/** A reviewed data-only export found in a module, with its exempt range. */
export interface DataExport {
	readonly name: string;
	readonly span: Span;
}

/**
 * Locate the {@link DATA_ONLY_EXPORTS} initializers in a module.
 *
 * The exemption is *structural*, not a name match: a name is honoured only
 * while its initializer contains no `(` and no `)`, i.e. while it is a plain
 * array/object of text. The moment a call expression appears in there the name
 * stops being exempt and every remote URL inside it is reported like any other.
 * That is what keeps the allowlist from becoming a back door.
 *
 * Both masks are read: the declaration is located in `code` (so a `const` word
 * inside a string cannot open a span) and the body is measured in `code` (so
 * the depth count sees code, not URL text).
 */
export function dataExportSpans(mask: JsMask): DataExport[] {
	const found: DataExport[] = [];
	for (const match of mask.code.matchAll(CONST_DECLARATION)) {
		const name = match[1] ?? "";
		if (!DATA_ONLY_EXPORTS.has(name)) {
			continue;
		}
		const start = (match.index ?? 0) + match[0].length;
		const end = endOfStatement(mask.code, start);
		const skeleton = mask.code.slice(start, end);
		if (skeleton.includes("(") || skeleton.includes(")")) {
			continue;
		}
		found.push({ name, span: { start, end } });
	}
	return found;
}

/**
 * Every remote URL literal in a shipped module, with its verdict.
 *
 * Exempt are the URLs inside a reviewed data-only export and inside a DNR
 * `regexFilter`, and any URL behind a local scheme (`blob:`/`data:` name an
 * origin but describe bytes this process made). Everything else comes back
 * with `exemptBy === ""`, which is a build failure.
 */
export function remoteLiteralsInModule(mask: JsMask): RemoteLiteral[] {
	const scannable = blankRegexFilters(mask.text);
	const dataExports = dataExportSpans(mask);
	const literals: RemoteLiteral[] = [];
	for (const match of scannable.matchAll(REMOTE_URL)) {
		const index = match.index ?? 0;
		const exempting = dataExports.find(
			(exported) => index >= exported.span.start && index < exported.span.end,
		);
		if (
			exempting === undefined &&
			LOCAL_SCHEME_PREFIX.test(scannable.slice(Math.max(0, index - SCHEME_PREFIX_WINDOW), index))
		) {
			continue;
		}
		literals.push({
			value: match[0],
			line: lineOf(mask.text, index),
			exemptBy: exempting?.name ?? "",
		});
	}
	return literals;
}

/**
 * Every violation in one shipped module: remote literals, dynamic-code sinks
 * and module specifiers that do not resolve to a packaged file.
 *
 * The import check is what catches the "it worked on my machine" remote code:
 * a specifier that resolves to nothing is a module the browser will try to
 * fetch, and a `node:` builtin is a module that cannot exist in a page at all.
 * A computed `import(expr)` is reported too: the gate cannot resolve it, and
 * "the gate cannot resolve it" is exactly the state in which a runtime fetch
 * hides.
 */
export function findModuleViolations(
	filePath: string,
	source: string,
	shipped: ReadonlySet<string>,
): JsViolation[] {
	const mask = maskJs(source);
	const violations: JsViolation[] = [];
	for (const literal of remoteLiteralsInModule(mask)) {
		if (literal.exemptBy !== "") {
			continue;
		}
		violations.push({
			line: literal.line,
			class: "remote-url-literal",
			detail: `remote URL literal in shipped code: ${literal.value} (ADR-P0028: ship the code, do not fetch it)`,
		});
	}
	for (const [pattern, why] of DYNAMIC_CODE_SINKS) {
		for (const match of mask.code.matchAll(pattern)) {
			violations.push({
				line: lineOf(mask.code, match.index ?? 0),
				class: "dynamic-code",
				detail: why,
			});
		}
	}
	for (const match of mask.text.matchAll(IMPORT_SPECIFIER)) {
		const prefix = match[1] ?? "";
		const specifier = match[3] ?? "";
		const index = match.index ?? 0;
		// A string that merely reads like an import is blanked in `code`, so a
		// prefix that survives there is a real `from`/`import`/`require`.
		if (!mask.code.startsWith(prefix, index)) {
			continue;
		}
		const resolved = classifyRef(filePath, specifier, shipped);
		if (specifier.startsWith("node:")) {
			// Checked before the remote branch: `classifyRef` calls any scheme the
			// package does not own "remote", and `node:` is one. Reporting it as
			// remote would be true and useless — the browser is not going to
			// fetch `node:fs`, the build is simply broken.
			violations.push({
				line: lineOf(mask.text, index),
				class: "node-builtin-import",
				detail: `import of '${specifier}': Node builtins do not exist in an extension page`,
			});
			continue;
		}
		if (resolved.kind === "remote") {
			violations.push({
				line: lineOf(mask.text, index),
				class: "remote-import",
				detail: `import of '${specifier}': every module must be inside the package`,
			});
			continue;
		}
		if (resolved.kind === "unresolved") {
			violations.push({
				line: lineOf(mask.text, index),
				class: "unresolved-import",
				detail: `import of '${specifier}' does not resolve to a packaged file`,
			});
		}
	}
	for (const match of mask.code.matchAll(COMPUTED_DYNAMIC_IMPORT)) {
		violations.push({
			line: lineOf(mask.code, match.index ?? 0),
			class: "unresolvable-dynamic-import",
			detail: "import(<expression>): the target cannot be checked, so it cannot be shipped",
		});
	}
	return violations;
}
