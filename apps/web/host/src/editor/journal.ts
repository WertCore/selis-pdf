/**
 * The mutation journal (SL-5.EDIT.01, 23-EDIT-MODEL-SPEC.md).
 *
 * Every change to the document is a typed Mutation appended to this journal.
 * The in-memory document is exactly `materialise(source, journal)`: the bytes
 * of the last mutation's `after` state, or the source bytes when the journal
 * is empty. There is no state that is not reconstructible from the two.
 *
 * A mutation carries its own inverse: for a document-level replacement the
 * inverse is the same mutation with `before` and `after` swapped. Undo applies
 * the inverses of one undo group, newest first; redo reapplies the group. A
 * group is what one user gesture did, even when it produced several mutations.
 *
 * On reload the source is fingerprinted again. A changed source is a hard
 * error: applying a journal to different bytes would silently corrupt them.
 */

/** The version of the on-disk (IndexedDB) session bundle. */
export const SESSION_SCHEMA = "selis-session/1";

/** A monotonic identity for one journal entry. */
export type MutationId = number;

/** Nanoseconds from the caller's clock. */
export type Nanos = number;

/** Who made a change; the local reader is the only actor today. */
export type ActorId = string;

/** How undo coalesces consecutive mutations into one group. */
export type Coalesce =
	| { readonly kind: "never" }
	| { readonly kind: "typing"; readonly idle: number }
	| { readonly kind: "dragging" }
	| { readonly kind: "explicit" };

/** A typed change to the document, with everything needed to invert it. */
export interface ReplaceDocumentMutation {
	readonly kind: "replace-document";
	/** The engine operation that produced the new state. */
	readonly op: string;
	/** The options the operation was called with (opaque, from the wire). */
	readonly options: Readonly<Record<string, unknown>>;
	/** The document before the change. */
	readonly before: Uint8Array;
	/** The document after the change. */
	readonly after: Uint8Array;
}

export type Mutation = ReplaceDocumentMutation;

/** A source fingerprint: size and a hash prefix, enough to catch a different file. */
export interface SourceFingerprint {
	readonly size: number;
	readonly hashPrefix: string;
}

/** One append-only entry of the journal. */
export interface JournalEntry {
	readonly id: MutationId;
	readonly mutation: Mutation;
	readonly inverse: Mutation;
	readonly at: Nanos;
	readonly actor: ActorId;
	readonly label: string;
	/** The undo group this entry belongs to. */
	readonly group: number;
}

/** A group of mutations that undo as one gesture. */
export interface UndoGroup {
	readonly label: string;
	/** Half-open range of entries in the journal. */
	readonly entries: readonly [number, number];
	readonly coalescing: Coalesce;
}

/** The serialised journal, for the `.selis` session bundle. */
export interface JournalBundle {
	readonly schema: typeof SESSION_SCHEMA;
	readonly source: SourceFingerprint;
	readonly entries: readonly {
		id: MutationId;
		mutation: SerialisedMutation;
		inverse: SerialisedMutation;
		at: Nanos;
		actor: ActorId;
		label: string;
		group: number;
	}[];
	readonly groups: readonly {
		label: string;
		entries: readonly [number, number];
		coalescing: Coalesce;
	}[];
	/** Where undo currently sits: a mutation index, or -1 at the source. */
	readonly cursor: number;
}

interface SerialisedMutation {
	readonly kind: "replace-document";
	readonly op: string;
	readonly options: Readonly<Record<string, unknown>>;
	readonly before: string;
	readonly after: string;
}

/** The error a journal refuses to apply a change to a different source with. */
export class SourceChangedError extends Error {
	constructor() {
		super("the source document changed while the session was open");
		this.name = "SourceChangedError";
	}
}

function encode(bytes: Uint8Array): string {
	let text = "";
	for (const byte of bytes) {
		text += String.fromCharCode(byte);
	}
	return btoa(text);
}

function decode(value: string): Uint8Array {
	const text = atob(value);
	const out = new Uint8Array(text.length);
	for (let i = 0; i < text.length; i += 1) {
		out[i] = text.charCodeAt(i);
	}
	return out;
}

