/**
 * The React chrome (SL-4.UI.14 stage 3).
 *
 * This is the first *rendering* layer in `apps/ui`: the viewer itself stays
 * headless (ADR-P0044), and nothing here is exported from the package barrel.
 * `mountChrome`/`ChromeApp` live in `./app.js`; this file is pure and
 * server-renderable, which is what lets the chrome's structure and accessible
 * names be tested without a DOM.
 *
 * The content inside `AppShell` is UI.02's real `PageListState`, not a second
 * page model: tile class names, geometry, labels, roving tabindex and the live
 * announcement all come from `createPageList`. UI.03 will paint the actual
 * bitmaps into these boxes; until then the boxes are honest placeholders and
 * the screen is still a working, keyboard-scrollable page list.
 */

import type { ThemePreferences } from "@selis/ui-kit";
import {
	AppShell,
	Button,
	Checkbox,
	Dialog,
	DialogClose,
	Menu,
	MenuItem,
	MenuLabel,
	MenuSeparator,
	Select,
	SelectGroup,
	SelectItem,
	Switch,
	Tabs,
	TabsContent,
	TabsList,
	TabsTrigger,
	Tooltip,
	useToast,
} from "@wertkit/ui";
import type { CSSProperties, KeyboardEvent, ReactElement, RefObject, UIEvent } from "react";
import type { PageFitMode, PageListState } from "../viewer/index.js";

/** Inline geometry carriers are document data, not design values. */
type CssVariables = CSSProperties & Record<`--${string}`, string>;

export interface ChromeScreenProps {
	readonly state: PageListState | null;
	readonly preferences: ThemePreferences;
	readonly scrollRef: RefObject<HTMLDivElement | null>;
	readonly onPreferencesChange: (next: ThemePreferences) => void;
	readonly onModeChange: (mode: PageFitMode) => void;
	readonly onZoomChange: (zoom: number) => void;
	readonly onPageChange: (page: number) => void;
	readonly onScroll: (scrollTop: number) => void;
	readonly onKeyDown: (key: string, event: KeyboardEvent<HTMLDivElement>) => void;
}

const MODE_LABELS: Readonly<Record<PageFitMode, string>> = {
	page: "Whole page",
	width: "Fit width",
	spread: "Two pages",
};

function modeLabel(mode: PageFitMode): string {
	return MODE_LABELS[mode];
}

function PageList({ state, scrollRef, onScroll, onKeyDown }: ChromeScreenProps): ReactElement {
	const { toast } = useToast();
	const listStyle: CssVariables = {
		"--page-list-width": `${state?.contentWidth ?? 0}px`,
		"--page-list-height": `${state?.contentHeight ?? 0}px`,
	};

	return (
		<div className="selis-chrome__workspace">
			<div
				ref={scrollRef}
				className="selis-chrome__doc-area"
				// biome-ignore lint/a11y/useSemanticElements: the viewport div is
				// the scroller whose element ref the windowing code animates;
				// `role="region"` with a name is the honest ARIA for it.
				role="region"
				aria-label={state?.regionLabel ?? "Document pages"}
				aria-busy={state === null}
				// biome-ignore lint/a11y/noNoninteractiveTabindex: this region is the keyboard surface — pages scroll with the arrow keys here — and leaving it out of the tab order would strand the workspace.
				tabIndex={0}
				onScroll={(event: UIEvent<HTMLDivElement>) => onScroll(event.currentTarget.scrollTop)}
				onKeyDown={(event) => onKeyDown(event.key, event)}
			>
				{state === null ? (
					<output className="selis-chrome__empty">Preparing the sample document…</output>
				) : (
					<ul className="selis-page-list--virtual" aria-label={state.listLabel} style={listStyle}>
						{state.tiles.map((tile) => {
							const tileStyle: CssVariables = {
								"--tile-x": `${tile.x}px`,
								"--tile-y": `${tile.y}px`,
								"--tile-w": `${tile.width}px`,
								"--tile-h": `${tile.height}px`,
							};
							return (
								<li
									key={tile.page}
									className={tile.className}
									aria-label={tile.label}
									aria-current={tile.current ? "page" : undefined}
									tabIndex={tile.tabIndex}
									style={tileStyle}
								>
									<span className="selis-page-tile__label">{tile.label}</span>
								</li>
							);
						})}
					</ul>
				)}
				<p className="selis-visually-hidden" aria-live="polite" aria-atomic="true">
					{state?.announcement ?? ""}
				</p>
			</div>

			<footer className="selis-chrome__statusbar" aria-label="Document status">
				<span>
					{state === null ? "No document" : `Page ${state.currentPage + 1} of ${state.pageCount}`}
				</span>
				{state === null ? null : (
					<>
						<span aria-hidden="true">·</span>
						<span>{modeLabel(state.mode)}</span>
						<span aria-hidden="true">·</span>
						<span>{Math.round(state.zoom * 100)}%</span>
						<span className="selis-chrome__statusbar-spacer" />
						<Tooltip content="Show a transient status message">
							<Button
								variant="ghost"
								size="sm"
								onClick={() => {
									toast({
										title: `Page ${(state?.currentPage ?? 0) + 1} is in view`,
										description: "Tile rendering belongs to UI.03; the page list is UI.02.",
										tone: "neutral",
									});
								}}
							>
								Notification
							</Button>
						</Tooltip>
					</>
				)}
			</footer>
		</div>
	);
}

