/**
 * Stage 3 chrome structure and accessible names, without a DOM.
 *
 * `renderToStaticMarkup` is deliberately the boundary: if the chrome needs a
 * browser to produce a meaningful screen, this test fails. It drives a real
 * UI.02 controller against the mock adapter, so the page labels, list label and
 * tile classes are the same values the browser harness receives.
 */

import type { ThemePreferences } from "@selis/ui-kit";
import { ToastProvider, TooltipProvider } from "@wertkit/ui";
import { createRef } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";
import { createMockAdapter } from "../platform/mock-adapter.js";
import { createPageList } from "../viewer/page-list.js";
import type { PageList } from "../viewer/page-list.js";
import { DEFAULT_CHROME_PREFERENCES, applyChromeTheme } from "./app.js";
import { ChromeScreen } from "./screen.js";

const cleanups: Array<() => void | Promise<void>> = [];

afterEach(async () => {
	while (cleanups.length > 0) {
		await cleanups.pop()?.();
	}
});

async function fixture(): Promise<{
	readonly html: string;
	readonly pageCount: number;
}> {
	const adapter = createMockAdapter();
	const source = adapter.addDocument({
		name: "sample.pdf",
		pageCount: 4,
		textPages: ["one", "two", "three", "four"],
	});
	const doc = await adapter.engine.open(source);
	let list: PageList | null = null;
	cleanups.push(async () => {
		list?.dispose();
		await adapter.dispose?.();
	});
	list = createPageList({ adapter, doc, mode: "page" });
	const state = list.update({
		width: 960,
		height: 720,
		scrollTop: 0,
		devicePixelRatio: 1,
	});
	const html = renderToStaticMarkup(
		<TooltipProvider>
			<ToastProvider>
				<ChromeScreen
					state={state}
					preferences={DEFAULT_CHROME_PREFERENCES}
					scrollRef={createRef<HTMLDivElement>()}
					onPreferencesChange={() => undefined}
					onModeChange={() => undefined}
					onZoomChange={() => undefined}
					onPageChange={() => undefined}
					onScroll={() => undefined}
					onKeyDown={() => undefined}
				/>
			</ToastProvider>
		</TooltipProvider>,
	);
	return { html, pageCount: doc.pageCount };
}

describe("ChromeScreen", () => {
	it("renders UI.02's real page list and the shell chrome", async () => {
		const { html, pageCount } = await fixture();
		expect(pageCount).toBe(4);
		expect(html).toContain("wk-AppShell_root");
		expect(html).toContain("Selis PDF");
		expect(html).toContain("sample.pdf");
		expect(html).toContain("selis-page-list--virtual");
		expect(html).toContain("Page 1 of 4");
		expect(html).toContain("Page 2 of 4");
		expect(html).toContain("Previous");
		expect(html).toContain("Next");
		expect(html).toContain("High contrast");
		expect(html).toContain("Compact density");
		expect(html).toContain("Reduce motion");
	});

	it("mounts every component family in the stage 3 surface", async () => {
		const { html } = await fixture();
		// Button is the trigger for several families; these assertions name the
		// accessible role each family contributes so a removed import is visible.
		expect(html).toContain('aria-haspopup="dialog"');
		expect(html).toContain('aria-haspopup="menu"');
		expect(html).toContain('role="tablist"');
		expect(html).toContain('role="combobox"');
		expect(html).toContain('role="checkbox"');
		expect(html).toContain('role="switch"');
		expect(html).toContain('data-state="closed"');
		expect(html).toContain('aria-live="polite"');
	});
});

describe("chrome theme application", () => {
	it("clears stale attributes before applying a new preference set", () => {
		const attributes = new Map<string, string>([
			["data-theme", "dark"],
			["data-contrast", "high"],
			["data-density", "compact"],
			["data-motion", "reduced"],
		]);
		const root = {
			removeAttribute(name: string) {
				attributes.delete(name);
			},
			setAttribute(name: string, value: string) {
				attributes.set(name, value);
			},
		} as unknown as HTMLElement;
		const preferences: ThemePreferences = {
			theme: "light",
			contrast: "normal",
			density: "comfortable",
			motion: "system",
		};
		applyChromeTheme(preferences, root);
		expect([...attributes.entries()]).toEqual([["data-theme", "light"]]);
	});
});
