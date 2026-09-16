//! The legacy CMS *content* ciphers — TDEA-CBC and RC2-CBC (SL-1.ENC.08).
//!
//! `adbe.pkcs7.s3`-era public-key documents (roughly 2001–2009) seal the
//! 24-byte seed+permissions payload under RC4 (40- or 128-bit), 3DES-CBC or
//! RC2-CBC, keyed by whatever the RSA key transport unwraps. Adobe, PDFium,
//! qpdf, PDFBox and MuPDF still decrypt those envelopes, so SL-1.ENC.08
//! accepts them on the **read path only**: ADR-P0019 keeps every Selis *write*
//! AES, and nothing in this module is reachable from `selis_pdf_engine`'s save
//! side (design note §5 / §1.1, plan `SL-1.ENC.08`).
//!
//! ## Why vendored rather than `des`/`rc2`
//!
//! Both crates were evaluated and *not used* as dependencies, on the model of
//! §3's DER decision:
//!
//! * `rc2` 0.9 moved to `cipher` **0.5** with MSRV **1.85** — outside the
//!   pinned `cipher` 0.4 tree this crate shares with `aes`/`rsa`/`p256` (it
//!   would add a second block-cipher trait world next to `cbc_decrypt_strict`).
//!   The `cipher` 0.4-era 0.8 line is its last compatible release and the
//!   cipher saw no functional change for years before the split.
//! * `des` 0.5 sits on the older `block-cipher` 0.3 line with the same
//!   trait-tree problem, and either crate is unvetted supply-chain surface — a
//!   `cargo vet` exemption and a `cargo deny` pass per crate — for two
//!   algorithms that exist here purely to read museum formats.
//! * The shipped use is *decrypt-only* on a hard-bounded 32-byte payload (the
//!   CMS envelope wraps one 24-byte seed+permissions block; document streams
//!   for `/V ≤ 3` go through [`crate::decrypt_data`]), and the tables are
//!   published FIPS/RFC data. This is the posture already taken for
//!   [`crate::rc4`], the DER reader (§3) and the RFC 3394 unwrap.
//!
//! The permutation network, S-box tables, schedule and round arithmetic are
//! transcribed from the RustCrypto `des` 0.5 / `rc2` 0.9 implementations
//! (MIT OR Apache-2.0, so vendoring them under ADR-P0021 is licence-clean)
//! with their trait plumbing stripped; the known-answer tests below compare
//! against `openssl enc -des-ecb/-des-ede3[-cbc]/-rc2[-40]-cbc` (OpenSSL
//! 1.1.1q) output, so the transcription is checked against an *independent*
//! implementation rather than only self-consistency.
//!
//! ## Wrong-key posture
//!
//! These ciphers add no new detector and remove none. TDEA-CBC and RC2-CBC
//! are 8-byte-block CBC modes, so [`crate::pkcs7::decrypt_payload`] applies
//! the same strict PKCS#7 + exact-24-byte-payload rule as the AES path
//! (≈2⁻¹⁶ accidental pass per try, §5). RC4 has no padding at all, so there
//! the only gate is that the RSAES-PKCS1-v1_5 / RSAES-OAEP key transport
//! unwrapped *before* any content key existed. Either way a wrong CEK never
//! reaches the file key — see the [`crate::pkcs7`] module docs plus the fuzz
//! target's "certificate never authenticates" invariant.

// ── single DES ────────────────────────────────────────────────────────────
// Transcribed from RustCrypto `des` 0.5 (`src/des.rs` + `src/consts.rs`,
// MIT OR Apache-2.0): the delta-swap permutation network, PC-1/PC-2/E/P
// bit-trick masks, the FIPS 46-3 schedule and the "direct-index" six-bit
// S-box layout. Only the block-cipher trait plumbing differs from upstream.

/// Data Encryption Standard with the 16 48-bit round subkeys of PC-2.
#[derive(Clone)]
pub struct Des {
    keys: [u64; 16],
}

/// Per-round rotations of the 28-bit halves of the key schedule.
const SHIFTS: [u8; 16] = [1, 1, 2, 2, 2, 2, 2, 2, 1, 2, 2, 2, 2, 2, 2, 1];

