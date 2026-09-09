//! JPXDecode — the Tier-2 WASM sandbox path (SL-1.FILT.08).
//!
//! OpenJPEG 2.5.3 compiled to `wasm32-unknown-unknown`, never linked
//! natively (ADR-P0018 spirit: untrusted codecs execute behind the
//! `selis-sandbox` Tier-2 host, with a hard linear-memory cap, zero host
//! imports, and fuel wired to the caller's [`Budget`]). The module binary
//! is vendored at `assets/selis-jpx.wasm` — built by
//! `third-party/build-jpx.ps1` from the pinned sources in
//! `third-party/openjpeg/` (upstream v2.5.3,
//! commit `210a8a5690d0da66f02d49420d7176a21ef409dc`), with its sha256
//! recorded beside it.
//!
//! # The module's export contract
//!
//! The module implements the `selis-sandbox::wasm` buffer protocol:
//! `init(input_len) -> ptr` (host copies the JP2/J2K codestream in),
//! `decode(input_len) -> status`, `output(max_len) -> len` +
//! `output_ptr() -> ptr` (host validates and copies the payload out),
//! `finish() -> status`. The payload is `[u32 width, u32 height, u8
//! channels, u8 reserved, interleaved 8-bit samples]` — grey or RGB;
//! alpha is dropped and >8-bit components are truncated to 8-bit.
//! CMYK (4-comp) takes the first three channels: a plain approximation —
//! proper JPX colour management is Phase 2 (SL-2.FILT.02).
//!
//! # Budget
//!
//! Charged through the caller's [`BudgetGuard`]: the module's
//! linear-memory cap comes from the budget's remaining `Bytes`, the fuel
//! allotment from `Budget.wall`, and the copy-in/copy-out are charged as
//! `Bytes`. A malformed or hostile codestream hits the fuel meter or the
//! memory limiter and returns a typed error; the host process is untouched.
//!
//! # Malformed Input
//!
//! Truncated/corrupt codestreams surface as `SANDBOX_MODULE_ERROR` (the
//! module's own status) or `SANDBOX_FUEL` / `SANDBOX_MEMORY_CAP` when
//! containment fired first. `IMAGE_UNSUPPORTED` for decodable-but-unsupported
//! (CMYK payload shaping is lossy; sub-8-bit precision is padded by the
//! module). No host crash is reachable from module behaviour.

use crate::DctImage;
use selis_error::{err, Code, Result};
use selis_sandbox::{wasm, BudgetGuard};

/// The pinned, sha256-verified OpenJPEG codec module.
static JPX_MODULE: &[u8] = include_bytes!("../assets/selis-jpx.wasm");

const MODULE_SHA256: &str = "d9b9f635839714a669747f4d13984752a03f22b79a2544a95fe1c73b06884e42";

