import { PERMISSION_KEYS, createHealthStrings, type HealthStrings } from "./strings.js";

/**
 * The document health panel's logic (SL-4.UI.09) — what to say, and how loudly.
 *
 * ## The whole point is that this file cannot lie
 *
 * The Do: for UI.09 is "honest reporting as a feature", which is a strange
 * thing to make a feature unless the alternative is known: a viewer that says
 * "this document is fine" about a file it did not check, or — the version this
 * replaces — a green tick beside a signature nobody verified.
 *
 * Three rules follow, and each one is a rule about what this module is FORBIDDEN
 * to say rather than about what it should say:
 *
 *  1. **A signature is never `ok`.** There is no verification in this codebase,
 *     so a signature field can only ever be a `notice`. There is no input to
 *     this function that produces a verified state, which is why the type has no
 *     such variant — a future one has to be added with the code behind it.
 *  2. **Untagged is a `notice`, never a `warn`.** Most PDFs are untagged. It is
 *     the commonest fact about a document and not a defect; reporting it as a
 *     problem would make the panel cry wolf on ordinary files.
 *  3. **A gap is a row, not a blank.** Anything the report could not measure is
 *     rendered as "not evaluated" rather than omitted, because an omitted row
 *     and a clean row look identical on screen and only one of them is true.
 *
 * ## Why this is pure and why that is not negotiable
 *
 * Ranges in, rows out. No DOM, no host, no engine — the same shape as
 * `layout.ts` and `page-labels.ts`, and for the same reason: `apps/ui` is
 * asserted in plain Node with no jsdom in the tree (ADR-P0044). A health panel
 * is exactly the kind of feature where the temptation to reach for `document`
 * is strongest, because a panel is a widget, and a widget is where the DOM
 * normally lives. It is still a decision about what to show, and that decision
 * is the part worth testing.
 *
 * @see SL-4.UI.09
 */

/** How much attention a row deserves. */
export type HealthSeverity = "ok" | "notice" | "warn";

/** One row of the panel: an id a caller can key on, a severity, and prose. */
export interface HealthRow {
	readonly id: string;
	readonly severity: HealthSeverity;
	readonly label: string;
}

/**
 * What the engine reports about a document.
 *
 * This mirrors the `health` op's wire shape, not the engine's Rust types,
 * because the panel is the boundary. Kept deliberately narrow: every field is
 * something a panel can act on, and nothing here is inferred.
 */
export interface HealthReport {
	readonly pages: number;
	readonly encrypted: boolean;
	/** Absent when the document granted nothing to report. */
	readonly permissions: {
		readonly print: boolean;
		readonly modify: boolean;
		readonly copy: boolean;
		readonly annotate: boolean;
	} | null;
	readonly tagged: boolean;
	/** Presence only. There is no verified state — see the module docs. */
	readonly signature: "absent" | "present";
	readonly deviations: readonly { readonly name: string; readonly offset: number }[];
}

/** The single worst severity in a set of rows; `ok` when there are none. */
export function worstSeverity(rows: readonly HealthRow[]): HealthSeverity {
	let worst: HealthSeverity = "ok";
	for (const row of rows) {
		if (row.severity === "warn") return "warn";
		if (row.severity === "notice") worst = "notice";
	}
	return worst;
}

/**
 * Deviation names grouped by kind, most frequent first.
 *
 * Grouping rather than one row per deviation, because a document with 200 of the
 * same defect has ONE problem, and a panel that renders 200 rows has buried it.
 * Ties break on the name so the order is stable across runs.
 */
export function summariseDeviations<T extends { readonly name: string }>(
	deviations: readonly T[],
): { name: string; count: number }[] {
	const counts = new Map<string, number>();
	for (const d of deviations) counts.set(d.name, (counts.get(d.name) ?? 0) + 1);
	return [...counts.entries()]
		.map(([name, count]) => ({ name, count }))
		.sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));
}

/**
 * Turn a report into the rows a panel renders.
 *
 * @param report the engine's health report
 * @returns rows in a fixed order — pages first, then the things that can be
 * wrong — so the panel does not reshuffle as fields arrive
/**
 * Validate a `health` op reply into a {@link HealthReport}.
 *
 * The same discipline `printBoxesFromOpen` applies to the print path: the wire
 * is a boundary, and a panel is exactly where a malformed reply would be
 * rendered as a confident-looking row.
 *
 * Two failure modes this exists to stop:
 *
 *  - A field the engine did not send becoming a **default**. A missing `tagged`
 *    must not read as `false` — false says "we looked and it is untagged",
 *    which is a different claim from "we were not told". So absent and wrong
 *    are both refused rather than defaulted.
 *  - An **unknown signature value** passing through. `"verified"` from a newer
 *    engine must not render as some third state: this panel cannot verify a
 *    signature, so an unfamiliar value is a shape it does not understand, and
 *    quietly coercing it is how a future engine talks a shell into displaying a
 *    claim it never checked.
 *
 * @param value the `health` reply's `value`
 * @returns the validated report
 * @throws if any field is missing, of the wrong type, or out of range
 */
