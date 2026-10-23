/**
 * The vendor bundle's complete runtime surface (SL-4.UI.14 stage 3).
 *
 * This file is the *only* entry esbuild bundles. Keeping it curated does three
 * things the root `@wertkit/ui` barrel cannot:
 *
 *   1. It makes the shipped third-party API a reviewed list. A chrome module
 *      that imports something absent here fails in the browser rather than
 *      silently pulling another component (and its Radix closure) into the
 *      bundle.
 *   2. It gives esbuild a tree-shaking root, so components the screen does not
 *      use do not ship.
 *   3. It exports exactly the React names the app's `tsc` output can ask for.
 *      `place-assets.mjs` rewrites `react`, `react/jsx-runtime`,
 *      `react-dom/client` and `@wertkit/ui` to this one file.
 *
 * Types are deliberately not re-exported: TypeScript resolves component types
 * from the real package at compile time, while the browser only needs these
 * runtime bindings.
 */

export {
	Fragment,
	StrictMode,
	Suspense,
	createContext,
	createElement,
	forwardRef,
	memo,
	useCallback,
	useContext,
	useEffect,
	useId,
	useLayoutEffect,
	useMemo,
	useReducer,
	useRef,
	useState,
	useSyncExternalStore,
} from "react";
export { jsx, jsxs } from "react/jsx-runtime";
export { createRoot } from "react-dom/client";
export {
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
	ToastProvider,
	Tooltip,
	TooltipProvider,
	useToast,
} from "@wertkit/ui";