/** A stable, cheap fingerprint: FNV-1a over the bytes, hex, first 16 chars. */
export function fingerprint(bytes: Uint8Array): SourceFingerprint {
	let hash = 0x811c9dc5;
	for (const byte of bytes) {
		hash ^= byte;
		hash = Math.imul(hash, 0x01000193);
	}
	return { size: bytes.length, hashPrefix: (hash >>> 0).toString(16).padStart(8, "0") };
}

function sameSource(a: SourceFingerprint, b: SourceFingerprint): boolean {
	return a.size === b.size && a.hashPrefix === b.hashPrefix;
}

function serialiseMutation(mutation: Mutation): SerialisedMutation {
	return {
		kind: mutation.kind,
		op: mutation.op,
		options: mutation.options,
		before: encode(mutation.before),
		after: encode(mutation.after),
	};
}

function deserialiseMutation(value: SerialisedMutation): Mutation {
	if (value.kind !== "replace-document") {
		throw new Error(`unknown mutation kind: ${value.kind}`);
	}
	return {
		kind: "replace-document",
		op: value.op,
		options: value.options,
		before: decode(value.before),
		after: decode(value.after),
	};
}

/** Build the mutation that undoes `mutation`. */
export function inverseOf(mutation: Mutation): Mutation {
	return {
		kind: mutation.kind,
		op: mutation.op,
		options: mutation.options,
		before: mutation.after,
		after: mutation.before,
	};
}

/**
 * The journal itself. `cursor` is the index of the last applied entry; -1
 * means the source state. Redo is discarded when a new mutation is applied,
 * but the discarded entries stay in `entries` so a later export still shows
 * everything that happened.
 */
export class Journal {
	readonly source: SourceFingerprint;
	readonly entries: JournalEntry[] = [];
	readonly groups: UndoGroup[] = [];
	#cursor = -1;
	#nextId = 1;
	readonly #clock: () => Nanos;
	readonly #actor: ActorId;

	constructor(
		source: Uint8Array,
		clock: () => Nanos = Date.now,
		actor: ActorId = "local",
		fingerprintOverride?: SourceFingerprint,
	) {
		this.source = fingerprintOverride ?? fingerprint(source);
		this.#clock = clock;
		this.#actor = actor;
	}

	/** The index of the last applied entry, or -1 at the source. */
	get position(): number {
		return this.#cursor;
	}

	/** Whether there is a group to undo. */
	get canUndo(): boolean {
		return this.#cursor >= 0;
	}

	/** Whether there is a discarded group to redo. */
	get canRedo(): boolean {
		return this.#cursor + 1 < this.entries.length;
	}

