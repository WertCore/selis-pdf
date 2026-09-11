//! Tests for the PKCS#7/CMS read path (SL-1.ENC.03).
//!
//! The DER *writer* helpers here exist only to build test fixtures — the
//! shipped code is decrypt-only (ADR-P0019). RSA unit tests use runtime
//! 512-bit keys (testing our code, not RSA's strength); the committed
//! engine-level fixtures use fixed 2048-bit keys (see the fixture generator
//! in `selis-pdf-engine/tests/pubkey_open.rs`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use super::*;
use selis_sandbox::{Budget, Surface};

// ---------------------------------------------------------------------------
// Test-side DER writer (fixtures only)
// ---------------------------------------------------------------------------

/// Encode a TLV with DER length octets.
fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = content.len();
    if n < 0x80 {
        out.push(n as u8);
    } else if n <= 0xFF {
        out.extend_from_slice(&[0x81, n as u8]);
    } else if n <= 0xFFFF {
        out.extend_from_slice(&[0x82, (n >> 8) as u8, (n & 0xFF) as u8]);
    } else {
        out.extend_from_slice(&[
            0x83,
            (n >> 16) as u8,
            ((n >> 8) & 0xFF) as u8,
            (n & 0xFF) as u8,
        ]);
    }
    out.extend_from_slice(content);
    out
}

/// Encode an OID from its arcs (base-128, most significant group first).
fn oid_bytes(arcs: &[u64]) -> Vec<u8> {
    let mut body = vec![(40 * arcs[0] + arcs[1]) as u8];
    for &arc in &arcs[2..] {
        let mut groups = Vec::new();
        let mut v = arc;
        loop {
            groups.push(v & 0x7F);
            v >>= 7;
            if v == 0 {
                break;
            }
        }
        for (i, &group) in groups.iter().enumerate().rev() {
            if i == 0 {
                body.push(group as u8);
            } else {
                body.push(0x80 | group as u8);
            }
        }
    }
    tlv(0x06, &body)
}

/// Encode a non-negative INTEGER.
fn integer_bytes(v: u64) -> Vec<u8> {
    let be = v.to_be_bytes();
    let first = be.iter().position(|&b| b != 0).unwrap_or(7);
    let mut body = be[first..].to_vec();
    if body.first().is_some_and(|b| b & 0x80 != 0) {
        body.insert(0, 0);
    }
    if body.is_empty() {
        body.push(0);
    }
    tlv(0x02, &body)
}

/// A test-side AES-KW wrap (RFC 3394 §2.2.1) — the mirror of
/// [`aes_kw_unwrap`], needed to build fixtures.
fn aes_kw_wrap(kek: &[u8], key: &[u8]) -> Option<Vec<u8>> {
    let cipher = AesCipher::new(kek.len(), kek)?;
    let n = key.len() / 8;
    let mut a: [u8; 8] = [0xA6, 0xA6, 0xA6, 0xA6, 0xA6, 0xA6, 0xA6, 0xA6];
    let mut r: Vec<[u8; 8]> = Vec::new();
    for i in 0..n {
        let start = i * 8;
        r.push(<[u8; 8]>::try_from(key.get(start..start + 8)?).ok()?);
    }
    for j in 0..6usize {
        for i in 0..n {
            let t = (j * n + i + 1) as u64;
            let mut input = [0u8; 16];
            input[..8].copy_from_slice(&a);
            input[8..].copy_from_slice(r.get(i)?);
            let mut block = Block::clone_from_slice(&input);
            cipher.encrypt_block(&mut block);
            let t_bytes = t.to_be_bytes();
            for k in 0..8 {
                a[k] = block[k];
            }
            for k in 0..8 {
                a[k] ^= t_bytes[k];
            }
            let slot = r.get_mut(i)?;
            for k in 0..8 {
                slot[k] = block[8 + k];
            }
        }
    }
    let mut out = a.to_vec();
    for block in &r {
        out.extend_from_slice(block);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Builders for the CMS subset
// ---------------------------------------------------------------------------

const AES128_CBC: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 1, 2];
const AES256_CBC: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 1, 42];
const DES_EDE3_CBC: &[u64] = &[1, 2, 840, 113_549, 3, 7];
const DATA: &[u64] = &[1, 2, 840, 113_549, 1, 7, 1];
const ENVELOPED_DATA: &[u64] = &[1, 2, 840, 113_549, 1, 7, 3];
const RSA_ENCRYPTION: &[u64] = &[1, 2, 840, 113_549, 1, 1, 1];
const DH_STDDH_SHA256_KDF: &[u64] = &[1, 3, 133, 16, 840, 63, 0, 4];

