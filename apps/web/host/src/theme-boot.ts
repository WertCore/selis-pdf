/**
 * The theme's page entry point.
 *
 * A page that wants the theme applies it by loading this module: the module
 * puts the page in the right theme, follows the system's colour scheme until
 * the reader chooses one, and wires the page's theme switch.
 *
 * It is an entry point rather than a side effect inside `theme.ts` on purpose:
 * `theme.ts` is a library of decisions, and importing it must not change the
 * page. This module is the one that does.
 */
import { startTheme } from "./theme.js";

startTheme();

// The home page and /tools.html are a byte-identical mirror, so the page's
// role cannot live in the markup without breaking that check. The boot module
// runs on both and marks the directory from its URL: a reader there came to
// pick a tool, not to read the pitch.
if (window.location.pathname === "/tools.html") {
  document.documentElement.setAttribute("data-page", "tools");
}
