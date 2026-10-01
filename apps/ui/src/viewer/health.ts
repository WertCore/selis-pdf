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
export function summariseDeviations(
	deviations: readonly { readonly name: string }[],
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
 */
export function healthRows(report: HealthReport): HealthRow[] {
	const rows: HealthRow[] = [];

	rows.push({
		id: "pages",
		severity: "ok",
		label: `${report.pages} page${report.pages === 1 ? "" : "s"}`,
	});

	rows.push({
		id: "encrypted",
		severity: report.encrypted ? "notice" : "ok",
		label: report.encrypted ? "Encrypted" : "Not encrypted",
	});

	// Permissions are only meaningful for an encrypted document, and their
	// ABSENCE is the honest answer for a clear one: there was no grant to
	// report. Rendering a row here for a clear document would invite the reader
	// to look for a permissions statement that does not exist.
	if (report.permissions !== null) {
		const denied = [
			!report.permissions.print && "printing",
			!report.permissions.modify && "editing",
			!report.permissions.copy && "copying",
			!report.permissions.annotate && "annotating",
		].filter((v): v is string => v !== false);
		rows.push({
			id: "permissions",
			severity: denied.length === 0 ? "ok" : "notice",
			label:
				denied.length === 0
					? "The document grants printing, editing, copying and annotating"
					: `The document forbids ${denied.join(", ")}`,
		});
	}

	// Untagged is a notice, never a warning. See rule 2 in the module docs.
	rows.push({
		id: "tagged",
		severity: report.tagged ? "ok" : "notice",
		label: report.tagged ? "Tagged" : "Not tagged — no reading order",
	});

	// A signature is NEVER `ok`. See rule 1.
	rows.push({
		id: "signature",
		severity: report.signature === "present" ? "notice" : "ok",
		label:
			report.signature === "present" ? "Signed — signature not verified" : "No signature field",
	});

	const summary = summariseDeviations(report.deviations);
	rows.push({
		id: "deviations",
		severity: summary.length === 0 ? "ok" : "warn",
		label:
			summary.length === 0
				? "No structural problems found"
				: `${summary.length} structural ${summary.length === 1 ? "problem" : "problems"}: ${summary
						.map((d) => (d.count === 1 ? d.name : `${d.count}x ${d.name}`))
						.join(", ")}`,
	});

	return rows;
}
