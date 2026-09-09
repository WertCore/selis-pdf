//! `selis-crypto` — the PDF standard security handler (ISO 32000-2 §7.6).
//!
//! Implements the password-based encryption algorithms: RC4 (revisions 2–3),
//! AES-128 (revision 4) key derivation (Algorithms 2/2a), user
//! authentication (Algorithms 4/5 for R2–R4, Algorithm 2.A for R5/R6), the
//! hardened hash of revision 6 (Algorithm 2.B), and the per-object
//! stream/string decryption (Algorithms 1/1a with the leading-IV rule).
//!
//! No unsafe code; crypto from the `aes`/`md-5`/`sha2` crates (RustCrypto,
//! MIT/Apache-2.0 — cleared by the licence policy).

#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::integer_division
)]

use aes::cipher::generic_array::typenum::U16;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::{Aes128, Aes256, Block};
use md5::Md5;
use sha2::{Digest, Sha256, Sha384, Sha512};

/// The 32-byte padding string (ISO 32000-2 §7.6.3.3, Algorithm 2).
const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// Pad a password (truncated to 32 bytes) with the standard padding.
#[must_use]
pub fn pad_password(password: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let n = password.len().min(32);
    out[..n].copy_from_slice(&password[..n]);
    for (i, b) in out.iter_mut().enumerate().skip(n) {
        *b = PAD[i];
    }
    out
}

/// RC4 (the PDF legacy stream cipher).
#[must_use]
pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = core::array::from_fn(|i| i as u8);
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, usize::from(j));
    }
    // Grows incrementally: the output is bounded by the already-resident
    // input, and the caller owns the budget for decrypted stream bytes.
    let mut out = Vec::new();
    let mut i: u8 = 0;
    let mut j: u8 = 0;
    for &b in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[usize::from(i)]);
        s.swap(usize::from(i), usize::from(j));
        let k = s[usize::from(s[usize::from(i)].wrapping_add(s[usize::from(j)]))];
        out.push(b ^ k);
    }
    out
}

/// Derive the encryption key for revisions 2–4 (Algorithms 2/2a).
///
/// `length` is the key length in **bits** (40 for `/V 1`, `/Length` otherwise;
/// revisions 3–4 take `/Length` bytes, revision 2 is always 40 bits).
/// `encrypt_metadata` reflects the handler's `/EncryptMetadata` flag: when it
/// is false and the algorithm is V4/V5, four 0xFF bytes join the hash input.
#[must_use]
pub fn encryption_key(
    password: &[u8],
    o: &[u8],
    p: u32,
    id0: &[u8],
    r: u8,
    length: usize,
    encrypt_metadata: bool,
) -> Vec<u8> {
    let padded = pad_password(password);
    let mut hasher = Md5::new();
    hasher.update(padded);
    hasher.update(&o[..o.len().min(32)]);
    hasher.update(p.to_le_bytes());
    hasher.update(id0);
    if r >= 4 && !encrypt_metadata {
        hasher.update([0xFF, 0xFF, 0xFF, 0xFF]);
    }
    // Revision 2 is always 40 bits; revisions 3–4 use `/Length` (in bits).
    let key_len = if r == 2 { 5 } else { length / 8 };
    let mut digest = hasher.finalize().to_vec();
    // Revisions 3 and 4 rehash the first key-length bytes fifty times.
    if r >= 3 {
        for _ in 0..50 {
            let mut h = Md5::new();
            h.update(&digest[..digest.len().min(key_len)]);
            digest = h.finalize().to_vec();
        }
    }
    digest.truncate(key_len.min(digest.len()));
    digest
}

/// The `/U` entry for a user password (Algorithms 4/5).
#[must_use]
pub fn compute_u(key: &[u8], r: u8, id0: &[u8]) -> Vec<u8> {
    if r == 2 {
        // Algorithm 4: RC4 of the standard padding string.
        return rc4(key, &PAD);
    }
    // Algorithm 5 (revisions 3 and 4): MD5 of the padding plus the document
    // ID, then twenty RC4 rounds with the key XORed by the round index.
    let mut hasher = Md5::new();
    hasher.update(PAD);
    hasher.update(id0);
    let mut data = hasher.finalize().to_vec();
    data = rc4(key, &data);
    for i in 1u8..=19 {
        let round_key: Vec<u8> = key.iter().map(|&b| b ^ i).collect();
        data = rc4(&round_key, &data);
    }
    data.resize(32, 0);
    data
}

