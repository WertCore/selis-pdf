/**
 * The document the reader had open, kept so that a reload does not lose it.
 *
 * ## Why this exists
 *
 * Every reader who has typed in a document and then hit reload by accident
 * has lost the work, and every one of them has blamed the editor. The
 * competitors do not lose it: the browser's own viewer restores the file
 * across a reload, Acrobat asks whether to save, and none of them closes the
 * tab under the reader.
 *
 * So the workspace keeps the document in the browser's own storage —
 * IndexedDB, which holds bytes and lives on the reader's device — and puts
 * it back when the page comes back.
 *
 * ## What it is not
 *
 * It is not an upload and not a sync: the bytes never leave the device, and
 * the page's no-upload recorder watches requests, not the local database.
 * And it is not a backup: the copy is the document the reader had open, and
 * it is replaced by the next document they open.
 */

/** The document the workspace keeps between visits. */
export interface KeptDocument {
	/** The document's name, as the reader's file had it. */
	readonly name: string;
	/** The document's bytes. */
	readonly bytes: Uint8Array;
	/** Whether the document carries changes the reader has not taken away. */
	readonly dirty: boolean;
}

/** The database the workspace keeps its document in. */
const DATABASE = "selis-editor";
/** The store the document itself lives in. */
const STORE = "document";
/** The key the document is kept under. */
const KEY = "current";

/**
 * The most bytes a document may be before the workspace stops keeping it.
 *
 * A document bigger than this is still perfectly editable; it simply is
 * not kept across a reload, because writing a hundred megabytes into the
 * browser's storage on every change is a cost the reader did not ask for.
 */
export const MAX_KEPT_BYTES = 64 * 1024 * 1024;

/**
 * Whether a document is one the workspace can keep.
 *
 * The rule is the size and nothing else: the bytes are the document, and a
 * document the reader can open is a document the reader can lose.
 */
export function canKeep(bytes: Uint8Array): boolean {
	return bytes.length <= MAX_KEPT_BYTES;
}

/** The storage, when the browser has one. */
function openDatabase(): Promise<IDBDatabase | null> {
	if (typeof indexedDB === "undefined") {
		return Promise.resolve(null);
	}
	return new Promise((resolve) => {
		let request: IDBOpenDBRequest;
		try {
			request = indexedDB.open(DATABASE, 1);
		} catch {
			resolve(null);
			return;
		}
		request.onupgradeneeded = () => {
			const database = request.result;
			if (!database.objectStoreNames.contains(STORE)) {
				database.createObjectStore(STORE);
			}
		};
		request.onsuccess = () => resolve(request.result);
		request.onerror = () => resolve(null);
		request.onblocked = () => resolve(null);
	});
}

/**
 * Keep the document, so the next visit to this page finds it.
 *
 * Returns whether it was kept: a browser with no storage, or a document too
 * big to keep, answers false and the workspace simply does not promise a
 * restore.
 */
export async function keepDocument(document: KeptDocument): Promise<boolean> {
	if (!canKeep(document.bytes)) {
		return false;
	}
	const database = await openDatabase();
	if (database === null) {
		return false;
	}
	try {
		await new Promise<void>((resolve, reject) => {
			const transaction = database.transaction(STORE, "readwrite");
			transaction.objectStore(STORE).put(
				{
					name: document.name,
					bytes: document.bytes,
					dirty: document.dirty,
				},
				KEY,
			);
			transaction.oncomplete = () => resolve();
			transaction.onerror = () => reject(transaction.error);
			transaction.onabort = () => reject(transaction.error);
		});
		return true;
	} catch {
		return false;
	} finally {
		database.close();
	}
}

/** The document the workspace kept, when there is one. */
export async function readKeptDocument(): Promise<KeptDocument | null> {
	const database = await openDatabase();
	if (database === null) {
		return null;
	}
	try {
		const stored = await new Promise<unknown>((resolve, reject) => {
			const transaction = database.transaction(STORE, "readonly");
			const request = transaction.objectStore(STORE).get(KEY);
			request.onsuccess = () => resolve(request.result);
			request.onerror = () => reject(request.error);
		});
		if (typeof stored !== "object" || stored === null) {
			return null;
		}
		const record = stored as { name?: unknown; bytes?: unknown; dirty?: unknown };
		if (typeof record.name !== "string" || !(record.bytes instanceof Uint8Array)) {
			return null;
		}
		return {
			name: record.name,
			bytes: record.bytes,
			dirty: record.dirty === true,
		};
	} catch {
		return null;
	} finally {
		database.close();
	}
}

/** Forget the document: the reader closed it, so it is gone. */
export async function forgetDocument(): Promise<void> {
	const database = await openDatabase();
	if (database === null) {
		return;
	}
	try {
		await new Promise<void>((resolve, reject) => {
			const transaction = database.transaction(STORE, "readwrite");
			transaction.objectStore(STORE).delete(KEY);
			transaction.oncomplete = () => resolve();
			transaction.onerror = () => reject(transaction.error);
		});
	} catch {
		// Nothing to do: the document is already gone, which is what the
		// reader asked for.
	} finally {
		database.close();
	}
}