function ChromeSidebar({
	state,
	preferences,
	onPreferencesChange,
	onModeChange,
}: Pick<
	ChromeScreenProps,
	"state" | "preferences" | "onPreferencesChange" | "onModeChange"
>): ReactElement {
	const { toast } = useToast();
	const patch = (next: Partial<ThemePreferences>): void => {
		onPreferencesChange({ ...preferences, ...next });
	};

	return (
		<div className="selis-chrome__sidebar">
			<Tabs defaultValue="view" className="selis-chrome__tabs">
				<TabsList aria-label="Inspector sections">
					<TabsTrigger value="view">View</TabsTrigger>
					<TabsTrigger value="document">Document</TabsTrigger>
				</TabsList>

				<TabsContent value="view" className="selis-chrome__tab-content">
					<div className="selis-chrome__field">
						<span className="selis-chrome__field-label">Fit mode</span>
						<Select
							aria-label="Fit mode"
							value={state?.mode ?? "page"}
							onValueChange={(value) => {
								if (value === "page" || value === "width" || value === "spread") {
									onModeChange(value);
								}
							}}
						>
							<SelectGroup label="Page layout">
								<SelectItem value="page">Whole page</SelectItem>
								<SelectItem value="width">Fit width</SelectItem>
								<SelectItem value="spread">Two pages</SelectItem>
							</SelectGroup>
						</Select>
					</div>

					<div className="selis-chrome__checks">
						<Checkbox
							label="High contrast"
							checked={preferences.contrast === "high"}
							onCheckedChange={(checked) => {
								patch({ contrast: checked === true ? "high" : "normal" });
							}}
						/>
						<Checkbox
							label="Compact density"
							checked={preferences.density === "compact"}
							onCheckedChange={(checked) => {
								patch({ density: checked === true ? "compact" : "comfortable" });
							}}
						/>
						<Switch
							label="Reduce motion"
							checked={preferences.motion === "reduced"}
							onCheckedChange={(checked) => {
								patch({ motion: checked ? "reduced" : "system" });
							}}
						/>
					</div>

					<Button
						variant="secondary"
						size="sm"
						onClick={() => {
							toast({
								title: "Chrome is mounted",
								description: "The page list, theme bridge and toast surface are live.",
								tone: "success",
							});
						}}
					>
						Test notification
					</Button>
				</TabsContent>

				<TabsContent value="document" className="selis-chrome__tab-content">
					<dl className="selis-chrome__facts">
						<dt>Document</dt>
						<dd>sample.pdf</dd>
						<dt>Pages</dt>
						<dd>{state === null ? "—" : state.pageCount}</dd>
						<dt>Current page</dt>
						<dd>{state === null ? "—" : state.currentPage + 1}</dd>
						<dt>Zoom</dt>
						<dd>{state === null ? "—" : `${Math.round(state.zoom * 100)}%`}</dd>
						<dt>Window clamped</dt>
						<dd>{state === null ? "—" : state.windowClamped ? "Yes" : "No"}</dd>
					</dl>
					<p className="selis-chrome__hint">
						The page list is UI.02&apos;s real virtualised state. Tile artwork is UI.03; the boxes
						here are placeholders with their final geometry.
					</p>
				</TabsContent>
			</Tabs>
		</div>
	);
}

