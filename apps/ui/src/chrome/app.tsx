/**
 * Chrome host wiring (SL-4.UI.14 stage 3).
 *
 * Two boundaries are deliberate here:
 *
 * - **Theme authority stays with `@selis/ui-kit`.** `applyTheme` owns the four
 *   attributes; this module only clears stale attributes before applying a new
 *   preference set, because `applyTheme` is additive by design. Attributes go
 *   on `document.documentElement` so Radix portals (mounted under `<body>`)
 *   inherit the same theme. `@wertkit/ui`'s ThemeProvider/ThemeScript are not
 *   used.
 * - **The viewer stays headless.** This module imports the mock adapter and the
 *   UI.02 controller, but every DOM measurement is passed in through
 *   `PageListViewport`; the controller itself never sees `window` or
 *   `document`. `src/index.ts` is untouched, so importing `@selis/ui` still
 *   does not import React.
 *
 * The demo document is intentionally a mock fixture: UI.03's compositor paints
 * tiles and UI.07 owns file opening, while stage 3's DoD is the chrome itself
 * rendering real page-list state.
 */

import { applyTheme } from "@selis/ui-kit";
import type { ThemePreferences } from "@selis/ui-kit";
import { ToastProvider, TooltipProvider } from "@wertkit/ui";
import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent, ReactElement } from "react";
import { createRoot } from "react-dom/client";
import type { PlatformAdapter } from "../platform/adapter.js";
import { createMockAdapter } from "../platform/mock-adapter.js";
import type { MockDocumentSpec } from "../platform/mock-adapter.js";
import type { DocHandle } from "../platform/types.js";
import type { PageFitMode } from "../viewer/layout.js";
import { createPageList } from "../viewer/page-list.js";
import type {
	PageList,
	PageListMetrics,
	PageListState,
	PageListViewport,
} from "../viewer/page-list.js";
import { ChromeScreen } from "./screen.js";

/** The default preference set: light, normal contrast, comfortable, system motion. */
export const DEFAULT_CHROME_PREFERENCES: ThemePreferences = {
	theme: "light",
	contrast: "normal",
	density: "comfortable",
	motion: "system",
};

const THEME_ATTRIBUTES = ["data-theme", "data-contrast", "data-density", "data-motion"] as const;

/**
 * Apply selis preferences to an element, clearing attributes a *previous*
 * preference set may have set.
 *
 * `applyTheme` only ever calls `setAttribute`. Switching high contrast back to
 * normal, or reduced motion back to system, would therefore leave the old
 * attribute in place. That is a token-layer contract worth fixing at the caller
 * rather than changing ui-kit's DOM-free primitive late in this stage.
 */
export function applyChromeTheme(
	preferences: ThemePreferences,
	root: HTMLElement = document.documentElement,
): void {
	for (const attribute of THEME_ATTRIBUTES) {
		root.removeAttribute(attribute);
	}
	applyTheme(preferences, root);
}

export interface ChromeAppProps {
	/** An existing adapter and document handle. Pass both or neither. */
	readonly adapter?: PlatformAdapter;
	readonly doc?: DocHandle;
	/** Initial theme preference set; defaults to {@link DEFAULT_CHROME_PREFERENCES}. */
	readonly initialPreferences?: ThemePreferences;
	/** Mock fixture used only when `adapter`/`doc` are omitted. */
	readonly fixture?: MockDocumentSpec;
}

const DEFAULT_FIXTURE: MockDocumentSpec = {
	name: "sample.pdf",
	pageCount: 12,
	textPages: [
		"The quick brown fox jumps over the lazy dog.",
		"Selis keeps document bytes on this device.",
		"The page list is virtualised, so a large document stays cheap.",
		"Arrow keys, Page Up, Page Down, Home and End move between pages.",
		"High contrast and reduced motion are selis theme preferences.",
		"React renders the chrome; UI.02 owns the page geometry.",
		"Radix portals mount under body, so theme attributes live on the root.",
		"Tile artwork is UI.03's compositor.",
		"Search highlights are UI.05.",
		"Navigation panels are UI.06.",
		"Print planning is UI.08.",
		"Settings and localisation follow in UI.10 and UI.11.",
	],
};

