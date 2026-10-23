/**
 * The chrome entry point (SL-4.UI.14 stage 3).
 *
 * This is a separate entry from `apps/ui/src/index.ts` on purpose: the package
 * barrel remains the DOM-free viewer API, while `chrome/index.js` is the only
 * module the web host loads React from. `place-assets.mjs` walks from this file
 * to ship the chrome closure, and rewrites its four third-party specifiers to
 * `vendor/vendor.js`.
 */

export {
	ChromeApp,
	DEFAULT_CHROME_PREFERENCES,
	applyChromeTheme,
	mountChrome,
	type ChromeAppProps,
	type ChromeMount,
} from "./app.js";
export { ChromeScreen, type ChromeScreenProps } from "./screen.js";