export function ChromeScreen(props: ChromeScreenProps): ReactElement {
	const { state, preferences, onPreferencesChange, onModeChange, onZoomChange, onPageChange } =
		props;
	const { toast } = useToast();
	const toggleTheme = (): void => {
		onPreferencesChange({
			...preferences,
			theme: preferences.theme === "dark" ? "light" : "dark",
		});
	};
	const atFirst = state === null || state.currentPage <= 0;
	const atLast = state === null || state.currentPage >= state.pageCount - 1;

	return (
		<AppShell
			className="selis-chrome"
			mainId="selis-chrome-main"
			sidebarWidth="272px"
			titlebar={
				<>
					<span className="selis-chrome__brand">Selis PDF</span>
					<span className="selis-chrome__doc-name">
						{state === null ? "Opening…" : "sample.pdf"}
					</span>
					<span className="selis-toolbar__spacer" />
					<Tooltip content="Previous page">
						<Button
							variant="ghost"
							size="sm"
							aria-label="Previous page"
							disabled={atFirst}
							onClick={() => {
								if (state !== null) onPageChange(state.currentPage - 1);
							}}
						>
							Previous
						</Button>
					</Tooltip>
					<Tooltip content="Next page">
						<Button
							variant="ghost"
							size="sm"
							aria-label="Next page"
							disabled={atLast}
							onClick={() => {
								if (state !== null) onPageChange(state.currentPage + 1);
							}}
						>
							Next
						</Button>
					</Tooltip>
					<Tooltip content="Zoom out">
						<Button
							variant="ghost"
							size="sm"
							aria-label="Zoom out"
							disabled={state === null}
							onClick={() => {
								if (state !== null) onZoomChange(Math.max(0.25, state.zoom / 1.2));
							}}
						>
							−
						</Button>
					</Tooltip>
					<Tooltip content="Zoom in">
						<Button
							variant="ghost"
							size="sm"
							aria-label="Zoom in"
							disabled={state === null}
							onClick={() => {
								if (state !== null) onZoomChange(Math.min(5, state.zoom * 1.2));
							}}
						>
							+
						</Button>
					</Tooltip>
					<Button variant="secondary" size="sm" onClick={toggleTheme}>
						{preferences.theme === "dark" ? "Light theme" : "Dark theme"}
					</Button>
					<Dialog
						title="About Selis PDF"
						description="Local-first PDF viewing, with no document bytes leaving this device."
						trigger={
							<Button variant="ghost" size="sm">
								About
							</Button>
						}
						footer={
							<DialogClose asChild>
								<Button variant="primary" size="sm">
									Close
								</Button>
							</DialogClose>
						}
					>
						<p>
							This chrome is the stage 3 integration check for UI.14: React, @wertkit/ui and
							UI.02&apos;s page-list controller in one screen.
						</p>
						<p>
							Theme, contrast, density and motion are applied by selis&apos;s own theme layer on the
							document root, so Radix portals inherit the same appearance.
						</p>
					</Dialog>
					<Menu
						trigger={
							<Button variant="ghost" size="sm">
								More
							</Button>
						}
						align="end"
					>
						<MenuLabel>Document</MenuLabel>
						<MenuItem
							shortcut="Ctrl+P"
							onSelect={() => {
								toast({
									title: "Print is a host action",
									description: "The print planner is UI.08; this menu is wired to it.",
									tone: "info",
								});
							}}
						>
							Print
						</MenuItem>
						<MenuItem
							onSelect={() => {
								toast({
									title: "Keyboard navigation",
									description: "Arrow keys, Page Up/Down, Home and End move the page list.",
									tone: "neutral",
								});
							}}
						>
							Keyboard shortcuts
						</MenuItem>
						<MenuSeparator />
						<MenuItem
							onSelect={() => {
								toast({
									title: "Nothing is uploaded",
									description: "Documents stay in this origin; the engine runs locally.",
									tone: "success",
								});
							}}
						>
							Privacy
						</MenuItem>
					</Menu>
				</>
			}
			sidebar={
				<ChromeSidebar
					state={state}
					preferences={preferences}
					onPreferencesChange={onPreferencesChange}
					onModeChange={onModeChange}
				/>
			}
		>
			<PageList {...props} />
		</AppShell>
	);
}
