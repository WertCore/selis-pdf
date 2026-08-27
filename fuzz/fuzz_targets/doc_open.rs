//! Fuzz target for the full COS document open: revisions + xref streams +
//! object streams, with damaged-file reconstruction as the fallback
//! (SL-1.COS.04 / SL-1.COS.10).
//!
//! Contract: no panic, no OOM, terminates within the budget.

#![no_main]

use libfuzzer_sys::fuzz_target;
use selis_pdf_cos::{self, xref};
use selis_sandbox::{Budget, Surface};

fuzz_target!(|data: &[u8]| {
    let budget = Budget::profile(Surface::Fuzz);
    let mut g = budget.guard();

    // Try the revision walk (classic xref tables and xref streams).
    let startxref = xref::find_startxref(data, 2048).unwrap_or(0);
    if let Ok(doc) = selis_pdf_cos::parse_revisions(data, startxref, &budget, &mut g) {
        // Walk every revision's entries and resolve reachable in-use objects.
        for view in doc.revisions() {
            let mut entries: Vec<u32> = view.entries.keys().copied().collect();
            entries.sort_unstable();
            for num in entries {
                if let Some(selis_pdf_cos::XrefEntry::InUse { offset, .. }) =
                    view.entries.get(&num)
                {
                    let _ = selis_pdf_cos::resolve_object(data, *offset, &budget, &mut g);
                }
            }
        }
    } else {
        // Fall back to damaged-file reconstruction.
        let _ = selis_pdf_cos::reconstruct(data, &budget, &mut g);
    }
});