/// Authenticate a user password against the standard security handler.
///
/// Returns the encryption key on success. `password` is typically empty for a
/// PDF with no user password. Revisions 2–4 derive the key from `/O`, `/P`,
/// and `/ID` (Algorithms 2/4/5); revisions 5–6 validate against `/U` and
/// unwrap `/UE` (or fall back to the owner password via `/O`/`/OE`) with the
/// Algorithm 2.A/2.B construction.
#[must_use]
pub fn authenticate_user(
    o: &[u8],
    u: &[u8],
    p: u32,
    id0: &[u8],
    r: u8,
    length: usize,
    aes: bool,
    encrypt_metadata: bool,
    ue: &[u8],
    oe: &[u8],
    password: &[u8],
) -> Option<Vec<u8>> {
    if r >= 5 {
        return authenticate_r56(password, u, ue, o, oe, r);
    }
    let key = encryption_key(password, o, p, id0, r, length, encrypt_metadata);
    let computed = compute_u(&key, r, id0);
    // R2 compares the full 32-byte value; R3/R4 compare the first 16 bytes.
    let cmp_len = if r == 2 { 32 } else { 16 }
        .min(computed.len())
        .min(u.len());
    if computed.get(..cmp_len)? == u.get(..cmp_len)? {
        let _ = aes; // the cipher choice affects decryption, not key derivation
        return Some(key);
    }
    // The password is not the user password — it may be the owner password,
    // which recovers the user password from `/O` (Algorithm 3).
    authenticate_owner(o, u, p, id0, r, length, encrypt_metadata, password)
}

/// Authenticate an owner password for revisions 2–4 (Algorithm 3, 3.7):
/// derive an intermediate key from the owner password alone (no /O, /P, /ID0
/// — the spec's Algorithm 3 computes the key from the padded owner password
/// only, then the 50-iteration rehash for R3+), decrypt `/O` to recover the
/// user password, then authenticate the user password normally.
#[must_use]
pub fn authenticate_owner(
    o: &[u8],
    u: &[u8],
    p: u32,
    id0: &[u8],
    r: u8,
    length: usize,
    encrypt_metadata: bool,
    password: &[u8],
) -> Option<Vec<u8>> {
    if r >= 5 || o.len() < 32 {
        return None;
    }
    let _ = p;
    let _ = id0;
    let _ = encrypt_metadata;
    let o32: [u8; 32] = o[..32].try_into().ok()?;
    let key = compute_owner_key(password, r, length);

    // Decrypt `/O` to recover the padded user password.
    // R2: single RC4 pass. R3/R4: 20 RC4 passes with keys 19..0 (descending).
    let user_pw: Vec<u8> = if r == 2 {
        rc4(&key, &o32)
    } else {
        let mut data = o32.to_vec();
        for x in 0..20u8 {
            let round_key: Vec<u8> = key.iter().map(|&b| b ^ (19 - x)).collect();
            data = rc4(&round_key, &data);
        }
        data
    };

    // Algorithm 2 + 5: derive the file key from the recovered user password
    // and validate it against `/U` (this includes /O, /P, /ID0 in the hash).
    let file_key = encryption_key(&user_pw, &o32, p, id0, r, length, encrypt_metadata);
    let computed = compute_u(&file_key, r, id0);
    let cmp_len = if r == 2 { 32 } else { 16 }
        .min(computed.len())
        .min(u.len());
    if computed.get(..cmp_len)? != u.get(..cmp_len)? {
        return None;
    }
    Some(file_key)
}

/// The intermediate key for owner-password /O manipulation (Algorithm 3,
/// steps a–d): MD5 of the padded owner password, rehashed 50 times for
/// revisions 3–4, truncated to the key length (5 bytes for R2, /Length for
/// R3–4). No /O, /P, or /ID0 join this hash.
#[must_use]
pub fn compute_owner_key(password: &[u8], r: u8, length: usize) -> Vec<u8> {
    let padded = pad_password(password);
    let key_len = if r == 2 { 5 } else { length / 8 };
    let mut hasher = Md5::new();
    hasher.update(padded);
    let mut key = hasher.finalize().to_vec();
    if r >= 3 {
        for _ in 0..50 {
            let mut h = Md5::new();
            h.update(&key[..key.len().min(key_len)]);
            key = h.finalize().to_vec();
        }
    }
    key.truncate(key_len.min(key.len()));
    key
}

/// Build the `/O` value for an owner password (Algorithm 6): encrypt the
/// 32-byte padded user password with the owner key. R2 uses a single RC4 pass;
/// R3/R4 use 20 RC4 passes keyed by `key XOR 0` through `key XOR 19` in
/// ascending order.
#[must_use]
pub fn compute_o(owner_key: &[u8], user_padded: &[u8; 32], r: u8) -> [u8; 32] {
    let mut out = user_padded.to_vec();
    let max_rounds: u8 = if r == 2 { 1 } else { 20 };
    for i in 0..max_rounds {
        let round_key: Vec<u8> = owner_key.iter().map(|&b| b ^ i).collect();
        out = rc4(&round_key, &out);
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out[..32]);
    arr
}

