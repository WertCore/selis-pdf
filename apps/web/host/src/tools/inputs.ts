/**
 * User-input parsing shared by the tools page and its tests.
 *
 * The engine's page-selection syntax is 0-based (`0,2,5-7`), which is right
 * for the CLI and wrong for a page picker a person reads. The page shows
 * 1-based numbers and shifts every range here, in one place, so no widget can
 * quietly disagree with another.
 */

/** A malformed user input, safe to show as-is. */
export class ToolInputError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "ToolInputError";
	}
}

function positiveNumber(text: string, label: string): number {
	const trimmed = text.trim();
	if (!/^[0-9]+$/.test(trimmed)) {
		throw new ToolInputError(`${label} must be a whole number, 1 or greater`);
	}
	const value = Number(trimmed);
	if (!Number.isSafeInteger(value) || value < 1) {
		throw new ToolInputError(`${label} must be 1 or greater`);
	}
	return value;
}

function positivePage(text: string, label: string): number {
	return positiveNumber(text, label) - 1;
}

/**
 * Convert a 1-based page selection (`1`, `1-3,7`) to the engine's 0-based
 * syntax (`0`, `0-2,6`). Every range endpoint is shifted exactly once; the
 * returned string is what the WASM protocol sends.
 */
export function shiftPageSelection(input: string): string {
	const text = input.trim();
	if (text === "") {
		throw new ToolInputError("enter the pages this tool should use");
	}
	const parts = text.split(",");
	const shifted: string[] = [];
	for (const part of parts) {
		const token = part.trim();
		if (token === "") {
			throw new ToolInputError("the page list has an empty item");
		}
		const range = token.match(/^([0-9]+)\s*-\s*([0-9]+)$/);
		if (range !== null) {
			const startText = range[1];
			const endText = range[2];
			if (startText === undefined || endText === undefined) {
				throw new ToolInputError("the page list has a malformed range");
			}
			const start = positivePage(startText, "The first page of a range");
			const end = positivePage(endText, "The last page of a range");
			if (start > end) {
				throw new ToolInputError("a page range must not run backwards");
			}
			shifted.push(`${start}-${end}`);
			continue;
		}
		if (!/^[0-9]+$/.test(token)) {
			throw new ToolInputError(`"${token}" is not a page number or range`);
		}
		shifted.push(String(positivePage(token, "A page number")));
	}
	return shifted.join(",");
}

/** Convert one 1-based page number to a 0-based index. */
export function pageIndex(input: string, label: string): number {
	return positivePage(input, label);
}

/** Parse a positive whole number that is a count, not a page index. */
export function pageCount(input: string, label: string): number {
	return positiveNumber(input, label);
}

/**
 * Convert a fractional megabyte target to the exact byte count the
 * split-by-size option expects. A megabyte here is 1024 × 1024 bytes, matching
 * the size the page displays for files; the result must still be a safe JSON
 * integer.
 */
export function sizeBytes(input: string, label: string): number {
	const trimmed = input.trim();
	if (!/^(?:[0-9]+(?:\.[0-9]+)?|\.[0-9]+)$/.test(trimmed)) {
		throw new ToolInputError(`${label} must be a positive number of MB`);
	}
	const megabytes = Number(trimmed);
	if (!Number.isFinite(megabytes) || megabytes <= 0) {
		throw new ToolInputError(`${label} must be greater than zero`);
	}
	const bytes = Math.round(megabytes * 1024 * 1024);
	if (!Number.isSafeInteger(bytes) || bytes < 1) {
		throw new ToolInputError(`${label} is outside the supported range`);
	}
	return bytes;
}

/** Whether the file begins with a PDF header. `%PDF-` is the only signature. */
export function hasPdfHeader(bytes: Uint8Array): boolean {
	if (bytes.length < 5) {
		return false;
	}
	return (
		bytes[0] === 0x25 &&
		bytes[1] === 0x50 &&
		bytes[2] === 0x44 &&
		bytes[3] === 0x46 &&
		bytes[4] === 0x2d
	);
}
