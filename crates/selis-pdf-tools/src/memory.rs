//! The browser tools session: input and output bytes held by path name.
//!
//! The tools implementation already speaks in paths (`merge(inputs, output)`).
//! In the browser those names are opaque keys into this table, so the same
//! code runs without a filesystem and without any path escaping the page.
//! The map is process-local and deliberately has no persistence.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

#[cfg(not(feature = "native-fs"))]
use crate::{CliError, CliResult};

fn store() -> &'static Mutex<BTreeMap<String, Vec<u8>>> {
    static STORE: OnceLock<Mutex<BTreeMap<String, Vec<u8>>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(BTreeMap::new()))
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

#[cfg(not(feature = "native-fs"))]
pub(crate) fn read_input(name: &str) -> Option<Vec<u8>> {
    store().lock().ok()?.get(name).cloned()
}

#[cfg(not(feature = "native-fs"))]
pub(crate) fn write_output(name: &str, bytes: &[u8]) -> CliResult<()> {
    let mut map = store()
        .lock()
        .map_err(|_| CliError("tool session lock is poisoned".to_string()))?;
    let _ = map.insert(name.to_string(), bytes.to_vec());
    Ok(())
}