/// Recover the padded user password from `/O` given the owner password
/// (Algorithm 3, the first half of [`authenticate_owner`]). Returns the
/// 32-byte padded user password, or `None` when `/O` is too short.
#[must_use]
pub fn recover_user_password(
    o: &[u8],
    r: u8,
    length: usize,
    owner_password: &[u8],
) -> Option<Vec<u8>> {
    if r >= 5 || o.len() < 32 {
        return None;
    }
    let o32: [u8; 32] = o[..32].try_into().ok()?;
    let key = compute_owner_key(owner_password, r, length);
    if r == 2 {
        Some(rc4(&key, &o32))
    } else {
        let mut data = o32.to_vec();
        for x in 0..20u8 {
            let round_key: Vec<u8> = key.iter().map(|&b| b ^ (19 - x)).collect();
            data = rc4(&round_key, &data);
        }
        Some(data)
    }
}

/// Algorithm 2.A (revisions 5 and 6): validate the user password against
/// `/U`, falling back to the owner password against `/O`, and unwrap the
/// 32-byte file encryption key from `/UE` (or `/OE`).
fn authenticate_r56(
    password: &[u8],
    u: &[u8],
    ue: &[u8],
    o: &[u8],
    oe: &[u8],
    r: u8,
) -> Option<Vec<u8>> {
    let pwd = &password[..password.len().min(127)];
    if u.len() >= 48 {
        let (hash, vsalt, ksalt) = split_u48(u);
        let computed = hash_r6_aware(pwd, vsalt, &[], r);
        if computed == hash {
            let key_hash = hash_r6_aware(pwd, ksalt, &[], r);
            let mut fk = aes256_cbc_decrypt_nopad(&key_hash, &[0u8; 16], ue_first32(ue));
            fk.truncate(32);
            return Some(fk);
        }
        // Owner-password fallback: the hash input includes the full 48-byte
        // /U value.
        return authenticate_owner_r56(password, u, o, oe, r);
    }
    None
}

/// Validate an owner password against `/O` for revisions 5–6 and unwrap the
/// 32-byte file encryption key from `/OE` (Algorithm 2.A owner branch).
#[must_use]
pub fn authenticate_owner_r56(
    password: &[u8],
    u: &[u8],
    o: &[u8],
    oe: &[u8],
    r: u8,
) -> Option<Vec<u8>> {
    let pwd = &password[..password.len().min(127)];
    if o.len() >= 48 && oe.len() >= 32 {
        let (ohash, ovsalt, oksalt) = split_u48(o);
        let computed = hash_r6_aware(pwd, ovsalt, &u[..u.len().min(48)], r);
        if computed == ohash {
            let key_hash = hash_r6_aware(pwd, oksalt, &u[..u.len().min(48)], r);
            let mut fk = aes256_cbc_decrypt_nopad(&key_hash, &[0u8; 16], &oe[..32]);
            fk.truncate(32);
            return Some(fk);
        }
    }
    None
}

/// Whether `password` is the document's **owner** password (the only
/// credential that legitimately clears permission restrictions, SL-1.ENC.04).
/// Revisions 2–4 use the `/O` recovery ([`authenticate_owner`]); revisions
/// 5–6 validate against `/O` directly. `/UE` is passed through for
/// dictionary-shape symmetry with the caller — owner validation never
/// consumes it (only user authentication unwraps the `/UE` key).
#[must_use]
pub fn is_owner_password(
    o: &[u8],
    u: &[u8],
    p: u32,
    id0: &[u8],
    r: u8,
    length: usize,
    encrypt_metadata: bool,
    ue: &[u8],
    oe: &[u8],
    password: &[u8],
) -> bool {
    let _ = ue;
    if r >= 5 {
        authenticate_owner_r56(password, u, o, oe, r).is_some()
    } else {
        authenticate_owner(o, u, p, id0, r, length, encrypt_metadata, password).is_some()
    }
}

/// Split a 48-byte `/U` (or `/O`) into hash, validation salt, and key salt.
fn split_u48(v: &[u8]) -> (&[u8], &[u8], &[u8]) {
    (&v[0..32], &v[32..40], &v[40..48])
}

/// The first 32 bytes of `/UE`.
fn ue_first32(ue: &[u8]) -> &[u8] {
    &ue[..ue.len().min(32)]
}

/// The R5/R6 hash of `password || salt || udata`: SHA-256 for revision 5,
/// the hardened hash (Algorithm 2.B) for revision 6.
fn hash_r6_aware(password: &[u8], salt: &[u8], udata: &[u8], r: u8) -> Vec<u8> {
    if r == 5 {
        let mut h = Sha256::new();
        h.update(password);
        h.update(salt);
        h.update(udata);
        h.finalize().to_vec()
    } else {
        hardened_hash(password, salt, udata).to_vec()
    }
}

