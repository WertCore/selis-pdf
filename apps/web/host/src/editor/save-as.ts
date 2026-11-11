/**
 * Writing a produced file to a place the reader chose.
 *
 * ## Why this is its own module
 *
 * The File System Access API asks the reader where a file should go and
 * hands back a handle; the bytes are then written into it. That is a
 * download the reader cannot lose: the file lands where they pointed, with
 * no download folder to go and look in.
 *
 * It is here, on its own, because it is the one piece of the export that can
 * be **tested without a browser** — and it has to be tested, because the
 * mistake it is prone to is quiet. The file's LENGTH and the file's BYTES are
 * both numbers to the careless eye, and `new Uint8Array(length)` builds an
 * array of that many zeros: the file is written, the right size, and empty.
 *
 * So the write takes the bytes themselves, and the type says so.
 */

/** The part of a `FileSystemFileHandle` this module uses. */
export interface WritableLike {
	write(data: Uint8Array): Promise<void> | void;
	close(): Promise<void> | void;
}

/** The part of a `FileSystemFileHandle` this module uses. */
export interface FileHandleLike {
	createWritable(): Promise<WritableLike>;
}

/**
 * Write a file's content into a handle the reader chose.
 *
 * The bytes go in as they are. Nothing here converts, copies, or re-reads
 * them: a file that comes out the wrong size is a bug in the caller, and the
 * only honest way to find it is to pass the bytes and let the test see what
 * was written.
 */
export async function writeToHandle(handle: FileHandleLike, content: Uint8Array): Promise<void> {
	const writable = await handle.createWritable();
	try {
		await writable.write(content);
	} finally {
		// The file is closed even when the write failed: an unclosed stream
		// leaves a half-written file where the reader asked for a whole one.
		await writable.close();
	}
}
