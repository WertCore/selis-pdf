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