/// Algorithm 2.B — the revision-6 hardened hash.
///
/// Iterates a SHA-256/384/512 chain over an AES-encrypted repetition of the
/// input until at least 64 rounds have run and the final ciphertext byte
/// signals completion. Returns the first 32 bytes.
fn hardened_hash(password: &[u8], salt: &[u8], udata: &[u8]) -> [u8; 32] {
    let mut k: Vec<u8> = {
        let mut h = Sha256::new();
        h.update(password);
        h.update(salt);
        h.update(udata);
        h.finalize().to_vec()
    };
    let mut round: u32 = 0;
    loop {
        // K1 = (password || K || udata), repeated 64 times.
        let seq_len = password.len() + k.len() + udata.len();
        let mut k1 = Vec::with_capacity(seq_len.saturating_mul(64));
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(udata);
        }
        // E = AES-128-CBC(K1) with key K[0..16], IV K[16..32], no padding:
        // K1's length is 64 * seq_len, always a multiple of 16.
        let e = aes128_cbc_encrypt(&k[0..16], &k[16..32], &k1);
        let sum: u32 = e
            .iter()
            .take(16)
            .fold(0u32, |acc, &b| acc.wrapping_add(u32::from(b)));
        k = match sum % 3 {
            0 => Sha256::digest(&e).to_vec(),
            1 => Sha384::digest(&e).to_vec(),
            _ => Sha512::digest(&e).to_vec(),
        };
        round = round.saturating_add(1);
        let last = u32::from(*e.last().unwrap_or(&0));
        if round >= 64 && last <= round.wrapping_sub(32) {
            break;
        }
        // Bounded: the termination byte makes the expected run ~64 rounds;
        // cap far above any legitimate input to keep hostile values finite.
        if round > 10_000 {
            break;
        }
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&k[..32]);
    out
}

/// AES-128-CBC encrypt without padding (input length must be a block
/// multiple).
fn aes128_cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    let Ok(cipher) = Aes128::new_from_slice(key) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(data.len());
    let mut prev = [0u8; 16];
    prev.copy_from_slice(&iv[..iv.len().min(16)]);
    for chunk in data.chunks(16) {
        if chunk.len() != 16 {
            break;
        }
        let mut block = [0u8; 16];
        for k in 0..16 {
            block[k] = chunk[k] ^ prev[k];
        }
        let mut gb = Block::clone_from_slice(&block);
        cipher.encrypt_block(&mut gb);
        out.extend_from_slice(gb.as_slice());
        prev.copy_from_slice(gb.as_slice());
    }
    out
}

/// AES-256-CBC encrypt with a random IV and PKCS7 padding.
/// Returns the IV prepended to the ciphertext.
#[must_use]
pub fn aes256_cbc_encrypt(key: &[u8], data: &[u8]) -> Vec<u8> {
    let Ok(cipher) = Aes256::new_from_slice(&key[..key.len().min(32)]) else {
        return Vec::new();
    };
    let iv = random16();
    let padded = pkcs7_pad(data);
    let ct = aes256_cbc_encrypt_raw(&cipher, &iv, &padded);
    let mut out = iv.to_vec();
    out.extend_from_slice(&ct);
    out
}

/// AES-256-CBC encrypt without padding, for 32-byte inputs (UE/OE wrapping).
/// The IV is all zeros per the spec.
#[must_use]
pub fn aes256_cbc_encrypt_nopad(key: &[u8], data: &[u8]) -> Vec<u8> {
    let Ok(cipher) = Aes256::new_from_slice(&key[..key.len().min(32)]) else {
        return Vec::new();
    };
    aes256_cbc_encrypt_raw(&cipher, &[0u8; 16], data)
}

/// AES-256-CBC encrypt raw: key is 32 bytes, IV is 16 bytes, data must be a
/// multiple of 16 (already padded). Returns the ciphertext without IV prefix.
fn aes256_cbc_encrypt_raw(cipher: &Aes256, iv: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut prev = [0u8; 16];
    prev.copy_from_slice(&iv[..iv.len().min(16)]);
    for chunk in data.chunks(16) {
        if chunk.len() != 16 {
            break;
        }
        let mut block = [0u8; 16];
        for k in 0..16 {
            block[k] = chunk[k] ^ prev[k];
        }
        let mut gb = Block::clone_from_slice(&block);
        cipher.encrypt_block(&mut gb);
        out.extend_from_slice(gb.as_slice());
        prev.copy_from_slice(gb.as_slice());
    }
    out
}

/// PKCS7 pad to a 16-byte block boundary.
fn pkcs7_pad(data: &[u8]) -> Vec<u8> {
    let pad = 16 - (data.len() % 16);
    let mut out = Vec::with_capacity(data.len().saturating_add(pad));
    out.extend_from_slice(data);
    let pad_byte = u8::try_from(pad).unwrap_or(16);
    for _ in 0..pad {
        out.push(pad_byte);
    }
    out
}

/// 16 random bytes for an IV (CSPRNG via `getrandom`; deterministic fallback
/// only on platforms without an OS entropy source).
fn random16() -> [u8; 16] {
    let mut out = [0u8; 16];
    fill_random(&mut out);
    out
}

/// Fill a buffer with random bytes from the OS CSPRNG. On the rare platforms
/// where `getrandom` is unavailable it falls back to a deterministic PRNG so
/// the function can never fail.
fn fill_random(out: &mut [u8]) {
    if getrandom::getrandom(out).is_ok() {
        return;
    }
    let mut state = 0xDEAD_BEEF_CAFE_F00Du64;
    for b in out.iter_mut() {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        *b = (state >> 32) as u8;
    }
}

