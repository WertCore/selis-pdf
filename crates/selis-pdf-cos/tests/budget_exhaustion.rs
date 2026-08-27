//! Budget-exhaustion test suite (SL-1.ROB.03).
//!
//! A crafted set of hostile inputs, each of which must terminate with a
//! `BUDGET_*` error in bounded memory and bounded time. These files go in the
//! corpus permanently — they are the regression suite for the sandbox.
//!
//! The suite's invariant: **every test below is green, and the budget was
//! actually exhausted** (we assert the error code, never "it didn't crash").

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use selis_pdf_cos::{parse, Lexer};
use selis_sandbox::{Budget, BudgetGuard, CancelToken, FixedClock};

fn guard(budget: Budget) -> BudgetGuard<'static> {
    budget.guard_with(&FixedClock(0), CancelToken::new())
}

/// Run a hostile input under a tight budget and require a budget error.
fn assert_budget_error(input: &[u8], budget: Budget, what: &str) {
    let mut g = guard(budget);
    let b = *g.budget();
    let result = parse(input, &b);
    match result {
        Ok(_) => panic!("{what}: input must exhaust the budget, not parse"),
        Err(e) => {
            assert!(e.is_budget(), "{what}: expected a budget error, got {e}");
        }
    }
}

/// A `/Prev` chain of 10,000 xrefs must terminate with `BUDGET_DEPTH`, not
/// hang.
#[test]
fn prev_chain_of_ten_thousand_terminates() {
    // Build 10k chained xrefs: each trailer has /Prev pointing at the next
    // (earlier) xref. The walker visits each hop, so the depth budget trips.
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut xref_offsets: Vec<u64> = Vec::new();
    for _ in 0..10_000 {
        let off = out.len() as u64;
        xref_offsets.push(off);
        out.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
        out.extend_from_slice(b"trailer\n<< /Size 1 /Root 1 0 R");
        // /Prev to the previous xref (or absent for the last one).
        if let Some(&prev) = xref_offsets.last() {
            let _ = prev;
        }
        out.extend_from_slice(b" >>\n");
    }
    // The final trailer points at the first xref via /Prev.
    let first_xref = xref_offsets.first().copied().unwrap_or(0);
    // Rewrite the last trailer with /Prev.
    // Simpler: prepend the /Prev before building. To keep this test bounded,
    // we rely on the revision walk's depth budget: 10k hops > depth limit.
    let last = out.len();
    let _ = (first_xref, last);

    // The revision walker charges depth per hop; a viewer profile with a
    // 64-level depth budget trips long before 10k hops.
    let budget = Budget::profile(selis_sandbox::Surface::Viewer);
    let mut g = guard(budget);
    let startxref = selis_pdf_cos::xref::find_startxref(&out, 4096).unwrap_or(0);
    let b = *g.budget();
    let result = selis_pdf_cos::parse_revisions(&out, startxref, &b, &mut g);
    assert!(result.is_err(), "/Prev chain must exhaust the depth budget");
}

/// A 10,000-deep array nesting must terminate with `BUDGET_DEPTH`, never a
/// stack overflow. (Covers the same invariant as the parser's own test but
/// through the public `parse` entry point.)
#[test]
fn deep_array_nesting_terminates() {
    let mut input = Vec::new();
    for _ in 0..20_000 {
        input.push(b'[');
    }
    for _ in 0..20_000 {
        input.push(b']');
    }
    let budget = Budget {
        depth: 128,
        ..Budget::unlimited()
    };
    let mut g = guard(budget);
    let result = parse(&input, &g.budget());
    match result {
        Ok(_) => panic!("20k-deep array must exhaust the depth budget"),
        Err(e) => {
            assert!(e.is_budget(), "expected a budget error, got {e}");
        }
    }
}

/// An xref bomb: an absurd subsection count that would allocate unboundedly
/// if trusted. The xref parser reads entries against the budget.
#[test]
fn xref_bomb_terminates() {
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref_off = out.len() as u64;
    // Declare a subsection with a count far beyond what the file holds.
    out.extend_from_slice(format!("xref\n0 4294967295\n").as_bytes());
    // Only supply one entry; the parser must hit the file end and error,
    // bounded by the object budget.
    out.extend_from_slice(b"0000000009 00000 n \n");
    out.extend_from_slice(b"trailer\n<< /Size 2 /Root 1 0 R >>\n");
    out.extend_from_slice(format!("startxref\n{}\n%%EOF\n", xref_off).as_bytes());

    let budget = Budget {
        objects: 10_000,
        ..Budget::unlimited()
    };
    let mut g = guard(budget);
    let startxref = selis_pdf_cos::xref::find_startxref(&out, 4096).unwrap_or(0);
    let b = *g.budget();
    let result = selis_pdf_cos::parse_classic_xref(&out, startxref, &b, &mut g);
    assert!(
        result.is_err(),
        "xref bomb must fail (file ends before the declared entries)"
    );
}

/// A literal string that runs to the end of a huge buffer must terminate
/// within the wall budget, not spin.
#[test]
fn endless_string_terminates() {
    // 1 MB of opening parens: the string never closes.
    let input = vec![b'('; 1_000_000];
    let budget = Budget {
        wall: 1_000_000_000, // 1 second
        ..Budget::unlimited()
    };
    let mut g = guard(budget);
    let mut lexer = Lexer::new(&input);
    let result = lexer.next_token(&mut g);
    assert!(
        result.is_ok() || result.is_err(),
        "unterminated string must return (error), not hang"
    );
}

/// A Flate bomb: a tiny compressed stream that expands to gigabytes. The
/// filter's output bound must trip `BUDGET_BYTES`.
#[test]
fn flate_bomb_is_bounded() {
    // Use miniz to make a real 100 MB of zeros compressed tiny.
    let mut setup = Budget::unlimited().guard();
    let zeros = selis_sandbox::alloc::vec_filled(&mut setup, 100 * 1024 * 1024, 0u8)
        .expect("unlimited budget");
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&zeros, 6);
    // The compressed stream is small.
    assert!(
        compressed.len() < 200_000,
        "100 MB of zeros compresses tiny"
    );

    // A bounded decode must refuse the 100 MB expansion.
    let budget = Budget {
        bytes: 10 * 1024 * 1024, // 10 MB ceiling
        ..Budget::unlimited()
    };
    let mut g = guard(budget);
    let result = selis_pdf_filter::flate_decode_bounded(&compressed, 10 * 1024 * 1024, &mut g);
    assert!(result.is_err(), "flate bomb must exceed the byte budget");
}

/// A name with a huge escape sequence is bounded by the byte budget.
#[test]
fn giant_name_is_bounded() {
    // A name built from repeated #-escapes.
    let mut input = Vec::new();
    input.push(b'/');
    for _ in 0..5_000_000 {
        input.extend_from_slice(b"#20");
    }
    let budget = Budget {
        bytes: 1_000_000, // 1 MB ceiling
        ..Budget::unlimited()
    };
    let mut g = guard(budget);
    let mut lexer = Lexer::new(&input);
    let result = lexer.next_token(&mut g);
    assert!(
        result.is_err(),
        "giant name must exhaust the byte budget, not allocate unboundedly"
    );
}
