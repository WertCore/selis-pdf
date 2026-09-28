/**
 * The public surface of SL-4.UI.11's runtime.
 *
 * `message.ts` is the template syntax, `runtime.ts` is the catalogue resolver,
 * `pseudo-locale.ts` is the CI detector built on both, and `viewer/` registers
 * the viewer's own catalogues against it. A host that wants to localise the
 * viewer imports from here; a host that wants only the formatter — the
 * extension's options page, which has no viewer — imports `message.js` and
 * `runtime.js` directly, and `i18n-boundary.test.ts` is the gate that keeps
 * those two files free of anything it would have to drag along.
 */

export type {
	MessageTemplate,
	MessageValue,
	MessageValues,
	PluralCategory,
	PluralMessage,
} from "./message.js";
export {
	formatNumber,
	interpolate,
	isBlankTemplate,
	isMessageTemplate,
	placeholderNamesIn,
	placeholdersIn,
	pluralCategory,
	selectPluralVariant,
	sourceTextOf,
} from "./message.js";

export type {
	Catalogue,
	CatalogueSet,
	Formatter,
	GapKind,
	LocaleCoverage,
	MessageGap,
	MessageKey,
	MessageOrigin,
	MessageRuntime,
	MessageRuntimeOptions,
	NamespaceCoverage,
	Resolution,
} from "./runtime.js";
export {
	MISSING_MESSAGE_PREFIX,
	MISSING_MESSAGE_SUFFIX,
	PSEUDO_LOCALE,
	SOURCE_LOCALE,
	createFormatter,
	createMessageRuntime,
	isMissingMessage,
	missingMessageText,
	namespaceOf,
	negotiateLocale,
} from "./runtime.js";
export { I18nConfigurationError, MissingMessageError } from "./runtime.js";

export { createPseudoCatalogue } from "./pseudo-locale.js";
