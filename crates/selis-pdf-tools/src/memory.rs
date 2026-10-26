//! The browser tools session: input and output bytes held by path name.
//!
//! The tools implementation already speaks in paths (`merge(inputs, output)`).
//! In the browser those names are opaque keys into this table, so the same
//! code runs without a filesystem and without any path escaping the page.
//! The map is process-local and deliberately has no persistence.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use crate::{CliError, CliResult};

thread_local! {
    // Set while a browser request is running. The CLI never sets it, so the
    // native filesystem path is unchanged; the flag exists because a workspace
    // test build unifies `native-fs` into the browser crate, and the wasm
    // binding must still use the in-memory table in that configuration.
    static MEMORY_SESSION: Cell<bool> = const { Cell::new(false) };
}

fn store() -> &'static Mutex<BTreeMap<String, Vec<u8>>> {
    static STORE: OnceLock<Mutex<BTreeMap<String, Vec<u8>>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Routes the path-based tool wrappers to the in-memory table until dropped.
///
/// The browser binding activates this for the duration of one request. It is
/// deliberately thread-local: the table is process-wide, but a CLI call on
/// another thread must keep reading real files.
pub struct MemorySession {
    previous: bool,
}

impl MemorySession {
    /// Activate the in-memory backend for the current thread.
    #[must_use]
    pub fn activate() -> Self {
        let previous = MEMORY_SESSION.with(|active| active.replace(true));
        Self { previous }
    }
}

impl Drop for MemorySession {
    fn drop(&mut self) {
        MEMORY_SESSION.with(|active| active.set(self.previous));
    }
}

/// Whether the current thread is inside an activated [`MemorySession`].
#[cfg(feature = "native-fs")]
pub(crate) fn active() -> bool {
    MEMORY_SESSION.with(Cell::get)
}

/// Put an input document into the current tool session.
///
/// The browser binding calls this before invoking an operation. The name is
/// an opaque key returned to the operation wrapper; it is not interpreted as
/// a filesystem path.
///
/// # Panics
///
/// Panics only if the process-wide lock was poisoned by a panic in another
/// tool call, which indicates a broken session rather than bad input.
pub fn insert_input(name: &str, bytes: Vec<u8>) {
    if let Ok(mut map) = store().lock() {
        let _ = map.insert(name.to_string(), bytes);
    }
}

/// Take a verified output document out of the current tool session.
///
/// Returns `None` when the operation failed (no output is ever committed on
/// failure) or when the name does not exist.
///
/// # Panics
///
/// Panics only if the process-wide lock was poisoned by a panic in another
/// tool call, which indicates a broken session rather than bad input.
#[must_use]
pub fn take_output(name: &str) -> Option<Vec<u8>> {
    store().lock().ok()?.remove(name)
}

/// Empty the session table. The browser calls this after a result is
/// downloaded; tests call it between cases.
///
/// # Panics
///
/// Panics only if the process-wide lock was poisoned by a panic in another
/// tool call, which indicates a broken session rather than bad input.
pub fn clear() {
    if let Ok(mut map) = store().lock() {
        map.clear();
    }
}

pub(crate) fn read_input(name: &str) -> Option<Vec<u8>> {
    store().lock().ok()?.get(name).cloned()
}

pub(crate) fn write_output(name: &str, bytes: &[u8]) -> CliResult<()> {
    let mut map = store()
        .lock()
        .map_err(|_| CliError("tool session lock is poisoned".to_string()))?;
    let _ = map.insert(name.to_string(), bytes.to_vec());
    Ok(())
}