/// An `AlgorithmIdentifier` with an optional parameter. `param` is the
/// already-encoded parameter TLV (e.g. a NULL `[05 00]` or an OCTET STRING
/// IV).
fn algorithm_identifier(oid_arcs: &[u64], param: Option<&[u8]>) -> Vec<u8> {
    let mut body = oid_bytes(oid_arcs);
    if let Some(p) = param {
        body.extend_from_slice(p);
    }
    tlv(0x30, &body)
}

/// An `EncryptedContentInfo` carrying `content` under the given algorithm.
fn encrypted_content_info(alg_oid: &[u64], iv: Option<&[u8]>, content: &[u8]) -> Vec<u8> {
    let param = iv.map(|iv| tlv(0x04, iv));
    let mut body = oid_bytes(DATA);
    body.extend_from_slice(&algorithm_identifier(alg_oid, param.as_deref()));
    body.extend_from_slice(&tlv(0xA0, &tlv(0x04, content)));
    tlv(0x30, &body)
}

/// A full `ContentInfo`/`EnvelopedData` blob with the given recipient TLVs.
fn enveloped_blob(recipients: &[Vec<u8>], eci: &[u8]) -> Vec<u8> {
    let mut set_body = Vec::new();
    for r in recipients {
        set_body.extend_from_slice(r);
    }
    let mut env_body = integer_bytes(2);
    env_body.extend_from_slice(&tlv(0x31, &set_body));
    env_body.extend_from_slice(eci);
    let enveloped = tlv(0x30, &env_body);
    let mut ci_body = oid_bytes(ENVELOPED_DATA);
    ci_body.extend_from_slice(&tlv(0xA0, &enveloped));
    tlv(0x30, &ci_body)
}

/// A `KeyTransRecipientInfo` (issuer-and-serial variant, rsaEncryption).
fn key_trans_recipient(encrypted_key: &[u8]) -> Vec<u8> {
    // RecipientIdentifier: IssuerAndSerialNumber ::= SEQUENCE { issuer Name,
    // serialNumber INTEGER } — one wrapping SEQUENCE.
    let mut rid_body = tlv(0x30, &[]); // issuer Name (empty RDNSequence)
    rid_body.extend_from_slice(&integer_bytes(1)); // serialNumber
    let rid = tlv(0x30, &rid_body);
    let mut body = integer_bytes(0); // version
    body.extend_from_slice(&rid);
    body.extend_from_slice(&algorithm_identifier(RSA_ENCRYPTION, Some(&[0x05, 0x00]))); // NULL
    body.extend_from_slice(&tlv(0x04, encrypted_key));
    tlv(0x30, &body)
}

/// A `KeyAgreeRecipientInfo` ([1] arm) with one originator point and one
/// wrapped key.
fn key_agree_recipient(originator_point: &[u8], wrapped: &[u8]) -> Vec<u8> {
    // OriginatorPublicKey ::= SEQUENCE { algorithm, BIT STRING }
    let mut bit_string = vec![0u8]; // no unused bits
    bit_string.extend_from_slice(originator_point);
    let mut opk_body = algorithm_identifier(&[1, 2, 840, 10045, 2, 1], None);
    opk_body.extend_from_slice(&tlv(0x03, &bit_string));
    let originator = tlv(0xA1, &tlv(0x30, &opk_body)); // [1] originatorKey
    let originator_wrapper = tlv(0xA0, &originator); // [0] EXPLICIT CHOICE

    let mut body = integer_bytes(3);
    body.extend_from_slice(&originator_wrapper);
    body.extend_from_slice(&algorithm_identifier(DH_STDDH_SHA256_KDF, None));
    // recipientEncryptedKeys: SEQUENCE OF { SEQUENCE { rid, OCTET STRING } }
    let mut entry = tlv(0x30, &[]); // empty issuer placeholder
    entry.extend_from_slice(&tlv(0x04, wrapped));
    let keys = tlv(0x30, &tlv(0x30, &entry));
    body.extend_from_slice(&keys);
    tlv(0xA1, &tlv(0x30, &body))
}

