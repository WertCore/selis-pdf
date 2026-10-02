/**
 * Keyboard operation of the viewer's own controls (SL-4.UI.07).
 *
 * ## Why this is a SEPARATE resolver
 *
 * `keyboard.ts` holds three maps — the page list, the search field, and the
 * outline panel — and its header explains why they are separate functions with
 * separate contexts: a combined resolver would need a precedence rule, and
 * precedence is where "the arrow key did nothing" bugs live. The shell's global
 * controls are a FOURTH such map for the same reason. They claim different keys
 * and must not be able to claim each other's.
 *
 * What makes this one different from the others is that it deals in
 * **availability**. The page list always exists; the outline panel only does
 * when it is open. A shortcut to a control that is not there is a dead end
 * wearing a keybinding — the reader presses it, nothing happens, and they learn
 * that the keyboard is not trustworthy here. So every command here is gated on
 * the control it drives actually being usable, and the resolver returns `null`
 * otherwise.
 *
 * ## What this refuses to do
 *
 * - **Claim a bare letter.** Only modified chords resolve. An unmodified `p`
 *  typing into the search box must never print the document.
 * - **Claim a key the browser or OS owns.** `Ctrl+T`, `Ctrl+W`, `Ctrl+N` and
 *  `F5` are listed as reserved so a future edit cannot quietly steal them; the
 *  viewer does not get to relabel the browser's own chrome.
 * - **Take `Escape` when there is nothing to dismiss.** Escape closing an
 *  already-closed panel means the reader can never tell whether the panel is
 *  open.
 * - **Offer `Ctrl+P` when no document is open.** Printing nothing is not a
 *  recovery; it is a dead end with a shortcut on it.
 *
 * ## Ctrl+P, specifically
 *
 * `Ctrl+P` is normally the browser's own print dialog, which for this app is
 * wrong: UI.08 builds a print-ready PDF rather than relying on browser printing,
 * because a Letter page at 300 DPI is 33 MB of RGBA for ONE page and cannot be
 * printed by the browser's own path at all. So the viewer claims `Ctrl+P` and
 * the shell must `preventDefault()` it. The resolver decides; the shell
 * prevents.
 *
 * @see SL-4.UI.07
 */

/** A key chord as the shell receives it. */
export interface KeyChord {
	/** The `KeyboardEvent.key` value. */
	readonly key: string;
	readonly ctrl: boolean;
	readonly shift: boolean;
	readonly alt: boolean;
	readonly meta: boolean;
}

/** What a chord asked the shell to do. `null` = not ours. */
export type ControlCommandAction =
	| "print"
	| "open"
	| "find"
	| "health"
	| "toggle-outline"
	| "cancel-print"
	| "dismiss";

export interface ControlCommand {
	readonly action: ControlCommandAction;
	/** The chord that produced it, for tests and UI.07's audit trail. */
	readonly key: string;
	/**
	 * True when the shell must `preventDefault()`.
	 *
	 * Stated here rather than left to the caller, because "should I stop the
	 * browser doing its own thing" is the question most likely to be answered
	 * inconsistently between call sites — and getting it wrong on `Ctrl+P`
	 * means both the browser dialog AND the viewer's own print appear.
	 */
	readonly preventDefault: boolean;
}

/**
 * What the viewer can actually do right now.
 *
 * Every field is an honest answer to "is this control usable", not "is this
 * component in the DOM". A mounted-but-disabled button is not available.
 */
export interface ControlContext {
	/** A document is open, so printing/searching/health are meaningful. */
	readonly hasDocument: boolean;
	/** The outline panel exists and can be shown or hidden. */
	readonly hasOutline: boolean;
	/** The outline panel is currently showing. */
	readonly outlineOpen: boolean;
	/** A print job is in flight, so Cancel is a live control. */
	readonly printRunning: boolean;
	/** Something is showing that a reader would want to dismiss. */
	readonly hasDismissable: boolean;
}

/** An idle viewer: no document, nothing to cancel, nothing to dismiss. */
export function idleControls(): ControlContext {
	return {
		hasDocument: false,
		hasOutline: false,
		outlineOpen: false,
		printRunning: false,
		hasDismissable: false,
	};
}

