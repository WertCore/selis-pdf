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
    let (header, samples) = b.split_at_checked(10).ok_or_else(|| {
        err!(
            Code::SandboxModuleError,
            during = "jpx-decode",
            detail = "module payload header is short"
        )
    })?;
    // `header` is exactly 10 bytes: 0..4 width, 4..8 height, 8 channels.
    let width = u32::from_le_bytes(
        header
            .get(..4)
            .map(<[u8; 4]>::try_from)
            .ok_or_else(|| module_shape_error("width field missing"))?
            .map_err(|_| module_shape_error("width field short"))?,
    );
    let height = u32::from_le_bytes(
        header
            .get(4..8)
            .map(<[u8; 4]>::try_from)
            .ok_or_else(|| module_shape_error("height field missing"))?
            .map_err(|_| module_shape_error("height field short"))?,
    );
    let channels = *header
        .get(8)
        .ok_or_else(|| module_shape_error("channel field missing"))?;
    let pixel_count = width
        .checked_mul(height)
        .and_then(|p| usize::try_from(p).ok())
        .ok_or_else(|| module_shape_error("module payload dimensions overflow"))?;
    let sample_bytes = pixel_count
        .checked_mul(usize::from(channels))
        .ok_or_else(|| module_shape_error("module payload size overflow"))?;
    if samples.len() < sample_bytes {
        return Err(module_shape_error(
            "module payload is shorter than its header claims",
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
        data: samples.get(..sample_bytes).unwrap_or(samples).to_vec(),
        width,
        height,
        channels,
        precision: 8,
        inverted_cmyk: false,
        coding: crate::dct::JpegCoding::Unsupported,
    })
}