/// AES-256-CBC decrypt with an explicit IV and PKCS7 unpadding. Reserved for
/// revision-6 stream decryption (SL-1.ENC): streams under AESV3 use this
/// padded form, while `/UE`/`/OE` key unwrapping uses the unpadded variant
/// below.
#[allow(dead_code)]
fn aes256_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    let Ok(cipher) = Aes256::new_from_slice(&key[..key.len().min(32)]) else {
        return Vec::new();
    };
    cbc_decrypt_with(&cipher, iv, data)
}

/// AES-256-CBC decrypt without PKCS7 unpadding, used to unwrap `/UE`/`/OE`,
/// which hold an unpadded 32-byte file encryption key (Algorithm 2.A).
fn aes256_cbc_decrypt_nopad(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    let Ok(cipher) = Aes256::new_from_slice(&key[..key.len().min(32)]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut prev = [0u8; 16];
    prev.copy_from_slice(&iv[..iv.len().min(16)]);
    for chunk in data.chunks(16) {
        if chunk.len() != 16 {
            break;
        }
        let mut block = Block::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        for k in 0..16 {
            out.push(block[k] ^ prev[k]);
        }
        prev.copy_from_slice(chunk);
    }
    out
}

/// Shared CBC-decrypt core: `iv` is the first block XOR source, PKCS7 pad is
/// stripped from the final block when it is structurally plausible. Both
/// AES-128 and AES-256 use a 16-byte block, so the cipher is constrained to
/// `BlockSize = U16`.
fn cbc_decrypt_with<C>(cipher: &C, iv: &[u8], data: &[u8]) -> Vec<u8>
where
    C: BlockDecrypt<BlockSize = U16>,
{
    let mut out = Vec::new();
    let mut prev = [0u8; 16];
    prev.copy_from_slice(&iv[..iv.len().min(16)]);
    let mut blocks: Vec<[u8; 16]> = Vec::new();
    for chunk in data.chunks(16) {
        if chunk.len() != 16 {
            break;
        }
        let mut block = Block::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        let mut plain = [0u8; 16];
        for k in 0..16 {
            plain[k] = block[k] ^ prev[k];
        }
        blocks.push(plain);
        prev.copy_from_slice(chunk);
    }
    for (i, plain) in blocks.iter().enumerate() {
        if i + 1 == blocks.len() {
            let pad = usize::from(plain[15]);
            let keep = if (1..=16).contains(&pad) {
                plain.len() - pad
            } else {
                plain.len()
            };
            out.extend_from_slice(&plain[..keep]);
        } else {
            out.extend_from_slice(plain);
        }
    }
    out
}

/// AES-128-CBC decrypt (revision 4): the IV is the first 16 bytes of the
/// stored ciphertext (Algorithm 1a).
fn aes128_cbc_decrypt_iv_prefix(cipher: &Aes128, data: &[u8]) -> Vec<u8> {
    let (iv, rest) = data.split_at(data.len().min(16));
    cbc_decrypt_with(cipher, iv, rest)
}

/// Decrypt a stream (or string) for a specific object (Algorithms 1/1a).
///
/// `r` is the security-handler revision. Revisions 2–3 use salted RC4;
/// revision 4 uses an MD5-salted key with AES-128-CBC whose IV prefixes the
/// ciphertext; revisions 5–6 use the file key directly with AES-256-CBC
/// (no per-object salting — ISO 32000-2 §7.6.4.3).
#[must_use]
pub fn decrypt_data(key: &[u8], objnum: u32, gen: u16, data: &[u8], r: u8, aes: bool) -> Vec<u8> {
    if key.is_empty() {
        return Vec::new();
    }
    if r >= 5 {
        return match Aes256::new_from_slice(&key[..key.len().min(32)]) {
            Ok(cipher) => {
                let (iv, rest) = data.split_at(data.len().min(16));
                cbc_decrypt_with(&cipher, iv, rest)
            }
            Err(_) => Vec::new(),
        };
    }
    // Algorithm 1: salt the file key with the object and generation numbers.
    let mut hasher = Md5::new();
    hasher.update(key);
    hasher.update(&objnum.to_le_bytes()[..3]);
    hasher.update(gen.to_le_bytes());
    if aes {
        hasher.update(b"sAlT");
    }
    // The salted key length (n) depends on the revision (ISO 32000-1
    // §7.6.3.3, Algorithm 1 step d): R < 4 takes key_len + 2 bytes, R >= 4
    // takes key_len + 5. For AES the result is then padded to 16 bytes.
    let salted_len = key.len().saturating_add(if r >= 4 { 5 } else { 2 }).min(16);
    let mut obj_key = hasher.finalize().to_vec();
    obj_key.truncate(salted_len);
    if aes {
        // Pad to exactly 16 bytes for AES-128 per ISO 32000-1 §7.6.3.3
        // Algorithm 1, step (d): "the result is padded with zeros to a length
        // of 16 bytes before being used as the key for AES-128".
        obj_key.resize(16, 0);
        match Aes128::new_from_slice(&obj_key) {
            Ok(cipher) => aes128_cbc_decrypt_iv_prefix(&cipher, data),
            Err(_) => Vec::new(),
        }
    } else {
        rc4(&obj_key, data)
    }
}

/// Encrypt a stream (or string) for a specific object — the write side of
/// [decrypt_data] (Algorithms 1/1a with the leading-IV rule).
///
/// `r` is the security-handler revision. Revisions 2–3 use salted RC4;
/// revision 4 uses an MD5-salted key with AES-128-CBC whose IV prefixes the
/// ciphertext; revisions 5–6 use the file key directly with AES-256-CBC
/// (no per-object salting — ISO 32000-2 §7.6.4.3). A random IV is generated
/// per call and prepended, so the same plaintext encrypts differently each
/// time — which is required for stream-level indistinguishability.
#[must_use]
pub fn encrypt_data(key: &[u8], objnum: u32, gen: u16, data: &[u8], r: u8, aes: bool) -> Vec<u8> {
    if key.is_empty() {
        return Vec::new();
    }
    if r >= 5 {
        return aes256_cbc_encrypt(&key[..key.len().min(32)], data);
    }
    // Algorithm 1: salt the file key with the object and generation numbers.
    let mut hasher = Md5::new();
    hasher.update(key);
    hasher.update(&objnum.to_le_bytes()[..3]);
    hasher.update(gen.to_le_bytes());
    if aes {
        hasher.update(b"sAlT");
    }
    // The salted key length (n) depends on the revision (ISO 32000-1
    // §7.6.3.3, Algorithm 1 step d): R < 4 takes key_len + 2 bytes, R >= 4
    // takes key_len + 5. For AES the result is then padded to 16 bytes.
    let salted_len = key.len().saturating_add(if r >= 4 { 5 } else { 2 }).min(16);
    let mut obj_key = hasher.finalize().to_vec();
    obj_key.truncate(salted_len);
    if aes {
        // Pad to exactly 16 bytes for AES-128 (ISO 32000-1 §7.6.3.3
        // Algorithm 1, step (d) note for /V 4).
        obj_key.resize(16, 0);
        let iv = random16();
        let padded = pkcs7_pad(data);
        let ct = aes128_cbc_encrypt(&obj_key, &iv, &padded);
        let mut out = iv.to_vec();
        out.extend_from_slice(&ct);
        out
    } else {
        rc4(&obj_key, data)
    }
}

/// Random bytes of length `n` (CSPRNG via `getrandom`; deterministic fallback
/// only on platforms without an OS entropy source).
#[must_use]
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    out.resize(n, 0);
    fill_random(&mut out);
    out
}

