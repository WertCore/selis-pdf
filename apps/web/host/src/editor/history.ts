/**
 * Undo and redo for the editor workspace.
 *
 * ## What this is, and what it is not
 *
 * `23-EDIT-MODEL-SPEC.md` describes the long-term edit model: a mutation
 * journal, undo groups, and a document materialised from the journal. None of
 * that is built — every `SL-5.EDIT.*` box in `15-PHASE-5-edit.md` is unticked —
 * and a workspace cannot borrow the spec's model without the spec's engine.
 *
 * So the workspace keeps the document's **bytes** for each step and restores
 * them verbatim. An undo is an exact restore rather than a re-derivation,
 * which is the property that actually matters to the person pressing the
 * button: what comes back is the document as it was, byte for byte, and not a
 * reconstruction that happens to look the same.
 *
 * The cost is memory — one copy of the document per step — so the depth is
 * capped. The cap is a constant here and nowhere else.
 */

/** How many document states the history keeps. */
export const HISTORY_LIMIT = 25;

/** One state of the document, and the change that produced it. */
export interface DocumentState {
	/** What produced this state, in the user's words ("Rotated page 2"). */
	readonly label: string;
	/** The document's bytes at this state. */
	readonly bytes: Uint8Array;
}

/** Copy a byte buffer, so a later mutation cannot reach into the history. */
function copy(bytes: Uint8Array): Uint8Array {
	return bytes.slice();
}

/**
 * A bounded stack of document states, with a cursor.
 *
 * The cursor is the state the document is in now. `undo` moves it back and
 * hands the caller the state to restore; `redo` moves it forward. Recording a
 * new state discards the redo tail, which is what every editor does: once the
 * document has changed again, the future it had is gone.
 */
export class DocumentHistory {
	readonly #limit: number;
	#states: DocumentState[] = [];
	#cursor = -1;

	constructor(limit: number = HISTORY_LIMIT) {
		if (!Number.isInteger(limit) || limit < 1) {
			throw new RangeError("the history must keep at least one state");
		}
		this.#limit = limit;
	}

	/** How many states are kept at once. */
	get limit(): number {
		return this.#limit;
	}

	/** How many states the history holds now. */
	get depth(): number {
		return this.#states.length;
	}

	/** Whether there is a state to go back to. */
	get canUndo(): boolean {
		return this.#cursor > 0;
	}

	/** Whether there is a state to go forward to. */
	get canRedo(): boolean {
		return this.#cursor >= 0 && this.#cursor < this.#states.length - 1;
	}

	/** The state the document is in now, or `null` before the first one. */
	get current(): DocumentState | null {
		return this.#states[this.#cursor] ?? null;
	}

	/** The label of the change an undo would revert, or `null`. */
	get undoLabel(): string | null {
		if (!this.canUndo) {
			return null;
		}
		return this.#states[this.#cursor]?.label ?? null;
	}

	/** The label of the change a redo would reapply, or `null`. */
	get redoLabel(): string | null {
		if (!this.canRedo) {
			return null;
		}
		return this.#states[this.#cursor + 1]?.label ?? null;
	}

	/**
	 * Start a session with the document as it was opened.
	 *
	 * The baseline is not an undo step: there is nothing before it to return
	 * to, so `canUndo` is false until the first change is recorded.
	 */
	reset(label: string, bytes: Uint8Array): void {
		this.#states = [{ label, bytes: copy(bytes) }];
		this.#cursor = 0;
	}

	/**
	 * Record a new state, discarding anything that was undone.
	 *
	 * The oldest state is dropped once the history is full, so the depth is
	 * bounded however long the session runs.
	 */
	push(label: string, bytes: Uint8Array): void {
		if (this.#cursor < 0) {
			this.reset(label, bytes);
			return;
		}
		this.#states = this.#states.slice(0, this.#cursor + 1);
		this.#states.push({ label, bytes: copy(bytes) });
		this.#cursor += 1;
		while (this.#states.length > this.#limit) {
			this.#states.shift();
			this.#cursor -= 1;
		}
	}

	/**
	 * Move back one state and return it, or `null` when there is nowhere to
	 * go.
	 */
	undo(): DocumentState | null {
		if (!this.canUndo) {
			return null;
		}
		this.#cursor -= 1;
		return this.#states[this.#cursor] ?? null;
	}

	/**
	 * Move forward one state and return it, or `null` when there is nowhere
	 * to go.
	 */
	redo(): DocumentState | null {
		if (!this.canRedo) {
			return null;
		}
		this.#cursor += 1;
		return this.#states[this.#cursor] ?? null;
	}
}
