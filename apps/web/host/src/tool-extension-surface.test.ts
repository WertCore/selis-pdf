/**
 * SL-1A.UI.04 — extension surface for tools.
 *
 * The built-in registry is left intact (the existing tools page keeps working);
 * extension cards are registered, validated and rendered through a typed DOM
 * seam. No DOM library is needed, and no card can carry script as its href.
 */

import { describe, expect, it } from "vitest";
import { TOOLS } from "./tool-registry.js";
import {
	MAX_EXTENSION_CARDS,
	ToolExtensionRegistry,
	type ToolExtensionCard,
	type ToolExtensionPanelElement,
	isSafeExtensionHref,
	renderToolExtensionPanel,
} from "./tool-extension-surface.js";

const CARD: ToolExtensionCard = {
	id: "my-archive-tool",
	label: "Archive tool",
	href: "/merge-pdf/",
	blurb: "Runs in the extension's own page.",
};

class MockElement implements ToolExtensionPanelElement {
	className = "";
	textContent = "";
	dataset: Record<string, string | undefined> = {};
	readonly children: MockElement[] = [];
	href = "";

	constructor(readonly tagName: string) {}

	append(...children: ToolExtensionPanelElement[]): void {
		for (const child of children) {
			this.children.push(child as MockElement);
		}
	}

	find(className: string): MockElement | undefined {
		if (this.className.split(/\s+/).includes(className)) {
			return this;
		}
		for (const child of this.children) {
			const found = child.find(className);
			if (found !== undefined) {
				return found;
			}
		}
		return undefined;
	}
}

describe("SL-1A.UI.04 tool extension surface", () => {
	it("registers, lists and unregisters a card without touching the built-in tools", () => {
		const before = TOOLS.length;
		const registry = new ToolExtensionRegistry();
		const result = registry.register(CARD, "test-ext");
		expect(result.ok).toBe(true);
		expect(registry.size).toBe(1);
		expect(registry.list()).toEqual([CARD]);
		if (result.ok) {
			expect(result.registration.id).toBe("my-archive-tool");
			expect(result.registration.source).toBe("test-ext");
			result.registration.unregister();
		}
		expect(registry.list()).toEqual([]);
		expect(TOOLS.length).toBe(before);
	});

	it("refuses a duplicate id and a built-in tool id", () => {
		const registry = new ToolExtensionRegistry();
		expect(registry.register(CARD).ok).toBe(true);
		const again = registry.register({ ...CARD, label: "Other" });
		expect(again.ok).toBe(false);
		if (!again.ok) {
			expect(again.reason).toMatch(/already registered/);
		}
		const collision = registry.register({ ...CARD, id: "merge" });
		expect(collision.ok).toBe(false);
		if (!collision.ok) {
			expect(collision.reason).toMatch(/built-in tool/);
		}
	});

	it("refuses malformed ids and labels", () => {
		const registry = new ToolExtensionRegistry();
		for (const id of ["Merge", "a", "has space", "../evil", "a".repeat(65)]) {
			expect(registry.register({ ...CARD, id }).ok, id).toBe(false);
		}
		expect(registry.register({ ...CARD, label: "  " }).ok).toBe(false);
	});

	it("accepts only same-origin, root-relative links", () => {
		expect(isSafeExtensionHref("/merge-pdf/")).toBe(true);
		for (const href of [
			"javascript:alert(1)",
			"data:text/html,x",
			"//evil.example/x",
			"https://evil.example/x",
			"/../secret",
			"/a\\b",
			"/a\u0000b",
			"merge-pdf/",
		]) {
			expect(isSafeExtensionHref(href), href).toBe(false);
		}
		const registry = new ToolExtensionRegistry();
		expect(registry.register({ ...CARD, href: "javascript:alert(1)" }).ok).toBe(false);
	});

	it("caps the number of cards", () => {
		const registry = new ToolExtensionRegistry();
		for (let i = 0; i < MAX_EXTENSION_CARDS; i += 1) {
			expect(
				registry.register({
					id: `ext-tool-${i}`,
					label: `Tool ${i}`,
					href: `/tool-${i}/`,
				}).ok,
			).toBe(true);
		}
		expect(
			registry.register({ id: "one-too-many", label: "Too many", href: "/extra/" }).ok,
		).toBe(false);
	});

	it("renders a labelled panel of real anchors through the DOM seam", () => {
		const host = new MockElement("div");
		const created: MockElement[] = [];
		const factory = (tag: string): ToolExtensionPanelElement => {
			const element = new MockElement(tag);
			created.push(element);
			return element;
		};
		const panel = renderToolExtensionPanel(
			[Card("one"), Card("two")],
			host,
			factory,
		) as MockElement;
		expect(panel.dataset.role).toBe("tool-extension-panel");
		expect(created.filter((element) => element.tagName === "a")).toHaveLength(2);
		const link = panel.children[0] as MockElement;
		expect(link.href).toBe("/tool-one/");
		expect(link.dataset.toolExtension).toBe("ext-one");
		expect(link.find("tool-extension-label")?.textContent).toBe("Tool one");
		expect(link.find("tool-extension-icon")?.textContent).toBe("★");
		expect(host.children).toContain(panel);
	});
});

function Card(suffix: string): ToolExtensionCard {
	return {
		id: `ext-${suffix}`,
		label: `Tool ${suffix}`,
		href: `/tool-${suffix}/`,
		icon: "★",
	};
}