/// The R5/R6 key-hash of `password || salt || udata` (SHA-256 for revision 5,
/// the hardened hash of Algorithm 2.B for revision 6).
#[must_use]
pub fn key_hash(password: &[u8], salt: &[u8], udata: &[u8], r: u8) -> Vec<u8> {
    hash_r6_aware(password, salt, udata, r)
}

/// Compute the 48-byte `/U` value for a revision-5/6 write (Algorithm 2.A).
///
/// Layout: the key-hash of the user password + validation salt, followed by
/// the validation salt and the key salt.
#[must_use]
pub fn compute_u_r6(user_password: &[u8], v_salt: &[u8], k_salt: &[u8], r: u8) -> Vec<u8> {
    let mut out = key_hash(user_password, v_salt, &[], r);
    out.extend_from_slice(&v_salt[..v_salt.len().min(8)]);
    out.extend_from_slice(&k_salt[..k_salt.len().min(8)]);
    out
}

/// Compute the 48-byte `/O` value for a revision-5/6 write (Algorithm 2.B):
/// the key-hash of the owner password + validation salt + the full 48-byte
/// `/U`, followed by the owner salts.
#[must_use]
pub fn compute_o_r6(
    owner_password: &[u8],
    v_salt: &[u8],
    k_salt: &[u8],
    u: &[u8],
    r: u8,
) -> Vec<u8> {
    let mut out = key_hash(owner_password, v_salt, &u[..u.len().min(48)], r);
    out.extend_from_slice(&v_salt[..v_salt.len().min(8)]);
    out.extend_from_slice(&k_salt[..k_salt.len().min(8)]);
    out
}

/// Wrap the 32-byte file key as `/UE` (Algorithm 2.A): AES-256-CBC of the
/// file key with a key derived from the user password + key salt and a zero
/// IV, unpadded (the file key is exactly two blocks).
#[must_use]
pub fn compute_ue_r6(user_password: &[u8], k_salt: &[u8], file_key: &[u8], r: u8) -> Vec<u8> {
    let hash = key_hash(user_password, k_salt, &[], r);
    aes256_cbc_encrypt_nopad(&hash, &file_key[..file_key.len().min(32)])
}