/// FIPS 46-3 S-boxes, re-laid so the raw six expansion bits index each table
/// directly (top two bits = row {b1,b6}, low four = column b2..b5).
#[rustfmt::skip]
const SBOXES: [[u8; 64]; 8] = [
    [
        14,  0,  4, 15, 13,  7,  1,  4,  2, 14, 15,  2, 11, 13,  8,  1,
         3, 10, 10,  6,  6, 12, 12, 11,  5,  9,  9,  5,  0,  3,  7,  8,
         4, 15,  1, 12, 14,  8,  8,  2, 13,  4,  6,  9,  2,  1, 11,  7,
        15,  5, 12, 11,  9,  3,  7, 14,  3, 10, 10,  0,  5,  6,  0, 13,
    ],
    [
        15,  3,  1, 13,  8,  4, 14,  7,  6, 15, 11,  2,  3,  8,  4, 14,
         9, 12,  7,  0,  2,  1, 13, 10, 12,  6,  0,  9,  5, 11, 10,  5,
         0, 13, 14,  8,  7, 10, 11,  1, 10,  3,  4, 15, 13,  4,  1,  2,
         5, 11,  8,  6, 12,  7,  6, 12,  9,  0,  3,  5,  2, 14, 15,  9,
    ],
    [
        10, 13,  0,  7,  9,  0, 14,  9,  6,  3,  3,  4, 15,  6,  5, 10,
         1,  2, 13,  8, 12,  5,  7, 14, 11, 12,  4, 11,  2, 15,  8,  1,
        13,  1,  6, 10,  4, 13,  9,  0,  8,  6, 15,  9,  3,  8,  0,  7,
        11,  4,  1, 15,  2, 14, 12,  3,  5, 11, 10,  5, 14,  2,  7, 12,
    ],
    [
         7, 13, 13,  8, 14, 11,  3,  5,  0,  6,  6, 15,  9,  0, 10,  3,
         1,  4,  2,  7,  8,  2,  5, 12, 11,  1, 12, 10,  4, 14, 15,  9,
        10,  3,  6, 15,  9,  0,  0,  6, 12, 10, 11,  1,  7, 13, 13,  8,
        15,  9,  1,  4,  3,  5, 14, 11,  5, 12,  2,  7,  8,  2,  4, 14,
    ],
    [
         2, 14, 12, 11,  4,  2,  1, 12,  7,  4, 10,  7, 11, 13,  6,  1,
         8,  5,  5,  0,  3, 15, 15, 10, 13,  3,  0,  9, 14,  8,  9,  6,
         4, 11,  2,  8,  1, 12, 11,  7, 10,  1, 13, 14,  7,  2,  8, 13,
        15,  6,  9, 15, 12,  0,  5,  9,  6, 10,  3,  4,  0,  5, 14,  3,
    ],
    [
        12, 10,  1, 15, 10,  4, 15,  2,  9,  7,  2, 12,  6,  9,  8,  5,
         0,  6, 13,  1,  3, 13,  4, 14, 14,  0,  7, 11,  5,  3, 11,  8,
         9,  4, 14,  3, 15,  2,  5, 12,  2,  9,  8,  5, 12, 15,  3, 10,
         7, 11,  0, 14,  4,  1, 10,  7,  1,  6, 13,  0, 11,  8,  6, 13,
    ],
    [
         4, 13, 11,  0,  2, 11, 14,  7, 15,  4,  0,  9,  8,  1, 13, 10,
         3, 14, 12,  3,  9,  5,  7, 12,  5,  2, 10, 15,  6,  8,  1,  6,
         1,  6,  4, 11, 11, 13, 13,  8, 12,  1,  3,  4,  7, 10, 14,  7,
        10,  9, 15,  5,  6,  0,  8, 15,  0, 14,  5,  2,  9,  3,  2, 12,
    ],
    [
        13,  1,  2, 15,  8, 13,  4,  8,  6, 10, 15,  3, 11,  7,  1,  4,
        10, 12,  9,  5,  3,  6, 14, 11,  5,  0,  0, 14, 12,  9,  7,  2,
         7,  2, 11,  1,  4, 14,  1,  7,  9,  4, 12, 10, 14,  8,  2, 13,
         0, 15,  6, 12, 10,  9, 13,  0, 15,  3,  3,  5,  5,  6,  8, 11,
    ],
];

/// Exchange the `delta`-spaced bits selected by `mask`.
fn delta_swap(a: u64, delta: u64, mask: u64) -> u64 {
    let b = (a ^ (a >> delta)) & mask;
    a ^ b ^ (b << delta)
}

/// PC-1 as delta swaps (result: the MSB is bit zero; the eight parity bits
/// have been dropped, so the value is left-shifted by eight).
fn pc1(key: u64) -> u64 {
    let mut key = key;
    key = delta_swap(key, 2, 0x3333_0000_3333_0000);
    key = delta_swap(key, 4, 0x0f0f_0f0f_0000_0000);
    key = delta_swap(key, 8, 0x009a_000a_00a2_00a8);
    key = delta_swap(key, 16, 0x0000_6c6c_0000_cccc);
    key = delta_swap(key, 1, 0x1045_5005_0055_0550);
    key = delta_swap(key, 32, 0x0000_0000_f0f0_f5fa);
    key = delta_swap(key, 8, 0x0055_0055_006a_00aa);
    key = delta_swap(key, 2, 0x0000_3333_3000_0300);
    key & 0xFFFF_FFFF_FFFF_FF00
}