/**
 * Chords the viewer must not claim.
 *
 * Listed rather than merely omitted, because the omission is invisible: a
 * future edit that adds `Ctrl+T` would look like an ordinary addition, whereas a
 * list makes it a deliberate act someone has to delete an entry from.
 */
const RESERVED = new Set<string>([
	"Ctrl+T",
	"Ctrl+N",
	"Ctrl+W",
	"Ctrl+Shift+T",
	"Ctrl+Shift+N",
	"Ctrl+Shift+W",
	"F5",
	"F11",
	"F12",
]);

/**
 * Name a chord for the reserved table and for messages.
 *
 * Order matters: `Ctrl+Shift+P` must not be read as `Ctrl+P`, because the
 * browser already assigns that one and claiming it would be a conflict.
 */
function chordName(chord: KeyChord): string {
	const parts: string[] = [];
	if (chord.ctrl) parts.push("Ctrl");
	if (chord.alt) parts.push("Alt");
	// Meta last, so `Cmd+Shift+P` reads in the order a Mac user expects.
	if (chord.meta) parts.push("Meta");
	if (chord.shift) parts.push("Shift");
	parts.push(chord.key);
	return parts.join("+");
}

/**
 * Resolve a chord to a shell command, or `null` if it is not ours.
 *
 * Total and context-gated, like the three maps in `keyboard.ts`: anything the
 * shell cannot currently do returns `null`, so a caller never has to check
 * whether the control it just resolved exists.
 *
 * The `Escape` branch is last and unconditional-by-intent but
 * availability-gated, which is the whole reason it is not simply `Escape →
 * dismiss`: closing something that is not open is how a reader loses track of
 * whether the panel is open at all.
 */
export function resolveControlKey(chord: KeyChord, context: ControlContext): ControlCommand | null {
	const name = chordName(chord);
	if (RESERVED.has(name)) return null;
	const command = (action: ControlCommandAction, preventDefault = true): ControlCommand => ({
		action,
		key: name,
		preventDefault,
	});

	// No modifier, no chord. An unmodified letter belongs to whatever the reader
	// is typing into; this map never claims it.
	const plain = !chord.ctrl && !chord.alt && !chord.meta;

	if (plain && chord.key === "Escape") {
		// Only while something is actually up. Escape on an idle viewer doing
		// nothing is correct, not a failure to fix.
		return context.hasDismissable || context.outlineOpen ? command("dismiss") : null;
	}

	// Ctrl/Cmd chords. `meta` is accepted alongside `ctrl` because on a Mac
	// these are the same muscle memory and a viewer that only honours Ctrl is a
	// viewer that does not work on half the keyboards in use.
	const mod = chord.ctrl || chord.meta;

	if (mod && !chord.alt) {
		switch (chord.key.toLowerCase()) {
			case "p":
				// The browser's own print dialog is wrong for this app - see the
				// header - so this one MUST preventDefault. Getting that wrong
				// produces two print dialogs, which is worse than either.
				return context.hasDocument ? command("print") : null;
			case "o":
				return command("open");
			case "f":
				return context.hasDocument ? command("find") : null;
			case ".":
				// Ctrl+. is the conventional "stop", and cancelling a print from
				// the keyboard is the accessibility point of the whole feature.
				return context.printRunning ? command("cancel-print") : null;
			default:
				return null;
		}
	}

	// Single-key chords for controls that are document-wide and have no
	// conventional accelerator.
	if (plain && chord.key === "F1") {
		return context.hasDocument ? command("health") : null;
	}
	if (plain && chord.key === "F2" && context.hasOutline) {
		return command("toggle-outline");
	}

	return null;
}

/**
 * Parse a chord name back into a {@link KeyChord}.
 *
 * Exists so {@link claimedChords} can be checked AGAINST the resolver rather
 * than merely next to it. A declared table and an implementation that disagree
 * is the whole drift this item is meant to prevent, and the only way to catch
 * it is to resolve each declared chord and compare. Returns `null` for a name
 * it cannot parse, so a typo in the table is a failing test rather than a
 * chord that silently does nothing.
 */
