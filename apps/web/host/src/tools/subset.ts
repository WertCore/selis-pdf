/**
 * The subset-fonts card's presentation helpers (SL-4.TOOLS.02).
 *
 * DOM-free and side-effect free, so the option copy and the result rows can be
 * tested without a browser. The numbers come from the engine's own
 * verification object; nothing here invents or estimates a count. The bytes
 * before/after are already shown by `verificationRows`, so this module adds
 * only the subset-specific facts and the measured saving.
 */

import { type Verification, type VerificationRow, formatBytes } from "./protocol.js";

/**
 * The honest option note for the card. It names exactly what is supported and
 * says that anything else is refused with a reason, so a reader knows why a
 * file might produce no download.
 */
export const SUBSET_FONTS_NOTE =
	"Replaces embedded TrueType fonts with subsets that keep only the glyphs the document's text actually shows — the pages look the same and the file gets smaller. Supports WinAnsi simple fonts and Identity-H CID fonts with an Identity CIDToGIDMap. CFF fonts, other encodings, encrypted files and rewrites that would not shrink the file are refused with a reason; nothing is changed in those cases. Your file is read and written on this device only — it is never uploaded.";

/**
 * The rows the subset-fonts card adds to its result. Called with the same
 * measured `Verification` the standard rows use, so the fonts/glyph counts and
 * the saved bytes are the engine's numbers, not a claim from the file name.
 *
 * When the engine did not report a subset (for example an older chunk, or an
 * operation that refused a font), the card says so rather than showing a
 * misleading zero.
 */
export function subsetReportRows(verification: Verification): VerificationRow[] {
	const fonts = verification.fonts_subset;
	const glyphs = verification.glyphs_kept;
	if (fonts === undefined || glyphs === undefined) {
		return [{ label: "Fonts", value: "no font was subset — the engine reported no subset data" }];
	}
	const saved = verification.bytes_in - verification.bytes_out;
	return [
		{ label: "Fonts subset", value: String(fonts) },
		{ label: "Glyphs kept", value: String(glyphs) },
		{
			label: "Size saved",
			value: `${formatBytes(saved)} (${formatBytes(verification.bytes_in)} → ${formatBytes(verification.bytes_out)})`,
		},
	];
}
