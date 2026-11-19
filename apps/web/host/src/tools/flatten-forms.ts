/**
 * The flatten-forms card's presentation helpers.
 *
 * DOM-free and side-effect free, so the option copy and the result rows can be
 * tested without a browser. Op 44 returns the ordinary `Verification` object:
 * the numbers here are the engine's own measured field counts, never a claim
 * copied from the request.
 */

import { type Verification, type VerificationRow } from "./protocol.js";

/**
 * The honest option note. It says what is baked, what is removed, and that a
 * field with no appearance must be filled with synthesis first — so a reader
 * knows why a value might not be visible after flattening.
 */
export const FLATTEN_FORMS_NOTE =
	"Bakes each widget's normal appearance into the page content, removes the widget and drops the AcroForm — the values become part of the page and no longer interactive. A field with no appearance of its own is removed with the form: fill the form with appearance synthesis first if you need its typed value baked in. Non-widget annotations are left untouched. Your file is read and written on this device only — it is never uploaded.";

/**
 * The rows the flatten-forms card adds to its result. The field counts come
 * straight from the engine's verification; when the engine did not report a
 * count the card says so rather than printing a misleading zero.
 */
export function flattenFormsReportRows(verification: Verification): VerificationRow[] {
	if (verification.fields_in === undefined) {
		return [{ label: "Form", value: "the engine reported no field counts" }];
	}
	return [
		{
			label: "Interactive fields",
			value:
				verification.fields_out === 0
					? `none — ${verification.fields_in} baked or removed`
					: `${verification.fields_out} remain`,
		},
	];
}