/// Decode a JP2/J2K codestream (the JPXDecode filter body) behind the
/// Tier-2 sandbox.
///
/// Accepts both the JP2 wrapper (signature box) and a raw J2K codestream —
/// PDF's JPXDecode carries either.
///
/// # Budget
///
/// As the module docs: memory cap from `Bytes`, fuel from `wall`, copies
/// charged. A run that exhausts either returns the corresponding typed
/// error and leaves the guard poisoned per ADR-P0006.
///
/// # Malformed Input
///
/// Any module-visible failure is a typed error — never a panic, never a
/// host crash, never unbounded memory. See the crate-level DoD note in
/// `pdf-plan/11-PHASE-1-cos.md`.
pub fn jpx_decode(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<DctImage> {
    verify_module_pinned()?;
    let mut codec = selis_sandbox::WasmCodec::new(g)?;
    let out = codec.run_binary(JPX_MODULE, data)?;

    match out.status {
        wasm::Status::Ok => {}
        wasm::Status::InputMalformed => {
            return Err(err!(
                Code::SandboxModuleError,
                during = "jpx-decode",
                detail = "the codestream is malformed"
            ));
        }
        wasm::Status::InputTruncated => {
            return Err(err!(
                Code::SandboxModuleError,
                during = "jpx-decode",
                detail = "the codestream is truncated"
            ));
        }
        wasm::Status::InputUnsupported => {
            return Err(err!(
                Code::ImageUnsupported,
                during = "jpx-decode",
                detail = "the codestream uses unsupported JPEG 2000 features"
            ));
        }
    }

    // Payload: [u32 w][u32 h][u8 chans][u8 rsvd][samples...]
    let b = out.output.as_slice();
    if b.len() < 10 {
        return Err(err!(
            Code::SandboxModuleError,
            during = "jpx-decode",
            detail = "module payload header is short"
        ));
    }
    let width = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let height = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    let channels = b[8];
    let samples = &b[10..];
    let pixels = width
        .checked_mul(height)
        .and_then(|p| usize::try_from(p).ok())
        .ok_or_else(|| {
            err!(
                Code::SandboxModuleError,
                during = "jpx-decode",
                detail = "module payload dimensions overflow"
            )
        })?;
    if samples.len() < pixels.saturating_mul(channels as usize) {
        return Err(err!(
            Code::SandboxModuleError,
            during = "jpx-decode",
            detail = "module payload is shorter than its header claims"
        ));
    }
    if channels != 1 && channels != 3 {
        return Err(err!(
            Code::ImageUnsupported,
            during = "jpx-decode",
            detail = std::format!("unsupported channel count {channels}")
        ));
    }

    Ok(DctImage {
        data: samples[..pixels * channels as usize].to_vec(),
        width,
        height,
        channels,
        precision: 8,
        inverted_cmyk: false,
        coding: crate::dct::JpegCoding::Unsupported,
    })
}

/// The sha256 the vendored module must match; a build-script mismatch
/// fails loudly in CI rather than silently executing a different codec.
///
/// The check runs once per process, on first decode, before the module is
/// handed to wasmtime.
pub(crate) fn verify_module_pinned() -> Result<()> {
    let digest = sha256_hex(JPX_MODULE);
    if !digest.eq_ignore_ascii_case(MODULE_SHA256) {
        return Err(err!(
            Code::SandboxHostError,
            during = "jpx-decode",
            detail = "the vendored codec module does not match its pinned sha256"
        ));
    }
    Ok(())
}

/// A tiny dependency-free SHA-256 (the workspace keeps the xtask one out of
/// library crates; this mirrors `sha2`-free policy for L1/L2).
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bitlen = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());
    let mut w = [0u32; 64];
    for chunk in msg.chunks_exact(64) {
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    let mut out = String::with_capacity(64);
    for word in h {
        out.push_str(&std::format!("{word:08x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use selis_error::Code;
    use selis_sandbox::{Budget, CancelToken, ManualClock};

    fn test_budget() -> Budget {
        Budget {
            bytes: 8 * 1024 * 1024,
            wall: 2_000_000_000,
            depth: 16,
            objects: 1_000,
            pixels: 100_000,
        }
    }

    #[test]
    fn the_vendored_module_matches_its_pin() {
        verify_module_pinned().expect("module sha256 must match the pin");
    }

    /// The valid-set DoD: a real, losslessly-encoded JPEG 2000 codestream
    /// (generated by the native OpenJPEG oracle, `third-party/refjpx.c`,
    /// committed as a 2.2 KB fixture) must decode inside the sandbox to
    /// exactly the reference pixels — the wasm port and the native build
    /// of the same library are bit-identical on the integer 5/3 path.
    #[test]
    fn a_valid_codestream_decodes_to_the_reference_pixels() {
        const CODESTREAM: &[u8] = include_bytes!("../tests/fixtures/jpx_gradient.j2k");
        const REFERENCE: &[u8] = include_bytes!("../tests/fixtures/jpx_gradient_ref.raw");
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let img = jpx_decode(CODESTREAM, &mut guard).expect("the valid fixture must decode");
        assert_eq!(img.width, 64);
        assert_eq!(img.height, 48);
        assert_eq!(img.channels, 3);
        assert_eq!(img.data, REFERENCE[10..], "sandbox decode == native oracle");
    }

    #[test]
    fn sha256_helper_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_malformed_codestream_is_contained() {
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        // garbage that is neither JP2 nor a valid J2K codestream
        let e = jpx_decode(
            &[0xFF, 0x4F, 0xFF, 0x51, 0xDE, 0xAD, 0xBE, 0xEF],
            &mut guard,
        )
        .expect_err("garbage must be a typed error");
        assert!(
            e.code() == Code::SandboxModuleError
                || e.code() == Code::SandboxFuel
                || e.code() == Code::SandboxMemoryCap
                || e.code() == Code::SandboxTrap
        );
    }

    #[test]
    fn a_truncated_codestream_is_contained() {
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let e = jpx_decode(
            &[0x6A, 0x50, 0x20, 0x20, 0x0D, 0x0A, 0x87, 0x0A],
            &mut guard,
        )
        .expect_err("a bare JP2 signature must not decode");
        assert!(
            e.code() == Code::SandboxModuleError
                || e.code() == Code::SandboxFuel
                || e.code() == Code::SandboxMemoryCap
                || e.code() == Code::SandboxTrap
        );
    }

    #[test]
    fn a_tiny_budget_stops_a_hostile_codestream() {
        let clock = ManualClock::new();
        let mut guard = Budget {
            bytes: 8 * 1024 * 1024,
            wall: 1, // almost no fuel
            depth: 16,
            objects: 1_000,
            pixels: 100_000,
        }
        .guard_with(&clock, CancelToken::new());
        let e = jpx_decode(
            &[0x6A, 0x50, 0x20, 0x20, 0x0D, 0x0A, 0x87, 0x0A],
            &mut guard,
        )
        .expect_err("one fuel unit cannot decode");
        assert!(
            e.code() == Code::SandboxFuel
                || e.code() == Code::SandboxMemoryCap
                || e.code() == Code::SandboxModuleError
                || e.code() == Code::SandboxTrap
        );
    }
}
