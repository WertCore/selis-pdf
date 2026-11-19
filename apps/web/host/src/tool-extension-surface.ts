/**
 * Extension surface for tools (SL-1A.UI.04).
 *
 * A typed hook where a first-party extension can register additional tool
 * cards into a panel, alongside the built-in registry. The existing tools page
 * is unchanged: `TOOLS`, the card grid and the generated pages still work on
 * their own, and a registered extension card is additive. Nothing here opens
 * a side panel, runs a content script, or sends a document anywhere.
 *
 * Registration is intentionally narrow. A card is a label plus a same-origin,
 * root-relative link — not arbitrary script — so an extension cannot smuggle a
 * `javascript:`/`data:` URL into the page or collide with a built-in tool id.
 * The panel renders through a small typed DOM seam so it is testable without
 * jsdom (the pattern from `editor/js-consent.ts`).
 */

import { TOOL_BY_ID } from "./tool-registry.js";

/** The most cards one extension may register, so a panel cannot grow without bound. */
export const MAX_EXTENSION_CARDS = 32;

/** One tool card contributed by an extension. */
export interface ToolExtensionCard {
	/** Stable, unique, lower-case id (not a built-in tool id). */
	readonly id: string;
	readonly label: string;
	/** Root-relative, same-origin path, e.g. `/merge-pdf/`. */
	readonly href: string;
	readonly blurb?: string;
	readonly icon?: string;
}

/** A registration handle; calling `unregister` removes the card again. */
export interface ToolExtensionRegistration {
	readonly id: string;
	readonly source: string;
	unregister(): void;
}

/** Typed result of a registration attempt. */
export type RegisterCardResult =
	| { readonly ok: true; readonly registration: ToolExtensionRegistration }
	| { readonly ok: false; readonly reason: string };

const ID_RE = /^[a-z][a-z0-9-]{1,63}$/;
const SENTINEL_ORIGIN = "https://selis.invalid";

/**
 * Whether a card link is a same-origin, root-relative path. A hostile URL is
 * refused before it can reach an `href`: no schemes, no protocol-relative
 * `//`, no userinfo, no traversal, no backslash, no control characters.
 */
export function isSafeExtensionHref(href: string): boolean {
	if (
		href === "" ||
		href.startsWith("//") ||
		/[\\\u0000-\u001f]/.test(href) ||
		href.includes("/..") ||
		href.toLowerCase().includes("%2e%2e")
	) {
		return false;
	}
	if (!href.startsWith("/")) {
		return false;
	}
	let url: URL;
	try {
		url = new URL(href, SENTINEL_ORIGIN);
	} catch {
		return false;
	}
	return (
		url.origin === SENTINEL_ORIGIN &&
		url.protocol === "https:" &&
		url.username === "" &&
		url.password === "" &&
		!url.pathname.includes("..")
	);
}

function validateCard(
	card: ToolExtensionCard,
	knownIds: ReadonlySet<string>,
): string | null {
	if (typeof card.id !== "string" || !ID_RE.test(card.id)) {
		return "malformed card id";
	}
	if (knownIds.has(card.id)) {
		return `card id "${card.id}" collides with a built-in tool`;
	}
	if (typeof card.label !== "string" || card.label.trim() === "" || card.label.length > 80) {
		return "malformed card label";
	}
	if (typeof card.href !== "string" || !isSafeExtensionHref(card.href)) {
		return "unsafe card href";
	}
	return null;
}

/**
 * The registry. A fresh instance refuses built-in tool ids by default, so the
 * extension surface cannot shadow a card the existing page already renders.
 */
export class ToolExtensionRegistry {
	private readonly cards: ToolExtensionCard[] = [];
	private readonly owners = new Map<string, string>();

	constructor(private readonly knownIds: ReadonlySet<string> = new Set(TOOL_BY_ID.keys())) {}

	get size(): number {
		return this.cards.length;
	}

	register(card: ToolExtensionCard, source = "extension"): RegisterCardResult {
		const problem = validateCard(card, this.knownIds);
		if (problem !== null) {
			return { ok: false, reason: problem };
		}
		if (this.owners.has(card.id)) {
			return { ok: false, reason: `card id "${card.id}" is already registered` };
		}
		if (this.cards.length >= MAX_EXTENSION_CARDS) {
			return { ok: false, reason: "extension card limit reached" };
		}
		this.cards.push(card);
		this.owners.set(card.id, source);
		const registration: ToolExtensionRegistration = {
			id: card.id,
			source,
			unregister: () => {
				const index = this.cards.findIndex((entry) => entry.id === card.id);
				if (index >= 0) {
					this.cards.splice(index, 1);
				}
				if (this.owners.get(card.id) === source) {
					this.owners.delete(card.id);
				}
			},
		};
		return { ok: true, registration };
	}

	/** A copy of the registered cards, in registration order. */
	list(): readonly ToolExtensionCard[] {
		return [...this.cards];
	}

	clear(): void {
		this.cards.length = 0;
		this.owners.clear();
	}
}

/** Minimal element surface for the panel; a real element satisfies it structurally. */
export interface ToolExtensionPanelElement {
	className: string;
	textContent: string;
	dataset: Record<string, string | undefined>;
	append(...children: ToolExtensionPanelElement[]): void;
}

/** An anchor in the panel additionally carries a real `href`. */
export interface ToolExtensionAnchorElement extends ToolExtensionPanelElement {
	href: string;
}

/** Element factory; the production one wraps `document.createElement`. */
export type ToolExtensionElementFactory = (tag: string) => ToolExtensionPanelElement;

/** The only place the real DOM is touched. */
export const browserToolExtensionElement: ToolExtensionElementFactory = (
	tag: string,
): ToolExtensionPanelElement => document.createElement(tag) as unknown as ToolExtensionPanelElement;

/**
 * Render the registered cards into a labelled panel. Real anchors are used, so
 * middle-click, "open in new tab", and a no-JS fallback need no script path.
 */
export function renderToolExtensionPanel(
	cards: readonly ToolExtensionCard[],
	host: ToolExtensionPanelElement,
	createElement: ToolExtensionElementFactory = browserToolExtensionElement,
): ToolExtensionPanelElement {
	const panel = createElement("section");
	panel.className = "tool-extension-panel";
	panel.dataset.role = "tool-extension-panel";
	for (const card of cards) {
		const anchor = createElement("a") as ToolExtensionAnchorElement;
		anchor.className = "tool-extension-card";
		anchor.href = card.href; // validated by isSafeExtensionHref at registration
		anchor.dataset.toolExtension = card.id;
		if (card.icon !== undefined) {
			const icon = createElement("span");
			icon.className = "tool-extension-icon";
			icon.textContent = card.icon;
			icon.dataset.role = "presentation";
			anchor.append(icon);
		}
		const label = createElement("span");
		label.className = "tool-extension-label";
		label.textContent = card.label;
		anchor.append(label);
		if (card.blurb !== undefined) {
			const blurb = createElement("span");
			blurb.className = "tool-extension-blurb";
			blurb.textContent = card.blurb;
			anchor.append(blurb);
		}
		panel.append(anchor);
	}
	host.append(panel);
	return panel;
}
