//! Input backend: real files in the CLI, an in-memory table in the browser.

use crate::{CliError, CliResult};

/// Read an input document by path.
#[cfg(feature = "native-fs")]
pub(crate) fn read_file(path: &str) -> CliResult<Vec<u8>> {
    std::fs::read(path).map_err(|e| CliError(format!("cannot read {path}: {e}")))
}

/// Read an input document from the in-memory table.
#[cfg(not(feature = "native-fs"))]
pub(crate) fn read_file(path: &str) -> CliResult<Vec<u8>> {
    crate::memory::read_input(path).ok_or_else(|| {
        CliError(format!(
            "cannot read {path}: input is not in the tool session"
        ))
    })
}