/// PC-2 as mask/rotate/multiply stages.
fn pc2(key: u64) -> u64 {
    let key = key.rotate_left(61);
    let b1 = (key & 0x0021_0000_0200_0000) >> 7;
    let b2 = (key & 0x0008_0200_1008_0000) << 1;
    let b3 = key & 0x0002_2000_0000_0000;
    let b4 = (key & 0x0000_0000_0010_0020) << 19;
    let b5 = (key.rotate_left(54) & 0x0005_3124_0000_0011).wrapping_mul(0x0000_0000_9420_0201)
        & 0xea40_1008_8000_0000;
    let b6 = (key.rotate_left(7) & 0x0022_1100_0001_2001).wrapping_mul(0x0001_0000_0061_0006)
        & 0x1185_0044_0000_0000;
    let b7 = (key.rotate_left(6) & 0x0000_5200_4020_0002).wrapping_mul(0x0000_0080_0000_00c1)
        & 0x0028_8110_0020_0000;
    let b8 =
        (key & 0x0100_0004_c001_1100).wrapping_mul(0x0000_0000_0000_4284) & 0x0400_0822_4440_0000;
    let b9 = (key.rotate_left(60) & 0x0000_0000_0082_0280).wrapping_mul(0x0000_0000_0008_9001)
        & 0x0000_0001_1088_0000;
    let b10 = (key.rotate_left(49) & 0x0000_0000_0002_4084).wrapping_mul(0x0000_0000_0204_0005)
        & 0x0000_0000_0a03_0000;
    b1 | b2 | b3 | b4 | b5 | b6 | b7 | b8 | b9 | b10
}

/// FP = IP⁻¹, as delta swaps.
fn fp(mut message: u64) -> u64 {
    message = delta_swap(message, 24, 0x0000_00FF_0000_00FF);
    message = delta_swap(message, 24, 0x0000_0000_FF00_FF00);
    message = delta_swap(message, 36, 0x0000_0000_0F0F_0F0F);
    message = delta_swap(message, 18, 0x0000_3333_0000_3333);
    delta_swap(message, 9, 0x0055_0055_0055_0055)
}

/// IP, as delta swaps.
fn ip(mut message: u64) -> u64 {
    message = delta_swap(message, 9, 0x0055_0055_0055_0055);
    message = delta_swap(message, 18, 0x0000_3333_0000_3333);
    message = delta_swap(message, 36, 0x0000_0000_0F0F_0F0F);
    message = delta_swap(message, 24, 0x0000_0000_FF00_FF00);
    delta_swap(message, 24, 0x0000_00FF_0000_00FF)
}

/// The expansion E: right half → 48 bits.
fn expand(block: u64) -> u64 {
    let b1 = (block << 31) & 0x8000_0000_0000_0000;
    let b2 = (block >> 1) & 0x7C00_0000_0000_0000;
    let b3 = (block >> 3) & 0x03F0_0000_0000_0000;
    let b4 = (block >> 5) & 0x000F_C000_0000_0000;
    let b5 = (block >> 7) & 0x0000_3F00_0000_0000;
    let b6 = (block >> 9) & 0x0000_00FC_0000_0000;
    let b7 = (block >> 11) & 0x0000_0003_F000_0000;
    let b8 = (block >> 13) & 0x0000_0000_0FC0_0000;
    let b9 = (block >> 15) & 0x0000_0000_003E_0000;
    let b10 = (block >> 47) & 0x0000_0000_0001_0000;
    b1 | b2 | b3 | b4 | b5 | b6 | b7 | b8 | b9 | b10
}

/// P inside f().
fn p_perm(block: u64) -> u64 {
    let block = block.rotate_left(44);
    let b1 = (block & 0x0000_0000_0020_0000) << 32;
    let b2 = (block & 0x0000_0000_0048_0000) << 13;
    let b3 = (block & 0x0000_0880_0000_0000) << 12;
    let b4 = (block & 0x0000_0020_2012_0000) << 25;
    let b5 = (block & 0x0000_0004_4200_0000) << 14;
    let b6 = (block & 0x0000_0000_0180_0000) << 37;
    let b7 = (block & 0x0000_0000_0400_0000) << 24;
    let b8 =
        (block & 0x0000_0202_8001_5000).wrapping_mul(0x0000_0200_8080_0083) & 0x0200_0a64_0000_0000;
    let b9 = (block.rotate_left(29) & 0x0100_1400_0000_00aa).wrapping_mul(0x0000_2102_1000_8081)
        & 0x0902_c012_0000_0000;
    let b10 =
        (block & 0x0000_0009_1004_0000).wrapping_mul(0x0000_000c_0400_0020) & 0x8410_0100_0000_0000;
    b1 | b2 | b3 | b4 | b5 | b6 | b7 | b8 | b9 | b10
}

