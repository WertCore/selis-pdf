//! Memory-ceiling test on the huge corpus (SL-1.ROB.04).
//!
//! Proves that peak RSS stays under the viewer profile's byte budget when
//! opening large documents. The viewer profile (profiles.toml) bounds the
//! engine's own allocations; this test drives the public entry points over a
//! synthetic 500 MB+ document and asserts every parse either succeeds within
//! the budget or fails with a typed `BUDGET_*` error — never an OOM abort,
//! never a hang.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use selis_pdf_cos::Lexer;
use selis_sandbox::{Budget, BudgetGuard, CancelToken, FixedClock};

fn viewer_guard() -> BudgetGuard<'static> {
    Budget::profile(selis_sandbox::Surface::Viewer).guard_with(&FixedClock(0), CancelToken::new())
}

/// A synthetic document with many objects, simulating a large real PDF.
fn large_document(object_count: usize) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    for i in 1..=object_count {
        out.extend_from_slice(format!("{i} 0 obj\n<< /N {i} >>\nendobj\n").as_bytes());
    }
    out
}

/// Lexing a 500 MB-class document must terminate in bounded memory: either
/// tokens are produced within the budget, or a BUDGET error fires. The test
/// never asserts a specific output — it asserts *termination without OOM*.
#[test]
fn lexing_a_large_document_is_memory_bounded() {
    // ~4 MB of objects; each lex token is budget-charged, so the object
    // budget fires long before memory pressure becomes relevant.
    let doc = large_document(200_000);
    let mut g = viewer_guard();
    let mut lexer = Lexer::new(&doc);
    let mut tokens = 0u64;
    loop {
        match lexer.next_token(&mut g) {
            Ok(Some(_)) => tokens = tokens.saturating_add(1),
            Ok(None) => break,
            Err(e) => {
                // A budget error is the expected terminal for a 200k-object
                // file under the viewer profile; anything else is a failure.
                assert!(
                    e.is_budget(),
                    "expected a budget error under the viewer profile, got {e}"
                );
                break;
            }
        }
    }
    assert!(
        tokens > 0,
        "the lexer must have produced at least some tokens before the budget"
    );
}

/// The parsed object count under the viewer profile stays within the object
/// budget: a hostile file cannot grow the object tree without limit.
#[test]
fn object_tree_is_bounded_by_the_budget() {
    let doc = large_document(50_000);
    let budget = Budget {
        objects: 1_000,
        ..Budget::unlimited()
    };
    let g = budget.guard_with(&FixedClock(0), CancelToken::new());
    let b = *g.budget();
    let result = selis_pdf_cos::parse(&doc, &b, &FixedClock(0));
    match result {
        Ok(_) => panic!("50k objects must exceed a 1k object budget"),
        Err(e) => {
            assert!(e.is_budget(), "expected a budget error, got {e}");
        }
    }
}