function viewportFor(element: HTMLElement): PageListViewport {
	return {
		width: Math.max(1, element.clientWidth),
		height: Math.max(1, element.clientHeight),
		scrollTop: element.scrollTop,
		devicePixelRatio:
			typeof window === "undefined" || !Number.isFinite(window.devicePixelRatio)
				? 1
				: Math.max(1, window.devicePixelRatio),
	};
}

function metricsFor(element: HTMLElement): PageListMetrics {
	const style = getComputedStyle(element);
	const pageGap = Number.parseFloat(style.getPropertyValue("--selis-density-page-gap"));
	const padding = Number.parseFloat(style.getPropertyValue("--selis-space-6"));
	return {
		pageGap: Number.isFinite(pageGap) ? pageGap : 16,
		padding: Number.isFinite(padding) ? padding : 16,
	};
}

export function ChromeApp({
	adapter: providedAdapter,
	doc: providedDoc,
	initialPreferences = DEFAULT_CHROME_PREFERENCES,
	fixture = DEFAULT_FIXTURE,
}: ChromeAppProps): ReactElement {
	const [state, setState] = useState<PageListState | null>(null);
	const [preferences, setPreferences] = useState<ThemePreferences>(initialPreferences);
	const scrollRef = useRef<HTMLDivElement>(null);
	const listRef = useRef<PageList | null>(null);
	/**
	 * A page navigation whose *scroll anchor* may not have settled yet.
	 *
	 * `goToPage` returns the offset that puts the page at the viewport top
	 * against the layout it computed at that instant. A horizontal scrollbar
	 * appearing or disappearing between the call and the next frame re-fits the
	 * page and moves that page's top by the scrollbar's thickness, so applying
	 * the offset once can land the reader one page short. The shell owns the
	 * user's intent ("go to page N"), so it keeps re-deriving the offset from
	 * the settled layout until the controller agrees, then drops the anchor.
	 */
	const pendingPageRef = useRef<number | null>(null);

	useEffect(() => {
		applyChromeTheme(preferences);
	}, [preferences]);

	useEffect(() => {
		let cancelled = false;
		let ownedAdapter: PlatformAdapter | null = null;
		let ownedDoc: DocHandle | null = null;
		let observer: ResizeObserver | null = null;

		const external = providedAdapter !== undefined && providedDoc !== undefined;
		if ((providedAdapter === undefined) !== (providedDoc === undefined) && !external) {
			throw new Error("ChromeApp: pass both adapter and doc, or neither");
		}

		async function start(): Promise<void> {
			let adapter: PlatformAdapter;
			let doc: DocHandle;
			if (external && providedAdapter !== undefined && providedDoc !== undefined) {
				adapter = providedAdapter;
				doc = providedDoc;
			} else {
				const mock = createMockAdapter();
				ownedAdapter = mock;
				const source = mock.addDocument(fixture);
				adapter = mock;
				doc = await mock.engine.open(source);
				ownedDoc = doc;
			}
			if (cancelled) {
				if (ownedAdapter !== null) await ownedAdapter.dispose?.();
				return;
			}

			const element = scrollRef.current;
			const options = {
				adapter,
				doc,
				mode: "page" as const,
				onState: (next: PageListState) => {
					if (!cancelled) setState(next);
				},
				...(element === null ? {} : { metrics: metricsFor(element) }),
			};
			const list = createPageList(options);
			listRef.current = list;

			if (element !== null) {
				list.update(viewportFor(element));
				observer = new ResizeObserver(() => {
					const current = scrollRef.current;
					if (current !== null && listRef.current === list) {
						list.update(viewportFor(current));
						settlePendingPage();
					}
				});
				observer.observe(element);
			} else {
				list.update({ width: 900, height: 700, scrollTop: 0, devicePixelRatio: 1 });
			}
		}

		void start();

		return () => {
			cancelled = true;
			observer?.disconnect();
			listRef.current?.dispose();
			listRef.current = null;
			if (ownedAdapter !== null) {
				const adapter = ownedAdapter;
				const doc = ownedDoc;
				void (doc === null
					? adapter.dispose?.()
					: adapter.engine
							.close(doc)
							.catch(() => undefined)
							.then(() => adapter.dispose?.()));
			}
		};
	}, [fixture, providedAdapter, providedDoc]);

	const applyScrollTop = (top: number): void => {
		const element = scrollRef.current;
		if (element !== null) element.scrollTop = top;
	};

	/**
	 * Re-derive a pending page navigation from the freshest layout.
	 *
	 * Called after a scroll or resize has given the controller a settled
	 * viewport. If the page under the applied offset is not the page the reader
	 * asked for, `goToPage` runs again against the current geometry; when it
	 * agrees, the anchor is dropped. Bounded by the loop only reaching here
	 * after a DOM event on the same element, so a stable layout converges in
	 * one pass and a settling scrollbar in a handful.
	 */
	const settlePendingPage = (): void => {
		const list = listRef.current;
		if (list === null) return;
		const pending = pendingPageRef.current;
		if (pending === null) return;
		if (list.state().currentPage === pending) {
			pendingPageRef.current = null;
			return;
		}
		const target = list.goToPage(pending);
		if (target.currentPage === pending) {
			pendingPageRef.current = null;
		}
		applyScrollTop(target.scrollTop);
	};

	const reportScroll = (scrollTop: number): void => {
		const list = listRef.current;
		const element = scrollRef.current;
		if (list === null || element === null) return;
		list.update({ ...viewportFor(element), scrollTop });
		settlePendingPage();
	};

	const changeMode = (mode: PageFitMode): void => {
		const list = listRef.current;
		if (list === null) return;
		applyScrollTop(list.setMode(mode));
	};

	const changeZoom = (zoom: number): void => {
		const list = listRef.current;
		if (list === null) return;
		applyScrollTop(list.setZoom(zoom));
	};

	const changePage = (page: number): void => {
		const list = listRef.current;
		if (list === null) return;
		pendingPageRef.current = page;
		applyScrollTop(list.goToPage(page).scrollTop);
	};

	const handleKey = (key: string, event: KeyboardEvent<HTMLDivElement>): void => {
		const list = listRef.current;
		if (list === null) return;
		const next = list.handleKey(key);
		if (next === null) return;
		event.preventDefault();
		applyScrollTop(next.scrollTop);
	};

	return (
		<TooltipProvider delayDuration={250}>
			<ToastProvider>
				<ChromeScreen
					state={state}
					preferences={preferences}
					scrollRef={scrollRef}
					onPreferencesChange={setPreferences}
					onModeChange={changeMode}
					onZoomChange={changeZoom}
					onPageChange={changePage}
					onScroll={reportScroll}
					onKeyDown={handleKey}
				/>
			</ToastProvider>
		</TooltipProvider>
	);
}

export interface ChromeMount {
	/** Unmount React and release the page-list controller's scheduler. */
	unmount(): void;
}

/**
 * Mount the chrome into a host element. Importing this module has no DOM side
 * effects; only calling `mountChrome` touches the document.
 */
export function mountChrome(element: HTMLElement, props: ChromeAppProps = {}): ChromeMount {
	applyChromeTheme(props.initialPreferences ?? DEFAULT_CHROME_PREFERENCES);
	const root = createRoot(element);
	root.render(<ChromeApp {...props} />);
	return {
		unmount() {
			root.unmount();
		},
	};
}