/// The 16 PC-2 subkeys from an 8-byte DES key (low bit of each byte = parity).
fn gen_keys(key: u64) -> [u64; 16] {
    let mut keys: [u64; 16] = [0; 16];
    let key = pc1(key) >> 8;
    let mut c = key >> 28;
    let mut d = key & 0x0FFF_FFFF;
    for (i, &shift) in SHIFTS.iter().enumerate() {
        c = rot28(c, shift);
        d = rot28(d, shift);
        keys[i] = pc2(((c << 28) | d) << 8);
    }
    keys
}

/// Rotate a 28-bit half left by `shift`.
fn rot28(val: u64, shift: u8) -> u64 {
    let mut val = val;
    let top_bits = val >> (28 - u64::from(shift));
    val <<= u64::from(shift);
    (val | top_bits) & 0x0FFF_FFFF
}

/// f(R, K): E, ⊕K, the eight S-boxes, P.
fn f(input: u64, key: u64) -> u64 {
    let mut val = expand(input as u64);
    val ^= key;
    val = apply_sboxes(val);
    p_perm(val)
}

/// The S-box layer; the six-bit index reaches the table directly.
fn apply_sboxes(input: u64) -> u64 {
    let mut output = 0u64;
    for (i, sbox) in SBOXES.iter().enumerate() {
        let val = (input >> (58 - i as u64 * 6)) & 0x3F;
        let idx = usize::try_from(val).unwrap_or(0);
        output |= u64::from(sbox[idx]) << (60 - i as u64 * 4);
    }
    output
}

/// One Feistel round on a packed L||R word.
fn round(input: u64, key: u64) -> u64 {
    let l = input & (0xFFFF_FFFF << 32);
    let r = input << 32;
    r | ((f(r, key) ^ l) >> 32)
}

impl Des {
    pub(crate) fn new(key: &[u8; 8]) -> Self {
        Self {
            keys: gen_keys(u64::from_be_bytes(*key)),
        }
    }

    fn encrypt_word(&self, mut data: u64) -> u64 {
        data = ip(data);
        for key in &self.keys {
            data = round(data, *key);
        }
        fp((data << 32) | (data >> 32))
    }

    fn decrypt_word(&self, mut data: u64) -> u64 {
        data = ip(data);
        for key in self.keys.iter().rev() {
            data = round(data, *key);
        }
        fp((data << 32) | (data >> 32))
    }

    fn crypt(&self, block: &mut [u8; 8], decrypting: bool) {
        let word = u64::from_be_bytes(*block);
        *block = if decrypting {
            self.decrypt_word(word)
        } else {
            self.encrypt_word(word)
        }
        .to_be_bytes();
    }

    /// The forward direction of single DES (test + xtask fixture use).
    pub fn encrypt(&self, block: &mut [u8; 8]) {
        self.crypt(block, false);
    }
}

// ── TDEA ──────────────────────────────────────────────────────────────────
// The EDE composition of upstream's `tdes.rs`.

/// Two- and three-key TDEA (encrypt-decrypt-encrypt).
#[derive(Clone)]
pub struct Tdes {
    k1: Des,
    k2: Des,
    k3: Des,
}

impl Tdes {
    /// A 24-byte (K1,K2,K3) or 16-byte (K1,K2,K1) TDEA key; any other length
    /// is not TDEA key material (`None` → the caller treats it as a
    /// wrong-key try, never as an accept).
    pub fn new(key: &[u8]) -> Option<Self> {
        let a = <[u8; 8]>::try_from(key.get(..8)?).ok()?;
        let b = <[u8; 8]>::try_from(key.get(8..16)?).ok()?;
        let c = match key.len() {
            24 => <[u8; 8]>::try_from(key.get(16..24)?).ok()?,
            16 => a,
            _ => return None,
        };
        Some(Self {
            k1: Des::new(&a),
            k2: Des::new(&b),
            k3: Des::new(&c),
        })
    }

    pub(crate) fn encrypt_word(&self, mut data: u64) -> u64 {
        data = self.k1.encrypt_word(data);
        data = self.k2.decrypt_word(data);
        self.k3.encrypt_word(data)
    }

    fn decrypt_word(&self, mut data: u64) -> u64 {
        data = self.k3.decrypt_word(data);
        data = self.k2.encrypt_word(data);
        self.k1.decrypt_word(data)
    }

    /// TDEA decryption of one 8-byte block — the shipped direction.
    pub(crate) fn decrypt_block(&self, block: &mut [u8; 8]) {
        *block = self.decrypt_word(u64::from_be_bytes(*block)).to_be_bytes();
    }

    /// The forward direction (TDEA encrypt). Used by the `xtask` fixture
    /// generator (which stays *independent* of the reader it cross-checks —
    /// the AES-KW/CBC precedent in `pkcs7_fixtures.rs`) and by the
    /// `decrypt_payload` round-trips; the shipped save path is AES (ADR-P0019).
    pub fn encrypt_block(&self, block: &mut [u8; 8]) {
        *block = self.encrypt_word(u64::from_be_bytes(*block)).to_be_bytes();
    }
}