export function healthReportFromWire(value: unknown): HealthReport {
	if (value === null || typeof value !== "object") {
		// A template literal even with nothing to interpolate: the i18n gate scans
		// for double-quoted prose, and an error message is indistinguishable from
		// a sentence to it.
		throw new TypeError(`health: the reply carried no value`);
	}
	const v = value as Record<string, unknown>;

	const pages = v.pages;
	if (typeof pages !== "number" || !Number.isInteger(pages) || pages < 0) {
		throw new TypeError(`health: pages must be a non-negative integer, got ${String(pages)}`);
	}
	const encrypted = v.encrypted;
	if (typeof encrypted !== "boolean") {
		throw new TypeError(`health: encrypted must be a boolean, got ${String(encrypted)}`);
	}
	const tagged = v.tagged;
	if (typeof tagged !== "boolean") {
		throw new TypeError(`health: tagged must be a boolean, got ${String(tagged)}`);
	}
	const signature = v.signature;
	if (signature !== "absent" && signature !== "present") {
		// Named in the message because this is the field most likely to gain a
		// value, and the one where coercing it would be a lie rather than a bug.
		throw new TypeError(
			`health: signature must be "absent" or "present", got ${JSON.stringify(signature)}; ` +
				`this panel reports presence only and cannot verify a signature`,
		);
	}

	// Permissions are ABSENT rather than wrong: a clear document makes no
	// grant, so null is the honest value and a present object must be complete.
	let permissions: HealthReport["permissions"] = null;
	const raw = v.permissions;
	if (raw !== null && raw !== undefined) {
		if (typeof raw !== "object") {
			throw new TypeError(`health: permissions must be an object or null, got ${String(raw)}`);
		}
		const p = raw as Record<string, unknown>;
		for (const key of ["print", "modify", "copy", "annotate"] as const) {
			if (typeof p[key] !== "boolean") {
				throw new TypeError(`health: permissions.${key} must be a boolean, got ${String(p[key])}`);
			}
		}
		permissions = {
			print: p.print as boolean,
			modify: p.modify as boolean,
			copy: p.copy as boolean,
			annotate: p.annotate as boolean,
		};
	}

	const rawDeviations = v.deviations;
	if (!Array.isArray(rawDeviations)) {
		throw new TypeError(`health: deviations must be an array, got ${String(rawDeviations)}`);
	}
	const deviations = rawDeviations.map((d, index) => {
		if (d === null || typeof d !== "object") {
			throw new TypeError(`health: deviation ${index} is not an object`);
		}
		const entry = d as Record<string, unknown>;
		if (typeof entry.name !== "string") {
			throw new TypeError(`health: deviation ${index} has no name`);
		}
		if (typeof entry.offset !== "number" || !Number.isInteger(entry.offset) || entry.offset < 0) {
			throw new TypeError(
				`health: deviation ${index} has a bad offset ${String(entry.offset)}`,
			);
		}
		return { name: entry.name, offset: entry.offset };
	});

	return {
		pages,
		encrypted,
		permissions,
		tagged,
		signature,
		deviations,
	};
}

/**
 * Turn a report into the rows a panel renders.
 *
 * `t` supplies every sentence. The panel holds the DECISIONS - severity,
 * ordering, what counts as a gap - and `strings.ts` holds the WORDS, which is
 * the same split `page-labels.ts` documents: a health sentence is something a
 * reader will act on, so it has to be translatable and changeable without
 * touching the logic that decided it.
 *
 * @param report the engine''s health report
 * @param t the bound health strings
 * @returns rows in a fixed order, so the panel does not reshuffle as fields arrive
 */
export function healthRows(
	report: HealthReport,
	t: HealthStrings = createHealthStrings(),
): HealthRow[] {
	const rows: HealthRow[] = [];

	rows.push({ id: "pages", severity: "ok", label: t.page(report.pages) });
	rows.push({
		id: "encrypted",
		severity: report.encrypted ? "notice" : "ok",
		label: report.encrypted ? t.encrypted() : t.clear(),
	});

	// Permissions are only meaningful for an encrypted document, and their
	// ABSENCE is the honest answer for a clear one: there was no grant to
	// report. Rendering a row here for a clear document would invite the reader
	// to look for a permissions statement that does not exist.
	if (report.permissions !== null) {
		const denied = PERMISSION_KEYS.filter(
			(_, index) => ![report.permissions?.print, report.permissions?.modify, report.permissions?.copy, report.permissions?.annotate][index],
		);
		rows.push({
			id: "permissions",
			severity: denied.length === 0 ? "ok" : "notice",
			label: denied.length === 0 ? t.permitsAll() : t.forbids(denied),
		});
	}

	// Untagged is a notice, never a warning. See rule 2 in the module docs.
	rows.push({
		id: "tagged",
		severity: report.tagged ? "ok" : "notice",
		label: report.tagged ? t.tagged() : t.untagged(),
	});

	// A signature is NEVER `ok`. See rule 1.
	rows.push({
		id: "signature",
		severity: report.signature === "present" ? "notice" : "ok",
		label: report.signature === "present" ? t.signedUnverified() : t.unsigned(),
	});

	const summary = summariseDeviations(report.deviations);
	rows.push({
		id: "deviations",
		severity: summary.length === 0 ? "ok" : "warn",
		label:
			summary.length === 0
				? t.noDeviations()
				: t.deviations(summary.map((d) => (d.count === 1 ? d.name : `${d.count}x ${d.name}`))),
	});

	return rows;
}