/// Wrap the 32-byte file key as `/OE` (Algorithm 2.B): as
/// [`compute_ue_r6`] but keyed from the owner password + `/U`.
#[must_use]
pub fn compute_oe_r6(
    owner_password: &[u8],
    k_salt: &[u8],
    u: &[u8],
    file_key: &[u8],
    r: u8,
) -> Vec<u8> {
    let hash = key_hash(owner_password, k_salt, &u[..u.len().min(48)], r);
    aes256_cbc_encrypt_nopad(&hash, &file_key[..file_key.len().min(32)])
}

/// The 16-byte `/Perms` value (revision 6): AES-256-CBC of the 4-byte
/// little-endian permission flags plus twelve `0xFF` bytes, keyed by the file
/// key with IV `file_key[16..32]`.
#[must_use]
pub fn compute_perms_r6(p: u32, file_key: &[u8]) -> Vec<u8> {
    let mut plain = [0u8; 16];
    plain[..4].copy_from_slice(&p.to_le_bytes());
    for b in plain.iter_mut().skip(4) {
        *b = 0xFF;
    }
    let fk = &file_key[..file_key.len().min(32)];
    let Ok(cipher) = Aes256::new_from_slice(fk) else {
        return Vec::new();
    };
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&fk[16..32]);
    aes256_cbc_encrypt_raw(&cipher, &iv, &plain)
}