// ── RC2 ───────────────────────────────────────────────────────────────────
// The RFC 2268 schedule + mixing rounds of upstream's `rc2` (trait plumbing
// and the `InOut` adapter removed; the block moves through `[u8; 8]`).

/// RC2 block cipher with the 64 × 16-bit schedule.
#[derive(Clone)]
pub struct Rc2 {
    keys: [u16; 64],
}

/// RFC 2268's "digits of pi" table.
#[rustfmt::skip]
const PI_TABLE: [u8; 256] = [
    217, 120, 249, 196,  25, 221, 181, 237,  40, 233, 253, 121,  74, 160, 216, 157,
    198, 126,  55, 131,  43, 118,  83, 142,  98,  76, 100, 136,  68, 139, 251, 162,
     23, 154,  89, 245, 135, 179,  79,  19,  97,  69, 109, 141,   9, 129, 125,  50,
    189, 143,  64, 235, 134, 183, 123,  11, 240, 149,  33,  34,  92, 107,  78, 130,
     84, 214, 101, 147, 206,  96, 178,  28, 115,  86, 192,  20, 167, 140, 241, 220,
     18, 117, 202,  31,  59, 190, 228, 209,  66,  61, 212,  48, 163,  60, 182,  38,
    111, 191,  14, 218,  70, 105,   7,  87,  39, 242,  29, 155, 188, 148,  67,   3,
    248,  17, 199, 246, 144, 239,  62, 231,   6, 195, 213,  47, 200, 102,  30, 215,
      8, 232, 234, 222, 128,  82, 238, 247, 132, 170, 114, 172,  53,  77, 106,  42,
    150,  26, 210, 113,  90,  21,  73, 116,  75, 159, 208,  94,   4,  24, 164, 236,
    194, 224,  65, 110,  15,  81, 203, 204,  36, 145, 175,  80, 161, 244, 112,  57,
    153, 124,  58, 133,  35, 184, 180, 122, 252,   2,  54,  91,  37,  85, 151,  49,
     45,  93, 250, 152, 227, 138, 146, 174,   5, 223,  41,  16, 103, 108, 186, 201,
    211,   0, 230, 207, 225, 158, 168,  44,  99,  22,   1,  63,  88, 226, 137, 169,
     13,  56,  52,  27, 171,  51, 255, 176, 187,  72,  12,  95, 185, 177, 205,  46,
    197, 243, 219,  71, 229, 165, 156, 119,  10, 166,  32, 104, 254, 127, 193, 173,
];

impl Rc2 {
    /// Expand `key` for `t1_bits` effective key bits. `rc2EffectiveKeyLength`
    /// absent/`0` in the DER means the whole key; a value larger than the key
    /// material is clamped down to it (OpenSSL's CMS writer has been observed
    /// with both `0`/whole-key and larger-than-key parameters).
    pub fn new(key: &[u8], t1_bits: usize) -> Option<Self> {
        if key.is_empty() || key.len() > 128 {
            return None;
        }
        let full = key.len().checked_mul(8)?;
        let t1 = match t1_bits {
            0 => full.min(1024),
            n => n.clamp(1, 1024).min(full),
        };
        let keys = Self::expand_key(key, t1);
        Some(Self { keys })
    }

    fn expand_key(key: &[u8], t1: usize) -> [u16; 64] {
        let key_len = key.len();
        let t8 = (t1 + 7) >> 3;
        let tm = (255 % (2u32.pow((8 + t1 - 8 * t8) as u32))) as usize;
        let mut buf = [0u8; 128];
        buf[..key_len].copy_from_slice(key);
        for i in key_len..128 {
            let pos = (u32::from(buf[i - 1]) + u32::from(buf[i - key_len])) & 0xff;
            buf[i] = PI_TABLE[pos as usize];
        }
        buf[128 - t8] = PI_TABLE[usize::from(buf[128 - t8] & tm as u8)];
        for i in (0..128 - t8).rev() {
            let pos = usize::from(buf[i + 1] ^ buf[i + t8]);
            buf[i] = PI_TABLE[pos];
        }
        let mut result = [0u16; 64];
        for i in 0..64 {
            result[i] = (u16::from(buf[2 * i + 1]) << 8) + u16::from(buf[2 * i]);
        }
        result
    }

    fn words(block: &[u8; 8]) -> [u16; 4] {
        [
            u16::from_le_bytes([block[0], block[1]]),
            u16::from_le_bytes([block[2], block[3]]),
            u16::from_le_bytes([block[4], block[5]]),
            u16::from_le_bytes([block[6], block[7]]),
        ]
    }

    fn write_words(block: &mut [u8; 8], r: [u16; 4]) {
        for (i, w) in r.iter().enumerate() {
            block[2 * i..2 * i + 2].copy_from_slice(&w.to_le_bytes());
        }
    }