/// Pad `payload` to a 16-byte boundary with PKCS#7 and AES-CBC encrypt it.
fn cbc_encrypt_test(alg: CekAlgorithm, cek: &[u8], payload: &[u8]) -> Option<Vec<u8>> {
    use aes::cipher::BlockEncrypt;
    let (cipher, iv) = match alg {
        CekAlgorithm::Aes128Cbc { iv } => (AesCipher::A128(Aes128::new_from_slice(cek).ok()?), iv),
        CekAlgorithm::Aes256Cbc { iv } => (AesCipher::A256(Aes256::new_from_slice(cek).ok()?), iv),
        _ => return None,
    };
    let pad = 16 - (payload.len() % 16);
    let mut padded = payload.to_vec();
    padded.extend(std::iter::repeat_n(pad as u8, pad));
    // CMS keeps the IV in the AlgorithmIdentifier parameters; the
    // encryptedContent is ciphertext only.
    let mut out = Vec::new();
    let mut prev: [u8; 16] = iv;
    for chunk in padded.chunks(16) {
        let block_bytes: [u8; 16] = <[u8; 16]>::try_from(chunk).ok()?;
        let mut block = Block::clone_from_slice(&block_bytes);
        for k in 0..16 {
            block[k] ^= prev[k];
        }
        match &cipher {
            AesCipher::A128(c) => c.encrypt_block(&mut block),
            AesCipher::A192(c) => c.encrypt_block(&mut block),
            AesCipher::A256(c) => c.encrypt_block(&mut block),
        }
        out.extend_from_slice(block.as_slice());
        prev.copy_from_slice(block.as_slice());
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Credential helpers
// ---------------------------------------------------------------------------

/// A runtime-generated RSA keypair (test-only strength).
fn test_rsa_key(bits: usize) -> rsa::RsaPrivateKey {
    let mut rng = rsa::rand_core::OsRng;
    rsa::RsaPrivateKey::new(&mut rng, bits).expect("rsa keygen")
}

/// Wrap the PKCS#1 v1.5 CEK for a recipient (test-side transport).
fn rsa_wrap_cek(key: &rsa::RsaPrivateKey, cek: &[u8]) -> Vec<u8> {
    use rsa::Pkcs1v15Encrypt;
    key.to_public_key()
        .encrypt(&mut rsa::rand_core::OsRng, Pkcs1v15Encrypt, cek)
        .expect("rsa encrypt")
}

/// A fixed EC P-256 keypair (deterministic, no RNG dependency).
fn test_ec_pair() -> (p256::SecretKey, p256::PublicKey) {
    let scalar = [0x42u8; 32]; // a valid non-zero scalar
    let secret = p256::SecretKey::from_slice(&scalar).expect("ec secret");
    let public = secret.public_key();
    (secret, public)
}

/// An EC secret key in PKCS#8 DER.
fn ec_to_pkcs8(secret: &p256::SecretKey) -> Vec<u8> {
    use p256::pkcs8::EncodePrivateKey;
    secret
        .to_pkcs8_der()
        .expect("pkcs8 encode")
        .as_bytes()
        .to_vec()
}

/// An RSA private key in PKCS#8 DER.
fn rsa_to_pkcs8(key: &rsa::RsaPrivateKey) -> Vec<u8> {
    use rsa::pkcs8::EncodePrivateKey;
    key.to_pkcs8_der()
        .expect("pkcs8 encode")
        .as_bytes()
        .to_vec()
}

fn guard() -> BudgetGuard<'static> {
    Budget::profile(Surface::Fuzz).guard()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn parse_reads_a_minimal_enveloped_data() {
    let mut g = guard();
    let content = vec![0x11u8; 32];
    let ktri = key_trans_recipient(&[0xAA; 64]);
    let eci = encrypted_content_info(AES128_CBC, Some(&[0x22; 16]), &content);
    let blob = enveloped_blob(&[ktri], &eci);
    let parsed = parse_enveloped_data(&blob, &mut g).expect("parse");
    assert_eq!(parsed.recipients.len(), 1);
    assert_eq!(
        parsed.cek_algorithm,
        CekAlgorithm::Aes128Cbc { iv: [0x22; 16] }
    );
    assert_eq!(parsed.encrypted_content, Some(content.as_slice()));
}

#[test]
fn trailing_bytes_and_wrong_types_are_malformed() {
    let mut g = guard();
    let ktri = key_trans_recipient(&[0xAA; 64]);
    let eci = encrypted_content_info(AES128_CBC, Some(&[0x22; 16]), &[0x11; 32]);
    let mut blob = enveloped_blob(&[ktri], &eci);
    blob.push(0xFF); // trailing junk
    assert!(parse_enveloped_data(&blob, &mut g).is_err());

    // A non-envelopedData ContentInfo type.
    let wrong_type = {
        let mut ci_body = oid_bytes(DATA); // id-data instead of envelopedData
        ci_body.extend_from_slice(&tlv(0xA0, &tlv(0x30, &integer_bytes(0))));
        tlv(0x30, &ci_body)
    };
    assert!(parse_enveloped_data(&wrong_type, &mut g).is_err());

    // Truncated blob.
    assert!(parse_enveloped_data(&blob[..blob.len() - 4], &mut g).is_err());

    // Empty recipient set: structurally parseable.
    let empty = enveloped_blob(&[], &eci);
    let parsed = parse_enveloped_data(&empty, &mut g).expect("empty set parses");
    assert!(parsed.recipients.is_empty());
}

#[test]
fn unsupported_content_algorithms_are_typed_errors() {
    let mut g = guard();
    let ktri = key_trans_recipient(&[0xAA; 64]);
    // 3DES-CBC: recognised, deliberately unimplemented.
    let eci = encrypted_content_info(DES_EDE3_CBC, Some(&[0x22; 8]), &[0x11; 32]);
    let blob = enveloped_blob(&[ktri], &eci);
    let e = parse_enveloped_data(&blob, &mut g).expect_err("3DES must be refused");
    assert_eq!(e.code(), Code::EncryptUnsupported);

    // RC4-era s3 files are gated at the /Encrypt layer; a bare RC4 OID here
    // is refused the same way.
    let rc4_oid: &[u64] = &[1, 2, 840, 113_549, 3, 4];
    let ktri = key_trans_recipient(&[0xAA; 64]);
    let eci = encrypted_content_info(rc4_oid, None, &[0x11; 24]);
    let blob = enveloped_blob(&[ktri], &eci);
    let e = parse_enveloped_data(&blob, &mut g).expect_err("RC4 must be refused");
    assert_eq!(e.code(), Code::EncryptUnsupported);
}

#[test]
fn negative_version_is_malformed() {
    let mut g = guard();
    let ktri = key_trans_recipient(&[0xAA; 64]);
    let eci = encrypted_content_info(AES128_CBC, Some(&[0x22; 16]), &[0x11; 32]);
    let mut blob = enveloped_blob(&[ktri], &eci);
    // The EnvelopedData version is the first INTEGER inside the envelope:
    // rewrite its content to 0x80 (negative).
    let pos = blob
        .windows(3)
        .position(|w| w == [0x02, 0x01, 0x02])
        .expect("version");
    blob[pos + 2] = 0x80;
    let e = parse_enveloped_data(&blob, &mut g).expect_err("negative version");
    assert_eq!(e.code(), Code::EncryptMalformed);
}

/// RSA key-transport round trip: encrypt a CEK to the recipient, decrypt the
/// payload, derive the file key — the full Algorithm 1.
#[test]
fn rsa_key_transport_round_trip() {
    let mut g = guard();
    let key = test_rsa_key(512);
    let cek = [0x5Au8; 16];
    let alg = CekAlgorithm::Aes128Cbc { iv: [0x07; 16] };
    // The 24-byte payload: seed(20) + permissions(4).
    let mut payload = [0x63u8; 24];
    payload[20..24].copy_from_slice(&0xFFFF_F0C0u32.to_le_bytes());
    let content = cbc_encrypt_test(alg, &cek, &payload).expect("encrypt");
    let encrypted_key = rsa_wrap_cek(&key, &cek);

    let ktri = key_trans_recipient(&encrypted_key);
    let eci = encrypted_content_info(AES128_CBC, Some(&[0x07; 16]), &content);
    let blob = enveloped_blob(&[ktri], &eci);

    let auth = authenticate_public_key(
        &[&blob],
        &PubKeyCredential::Rsa(rsa_to_pkcs8(&key)),
        128,
        false,
        true,
        &mut g,
    )
    .expect("authenticate");
    assert_eq!(auth.key.len(), 16);
    assert_eq!(auth.permissions, 0xFFFF_F0C0);
    // The key must match an independent derivation of Algorithm 1.
    let mut input = payload[..20].to_vec();
    input.extend_from_slice(&blob);
    let expected = sha1_test(&input)[..16].to_vec();
    assert_eq!(auth.key, expected);
}

/// The wrong RSA key yields a typed `RecipientNoMatch`, never a key.
#[test]
fn wrong_rsa_key_is_a_typed_error() {
    let mut g = guard();
    let key = test_rsa_key(512);
    let wrong = test_rsa_key(512);
    let cek = [0x5Au8; 16];
    let alg = CekAlgorithm::Aes128Cbc { iv: [0x07; 16] };
    let payload = [0x63u8; 24];
    let content = cbc_encrypt_test(alg, &cek, &payload).expect("encrypt");
    let encrypted_key = rsa_wrap_cek(&key, &cek);
    let ktri = key_trans_recipient(&encrypted_key);
    let eci = encrypted_content_info(AES128_CBC, Some(&[0x07; 16]), &content);
    let blob = enveloped_blob(&[ktri], &eci);

    let e = authenticate_public_key(
        &[&blob],
        &PubKeyCredential::Rsa(rsa_to_pkcs8(&wrong)),
        128,
        false,
        true,
        &mut g,
    )
    .expect_err("wrong key must not authenticate");
    assert_eq!(e.code(), Code::RecipientNoMatch);

    // An EC credential against an RSA transport: no match either.
    let (secret, _) = test_ec_pair();
    let e = authenticate_public_key(
        &[&blob],
        &PubKeyCredential::EcP256(ec_to_pkcs8(&secret)),
        128,
        false,
        true,
        &mut g,
    )
    .expect_err("wrong credential type must not authenticate");
    assert_eq!(e.code(), Code::RecipientNoMatch);
}

/// ECDH + AES-KW round trip (the EC P-256 path of the subset).
#[test]
fn ecdh_key_agreement_round_trip() {
    let mut g = guard();
    let (secret, public) = test_ec_pair();
    let cek = [0x77u8; 32];
    let shared = p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), public.as_affine());
    let shared_bytes = *shared.raw_secret_bytes();
    let kek = kdf_x963_sha256(
        // The shared secret the writer would compute (same fixed keypair).
        shared_bytes.as_slice(),
        32,
    );
    let wrapped = aes_kw_wrap(&kek, &cek).expect("kw wrap");
    let payload = [0x41u8; 24];
    let alg = CekAlgorithm::Aes256Cbc { iv: [0x09; 16] };
    let content = cbc_encrypt_test(alg, &cek, &payload).expect("encrypt");

    use p256::elliptic_curve::sec1::ToEncodedPoint;
    let point = public.to_encoded_point(false); // uncompressed
    let kari = key_agree_recipient(point.as_bytes(), &wrapped);
    let eci = encrypted_content_info(AES256_CBC, Some(&[0x09; 16]), &content);
    let blob = enveloped_blob(&[kari], &eci);

    let auth = authenticate_public_key(
        &[&blob],
        &PubKeyCredential::EcP256(ec_to_pkcs8(&secret)),
        256,
        true,
        true,
        &mut g,
    )
    .expect("authenticate");
    assert_eq!(auth.key.len(), 32);
    let mut input = payload[..20].to_vec();
    input.extend_from_slice(&blob);
    let expected = sha2::Sha256::digest(&input).to_vec();
    assert_eq!(auth.key, expected);
}