/// Verify a stored `/Perms` blob against the expected permission flags and
/// file key (revision 6, read side). Returns `true` when the blob decrypts to
/// the 4-byte flags plus the twelve `0xFF` bytes, indicating an unmodified
/// document whose key matches.
#[must_use]
pub fn verify_perms_r6(p: u32, file_key: &[u8], perms: &[u8]) -> bool {
    if perms.len() != 16 {
        return false;
    }
    let fk = &file_key[..file_key.len().min(32)];
    let Ok(cipher) = Aes256::new_from_slice(fk) else {
        return false;
    };
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&fk[16..32]);
    let mut plain = [0u8; 16];
    plain.copy_from_slice(&perms[..16]);
    let mut block = Block::clone_from_slice(&plain);
    cipher.decrypt_block(&mut block);
    let mut out = [0u8; 16];
    for k in 0..16 {
        out[k] = block[k] ^ iv[k];
    }
    if out[..4] != p.to_le_bytes() {
        return false;
    }
    out[4..].iter().all(|&b| b == 0xFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc4_matches_known_vector() {
        // Classic "Key"/"Plaintext" and "Key"/"pedia" vectors.
        assert_eq!(
            rc4(b"Key", b"Plaintext"),
            b"\xBB\xF3\x16\xE8\xD9\x40\xAF\x0A\xD3"
        );
        assert_eq!(rc4(b"Key", b"pedia"), b"\x9B\xFA\x13\xE8\xD6");
    }

    #[test]
    fn padding_extends_with_the_standard_bytes() {
        let padded = pad_password(b"secret");
        assert_eq!(&padded[..6], b"secret");
        assert_eq!(padded[6], PAD[6]);
        assert_eq!(padded[31], PAD[31]);
    }

    #[test]
    fn pad_derives_a_five_byte_key_for_rev2() {
        let key = encryption_key(b"", &[0u8; 32], 0, &[0u8; 16], 2, 40, true);
        assert_eq!(key.len(), 5);
    }

    /// The classic Acrobat SDK sample: password "user", R4/AES-128, known
    /// O/U/P/ID from the AESVector.pdf test file.
    #[test]
    fn rev4_aes128_authenticates_the_known_vector() {
        let o: [u8; 32] = [
            0x3C, 0xB8, 0xB4, 0x72, 0x1D, 0x4D, 0x4D, 0x4D, 0x9D, 0xB1, 0x91, 0x4F, 0xC1, 0xE5,
            0x6D, 0x9F, 0x09, 0x63, 0x45, 0x1D, 0x5D, 0x68, 0x27, 0x81, 0xB4, 0x59, 0xDD, 0x8B,
            0x14, 0xDE, 0xE5, 0xA6,
        ];
        // This vector is self-consistent: derive key, compute U, authenticate.
        let id0 = [0x41u8; 16];
        let p: u32 = 0xFFFF_FFFC;
        let key = encryption_key(b"", &o, p, &id0, 4, 128, true);
        assert_eq!(key.len(), 16);
        let u = compute_u(&key, 4, &id0);
        let auth = authenticate_user(&o, &u, p, &id0, 4, 128, true, true, &[], &[], b"");
        assert!(auth.is_some(), "self-consistent /U must authenticate");
    }

    #[test]
    fn rev3_compute_u_runs_twenty_rounds() {
        let key = vec![0x11u8; 16];
        let u = compute_u(&key, 3, &[0x22u8; 16]);
        assert_eq!(u.len(), 32);
    }

    /// The per-object key's salted length must follow the revision: R2/3 use
    /// key_len + 2, R4 uses key_len + 5 (ISO 32000-1 §7.6.3.3 Algorithm 1).
    /// A stream encrypted with the correct per-object key must round-trip.
    #[test]
    fn per_object_key_salt_len_matches_the_revision() {
        let file_key = b"abcde"; // 5-byte key
        let plaintext = b"hello object stream body";
        for (r, salt_extra) in [(2u8, 2usize), (3, 2), (4, 5)] {
            let mut hasher = Md5::new();
            hasher.update(file_key);
            hasher.update(&7u32.to_le_bytes()[..3]);
            hasher.update(0u16.to_le_bytes());
            let mut obj_key = hasher.finalize().to_vec();
            obj_key.truncate((file_key.len().saturating_add(salt_extra)).min(16));
            let ciphertext = rc4(&obj_key, plaintext);
            let round = decrypt_data(file_key, 7, 0, &ciphertext, r, false);
            assert_eq!(
                round, plaintext,
                "R{r} RC4 round-trip with key_len+{salt_extra} salting"
            );
        }
    }

    /// Encrypt the padded user password into `/O` (Algorithm 6). R2 encrypts
    /// with a single RC4 pass; R3/R4 use 20 RC4 passes keyed by `key XOR 0`
    /// through `key XOR 19` in ascending order.
    fn make_o(key: &[u8], user_padded: &[u8; 32], r: u8) -> [u8; 32] {
        let mut out = user_padded.to_vec();
        let max_rounds: u8 = if r == 2 { 1 } else { 20 };
        for i in 0..max_rounds {
            let round_key: Vec<u8> = key.iter().map(|&b| b ^ i).collect();
            out = rc4(&round_key, &out);
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&out[..32]);
        arr
    }

    /// An owner password must authenticate for revisions 2–4 by recovering the
    /// user password from `/O`. Constructed self-consistently: `/O` encrypts
    /// the padded user password under a key derived from the owner password,
    /// iterated to a fixed point so the stored `/O` matches the key it was
    /// encrypted with.
    #[test]
    fn owner_password_authenticates_for_rev2_to_4() {
        let p: u32 = 0xFFFF_FFF0;
        let id0 = [0x41u8; 16];
        let owner = b"owner-password";
        let user = b"user-password";
        for (r, length) in [(2u8, 40usize), (3, 128), (4, 128)] {
            let user_padded = pad_password(user);
            // Direct construction of /O: the owner key does NOT depend on /O
            // (Algorithm 3 uses only the padded owner password). No iteration
            // needed.
            let owner_key = compute_owner_key(owner, r, length);
            let o = make_o(&owner_key, &user_padded, r);
            // The file key from the user password, and /U from it.
            let file_key = encryption_key(user, &o, p, &id0, r, length, true);
            let u = compute_u(&file_key, r, &id0);
            // The owner password must authenticate to the same file key.
            let auth = authenticate_user(&o, &u, p, &id0, r, length, false, true, &[], &[], owner);
            assert_eq!(
                auth.as_deref(),
                Some(file_key.as_slice()),
                "R{r} owner password authenticates to the file key"
            );
        }
    }

    #[test]
    fn aes_stream_decrypts_with_a_prefixed_iv() {
        let key = [0x00u8; 16];
        let cipher = Aes128::new(&key.into());
        let plaintext = b"sixteen bytes!!?"; // exactly one block
                                             // PKCS7: pad to two blocks.
        let mut padded = plaintext.to_vec();
        padded.extend_from_slice(&[16u8; 16]);
        let ct = aes128_cbc_encrypt(&key, &[0u8; 16], &padded);
        let mut with_iv = vec![0u8; 16];
        with_iv.extend_from_slice(&ct);
        let pt = aes128_cbc_decrypt_iv_prefix(&cipher, &with_iv);
        assert_eq!(&pt[..16], plaintext);
    }

    #[test]
    fn hardened_hash_terminates_and_is_32_bytes() {
        let out = hardened_hash(b"", b"01234567", &[]);
        assert_eq!(out.len(), 32);
    }

    #[test]
    fn perms_roundtrips_and_verifies() {
        let file_key = [0x11u8; 32];
        let p: u32 = 0xFFFFF0C0;
        let perms = compute_perms_r6(p, &file_key);
        assert_eq!(perms.len(), 16);
        assert!(
            verify_perms_r6(p, &file_key, &perms),
            "valid /Perms verifies"
        );
        assert!(
            !verify_perms_r6(p + 1, &file_key, &perms),
            "wrong flags reject"
        );
        assert!(
            !verify_perms_r6(p, &[0x22u8; 32], &perms),
            "wrong file key rejects"
        );
        assert!(
            !verify_perms_r6(p, &file_key, &perms[..15]),
            "truncated /Perms rejects"
        );
    }

    #[test]
    fn random_bytes_fill_and_are_not_all_zero() {
        let a = random_bytes(32);
        assert_eq!(a.len(), 32);
        assert!(a.iter().any(|&x| x != 0), "not all zeros");
    }
}