    fn mix(&self, r: &mut [u16; 4], j: &mut usize) {
        r[0] = r[0]
            .wrapping_add(self.keys[*j])
            .wrapping_add(r[3] & r[2])
            .wrapping_add(!r[3] & r[1]);
        *j += 1;
        r[0] = r[0].rotate_left(1);

        r[1] = r[1]
            .wrapping_add(self.keys[*j])
            .wrapping_add(r[0] & r[3])
            .wrapping_add(!r[0] & r[2]);
        *j += 1;
        r[1] = r[1].rotate_left(2);

        r[2] = r[2]
            .wrapping_add(self.keys[*j])
            .wrapping_add(r[1] & r[0])
            .wrapping_add(!r[1] & r[3]);
        *j += 1;
        r[2] = r[2].rotate_left(3);

        r[3] = r[3]
            .wrapping_add(self.keys[*j])
            .wrapping_add(r[2] & r[1])
            .wrapping_add(!r[2] & r[0]);
        *j += 1;
        r[3] = r[3].rotate_left(5);
    }

    /// The RFC 2268 mash step (encrypt direction only; see [mix]).
    fn mash(&self, r: &mut [u16; 4]) {
        r[0] = r[0].wrapping_add(self.keys[usize::from(r[3] & 63)]);
        r[1] = r[1].wrapping_add(self.keys[usize::from(r[0] & 63)]);
        r[2] = r[2].wrapping_add(self.keys[usize::from(r[1] & 63)]);
        r[3] = r[3].wrapping_add(self.keys[usize::from(r[2] & 63)]);
    }

    fn reverse_mix(&self, r: &mut [u16; 4], j: &mut usize) {
        r[3] = r[3].rotate_right(5);
        r[3] = r[3]
            .wrapping_sub(self.keys[*j])
            .wrapping_sub(r[2] & r[1])
            .wrapping_sub(!r[2] & r[0]);
        *j -= 1;

        r[2] = r[2].rotate_right(3);
        r[2] = r[2]
            .wrapping_sub(self.keys[*j])
            .wrapping_sub(r[1] & r[0])
            .wrapping_sub(!r[1] & r[3]);
        *j -= 1;

        r[1] = r[1].rotate_right(2);
        r[1] = r[1]
            .wrapping_sub(self.keys[*j])
            .wrapping_sub(r[0] & r[3])
            .wrapping_sub(!r[0] & r[2]);
        *j -= 1;

        r[0] = r[0].rotate_right(1);
        r[0] = r[0]
            .wrapping_sub(self.keys[*j])
            .wrapping_sub(r[3] & r[2])
            .wrapping_sub(!r[3] & r[1]);
        *j = j.wrapping_sub(1);
    }

    fn reverse_mash(&self, r: &mut [u16; 4]) {
        r[3] = r[3].wrapping_sub(self.keys[usize::from(r[2] & 63)]);
        r[2] = r[2].wrapping_sub(self.keys[usize::from(r[1] & 63)]);
        r[1] = r[1].wrapping_sub(self.keys[usize::from(r[0] & 63)]);
        r[0] = r[0].wrapping_sub(self.keys[usize::from(r[3] & 63)]);
    }

    /// RC2 decryption of one 8-byte block — the shipped direction.
    pub(crate) fn decrypt_block(&self, block: &mut [u8; 8]) {
        let mut b = Self::words(block);
        let mut j = 63usize;
        for i in 0..16 {
            self.reverse_mix(&mut b, &mut j);
            if i == 4 || i == 10 {
                self.reverse_mash(&mut b);
            }
        }
        Self::write_words(block, b);
    }

    /// The forward direction (RC2 encrypt; `xtask` fixtures + the
    /// `decrypt_payload` round-trips — see [`Tdes::encrypt_block`] for the
    /// ADR-P0019 note).
    pub fn encrypt_block(&self, block: &mut [u8; 8]) {
        let mut b = Self::words(block);
        let mut j = 0usize;
        for i in 0..16 {
            self.mix(&mut b, &mut j);
            if i == 4 || i == 10 {
                self.mash(&mut b);
            }
        }
        Self::write_words(block, b);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;

    const IV: [u8; 8] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77];
    /// 24 bytes — the seed+permissions payload length a CMS envelope seals.
    const PT24: &str = "0102030405060708090A0B0C0D0E0F101112131415161718";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    fn eq8(a: &mut [u8; 8], b: &[u8; 8]) {
        for i in 0..8 {
            a[i] ^= b[i];
        }
    }

    /// CBC *encryption*: C_i = op(P_i ⊕ C_{i-1}) with C_{-1} = `iv`.
    fn cbc_encrypt(op: &mut dyn FnMut(&mut [u8; 8]), iv: &[u8; 8], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut prev = *iv;
        for chunk in data.chunks(8) {
            let mut block = <[u8; 8]>::try_from(chunk).expect("block");
            eq8(&mut block, &prev);
            op(&mut block);
            out.extend_from_slice(&block);
            prev = block;
        }
        out
    }

