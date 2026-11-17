import { Journal } from "./journal.js";

/**
 * Undo and redo for the editor workspace.
 *
 * ## What this is, and what it is not
 *
 * `23-EDIT-MODEL-SPEC.md` describes the long-term edit model: a mutation
 * journal, undo groups, and a document materialised from the journal. None of
 * that is built G�� every `SL-5.EDIT.*` box in `15-PHASE-5-edit.md` is unticked G��
 * and a workspace cannot borrow the spec's model without the spec's engine.
 *
 * So the workspace keeps the document's **bytes** for each step and restores
 * them verbatim. An undo is an exact restore rather than a re-derivation,
 * which is the property that actually matters to the person pressing the
 * button: what comes back is the document as it was, byte for byte, and not a
 * reconstruction that happens to look the same.
 *
 * Each step ALSO records the engine operation that produced it, in the
 * order it happened, with an id and a time. That is the journal's spine: the
 * reader can see what each step did, and a later slice can materialise the
 * document from the operations instead of keeping a byte copy per step. The
 * inverse of an entry is the bytes of the entry before it G�� a snapshot
 * inverse, which is why undo is exact today.
 *
 * The cost is memory G�� one copy of the document per step G�� so the depth is
 * capped. The cap is a constant here and nowhere else.
 */

/** How many document states the history keeps. */
export const HISTORY_LIMIT = 25;

/** The engine operation that produced one journal entry. */
export interface OperationRecord {
	/** The operation's wire name, e.g. `edit_text` or `redact`. */
	readonly op: string;
	/** The options the operation was called with, for the journal. */
	readonly options?: Readonly<Record<string, unknown>>;
}

/** One journal entry: a state of the document, and the change that made it. */
export interface DocumentState {
	/** What produced this state, in the user's words ("Rotated page 2"). */
	readonly label: string;
	/** The document's bytes at this state. */
	readonly bytes: Uint8Array;
	/** The engine operation that produced it, when the editor drove one. */
	readonly operation: OperationRecord | null;
	/** A monotonic id, so an entry can be named without its bytes. */
	readonly id: number;
	/** When the entry was recorded, from the caller's clock. */
	readonly at: number;
}

/** The entry to record, before its id and time are assigned. */
export interface JournalInput {
	readonly label: string;
	readonly bytes: Uint8Array;
	readonly operation?: OperationRecord | null;
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
	#nextId = 1;
	/** Injectable for tests; production passes Date.now(). */
	readonly #clock: () => number;

	constructor(limit: number = HISTORY_LIMIT, clock: () => number = Date.now) {
		if (!Number.isInteger(limit) || limit < 1) {
			throw new RangeError("the history must keep at least one state");
		}
		this.#limit = limit;
		this.#clock = clock;
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
	 * The states the history holds, oldest first.
	 *
	 * This is the reader's view of the stack: a panel that lists them makes
	 * the undo visible, and a reader who can see it can also pick the state
	 * they want to go back to.
	 */
	get entries(): readonly DocumentState[] {
		return this.#states;
	}

	/** Which of the states the document is in now. */
	get cursor(): number {
		return this.#cursor;
	}

	/**
	 * Move the cursor to a state without changing it.
	 *
	 * The caller restores the bytes itself G�� this only says which state the
	 * document is in, so the history and the document agree afterwards.
	 */
	setCursor(index: number): void {
		if (!Number.isInteger(index) || index < 0 || index >= this.#states.length) {
			throw new RangeError("that state is not in the history");
		}
		this.#cursor = index;
	}

	/**
	 * Start a session with the document as it was opened.
	 *
	 * The baseline is not an undo step: there is nothing before it to return
	 * to, so `canUndo` is false until the first change is recorded.
	 */
	reset(label: string, bytes: Uint8Array, operation: OperationRecord | null = null): void {
		this.#states = [
			{ label, bytes: copy(bytes), operation, id: this.#nextId, at: this.#clock() },
		];
		this.#nextId += 1;
		this.#cursor = 0;
	}

	/**
	 * Record a new state, discarding anything that was undone.
	 *
	 * The oldest state is dropped once the history is full, so the depth is
	 * bounded however long the session runs.
	 */
	push(label: string, bytes: Uint8Array, operation: OperationRecord | null = null): void {
		if (this.#cursor < 0) {
			this.reset(label, bytes, operation);
			return;
		}
		this.#states = this.#states.slice(0, this.#cursor + 1);
		this.#states.push({
			label,
			bytes: copy(bytes),
			operation,
			id: this.#nextId,
			at: this.#clock(),
		});
		this.#nextId += 1;
		this.#cursor += 1;
		while (this.#states.length > this.#limit) {
			this.#states.shift();
			this.#cursor -= 1;
		}
	}

	/**
	 * The inverse of an entry: the bytes of the state before it.
	 *
	 * The spec's inverse is a typed Mutation; this one is a snapshot, which is
	 * why an undo here is byte-exact. A journal entry at index 0 has no inverse
	 * G�� it is the document as it was opened.
	 */
	inverseOf(index: number): Uint8Array | null {
		if (index <= 0 || index >= this.#states.length) {
			return null;
		}
		return this.#states[index - 1]?.bytes ?? null;
	}

	/**
	 * The typed mutation journal for these states, rebuilt on demand.
	 *
	 * The workspace keeps byte snapshots so an undo is an exact restore, but
	 * every state is a typed mutation with a before and after; the journal is
	 * therefore reconstructible from the history and is what the .selis bundle
	 * persists. materialise(source, journal) is the current bytes.
	 */
	toJournal(source: Uint8Array): Journal {
		const journal = new Journal(source);
		for (let index = 1; index < this.#states.length; index += 1) {
			const state = this.#states[index];
			const before = this.#states[index - 1];
			if (state === undefined || before === undefined) continue;
			journal.apply(
				{
					kind: "replace-document",
					op: state.operation?.op ?? "unknown",
					options: state.operation?.options ?? {},
					before: before.bytes,
					after: state.bytes,
				},
				state.label,
				{ kind: "explicit" },
			);
		}
		journal.setCursor(this.#cursor - 1);
		return journal;
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