/// A tampered AES-KW blob (wrong key path) fails the integrity register.
#[test]
fn tampered_kw_blob_does_not_unwrap() {
    let kek = [0x11u8; 16];
    let wrapped = aes_kw_wrap(&kek, &[0x22u8; 16]).expect("wrap");
    let mut broken = wrapped.clone();
    let mid = broken.len() / 2;
    broken[mid] ^= 0xFF;
    assert!(aes_kw_unwrap(&kek, &broken).is_none());
    assert!(aes_kw_unwrap(&kek, &wrapped).is_some());
    // Bad KEK length and bad block size are rejected structurally.
    assert!(aes_kw_unwrap(&[0x11u8; 15], &wrapped).is_none());
    assert!(aes_kw_unwrap(&kek, &wrapped[..15]).is_none());
}

/// RFC 3394 §4.1 test vector: 128-bit KEK wrapping 128-bit key data.
#[test]
fn aes_kw_matches_rfc3394_vector() {
    let kek = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
        0x0F,
    ];
    let key = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
        0xFF,
    ];
    let expected: [u8; 24] = [
        0x1F, 0xA6, 0x8B, 0x0A, 0x81, 0x12, 0xB4, 0x47, 0xAE, 0xF3, 0x4B, 0xD8, 0xFB, 0x5A, 0x7B,
        0x82, 0x9D, 0x3E, 0x86, 0x23, 0x71, 0xD2, 0xCF, 0xE5,
    ];
    let wrapped = aes_kw_wrap(&kek, &key).expect("wrap");
    assert_eq!(wrapped, expected);
    let unwrapped = aes_kw_unwrap(&kek, &expected).expect("unwrap");
    assert_eq!(unwrapped, key);
}

