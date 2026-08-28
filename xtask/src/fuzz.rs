//! Fuzzing harness verification (SL-0.SEC.02).
//!
//! Verifies the fuzz targets compile with nightly + cargo-fuzz. To actually
//! run a target, call `cargo fuzz run <target>` from the workspace root.

use crate::run;

/// Build all fuzz targets to verify the harness compiles under nightly.
pub fn check() -> Result<(), String> {
    run("cargo", &["+nightly", "fuzz", "build"])
}