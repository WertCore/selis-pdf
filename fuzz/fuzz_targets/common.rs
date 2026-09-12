//! Shared fuzz-target helpers (SL-1.ROB.02).

use std::sync::Once;

static INSTALL: Once = Once::new();

/// Replaces libfuzzer-sys's abort-on-panic hook with a printing one.
///
/// The font-parser targets exercise upstream crates (skrifa, read-fonts,
/// swash) that carry open-ended arithmetic-overflow panic families on
/// hostile font data; fuzz builds force overflow checks on. Those panics
/// are contained by typed-error/deviation boundaries inside
/// `selis-font`/`selis-shape` (see plan task SL-1.ROB.06) and must surface
/// as deviations, not campaign crashes. A panic that escapes those
/// boundaries still unwinds into libfuzzer-sys's own `catch_unwind` and
/// aborts the process as usual.
pub fn install_printing_panic_hook() {
    INSTALL.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            eprintln!("{info}");
        }));
    });
}