	/** The bytes of the document at the current position. */
	materialise(source: Uint8Array): Uint8Array {
		if (!sameSource(this.source, fingerprint(source))) {
			throw new SourceChangedError();
		}
		const entry = this.entries[this.#cursor];
		return entry === undefined ? source.slice() : entry.mutation.after.slice();
	}

	/**
	 * Append a change and move the cursor to it. Consecutive typing or
	 * dragging changes coalesce into one group; everything else starts a new
	 * one. Returns the new entry's id.
	 */
	apply(
		mutation: Mutation,
		label: string,
		coalescing: Coalesce = { kind: "never" },
	): MutationId {
		const now = this.#clock();
		const last = this.groups.at(-1);
		const lastGroupIndex = this.groups.length - 1;
		const canCoalesce =
			last !== undefined &&
			last.entries[1] === this.entries.length &&
			last.coalescing.kind === coalescing.kind &&
			(coalescing.kind === "dragging" ||
				(coalescing.kind === "typing" &&
					last.coalescing.kind === "typing" &&
					now - lastEntriesAt(this, lastGroupIndex) <= coalescing.idle));
		const groupIndex = canCoalesce ? lastGroupIndex : this.groups.length;
		if (!canCoalesce) {
			this.groups.push({
				label,
				entries: [this.entries.length, this.entries.length + 1],
				coalescing,
			});
		} else {
			const current = this.groups[groupIndex];
			if (current !== undefined) {
				this.groups[groupIndex] = {
					...current,
					entries: [current.entries[0], current.entries[1] + 1],
				};
			}
		}
		const entry: JournalEntry = {
			id: this.#nextId,
			mutation,
			inverse: inverseOf(mutation),
			at: now,
			actor: this.#actor,
			label,
			group: groupIndex,
		};
		this.#nextId += 1;
		// Discard the redo tail: a new change makes the old future unreachable.
		if (this.#cursor + 1 < this.entries.length) {
			this.entries.length = this.#cursor + 1;
			const kept = this.groups.filter((group) => group.entries[0] <= this.#cursor);
			this.groups.length = 0;
			this.groups.push(...kept);
		}
		this.entries.push(entry);
		this.#cursor = this.entries.length - 1;
		return entry.id;
	}

	/** Undo one group, applying its inverses newest first. */
	undo(source: Uint8Array): Uint8Array {
		const current = this.entries[this.#cursor];
		if (current === undefined) {
			return source.slice();
		}
		const group = current.group;
		let state = current.mutation.after.slice();
		for (let i = this.#cursor; i >= 0; i -= 1) {
			const entry = this.entries[i];
			if (entry === undefined || entry.group !== group) break;
			state = entry.inverse.after.slice();
		}
		this.#cursor = firstIndexOfGroup(this, group) - 1;
		return state;
	}

	/** Redo one group, reapplying its mutations oldest first. */
	redo(source: Uint8Array): Uint8Array {
		const next = this.entries[this.#cursor + 1];
		if (next === undefined) {
			return this.materialise(source);
		}
		const group = next.group;
		let state = next.inverse.before.slice();
		for (let i = this.#cursor + 1; i < this.entries.length; i += 1) {
			const entry = this.entries[i];
			if (entry === undefined || entry.group !== group) break;
			state = entry.mutation.after.slice();
		}
		this.#cursor = lastIndexOfGroup(this, group);
		return state;
	}

	/** Move the cursor to an entry without changing the bytes (the caller does). */
	setCursor(index: number): void {
		if (!Number.isInteger(index) || index < -1 || index >= this.entries.length) {
			throw new RangeError("that entry is not in the journal");
		}
		this.#cursor = index;
	}

	/** Serialise for the `.selis` session bundle. */
	toBundle(): JournalBundle {
		return {
			schema: SESSION_SCHEMA,
			source: this.source,
			entries: this.entries.map((entry) => ({
				id: entry.id,
				mutation: serialiseMutation(entry.mutation),
				inverse: serialiseMutation(entry.inverse),
				at: entry.at,
				actor: entry.actor,
				label: entry.label,
				group: entry.group,
			})),
			groups: this.groups.map((group) => ({
				label: group.label,
				entries: [...group.entries],
				coalescing: group.coalescing,
			})),
			cursor: this.#cursor,
		};
	}

	/** Rebuild a journal from a bundle, revalidating the source first. */
	static fromBundle(bundle: JournalBundle, source: Uint8Array, clock?: () => Nanos, actor?: ActorId): Journal {
		if (bundle.schema !== SESSION_SCHEMA) {
			throw new Error(`unknown session bundle schema: ${bundle.schema}`);
		}
		if (!sameSource(bundle.source, fingerprint(source))) {
			throw new SourceChangedError();
		}
		const journal = new Journal(source, clock, actor, bundle.source);
		for (const value of bundle.entries) {
			journal.entries.push({
				id: value.id,
				mutation: deserialiseMutation(value.mutation),
				inverse: deserialiseMutation(value.inverse),
				at: value.at,
				actor: value.actor,
				label: value.label,
				group: value.group,
			});
		}
		for (const group of bundle.groups) {
			journal.groups.push({
				label: group.label,
				entries: [group.entries[0], group.entries[1]],
				coalescing: group.coalescing,
			});
		}
		journal.#cursor = bundle.cursor;
		journal.#nextId = journal.entries.reduce((max, entry) => Math.max(max, entry.id), 0) + 1;
		return journal;
	}
}

function lastEntriesAt(journal: Journal, groupIndex: number): Nanos {
	for (let i = journal.entries.length - 1; i >= 0; i -= 1) {
		const entry = journal.entries[i];
		if (entry !== undefined && entry.group === groupIndex) {
			return entry.at;
		}
	}
	return 0;
}

function firstIndexOfGroup(journal: Journal, group: number): number {
	const index = journal.entries.findIndex((entry) => entry.group === group);
	return index < 0 ? journal.entries.length : index;
}

function lastIndexOfGroup(journal: Journal, group: number): number {
	for (let i = journal.entries.length - 1; i >= 0; i -= 1) {
		if (journal.entries[i]?.group === group) return i;
	}
	return -1;
}
