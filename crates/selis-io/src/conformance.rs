//! The `DocSource` conformance suite (SL-4.WASM.05 DoD).
//!
//! One behavioural suite every `DocSource` — `MemSource`, `FileSource`,
//! `HttpRangeSource`, `BlobSource`, `OpfsSource`, `FsaSource`, `FaultSource`
//! — must pass, including the fault cases (truncation, corruption, latency,
//! range-refusal, size lying). The suite is generic over `DocSource` so a
//! behavioural difference between adapters is a test failure, not a support
//! ticket (24-BINDINGS-SPEC §1.7 applied to sources).
//!
//! The shared assertion is [`assert_fully_resident_conformance`]: it drives a
//! fully-resident source through exact reads, partial tail reads, EOF, the
//! `available` set, `request` no-op-ness, and random-access. Fault legs live
//! in `fault_conformance` (via `FaultSource` with each `FaultConfig`) and in
//! the per-adapter `SOURCE_CHANGED` tests — a file replaced under us is an
//! error, never silent corruption.

use crate::{Availability, DocSource, RangeSet};

/// Assert a fully-resident source behaves identically for `expected`.
///
/// # Panics
///
/// Panics with a descriptive message when the source deviates — this is a
/// test-only helper, so panics are the failure shape.
pub fn assert_fully_resident_conformance<S: DocSource>(source: &S, expected: &[u8]) {
    let len = expected.len() as u64;
    assert_eq!(source.len(), Some(len), "len() must report the full length");
    assert!(
        source.is_random_access(),
        "fully-resident sources must be random-access"
    );
    assert_eq!(
        source.available().total_bytes(),
        len,
        "available() must cover the whole file"
    );
    if len > 0 {
        assert!(
            source.available().contains(0, len),
            "available() must contain [0, len)"
        );
    }

    // Exact read from the start.
    if !expected.is_empty() {
        let n = expected.len().min(8);
        let mut buf = vec![0u8; n];
        match source.read_at(0, &mut buf).expect("read_at(0) must succeed") {
            Availability::Filled(k) => {
                assert_eq!(k, n, "read_at(0) must fill the whole buffer");
                assert_eq!(&buf, &expected[..n], "bytes must match");
            }
            other => panic!("read_at(0) must be Filled, got {other:?}"),
        }
    }

    // Mid-file read.
    if expected.len() > 10 {
        let mut buf = [0u8; 4];
        match source.read_at(5, &mut buf).expect("mid read must succeed") {
            Availability::Filled(k) => {
                assert_eq!(k, 4);
                assert_eq!(&buf, &expected[5..9]);
            }
            other => panic!("mid read must be Filled, got {other:?}"),
        }
    }

    // Partial tail read: a short buffer at the end gets a short fill.
    if !expected.is_empty() {
        let tail = expected.len().saturating_sub(2);
        let mut buf = [0u8; 8];
        match source
            .read_at(tail as u64, &mut buf)
            .expect("tail read must succeed")
        {
            Availability::Filled(k) => {
                assert_eq!(k, expected.len() - tail);
                assert_eq!(&buf[..k], &expected[tail..]);
            }
            other => panic!("tail read must be Filled, got {other:?}"),
        }
    }

    // EOF at and beyond the end.
    {
        let mut buf = [0u8; 4];
        assert_eq!(
            source.read_at(len, &mut buf).expect("eof read"),
            Availability::Eof,
            "read at len must be Eof"
        );
        assert_eq!(
            source
                .read_at(len.saturating_add(1000), &mut buf)
                .expect("eof read"),
            Availability::Eof,
            "read beyond len must be Eof"
        );
    }

    // Empty buffer never panics and never claims bytes.
    {
        let mut empty: [u8; 0] = [];
        let r = source.read_at(0, &mut empty).expect("empty read");
        assert!(
            matches!(r, Availability::Filled(0) | Availability::Eof),
            "empty read must be Filled(0) or Eof, got {r:?}"
        );
    }

    // request() is a safe no-op for resident sources.
    source.request(&RangeSet::one(0, len));
    source.request(&RangeSet::new());
}

