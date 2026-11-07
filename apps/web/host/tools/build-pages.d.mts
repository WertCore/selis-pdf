/**
 * Types for the plain-ESM page generator, so `tsc` can check the tests that
 * import it without turning the generator itself into TypeScript.
 */

/** Top-level names a slug may not take, mapped to why each is reserved. */
export const RESERVED_TOP_LEVEL: Readonly<Record<string, string>>;

export function escapeHtml(value: string): string;

/** Return every problem with the registry; an empty list means publishable. */
export function validateRegistry(
	tools: readonly unknown[],
	groups?: readonly { readonly id: string }[],
): string[];

export function renderToolPage(tool: unknown, byId: ReadonlyMap<string, unknown>): string;

export function assertPageShape(page: string, slug: string): void;

export function renderPagesJson(tools: readonly unknown[]): string;

export function renderSitemap(tools: readonly unknown[]): string;

export function renderRobots(): string;

/** Relative path → file contents, for every file the build writes. */
export function renderOutputs(
	tools?: readonly unknown[],
	groups?: readonly unknown[],
): Map<string, string>;