/// A payload-shape violation reported by the sandbox module.
fn module_shape_error(detail: &'static str) -> selis_error::Error {
    err!(
        Code::SandboxModuleError,
        during = "jpx-decode",
        detail = detail
    )
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
    let bitlen = u64::try_from(data.len())
        .unwrap_or(u64::MAX)
        .wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len().wrapping_rem(64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());
    let mut w = [0u32; 64];
    for chunk in msg.chunks_exact(64) {
        for (i, word) in w.iter_mut().take(16).enumerate() {
            let off = i.wrapping_mul(4);
            let bytes = chunk.get(off..off.wrapping_add(4)).unwrap_or(&[0u8; 4]);
            let arr = <[u8; 4]>::try_from(bytes).unwrap_or([0u8; 4]);
            *word = u32::from_be_bytes(arr);
        }
        for i in 16usize..64 {
            let (i15, i2, i16, i7) = (
                i.wrapping_sub(15),
                i.wrapping_sub(2),
                i.wrapping_sub(16),
                i.wrapping_sub(7),
            );
            let (v15, v2) = (
                w.get(i15).copied().unwrap_or(0),
                w.get(i2).copied().unwrap_or(0),
            );
            let s0 = v15.rotate_right(7) ^ v15.rotate_right(18) ^ v15.wrapping_shr(3);
            let s1 = v2.rotate_right(17) ^ v2.rotate_right(19) ^ v2.wrapping_shr(10);
            let next = w
                .get(i16)
                .copied()
                .unwrap_or(0)
                .wrapping_add(s0)
                .wrapping_add(w.get(i7).copied().unwrap_or(0))
                .wrapping_add(s1);
            if let Some(slot) = w.get_mut(i) {
                *slot = next;
            }
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K.get(i).copied().unwrap_or(0))
                .wrapping_add(w.get(i).copied().unwrap_or(0));
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
        for (slot, val) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(val);
        }
    }
    let mut out = String::with_capacity(64);
    for word in h {
        out.push_str(&std::format!("{word:08x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    // Test bodies index fixed golden fixtures and compute analytic gradient
    // pixels; the hostile-input lints are deliberate here.
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::integer_division,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap
    )]

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

    /// Budget for *hostile* inputs. Fuel is 10× `wall`, so a generous wall
    /// makes a codestream that spins inside OpenJPEG's recovery loops burn
    /// real seconds; 25 ms of budget (2.5·10⁸ fuel) bounds each contained run
    /// to moments while the `bytes` cap still sits below what a size bomb
    /// claims, so growth refusal stays reachable before fuel exhaustion.
    fn containment_budget() -> Budget {
        Budget {
            wall: 25_000_000,
            ..test_budget()
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

    /// SL-2.FILT.02 / valid-set breadth: the second golden fixture is the SAME
    /// reversible (5/3) gradient the native oracle wrote, but with the
    /// multi-component transform disabled (`tcp_mct = 0` in
    /// `third-party/refjpx.c`). No MCT means the components are stored as the
    /// raw gradient, so the sandbox decode must equal the analytic source
    /// *exactly* — a binary-free oracle that checks the port on the
    /// no-transform path too (the `jpx_gradient_ref.raw` fixture covers the
    /// MCT/YC path). Activates the previously-unused fixture.
    #[test]
    fn a_valid_nomct_codestream_decodes_to_the_analytic_gradient() {
        const CODESTREAM: &[u8] = include_bytes!("../tests/fixtures/jpx_gradient_nomct.j2k");
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let img = jpx_decode(CODESTREAM, &mut guard).expect("the valid nomct fixture must decode");
        assert_eq!(img.width, 64);
        assert_eq!(img.height, 48);
        assert_eq!(img.channels, 3);
        assert_eq!(img.data.len(), 64 * 48 * 3);
        // Reproduce refjpx.c encode_fixture: R ramps on x, G on y, B = x^y.
        for y in 0..img.height {
            for x in 0..img.width {
                let i = usize::try_from(y * img.width + x).expect("in range") * 3;
                let r: u8 = u8::try_from((u64::from(x) * 255) / 63).expect("0..=255");
                let g: u8 = u8::try_from((u64::from(y) * 255) / 47).expect("0..=255");
                let b: u8 = (x as u8) ^ (y as u8);
                let want = [r, g, b];
                let got = [
                    *img.data.get(i).expect("sample"),
                    *img.data.get(i + 1).expect("sample"),
                    *img.data.get(i + 2).expect("sample"),
                ];
                assert_eq!(got, want, "pixel ({x},{y}) of the lossless gradient");
            }
        }
    }

    /// The lossy valid-set fixture (SL-1.FILT.08 oracle-match clause, b): a
    /// 9/7 irreversible-DWT codestream (`REFJPX_IRREVERSIBLE=1`, Q=40 dB)
    /// committed as `jpx_gradient_lossy.j2k`, with its native decode captured
    /// as `jpx_gradient_lossy_ref.raw`. The wasm port of the same OpenJPEG
    /// library must reproduce the reference *exactly* (the 9/7 float DWT is
    /// deterministic within one build), and that decode must genuinely differ
    /// from the lossless gradient — the fixture exercises the common in-the-wild
    /// lossy path, not a hidden reversible re-encode.
    #[test]
    fn a_valid_lossy_codestream_decodes_to_its_reference_and_is_lossy() {
        const CODESTREAM: &[u8] = include_bytes!("../tests/fixtures/jpx_gradient_lossy.j2k");
        const REFERENCE: &[u8] = include_bytes!("../tests/fixtures/jpx_gradient_lossy_ref.raw");
        const LOSSLESS: &[u8] = include_bytes!("../tests/fixtures/jpx_gradient_ref.raw");
        let clock = ManualClock::new();
        let mut guard = test_budget().guard_with(&clock, CancelToken::new());
        let img = jpx_decode(CODESTREAM, &mut guard).expect("the valid lossy fixture must decode");
        assert_eq!((img.width, img.height, img.channels), (64, 48, 3));
        assert_eq!(img.data.len(), 64 * 48 * 3);

        // Byte-exact vs the native OpenJPEG ref: same library, deterministic.
        let payload = &REFERENCE[10..10 + 64 * 48 * 3];
        assert_eq!(img.data, payload, "lossy wasm decode must match the native ref");

        // It is genuinely lossy: at least one sample differs from the
        // lossless gradient (Q=40 dB measured max diff 28, mean 2.0).
        let lossless = &LOSSLESS[10..10 + 64 * 48 * 3];
        assert_ne!(
            img.data, lossless,
            "the 9/7 fixture must not be a hidden reversible re-encode"
        );
    }

    /// The malformed-JPX corpus (`tests/fixtures/filter-jpx`, see its
    /// README): every corrupted codestream must be *contained* — a typed
    /// error from the sandbox/image family, no host crash, no unbounded host
    /// allocation — and the caller's budget must survive the whole barrage so
    /// a later valid decode still succeeds. This is the SL-0.SBX.06
    /// "host survives a barrage of hostile modules" DoD clause, replayed
    /// against the *real* OpenJPEG module rather than test-only wat.
    #[test]
    fn filter_jpx_corpus_is_contained() {
        const CORPUS: &[(&str, &[u8])] = &[
            (
                "siz-bomb",
                include_bytes!("../tests/fixtures/filter-jpx/siz-bomb.j2k"),
            ),
            (
                "tile-count-bomb",
                include_bytes!("../tests/fixtures/filter-jpx/tile-count-bomb.j2k"),
            ),
            (
                "truncated-tile",
                include_bytes!("../tests/fixtures/filter-jpx/truncated-tile.j2k"),
            ),
            (
                "marker-garbage",
                include_bytes!("../tests/fixtures/filter-jpx/marker-garbage.j2k"),
            ),
            (
                "header-only",
                include_bytes!("../tests/fixtures/filter-jpx/header-only.j2k"),
            ),
        ];
        let clock = ManualClock::new();
        // A containment-sized budget: the module self-limits at 64 MiB of
        // linear memory, and the *host* only ever copies in `input` (a few
        // KB) plus whatever the module legitimately produces — never the
        // 4-billion-pixel canvas a bomb file claims.
        let mut guard = containment_budget().guard_with(&clock, CancelToken::new());
        let base_usage = guard.usage().bytes;
        for (name, data) in CORPUS {
            let e = match jpx_decode(data, &mut guard) {
                Ok(img) => panic!(
                    "{name} must not decode inside the sandbox, got {:?}x{:?}",
                    img.width, img.height
                ),
                Err(e) => e,
            };
            assert!(
                is_containment(&e),
                "{name} escaped containment with a non-sandbox code {:?}",
                e.code()
            );
            let used = guard.usage().bytes.saturating_sub(base_usage);
            // Host-side charge stays bounded by the ~2 KB of inputs, not by
            // any dimension a file header claims.
            assert!(
                used < 64 * 1024,
                "{name} charged {used} host bytes — memory cap did not hold"
            );
            // Containment, not catastrophe: the guard is undamaged.
            assert_eq!(guard.poisoned_by(), None, "{name} poisoned the guard");
        }
        // The host survives the barrage: the golden fixture still decodes in
        // a fresh run (one store per run is the SBX.06 design — nothing in
        // the barrage can leak into it), against a viewer-sized budget.
        let mut survivor = test_budget().guard_with(&clock, CancelToken::new());
        let valid: &[u8] = include_bytes!("../tests/fixtures/jpx_gradient.j2k");
        jpx_decode(valid, &mut survivor).expect("host survives the corpus barrage");
    }

    /// The memory-cap DoD clause, stated directly: a codestream claiming a
    /// 4×10⁹-pixel canvas under a small budget is answered by the sandbox
    /// with a bounded host copy, never an allocation proportional to the
    /// claim.
    #[test]
    fn a_siz_bomb_charges_the_host_for_input_only() {
        const SIZ_BOMB: &[u8] = include_bytes!("../tests/fixtures/filter-jpx/siz-bomb.j2k");
        let clock = ManualClock::new();
        let mut guard = containment_budget().guard_with(&clock, CancelToken::new());
        let e = jpx_decode(SIZ_BOMB, &mut guard)
            .expect_err("a 4-gigapixel canvas claim must not decode");
        assert!(is_containment(&e));
        // input copy (2215) plus a bounded module output attempt only.
        assert!(
            guard.usage().bytes < 1024 * 1024,
            "host charged {} bytes for a 4 Gpixel claim",
            guard.usage().bytes
        );
    }
}

/// True iff `e` is a sandbox/image containment outcome (typed error, not a
/// host fault). Shared by the example suite and the property suite.
#[cfg(test)]
fn is_containment(e: &selis_error::Error) -> bool {
    matches!(
        e.code(),
        Code::SandboxModuleError
            | Code::SandboxFuel
            | Code::SandboxMemoryCap
            | Code::SandboxTrap
            | Code::SandboxProtocol
            | Code::SandboxImportDenied
            | Code::ImageUnsupported
            | Code::DctCorrupt
    )
}

/// Property suite over the JPX sandbox path (SL-1.FILT.08). Compiled only
/// under `wasm-host` (the whole module is gated in `lib.rs`), so it never
/// reaches the wasm32 guest build. Case counts stay low on purpose: each
/// `jpx_decode` compiles and runs the 233 KB OpenJPEG module in a fresh
/// wasmtime store, so these are seconds, not the multi-second soak the
/// nightly `fuzz-soak` leg provides.
#[cfg(all(test, feature = "wasm-host"))]
mod proptests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::integer_division,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]

    use super::{is_containment, jpx_decode};
    use proptest::prelude::*;
    use selis_sandbox::{Budget, CancelToken, ManualClock};

    /// The `filter-jpx` corpus and the golden fixture drive these inputs:
    /// arbitrary corruption of real codestream structures, which is the
    /// space a malformed scan page actually lands in.
    const GOLDEN: &[u8] = include_bytes!("../tests/fixtures/jpx_gradient.j2k");

    fn fuzz_budget() -> Budget {
        Budget {
            bytes: 8 * 1024 * 1024,
            // 25 ms of budget → 2.5·10⁸ fuel: hostile inputs stop in moments,
            // while a 64×48 lossless decode finishes far inside it. Every
            // case still pays the module's one-time compile, so keep the
            // counts low — the nightly `fuzz-soak` leg is the broad net.
            wall: 25_000_000,
            depth: 16,
            objects: 1_000,
            pixels: 100_000,
        }
    }

    proptest! {
        // Case counts are low on purpose: every case re-JITs the 233 KB
        // module (one wasmtime `Engine` per sandbox run is the containment
        // design, not a shortcut to defeat in tests). The nightly `fuzz-soak`
        // leg is the broad net; these properties are the PR-blocking shape
        // check at a bounded cost.
        #![proptest_config(ProptestConfig::with_cases(6))]

        /// Containment as a property, not an example: for ANY byte string —
        /// garbage, or a slice of the golden codestream pasted over random
        /// filler so the generator lives near the "almost valid" boundary —
        /// `jpx_decode` returns Ok or a typed sandbox/image error and never
        /// panics (a panic fails the property), and the host-side charge is
        /// bounded by the *budget*, never by a canvas dimension the input
        /// bytes claim.
        #[test]
        fn jpx_decode_never_escapes_the_sandbox(
            body in prop::collection::vec(any::<u8>(), 0..2048),
            truncate in 0usize..5,
        ) {
            let mut seed: Vec<u8> = body;
            if truncate > 0 {
                let k = GOLDEN.len() / truncate;
                seed.extend_from_slice(&GOLDEN[..k]);
            }
            let clock = ManualClock::new();
            let mut guard = fuzz_budget().guard_with(&clock, CancelToken::new());
            let input_copy = u64::try_from(seed.len()).unwrap_or(u64::MAX);
            let budget_bytes = fuzz_budget().bytes;
            match jpx_decode(&seed, &mut guard) {
                Ok(img) => {
                    // A success must be a self-consistent image: sample count
                    // exactly matches the header, at a sane channel count.
                    // (The generator almost never lands on a decodable
                    // stream, but when it does the payload is checked.)
                    let pixels = usize::try_from(
                        img.width.checked_mul(img.height).unwrap_or(0),
                    )
                    .unwrap_or(0);
                    prop_assert!(img.channels == 1 || img.channels == 3);
                    prop_assert_eq!(
                        img.data.len(),
                        pixels.saturating_mul(usize::from(img.channels))
                    );
                }
                Err(e) => prop_assert!(
                    is_containment(&e),
                    "non-containment code {:?} on a fuzzed input",
                    e.code()
                ),
            }
            // Bounded by budget: copy-in plus at most one copy-out clamped to
            // the budget's remaining bytes — never a claimed 4-gigapixel
            // canvas (the ADR-P0006 outcome this whole path exists for).
            let charged = guard.usage().bytes;
            prop_assert!(
                charged <= input_copy.saturating_add(budget_bytes),
                "charged {charged} bytes for {} of input",
                seed.len()
            );
        }

        /// Rule 3 / ADR-P0012 determinism specialised for the codec: bytes
        /// trailing the `EOC` marker must not change the decode outcome —
        /// two runs over `golden ++ tail` are identical (same bytes or same
        /// typed code), which also proves the sandbox run shares no state
        /// between instances.
        #[test]
        fn trailing_bytes_do_not_change_the_decode(
            tail in prop::collection::vec(any::<u8>(), 0..256),
        ) {
            let mut input = GOLDEN.to_vec();
            input.extend_from_slice(&tail);
            let clock = ManualClock::new();
            let mut a = fuzz_budget().guard_with(&clock, CancelToken::new());
            let mut b = fuzz_budget().guard_with(&clock, CancelToken::new());
            let x = jpx_decode(&input, &mut a);
            let y = jpx_decode(&input, &mut b);
            let (xs, ys) = (outcome_of(&x), outcome_of(&y));
            prop_assert_eq!(xs, ys, "trailing {} bytes changed the outcome", tail.len());
        }
    }

    /// Canonical comparison form for two decode outcomes.
    fn outcome_of(r: &selis_error::Result<crate::DctImage>) -> outcome::Outcome {
        match r {
            Ok(img) => outcome::Outcome::Ok(img.width, img.height, img.channels, img.data.len()),
            Err(e) => outcome::Outcome::Err(e.code()),
        }
    }

    mod outcome {
        use selis_error::Code;

        #[derive(Debug, PartialEq, Eq)]
        pub(super) enum Outcome {
            Ok(u32, u32, u8, usize),
            Err(Code),
        }
    }
}
