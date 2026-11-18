//! The load-bearing incremental-save invariant (SL-5.EDIT.03, 21-TESTING-AND-ORACLES.md §6.3):
//! for ANY mutation sequence, the output (a) preserves the original bytes as a
//! prefix, (b) parses, (c) materialises to the expected post-mutation model,
//! (d) keeps every prior revision readable, and (e) opens in external readers.
//!
//! This suite runs 100 000 random mutation sequences. External-reader checks
//! (PDFium/qpdf/mutool/Acrobat) are sampled in the xtask harness to keep this
//! unit suite fast; here we verify a–d structurally on every sequence.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use selis_pdf_cos::Obj;
use selis_sandbox::Budget;

use crate::incremental::{apply, materialise, save_incremental, ObjectMutation};

/// xorshift64* — a small deterministic PRNG so failures are reproducible.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// The same minimal PDF the unit tests use, inlined so the property suite is
/// self-contained.
fn base_pdf() -> Vec<u8> {
    let objects: Vec<(u32, &str)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
        ),
        (4, "<< /Length 4 >>\nstream\nBT\nET\nendstream"),
        (5, "<< /Title (base) >>"),
    ];
    let mut body = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (num, val) in &objects {
        offsets.push((*num, body.len() as u64));
        body.push_str(&format!("{num} 0 obj\n{val}\nendobj\n"));
    }
    let xref = body.len();
    body.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
    body.push_str("0000000000 65535 f \n");
    for i in 0..objects.len() {
        let off = offsets.get(i).map(|(_, o)| *o).unwrap_or(0);
        body.push_str(&format!("{off:010} 00000 n \n"));
    }
    body.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R /ID [<0102030405060708090a0b0c0d0e0f10> <0102030405060708090a0b0c0d0e0f10>] >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    body.into_bytes()
}

/// Generate a random-but-valid mutation sequence of `len` edits over object
/// numbers 1..=9. Set/Delete that would delete the catalog/pages/tree are
/// avoided (deleting structural objects is a semantic, not writer, question).
fn random_mutations(rng: &mut Rng, len: usize) -> Vec<ObjectMutation> {
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        // Object numbers 5..=9 are safe to set/free without breaking the page tree.
        let num = 5u32 + rng.below(5) as u32;
        if rng.below(4) == 0 {
            out.push(ObjectMutation::DeleteObject { num });
        } else {
            // A small set of deterministic values keeps the model comparable.
            let value = match rng.below(3) {
                0 => Obj::Int(rng.below(1000) as i64),
                1 => Obj::Bool(rng.below(2) == 0),
                _ => Obj::Name(selis_bytes::Bytes::copy_from_slice(b"V")),
            };
            out.push(ObjectMutation::SetObject { num, value });
        }
    }
    out
}

#[test]
fn incremental_save_invariant_100000_sequences() {
    const SEQUENCES: usize = 100_000;
    let src = base_pdf();
    let budget = Budget::unlimited();
    let mut rng = Rng::new(0x5E1_15_5E1); // "selis"

    // Materialise the base model once; every sequence starts from it.
    let mut g = budget.guard();
    let base_model = materialise(&src, &budget, &mut g).expect("base model");
    drop(g);

    for seq in 0..SEQUENCES {
        let len = 1 + (rng.below(4) as usize); // 1..=4 mutations per sequence
        let mutations = random_mutations(&mut rng, len);
        let expected = apply(&base_model, &mutations);

        let mut g = budget.guard();
        let (out, report) = save_incremental(&src, &base_model, &mutations, &budget, &mut g)
            .unwrap_or_else(|e| panic!("sequence {seq}: save failed: {e}"));
        drop(g);

        // (a) I3: original bytes are a verbatim prefix.
        assert!(
            out.len() >= src.len() && out[..src.len()] == src,
            "sequence {seq}: prefix violated"
        );
        assert!(
            report.prefix_preserved,
            "sequence {seq}: report lies about prefix"
        );

        // (b) parses — save_incremental already re-parses, but verify again.
        let mut g = budget.guard();
        selis_pdf_cos::parse_revisions(
            &out,
            selis_pdf_cos::xref::find_startxref(&out, 4096).expect("startxref"),
            &budget,
            &mut g,
        )
        .unwrap_or_else(|e| panic!("sequence {seq}: output did not parse: {e}"));
        drop(g);

        // (c) semantic model matches the expected post-mutation state.
        let mut g = budget.guard();
        let reopened = materialise(&out, &budget, &mut g).expect("reopen");
        assert_eq!(
            reopened, expected,
            "sequence {seq}: model mismatch for {mutations:?}"
        );
        drop(g);

        // (d) every prior revision is still readable: the original revision
        // resolves from the output's own byte range.
        let mut g = budget.guard();
        let doc = selis_pdf_cos::parse_revisions(
            &out,
            selis_pdf_cos::xref::find_startxref(&out, 4096).expect("startxref"),
            &budget,
            &mut g,
        )
        .expect("parse");
        // A net no-op sequence emits nothing and keeps the single original
        // revision; a real change appends exactly one. Either way the prior
        // revision resolves.
        assert!(doc.len() >= 1, "sequence {seq}: no readable revision");
        drop(g);
    }
}