export function parseChord(name: string): KeyChord | null {
	const parts = name.split("+");
	if (parts.length === 0) return null;
	const key = parts[parts.length - 1];
	if (key === undefined || key === "") return null;
	return {
		key,
		ctrl: parts.includes("Ctrl"),
		shift: parts.includes("Shift"),
		alt: parts.includes("Alt"),
		meta: parts.includes("Meta"),
	};
}

/**
 * Every chord this shell claims, with the action it performs.
 *
 * Exists so UI.07's audit can be a TEST over real data rather than a
 * hand-maintained list in the plan that drifts. A control added to the shell
 * without a chord shows up here as a gap, and the accessibility test walks
 * `shellControls()` rather than a prose list nobody re-reads.
 */
export function claimedChords(): ReadonlyArray<{ chord: string; action: ControlCommandAction }> {
	return [
		{ chord: "Ctrl+P", action: "print" },
		{ chord: "Meta+P", action: "print" },
		{ chord: "Ctrl+O", action: "open" },
		{ chord: "Meta+O", action: "open" },
		{ chord: "Ctrl+F", action: "find" },
		{ chord: "Meta+F", action: "find" },
		{ chord: "Ctrl+.", action: "cancel-print" },
		{ chord: "Meta+.", action: "cancel-print" },
		{ chord: "F1", action: "health" },
		{ chord: "F2", action: "toggle-outline" },
		{ chord: "Escape", action: "dismiss" },
	];
}

/**
 * The controls the viewer offers, and whether each is one a reader can reach.
 *
 * The Do for UI.07 is "keyboard-only operation of every control", which is a
 * claim about COVERAGE: a control nobody can reach by keyboard fails the item
 * even if it works perfectly with a mouse. Listing them as data lets the gate
 * check that every one of them has a chord, rather than trusting a reviewer to
 * notice a button that was added without a key.
 */
export interface ShellControl {
	readonly id: string;
	/** The label a reader hears. */
	readonly label: string;
	/** The chord that drives it, or `null` if it has none — a gate failure. */
	readonly chord: string | null;
	/** Why there is no chord, when there is none. Honest, not empty. */
	readonly reason: string | null;
}

/**
 * The controls the viewer offers, and whether each is one a reader can reach.
 *
 * The Do for UI.07 is "keyboard-only operation of every control", which is a
 * claim about COVERAGE: a control nobody can reach by keyboard fails the item
 * even if it works perfectly with a mouse. Listing them as data lets the gate
 * check that every one of them has a chord, rather than trusting a reviewer to
 * notice a button that was added without a key.
 *
 * `catalogue` is REQUIRED and has no default. A default here would be an inline
 * English literal - exactly what `strings.test.ts` forbids - and a convenience
 * default is how one gets added. The host passes the catalogue it already has.
 */
export function shellControls(
	catalogue: Readonly<Record<string, string>>,
): readonly ShellControl[] {
	return [
		{
			id: "selis-print",
			label: catalogue["control.print.label"] ?? "",
			chord: "Ctrl+P",
			reason: null,
		},
		{
			id: "selis-health",
			label: catalogue["control.health.label"] ?? "",
			chord: "F1",
			reason: null,
		},
		{
			id: "selis-search",
			label: catalogue["control.find.label"] ?? "",
			chord: "Ctrl+F",
			reason: null,
		},
		{
			id: "selis-print-cancel",
			label: catalogue["control.cancelPrint.label"] ?? "",
			chord: "Ctrl+.",
			reason: null,
		},
		{
			id: "selis-outline",
			label: catalogue["control.outline.label"] ?? "",
			chord: "F2",
			reason: null,
		},
		{
			id: "selis-open",
			label: catalogue["control.open.label"] ?? "",
			chord: "Ctrl+O",
			reason: null,
		},
		{
			id: "selis-failure-action",
			label: catalogue["control.recovery.label"] ?? "",
			// Reached by Tab and activated by Enter, which is how EVERY native
			// button is operated. Declared here so the coverage claim is
			// explicit rather than an unstated assumption that native controls
			// are fine - and so a future replacement with a <div> is caught.
			chord: "Tab/Enter",
			reason: null,
		},
	];
}