    /// CBC *decryption*: P_i = op(C_i) ⊕ C_{i-1} with C_{-1} = `iv`. This is
    /// what `decrypt_payload` does for the 8-byte legacy modes; the
    /// encrypting helper above is *test-only* (ADR-P0019: writes never use
    /// these ciphers).
    fn cbc_decrypt(op: &mut dyn FnMut(&mut [u8; 8]), iv: &[u8; 8], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut prev = *iv;
        for chunk in data.chunks(8) {
            let mut block = <[u8; 8]>::try_from(chunk).expect("block");
            let saved = block;
            op(&mut block);
            eq8(&mut block, &prev);
            out.extend_from_slice(&block);
            prev = saved;
        }
        out
    }

    fn pkcs7_pad(data: &[u8], block: usize) -> Vec<u8> {
        let n = block - (data.len() % block);
        let mut out = data.to_vec();
        out.resize(data.len() + n, n as u8);
        out
    }

    #[test]
    fn des_known_answer_vectors() {
        // FIPS 46-3 and the `openssl enc -des-ecb -nopad` per-key set. These
        // cover a weak key, a self-dual-looking pt, a key with the MSB of a
        // schedule byte set and plain ones, so a per-key schedule slip can't
        // pass.
        for (key, pt, ct) in [
            ("133457799BBCDFF1", "0123456789ABCDEF", "85E813540F0AB405"),
            ("FEDCBA9876543210", "0123456789ABCDEF", "ED39D950FA74BCC4"),
            ("89ABCDEF01234567", "0123456789ABCDEF", "DC0BA72396290C07"),
            ("0123456789ABCDEF", "0123456789ABCDEF", "56CC09E7CFDC4CEF"),
            ("0E329232EA6D0D73", "0123456789ABCDEF", "31AA59FEB64386A6"),
            ("0000000000000000", "0123456789ABCDEF", "617B3A0CE8F07100"),
        ] {
            let d = Des::new(&<[u8; 8]>::try_from(hex(key)).expect("key"));
            let mut block = <[u8; 8]>::try_from(hex(pt)).expect("pt");
            let original = block;
            d.encrypt(&mut block);
            assert_eq!(hex(ct), block.to_vec(), "enc {key}");
            d.crypt(&mut block, true);
            assert_eq!(original.to_vec(), block.to_vec(), "dec {key}");
        }
    }

    #[test]
    fn tdea_known_answer_vectors() {
        // `openssl enc -des-ede3 -nopad` / `-des-ede3-cbc -nopad -K` repeated-
        // key checks, plus the CMS-shaped 24-byte padded CBC payload.
        let t = Tdes::new(&hex("0123456789ABCDEFFEDCBA987654321089ABCDEF01234567")).expect("tdea");
        let mut block = <[u8; 8]>::try_from(hex("0123456789ABCDEF")).expect("pt");
        let original = block;
        t.encrypt_block(&mut block);
        assert_eq!(hex("691747FD88B6D228"), block.to_vec());
        t.decrypt_block(&mut block);
        assert_eq!(original.to_vec(), block.to_vec());

        // K1 = K2 = K3 reduces TDEA to single DES.
        let k = hex("133457799BBCDFF1");
        let mut kk = k.clone();
        kk.extend_from_slice(&k);
        kk.extend_from_slice(&k);
        let t3 = Tdes::new(&kk).expect("k1k2k3");
        let mut block = <[u8; 8]>::try_from(hex("0123456789ABCDEF")).expect("pt");
        t3.encrypt_block(&mut block);
        assert_eq!(hex("85E813540F0AB405"), block.to_vec());

        // Three distinct keys (openssl -des-ede3 on 87*8) pins the *order*:
        // upstream's EDE3 is d3←d2←d1 on decrypt, so an inverted composition
        // fails here.
        let t3b = Tdes::new(&hex("0123456789ABCDEFFEDCBA98765432100F1E2D3C4B5A6978"))
            .expect("tdea-3keys");
        let mut b2 = <[u8; 8]>::try_from(hex("8787878787878787")).expect("pt");
        t3b.encrypt_block(&mut b2);
        assert_eq!(hex("45C052B4DD316494"), b2.to_vec());
        t3b.decrypt_block(&mut b2);
        assert_eq!(hex("8787878787878787"), b2.to_vec());

        // TDEA-CBC over the 24-byte payload padded to 32, IV 0011223344556677
        // (`openssl enc -des-ede3-cbc`).
        let plain = hex(PT24);
        let ct = cbc_encrypt(&mut |b| t.encrypt_block(b), &IV, &pkcs7_pad(&plain, 8));
        assert_eq!(
            hex("a258ff3d9856580d4d8c838b8ba75ec6e618f335ce8d73a8"),
            ct[..24]
        );
        assert_eq!(hex("9c2ce11600253c5f").as_slice(), &ct[24..]);
        assert_eq!(
            pkcs7_pad(&plain, 8),
            cbc_decrypt(&mut |b| t.decrypt_block(b), &IV, &ct)
        );
    }