/// Budget exhaustion during the CMS parse is a typed error, not a panic.
#[test]
fn cms_parse_respects_the_budget() {
    let mut g = Budget {
        bytes: 4,
        ..Budget::profile(Surface::Fuzz)
    }
    .guard();
    let ktri = key_trans_recipient(&[0xAA; 64]);
    let eci = encrypted_content_info(AES128_CBC, Some(&[0x22; 16]), &[0x11; 32]);
    let blob = enveloped_blob(&[ktri], &eci);
    assert!(parse_enveloped_data(&blob, &mut g).is_err());
}

/// A hostile heap of empty recipient TLVs terminates within its byte budget.
#[test]
fn empty_recipient_set_terminates() {
    let mut g = guard();
    // A SET of 512 zero-length SEQUENCE entries: the parse must terminate
    // (and fail on the missing EncryptedContentInfo).
    let mut set_body = Vec::new();
    for _ in 0..512 {
        set_body.extend_from_slice(&tlv(0x30, &[]));
    }
    let mut env_body = integer_bytes(0);
    env_body.extend_from_slice(&tlv(0x31, &set_body));
    let enveloped = tlv(0x30, &env_body);
    let mut ci_body = oid_bytes(ENVELOPED_DATA);
    ci_body.extend_from_slice(&tlv(0xA0, &enveloped));
    let blob = tlv(0x30, &ci_body);
    assert!(parse_enveloped_data(&blob, &mut g).is_err());
}

/// The Debug impl of the credential must never print key material.
#[test]
fn credential_debug_is_redacted() {
    let cred = PubKeyCredential::Rsa(vec![0xDE, 0xAD, 0xBE, 0xEF]);
    let s = format!("{cred:?}");
    assert!(!s.contains("deadbeef"));
    assert!(!s.contains("222"));
    assert!(s.contains("redacted"));
}

/// SHA-1 for the Algorithm-1 expectation (the test-side mirror).
fn sha1_test(input: &[u8]) -> Vec<u8> {
    use sha1::Digest as _;
    sha1::Sha1::digest(input).to_vec()
}