/// The fault matrix every adapter family must survive (via `FaultSource`,
/// the deterministic injector): truncation, corruption, latency, refusal,
/// and lying. This proves the *fault contract* once; per-adapter
/// `SOURCE_CHANGED` legs prove each adapter detects replacement.
pub fn assert_fault_matrix_conformance(data: &[u8]) {
    use crate::{FaultConfig, FaultSource};

    // Truncation: reads stop at the cut, len() stays honest.
    {
        let cut = (data.len() as u64).min(4);
        let src = FaultSource::new(
            data.to_vec(),
            FaultConfig {
                truncate_at: Some(cut),
                ..FaultConfig::default()
            },
        );
        let mut buf = vec![0u8; 32];
        if cut == 0 {
            assert_eq!(
                src.read_at(0, &mut buf).expect("truncated read"),
                Availability::Eof,
                "empty truncation must be Eof"
            );
        } else {
            match src.read_at(0, &mut buf).expect("truncated read") {
                Availability::Filled(k) => assert_eq!(k as u64, cut.min(32)),
                other => panic!("truncated read must be Filled, got {other:?}"),
            }
        }
        assert_eq!(
            src.read_at(cut, &mut buf).expect("eof at cut"),
            Availability::Eof
        );
    }

    // Latency: Pending until release, then Filled.
    {
        let src = FaultSource::new(
            data.to_vec(),
            FaultConfig {
                hold_reads: true,
                ..FaultConfig::default()
            },
        );
        let mut buf = [0u8; 4];
        assert!(
            matches!(
                src.read_at(0, &mut buf).expect("held"),
                Availability::Pending { .. }
            ),
            "held reads must be Pending"
        );
        src.release();
        if data.is_empty() {
            assert_eq!(
                src.read_at(0, &mut buf).expect("released"),
                Availability::Eof,
                "released empty reads must be Eof"
            );
        } else {
            assert!(
                matches!(
                    src.read_at(0, &mut buf).expect("released"),
                    Availability::Filled(_)
                ),
                "released reads must be Filled"
            );
        }
    }

    // Range refusal: random access off.
    {
        let src = FaultSource::new(
            data.to_vec(),
            FaultConfig {
                refuse_ranges: true,
                ..FaultConfig::default()
            },
        );
        assert!(!src.is_random_access(), "refusal must disable seeking");
    }

    // Lying: len() claims more, reads stop at the real end.
    {
        let src = FaultSource::new(
            data.to_vec(),
            FaultConfig {
                length_lying: true,
                length_lying_offset: 4096,
                ..FaultConfig::default()
            },
        );
        assert_eq!(src.len(), Some(data.len() as u64 + 4096));
        let mut buf = [0u8; 8];
        assert_eq!(
            src.read_at(data.len() as u64, &mut buf).expect("eof"),
            Availability::Eof,
            "lying reads still stop at the real end"
        );
    }

    // Corruption is deterministic from the seed (same input, same output).
    {
        let cfg = FaultConfig {
            corrupt_rate: 0.5,
            ..FaultConfig::default()
        };
        let a = FaultSource::new(data.to_vec(), cfg);
        let b = FaultSource::new(data.to_vec(), cfg);
        let mut ba = vec![0u8; data.len().min(64)];
        let mut bb = vec![0u8; data.len().min(64)];
        if !ba.is_empty() {
            let ra = a.read_at(0, &mut ba).expect("corrupt read");
            let rb = b.read_at(0, &mut bb).expect("corrupt read");
            assert_eq!(ra, rb, "corruption must be deterministic");
            assert_eq!(ba, bb);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlobSource, FsaSource, MemSource, OpfsSource};

    fn sample() -> Vec<u8> {
        b"%PDF-1.4 conformance sample bytes 0123456789".to_vec()
    }

    #[test]
    fn mem_passes_conformance() {
        let data = sample();
        let src = MemSource::new(&data);
        assert_fully_resident_conformance(&src, &data);
    }

    #[test]
    fn blob_passes_conformance() {
        let data = sample();
        let src = BlobSource::new(data.clone(), "sample.pdf");
        assert_fully_resident_conformance(&src, &data);
    }

    #[test]
    fn opfs_passes_conformance() {
        let data = sample();
        let src = OpfsSource::new("/selis/sample.pdf", data.clone());
        assert_fully_resident_conformance(&src, &data);
    }

    #[test]
    fn fsa_passes_conformance() {
        let data = sample();
        let src = FsaSource::new("h1", "sample.pdf", data.clone());
        assert_fully_resident_conformance(&src, &data);
    }

    #[test]
    fn empty_sources_pass_conformance() {
        let empty: Vec<u8> = Vec::new();
        assert_fully_resident_conformance(&MemSource::new(&empty), &empty);
        assert_fully_resident_conformance(&BlobSource::new(empty.clone(), "e.pdf"), &empty);
        assert_fully_resident_conformance(&OpfsSource::new("/e.pdf", empty.clone()), &empty);
        assert_fully_resident_conformance(&FsaSource::new("h", "e.pdf", empty.clone()), &empty);
    }

    #[test]
    fn fault_matrix_passes() {
        assert_fault_matrix_conformance(&sample());
        assert_fault_matrix_conformance(b"");
        assert_fault_matrix_conformance(&vec![0xABu8; 1024]);
    }

    #[test]
    fn all_adapters_agree_byte_for_byte() {
        // The DoD's cross-adapter leg: the same bytes through every adapter
        // read identically at every offset.
        let data = sample();
        let mem = MemSource::new(&data);
        let blob = BlobSource::new(data.clone(), "a.pdf");
        let opfs = OpfsSource::new("/a.pdf", data.clone());
        let fsa = FsaSource::new("h", "a.pdf", data.clone());
        for off in 0..data.len() as u64 {
            let mut b0 = [0u8; 1];
            let mut b1 = [0u8; 1];
            let mut b2 = [0u8; 1];
            let mut b3 = [0u8; 1];
            let r0 = mem.read_at(off, &mut b0).expect("mem");
            let r1 = blob.read_at(off, &mut b1).expect("blob");
            let r2 = opfs.read_at(off, &mut b2).expect("opfs");
            let r3 = fsa.read_at(off, &mut b3).expect("fsa");
            assert_eq!(r0, r1);
            assert_eq!(r0, r2);
            assert_eq!(r0, r3);
            assert_eq!(b0, b1);
            assert_eq!(b0, b2);
            assert_eq!(b0, b3);
        }
    }
}