    #[test]
    fn tdea_key_lengths() {
        assert!(Tdes::new(&hex("0123456789ABCDEFFEDCBA9876543210")).is_some());
        assert!(Tdes::new(&hex("0123456789ABCDEFFEDCBA987654321089ABCDEF01234567")).is_some());
        for bad in [
            String::new(),
            "0123456789abcdef".to_string(),
            "0123456789abcdef0123456789abcdef01".to_string(),
            "0123456789abcdef0123456789abcdef01234567".to_string(),
        ] {
            assert!(Tdes::new(&hex(bad.as_str())).is_none(), "{bad}");
        }
    }

    #[test]
    fn rc2_known_answer_vectors() {
        // `openssl enc -rc2-cbc -K 0123… -iv 0000… -nopad` on 8 bytes
        // (openssl's `rc2` EVP defaults to a 128-bit effective key).
        let t = Rc2::new(&hex("0123456789ABCDEF0011223344556677"), 1024).expect("expand");
        let mut block = <[u8; 8]>::try_from(hex("0123456789ABCDEF")).expect("pt");
        let original = block;
        t.encrypt_block(&mut block);
        assert_eq!(hex("59cc647e3231a5a9"), block.to_vec());
        t.decrypt_block(&mut block);
        assert_eq!(original.to_vec(), block.to_vec());

        // `openssl enc -rc2-40-cbc -K 0123456789` (five key bytes, effective
        // length 40) — the export-era RC2 a 2003–2005 envelope carries.
        let t40 = Rc2::new(&hex("0123456789"), 40).expect("expand 40");
        let mut block = <[u8; 8]>::try_from(hex("0123456789ABCDEF")).expect("pt");
        t40.encrypt_block(&mut block);
        assert_eq!(hex("b259d15414a9e840"), block.to_vec());
        t40.decrypt_block(&mut block);
        assert_eq!(hex("0123456789ABCDEF"), block.to_vec());

        // RC2-CBC over the CMS-shaped padded 24-byte payload,
        // `openssl enc -rc2-cbc -K 0123… -iv 0011223344556677`.
        let plain = hex(PT24);
        let ct = cbc_encrypt(&mut |b| t.encrypt_block(b), &IV, &pkcs7_pad(&plain, 8));
        assert_eq!(
            hex("b5fdcf7e2064fe1997319991f07e99aeef89df58c291ea26e2ca251ce5aef208"),
            ct
        );
        assert_eq!(
            pkcs7_pad(&plain, 8),
            cbc_decrypt(&mut |b| t.decrypt_block(b), &IV, &ct)
        );
    }

    #[test]
    fn rc2_effective_lengths_and_limits() {
        // CMS writers put what they like in `rc2EffectiveKeyLength`; every
        // legal value must expand and invert, and the DER DEFAULT 0 must mean
        // "the whole key".
        for eff in [0usize, 1, 8, 40, 58, 64, 128, 160, 704, 1024] {
            let key = hex("0123456789ABCDEF0011223344556677");
            let c = Rc2::new(&key, eff).unwrap_or_else(|| panic!("expand {eff}"));
            let mut block = <[u8; 8]>::try_from(hex("F0E1D2C3B4A59687")).expect("pt");
            let original = block;
            c.encrypt_block(&mut block);
            assert_ne!(original, block, "eff {eff} must alter data");
            c.decrypt_block(&mut block);
            assert_eq!(original, block, "eff {eff} round trip");
        }
        // A 16-byte key with the DER's larger bit count clamps to the key.
        let t = Rc2::new(&hex("0123456789ABCDEF0011223344556677"), 1024).expect("clamp 1024");
        let whole = Rc2::new(&hex("0123456789ABCDEF0011223344556677"), 0).expect("whole");
        assert_eq!(t.keys, whole.keys, "clamped == DER-DEFAULT full key");

        assert!(Rc2::new(&[], 40).is_none());
        assert!(Rc2::new(&[0u8; 129], 40).is_none());
    }

    #[test]
    fn rc2_matches_openssl_padded_cbc_payload() {
        // Same shape as the CMS content of an `adbe.pkcs7.s3` file: 24 bytes
        // padded to 32; the chain here re-checks `cbc_encrypt`/`cbc` too.
        let t = Rc2::new(&hex("0123456789ABCDEF0011223344556677"), 1024).expect("expand");
        let iv = <[u8; 8]>::try_from(hex("0011223344556677")).expect("iv");
        let plain = pkcs7_pad(&hex(PT24), 8);
        let want = hex("b5fdcf7e2064fe1997319991f07e99aeef89df58c291ea26e2ca251ce5aef208");
        let ct = cbc_encrypt(&mut |b| t.encrypt_block(b), &iv, &plain);
        assert_eq!(want, ct);
        assert_eq!(plain, cbc_decrypt(&mut |b| t.decrypt_block(b), &iv, &want));
    }
}
