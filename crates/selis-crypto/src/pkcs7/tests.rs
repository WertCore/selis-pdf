//! Tests for the PKCS#7/CMS read path (SL-1.ENC.03).
//!
//! The DER *writer* helpers here exist only to build test fixtures Ã¢â‚¬â€ the
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

/// A test-side AES-KW wrap (RFC 3394 Ã‚Â§2.2.1) Ã¢â‚¬â€ the mirror of
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
            let mut block: Block = input.into();
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
    // serialNumber INTEGER } Ã¢â‚¬â€ one wrapping SEQUENCE.
    let mut rid_body = tlv(0x30, &[]); // issuer Name (empty RDNSequence)
    rid_body.extend_from_slice(&integer_bytes(1)); // serialNumber
    let rid = tlv(0x30, &rid_body);
    key_trans_with_rid(&rid, encrypted_key)
}

/// The same `KeyTransRecipientInfo` with a caller-supplied (already-encoded)
/// `RecipientIdentifier` TLV.
fn key_trans_with_rid(rid: &[u8], encrypted_key: &[u8]) -> Vec<u8> {
    let mut body = integer_bytes(0); // version
    body.extend_from_slice(rid);
    body.extend_from_slice(&algorithm_identifier(RSA_ENCRYPTION, Some(&[0x05, 0x00]))); // NULL
    body.extend_from_slice(&tlv(0x04, encrypted_key));
    tlv(0x30, &body)
}

/// A `KeyAgreeRecipientInfo` ([1] arm) with one originator point and one
/// wrapped key, addressed by `rid` (already-encoded `RecipientIdentifier`
/// TLV bytes Ã¢â‚¬â€ SL-1.ENC.07 parses it).
fn key_agree_recipient(originator_point: &[u8], wrapped: &[u8], rid: &[u8]) -> Vec<u8> {
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
    let mut entry = rid.to_vec();
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
        let mut block: Block = block_bytes.into();
        for k in 0..16 {
            block[k] ^= prev[k];
        }
        match &cipher {
            AesCipher::A128(c) => c.encrypt_block(&mut block),
            AesCipher::A192(c) => c.encrypt_block(&mut block),
            AesCipher::A256(c) => c.encrypt_block(&mut block),
        }
        out.extend_from_slice(&block);
        prev.copy_from_slice(&block);
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
fn legacy_content_algorithms_parse_for_read() {
    // SL-1.ENC.08: RC4 / 3DES-CBC / RC2-CBC are *read* support; they parse
    // into their CekAlgorithm with the parameters taken from the DER.
    let mut g = guard();
    let ktri = key_trans_recipient(&[0xAA; 64]);

    let rc4_oid: &[u64] = &[1, 2, 840, 113_549, 3, 4];
    let eci = encrypted_content_info(rc4_oid, None, &[0x11; 24]);
    let blob = enveloped_blob(&[ktri.clone()], &eci);
    let parsed = parse_enveloped_data(&blob, &mut g).expect("rc4 parses");
    assert_eq!(parsed.cek_algorithm, CekAlgorithm::Rc4);

    let eci = encrypted_content_info(DES_EDE3_CBC, Some(&[0x22; 8]), &[0x11; 32]);
    let blob = enveloped_blob(&[ktri.clone()], &eci);
    let parsed = parse_enveloped_data(&blob, &mut g).expect("3des parses");
    assert_eq!(
        parsed.cek_algorithm,
        CekAlgorithm::TdeaCbc { iv: [0x22; 8] }
    );

    // RC2-CBC-Parameter carries the effective key length and the IV.
    let rc2_oid: &[u64] = &[1, 2, 840, 113_549, 3, 2];
    let rc2_param = tlv(0x30, &{
        let mut b = integer_bytes(128);
        b.extend_from_slice(&tlv(0x04, &[0x33; 8]));
        b
    });
    let mut body = oid_bytes(DATA);
    body.extend_from_slice(&algorithm_identifier(rc2_oid, Some(&rc2_param)));
    body.extend_from_slice(&tlv(0xA0, &tlv(0x04, &[0x11; 32])));
    let eci = tlv(0x30, &body);
    let blob = enveloped_blob(&[ktri], &eci);
    let parsed = parse_enveloped_data(&blob, &mut g).expect("rc2 parses");
    assert_eq!(
        parsed.cek_algorithm,
        CekAlgorithm::Rc2Cbc {
            iv: [0x33; 8],
            effective_bits: 128
        }
    );
}

#[test]
fn legacy_content_iv_lengths_are_enforced() {
    let mut g = guard();
    let ktri = key_trans_recipient(&[0xAA; 64]);
    // 3DES with a 16-byte IV (AES's size) is malformed, not a silent read.
    let eci = encrypted_content_info(DES_EDE3_CBC, Some(&[0x22; 16]), &[0x11; 32]);
    let blob = enveloped_blob(&[ktri], &eci);
    let e = parse_enveloped_data(&blob, &mut g).expect_err("wrong IV size");
    assert_eq!(e.code(), Code::EncryptMalformed);
}

#[test]
fn unsupported_content_algorithms_are_typed_errors() {
    let mut g = guard();
    let ktri = key_trans_recipient(&[0xAA; 64]);

    // AES key wrap is a *key-management* algorithm, never a content one.
    let wrap_oid: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 1, 5];
    let eci = encrypted_content_info(wrap_oid, Some(&[0x22; 16]), &[0x11; 32]);
    let blob = enveloped_blob(&[ktri.clone()], &eci);
    let e = parse_enveloped_data(&blob, &mut g).expect_err("wrap-as-content refused");
    assert_eq!(e.code(), Code::EncryptUnsupported);

    // Camellia / IDEA / anything else off the subset list.
    let camellia: &[u64] = &[1, 2, 410, 200004, 4, 4];
    let eci = encrypted_content_info(camellia, Some(&[0x22; 16]), &[0x11; 32]);
    let blob = enveloped_blob(&[ktri], &eci);
    let e = parse_enveloped_data(&blob, &mut g).expect_err("camellia refused");
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
/// payload, derive the file key Ã¢â‚¬â€ the full Algorithm 1.
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
        &PubKeyCredential::rsa(rsa_to_pkcs8(&key)),
        128,
        false,
        true,
        &mut g,
    )
    .expect("authenticate");
    assert_eq!(auth.key.len(), 16);
    assert_eq!(auth.permissions, 0xFFFF_F0C0);
    assert_eq!(auth.matched_by, Matched::Structurally);
    assert_eq!(auth.recipient_index, 0);
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
        &PubKeyCredential::rsa(rsa_to_pkcs8(&wrong)),
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
        &PubKeyCredential::ec_p256(ec_to_pkcs8(&secret)),
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
        &shared_bytes,
        32,
    );
    let wrapped = aes_kw_wrap(&kek, &cek).expect("kw wrap");
    let payload = [0x41u8; 24];
    let alg = CekAlgorithm::Aes256Cbc { iv: [0x09; 16] };
    let content = cbc_encrypt_test(alg, &cek, &payload).expect("encrypt");

    use p256::elliptic_curve::sec1::ToEncodedPoint;
    let point = public.to_encoded_point(false); // uncompressed
    let kari = key_agree_recipient(
        point.as_bytes(),
        &wrapped,
        &tlv(0x80, &[0x33u8; 20]), // SKI-addressed recipient
    );
    let eci = encrypted_content_info(AES256_CBC, Some(&[0x09; 16]), &content);
    let blob = enveloped_blob(&[kari], &eci);

    let auth = authenticate_public_key(
        &[&blob],
        &PubKeyCredential::ec_p256(ec_to_pkcs8(&secret)),
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

/// RFC 3394 Ã‚Â§4.1 test vector: 128-bit KEK wrapping 128-bit key data.
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
    let cred = PubKeyCredential::rsa(vec![0xDE, 0xAD, 0xBE, 0xEF]);
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

// ---------------------------------------------------------------------------
// SL-1.ENC.07 Ã¢â‚¬â€ certificate-identity recipient selection
// ---------------------------------------------------------------------------

use crate::test_fixtures::{certificate, serial_content};
use crate::x509;

/// An `IssuerAndSerialNumber` `RecipientIdentifier` (with its wrapping TLV)
/// derived from an already-parsed certificate identity.
fn rid_from_identity(issuer: &[u8], serial: &[u8]) -> Vec<u8> {
    let mut body = tlv(0x30, issuer);
    body.extend_from_slice(&tlv(0x02, serial));
    tlv(0x30, &body)
}

/// Build an RSA-key-transport blob addressed to `identifier`.
fn rsa_blob_for(key: &rsa::RsaPrivateKey, identifier: &[u8]) -> Vec<u8> {
    let cek = [0x5Au8; 16];
    let alg = CekAlgorithm::Aes128Cbc { iv: [0x07; 16] };
    let mut payload = [0x63u8; 24];
    payload[20..24].copy_from_slice(&0xFFFF_F0C0u32.to_le_bytes());
    let content = cbc_encrypt_test(alg, &cek, &payload).expect("encrypt");
    let encrypted_key = rsa_wrap_cek(key, &cek);
    let ktri = key_trans_with_rid(identifier, &encrypted_key);
    let eci = encrypted_content_info(AES128_CBC, Some(&[0x07; 16]), &content);
    enveloped_blob(&[ktri], &eci)
}

/// A credential's `Auto`/`Certificate` pass selects the recipient whose
/// `issuerAndSerialNumber` matches the supplied chain before any unwrap.
#[test]
fn certificate_identity_selects_the_addressed_recipient() {
    let mut g = guard();
    let key = test_rsa_key(2048);
    let cert = certificate(&[0x00], 0x4D, "selis-recip", false, None);
    let ident = x509::parse_identity(&cert, &mut g).expect("identity");
    let rid = rid_from_identity(ident.issuer, ident.serial);

    // A wrong-addressed blob first, then the correctly addressed one: the
    // identity pass must take the second, and prove it matched by identity.
    let decoy = rsa_blob_for(&key, &rid_from_identity(&[], &serial_content(9999)));
    let target = rsa_blob_for(&key, &rid);
    let cred = PubKeyCredential::rsa(rsa_to_pkcs8(&key)).with_certificate(cert.clone());
    let auth = authenticate_public_key(&[&decoy, &target], &cred, 128, false, true, &mut g)
        .expect("matched by identity");
    assert_eq!(auth.matched_by, Matched::ByIssuerAndSerialNumber);
    assert_eq!(auth.recipient_index, 1, "the identity-matched blob wins");

    // match_by=Certificate with a chain that matches nothing is a typed
    // RECIPIENT_NO_MATCH, even though the key could open a blob structurally.
    let unrelated = certificate(&[0x00], 0x1234, "someone-else", false, None);
    let strict = PubKeyCredential::rsa(rsa_to_pkcs8(&key))
        .with_certificate(unrelated)
        .matching(MatchBy::Certificate);
    let e = authenticate_public_key(&[&decoy, &target], &strict, 128, false, true, &mut g)
        .expect_err("Certificate mode must not fall through");
    assert_eq!(e.code(), Code::RecipientNoMatch);

    // Ã¢â‚¬Â¦while the same strict credential that DOES match still opens.
    let ok = PubKeyCredential::rsa(rsa_to_pkcs8(&key))
        .with_certificate(cert)
        .matching(MatchBy::Certificate);
    let auth = authenticate_public_key(&[&decoy, &target], &ok, 128, false, true, &mut g)
        .expect("the addressed chain opens");
    assert_eq!(auth.matched_by, Matched::ByIssuerAndSerialNumber);
    assert_eq!(auth.recipient_index, 1);
}

/// A bare private key (no chain) always uses the structural first-valid scan
/// and reports `Matched::Structurally`.
#[test]
fn bare_key_selection_is_structural() {
    let mut g = guard();
    let key = test_rsa_key(2048);
    let pkcs8 = rsa_to_pkcs8(&key);
    let cert = certificate(&[0x00], 0x4D, "selis-recip", false, None);
    let ident = x509::parse_identity(&cert, &mut g).expect("identity");
    let rid = rid_from_identity(ident.issuer, ident.serial);
    let blob = rsa_blob_for(&key, &rid);
    let auth = authenticate_public_key(
        &[&blob],
        &PubKeyCredential::rsa(pkcs8), // no certificate supplied
        128,
        false,
        true,
        &mut g,
    )
    .expect("structural match");
    assert_eq!(auth.matched_by, Matched::Structurally);
}

/// A subject-key-identifier recipient is matched by both the extension form
/// and the RFC 5280 method-1 (SHA-1 of the public key) fallback.
#[test]
fn subject_key_identifier_matches_the_chain() {
    let mut g = guard();
    let key = test_rsa_key(2048);
    let pkcs8 = rsa_to_pkcs8(&key);

    // (a) The recipient carries the certificate's extension SKI.
    let ski = [0x11u8; 20];
    let cert_ext = certificate(&[0x00], 5, "ski-ext", false, Some(&ski));
    // An RSA cert has no method-1 fallback, so the extension is what binds.
    // Build a blob addressed by that SKI (subjectKeyIdentifier [0]).
    let blob_ski = rsa_blob_for(&key, &tlv(0x80, &ski));
    let auth = authenticate_public_key(
        &[&blob_ski],
        &PubKeyCredential::rsa(pkcs8.clone()).matching(MatchBy::Certificate),
        128,
        false,
        true,
        &mut g,
    );
    // Certificate mode with no certificate supplied matches nothing.
    assert_eq!(
        auth.err().map(|e| e.code()),
        Some(Code::RecipientNoMatch),
        "certificate mode requires a chain"
    );
    // Ã¢â‚¬Â¦with the extension-bearing chain, `Auto` matches by identity.
    let auth = authenticate_public_key(
        &[&blob_ski],
        &PubKeyCredential::rsa(pkcs8.clone()).with_certificate(cert_ext),
        128,
        false,
        true,
        &mut g,
    )
    .expect("SKI extension match");
    assert_eq!(auth.matched_by, Matched::BySubjectKeyIdentifier);

    // (b) An EC cert with no extension: the method-1 SHA-1 of the key.
    let point = [0x04u8, 9, 8, 7];
    let ski_bytes = <[u8; 20]>::try_from(sha1_test(&point).as_slice()).expect("20");
    let cert_ec = certificate(&point, 6, "ski-ec", true, None);
    let blob_ec_ski = rsa_blob_for(&key, &tlv(0x80, &ski_bytes));
    let auth = authenticate_public_key(
        &[&blob_ec_ski],
        &PubKeyCredential::rsa(pkcs8).with_certificate(cert_ec),
        128,
        false,
        true,
        &mut g,
    )
    .expect("method-1 SKI match");
    assert_eq!(auth.matched_by, Matched::BySubjectKeyIdentifier);
}

/// Serials that differ only by DER minimality padding compare equal.
#[test]
fn serial_equality_is_value_based() {
    assert!(serial_eq(&[0x4D], serial_content(0x4D).as_slice()));
    assert!(serial_eq(&[0x00, 0x4D], &[0x4D]));
    assert!(!serial_eq(&[0x4D], &[0x4E]));
}

/// A single blob addressed by **duplicate identical** `RecipientIdentifier`s
/// (two `KeyTransRecipientInfo` entries quoting the same issuer/serial Ã¢â‚¬â€ a
/// writer bug, seen in the wild): the identity pass keeps trying after the
/// first transport fails to unwrap and opens the recipient whose key really
/// matches; it never mistakes the failing twin for "no match", and never
/// opens on the wrong key pair.
#[test]
fn duplicate_identifiers_try_every_matched_transport() {
    let mut g = guard();
    let key = test_rsa_key(2048);
    let other = test_rsa_key(2048);
    let cert = certificate(&[0x00], 0x4D, "selis-duplicate", false, Some(&[0x77u8; 20]));
    let ident = x509::parse_identity(&cert, &mut g).expect("identity");
    let rid = rid_from_identity(ident.issuer, ident.serial);

    let cek = [0x5Au8; 16];
    let alg = CekAlgorithm::Aes128Cbc { iv: [0x07; 16] };
    let mut payload = [0x63u8; 24];
    payload[20..24].copy_from_slice(&0xFFFF_F0C0u32.to_le_bytes());
    let content = cbc_encrypt_test(alg, &cek, &payload).expect("encrypt");
    // First twin: CEK wrapped under a key the credential is NOT (fails the
    // unwrap); second twin: the right key.
    let wrong = rsa_wrap_cek(&other, &cek);
    let right = rsa_wrap_cek(&key, &cek);
    let blob = enveloped_blob(
        &[
            key_trans_with_rid(&rid, &wrong),
            key_trans_with_rid(&rid, &right),
        ],
        &encrypted_content_info(AES128_CBC, Some(&[0x07; 16]), &content),
    );

    // Identity mode (`Certificate`): matches the duplicated rid, keeps
    // trying, opens on the second transport Ã¢â‚¬â€ still BY identity.
    let cred = PubKeyCredential::rsa(rsa_to_pkcs8(&key))
        .with_certificate(cert.clone())
        .matching(MatchBy::Certificate);
    let auth = authenticate_public_key(&[&blob], &cred, 128, false, true, &mut g)
        .expect("a matched-but-failing twin must not abort the pass");
    assert_eq!(auth.matched_by, Matched::ByIssuerAndSerialNumber);
    assert_eq!(auth.permissions, 0xFFFF_F0C0);

    // A key matching *neither* twin with the same chain: the identity pass
    // matches both twins, unwraps fail on both, and `Certificate` mode
    // still refuses typed Ã¢â‚¬â€ a matched identifier with non-matching key
    // material is never a silent fall-through.
    let third = test_rsa_key(1024);
    let e = authenticate_public_key(
        &[&blob],
        &PubKeyCredential::rsa(rsa_to_pkcs8(&third))
            .with_certificate(cert)
            .matching(MatchBy::Certificate),
        128,
        false,
        true,
        &mut g,
    )
    .expect_err("neither twin opens");
    assert_eq!(e.code(), Code::RecipientNoMatch);
}

/// A malformed certificate in the chain is a typed error, never silently
/// dropped (a corrupt chain must not degrade to structural selection).
#[test]
fn damaged_chain_certificate_is_malformed() {
    let mut g = guard();
    let key = test_rsa_key(2048);
    let blob = rsa_blob_for(&key, &tlv(0x30, &[])); // no ISN: won't match anyway
    let e = authenticate_public_key(
        &[&blob],
        &PubKeyCredential::rsa(rsa_to_pkcs8(&key)).with_certificate(vec![0x30, 0x00]),
        128,
        false,
        true,
        &mut g,
    )
    .expect_err("bad cert");
    assert_eq!(e.code(), Code::EncryptMalformed);
}

/// The permission decoder never yields a silent grant: only the exact
/// 24-byte payload decodes; anything else is `None`.
#[test]
fn permission_block_decoder_is_strict() {
    // A structurally valid 24-byte payload decodes the little-endian block.
    let mut payload = [0u8; 24];
    payload[20..].copy_from_slice(&0xFFFF_F0C0u32.to_le_bytes());
    assert_eq!(decode_permission_block(&payload), Some(0xFFFF_F0C0));
    assert_eq!(payload_seed(&payload).map(<[u8]>::len), Some(20));
    // Any wrong length is None Ã¢â‚¬â€ never zero, never partial.
    assert_eq!(decode_permission_block(&payload[..23]), None);
    assert_eq!(decode_permission_block(&payload[..20]), None);
    assert_eq!(decode_permission_block(&[0u8; 25][..]), None);
    assert_eq!(decode_permission_block(&[]), None);
    assert!(payload_seed(&payload[..23]).is_none());
}

// ---------------------------------------------------------------------------
// SL-1.ENC.08: legacy content algorithms (RC4 / 3DES-CBC / RC2-CBC) and the
// RSAES-OAEP key transport. The test-side "encrypt" helpers exist only to
// build fixtures; the shipped path is decrypt-only (ADR-P0019).
// ---------------------------------------------------------------------------

const RSAES_OAEP: &[u64] = &[1, 2, 840, 113_549, 1, 1, 7];
const RC4_OID: &[u64] = &[1, 2, 840, 113_549, 3, 4];
const RC2_OID: &[u64] = &[1, 2, 840, 113_549, 3, 2];

/// A `KeyTransRecipientInfo` under an arbitrary `keyEncryptionAlgorithm`
/// (the caller supplies the already-encoded parameter TLV or `None`).
fn key_trans_alg(
    rid: &[u8],
    encrypted_key: &[u8],
    alg_oid: &[u64],
    param: Option<&[u8]>,
) -> Vec<u8> {
    let mut body = integer_bytes(0); // version
    body.extend_from_slice(rid);
    body.extend_from_slice(&algorithm_identifier(alg_oid, param));
    body.extend_from_slice(&tlv(0x04, encrypted_key));
    tlv(0x30, &body)
}

/// The default `RecipientIdentifier` used by the other fixtures.
fn default_rid() -> Vec<u8> {
    let mut rid_body = tlv(0x30, &[]);
    rid_body.extend_from_slice(&integer_bytes(1));
    tlv(0x30, &rid_body)
}

/// Encrypt `payload` (+ PKCS#7 to the mode's block) with the CEK Ã¢â‚¬â€ test-side
/// only, mirroring what `authenticate_public_key` reads back. RC4 uses no
/// padding, so the content *is* the payload.
fn content_encrypt(algorithm: CekAlgorithm, cek: &[u8], payload: &[u8]) -> Vec<u8> {
    match algorithm {
        CekAlgorithm::Rc4 => crate::rc4(cek, payload),
        CekAlgorithm::Aes128Cbc { .. }
        | CekAlgorithm::Aes192Cbc { .. }
        | CekAlgorithm::Aes256Cbc { .. } => {
            cbc_encrypt_test(algorithm, cek, payload).expect("aes encrypt")
        }
        CekAlgorithm::TdeaCbc { iv } => {
            let cipher = crate::legacy::Tdes::new(cek).expect("tdea");
            let mut padded = payload.to_vec();
            let pad = 8 - (padded.len() % 8);
            padded.extend(std::iter::repeat_n(pad as u8, pad));
            let mut out = Vec::new();
            let mut prev = iv;
            for chunk in padded.chunks(8) {
                let mut block = <[u8; 8]>::try_from(chunk).expect("block");
                for k in 0..8 {
                    block[k] ^= prev[k];
                }
                cipher.encrypt_block(&mut block);
                out.extend_from_slice(&block);
                prev = block;
            }
            out
        }
        CekAlgorithm::Rc2Cbc { iv, effective_bits } => {
            let cipher = crate::legacy::Rc2::new(cek, effective_bits).expect("rc2");
            let mut padded = payload.to_vec();
            let pad = 8 - (padded.len() % 8);
            padded.extend(std::iter::repeat_n(pad as u8, pad));
            let mut out = Vec::new();
            let mut prev = iv;
            for chunk in padded.chunks(8) {
                let mut block = <[u8; 8]>::try_from(chunk).expect("block");
                for k in 0..8 {
                    block[k] ^= prev[k];
                }
                cipher.encrypt_block(&mut block);
                out.extend_from_slice(&block);
                prev = block;
            }
            out
        }
    }
}

fn seed_payload(seed_byte: u8, perms: u32) -> [u8; 24] {
    let mut payload = [seed_byte; 24];
    payload[20..].copy_from_slice(&perms.to_le_bytes());
    payload
}

/// One authenticated pass over a single-recipient blob under `algorithm`.
fn authenticate_alg(
    key: &rsa::RsaPrivateKey,
    cek: &[u8],
    algorithm: CekAlgorithm,
    payload: &[u8],
    g: &mut BudgetGuard<'_>,
) -> Result<PubKeyAuth> {
    let (enc_oid, param) = content_alg_der(algorithm);
    let blob = enveloped_with(
        key,
        cek,
        &enc_oid,
        param.as_deref(),
        &content_encrypt(algorithm, cek, payload),
    );
    authenticate_public_key(
        &[&blob],
        &PubKeyCredential::rsa(rsa_to_pkcs8(key)),
        128,
        false,
        true,
        g,
    )
}

/// The DER name + parameters of a `CekAlgorithm` (fixtures rebuild blobs
/// from what the code reads, never the other way round).
fn content_alg_der(algorithm: CekAlgorithm) -> (&'static [u64], Option<Vec<u8>>) {
    match algorithm {
        CekAlgorithm::Rc4 => (RC4_OID, None),
        CekAlgorithm::TdeaCbc { iv } => (DES_EDE3_CBC, Some(tlv(0x04, &iv))),
        CekAlgorithm::Rc2Cbc { iv, effective_bits } => {
            let mut b = integer_bytes(effective_bits as u64);
            b.extend_from_slice(&tlv(0x04, &iv));
            (RC2_OID, Some(tlv(0x30, &b)))
        }
        CekAlgorithm::Aes128Cbc { iv } => (AES128_CBC, Some(tlv(0x04, &iv))),
        CekAlgorithm::Aes192Cbc { iv } => (AES192_CBC_TEST, Some(tlv(0x04, &iv))),
        CekAlgorithm::Aes256Cbc { iv } => (AES256_CBC, Some(tlv(0x04, &iv))),
    }
}

/// A v1.5-transported blob around `content` (the `[0] IMPLICIT` content shape
/// real CMS writers emit, which is *also* what the legacy tests must read).
fn enveloped_with(
    key: &rsa::RsaPrivateKey,
    cek: &[u8],
    content_oid: &[u64],
    content_param: Option<&[u8]>,
    content: &[u8],
) -> Vec<u8> {
    let encrypted_key = rsa_wrap_cek(key, cek);
    let mut body = oid_bytes(DATA);
    body.extend_from_slice(&algorithm_identifier(content_oid, content_param));
    body.extend_from_slice(&tlv(0x80, content));
    let eci = tlv(0x30, &body);
    let ktri = key_trans_alg(
        &default_rid(),
        &encrypted_key,
        RSA_ENCRYPTION,
        Some(&[0x05, 0x00]),
    );
    enveloped_blob(&[ktri], &eci)
}

/// OAEP-wrap a CEK Ã¢â‚¬â€ test-side only; the shipped write path never encrypts.
fn rsa_oaep_wrap(key: &rsa::RsaPrivateKey, cek: &[u8], hash: OaepHash) -> Vec<u8> {
    use rsa::Oaep;
    let pub_key = key.to_public_key();
    let mut rng = rsa::rand_core::OsRng;
    match hash {
        OaepHash::Sha1 => pub_key
            .encrypt(&mut rng, Oaep::new::<sha1::Sha1>(), cek)
            .expect("oaep sha1"),
        OaepHash::Sha256 => pub_key
            .encrypt(&mut rng, Oaep::new::<sha2::Sha256>(), cek)
            .expect("oaep sha256"),
        OaepHash::Sha384 => pub_key
            .encrypt(&mut rng, Oaep::new::<sha2::Sha384>(), cek)
            .expect("oaep sha384"),
        OaepHash::Sha512 => pub_key
            .encrypt(&mut rng, Oaep::new::<sha2::Sha512>(), cek)
            .expect("oaep sha512"),
    }
}

/// An RSAES-OAEP-transported blob around an AES payload.
fn oaep_envelope(
    encrypted_key: &[u8],
    cek: &[u8],
    algorithm: CekAlgorithm,
    payload: &[u8],
    params: Option<&[u8]>,
) -> Vec<u8> {
    let (content_oid, content_param) = content_alg_der(algorithm);
    let mut body = oid_bytes(DATA);
    body.extend_from_slice(&algorithm_identifier(content_oid, content_param.as_deref()));
    body.extend_from_slice(&tlv(0x80, &content_encrypt(algorithm, cek, payload)));
    let eci = tlv(0x30, &body);
    let ktri = key_trans_alg(&default_rid(), encrypted_key, RSAES_OAEP, params);
    enveloped_blob(&[ktri], &eci)
}

/// A second AES-192 constant the shared helpers do not import by name.
const AES192_CBC_TEST: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 1, 22];

#[test]
fn rc4_s3_payload_authenticates() {
    let mut g = guard();
    let key = test_rsa_key(512);
    let cek = [0x5au8; 16]; // RC4-128 content key
    let payload = seed_payload(0x63, 0xFFFF_F0C0);
    let auth =
        authenticate_alg(&key, &cek, CekAlgorithm::Rc4, &payload, &mut g).expect("rc4 opens");
    assert_eq!(auth.key.len(), 16);
    assert_eq!(auth.permissions, 0xFFFF_F0C0);
    assert_eq!(auth.matched_by, Matched::Structurally);
}

#[test]
fn rc4_40_payload_authenticates() {
    let mut g = guard();
    let key = test_rsa_key(512);
    let cek = [0x11u8; 5]; // export-era 40-bit RC4 key
    let payload = seed_payload(0x17, 0xFFFF_FFC0);
    let auth = authenticate_alg(&key, &cek, CekAlgorithm::Rc4, &payload, &mut g).expect("rc4-40");
    assert_eq!(auth.permissions, 0xFFFF_FFC0);
}

#[test]
fn tdea_and_rc2_payloads_authenticate() {
    let mut g = guard();
    let key = test_rsa_key(512);
    let payload = seed_payload(0x24, 0xFFFF_F0C0);
    let t = authenticate_alg(
        &key,
        &[0x77u8; 24],
        CekAlgorithm::TdeaCbc { iv: [0x41; 8] },
        &payload,
        &mut g,
    )
    .expect("3des opens");
    assert_eq!(t.permissions, 0xFFFF_F0C0);
    let r = authenticate_alg(
        &key,
        &[0x33u8; 16],
        CekAlgorithm::Rc2Cbc {
            iv: [0x42; 8],
            effective_bits: 128,
        },
        &payload,
        &mut g,
    )
    .expect("rc2 opens");
    assert_eq!(r.permissions, 0xFFFF_F0C0);
    // TDEA two-key (K1,K2,K1) and a 40-bit RC2 run through the same path.
    let t2 = authenticate_alg(
        &key,
        &[0x1eu8; 16],
        CekAlgorithm::TdeaCbc { iv: [0x9u8; 8] },
        &payload,
        &mut g,
    )
    .expect("2-key tdea");
    assert_eq!(t2.permissions, 0xFFFF_F0C0);
    let r2 = authenticate_alg(
        &key,
        &[0x5eu8; 16],
        CekAlgorithm::Rc2Cbc {
            iv: [0x3u8; 8],
            effective_bits: 40,
        },
        &payload,
        &mut g,
    )
    .expect("rc2-40");
    assert_eq!(r2.permissions, 0xFFFF_F0C0);
}

/// A wrong RSA key under a *legacy* content cipher: the transport unwrap fails
/// first, so the payload cipher's (weaker) detection never even sees a key.
#[test]
fn legacy_wrong_key_is_recipient_no_match() {
    let mut g = guard();
    let key = test_rsa_key(512);
    let wrong = test_rsa_key(512);
    for algorithm in [
        CekAlgorithm::Rc4,
        CekAlgorithm::TdeaCbc { iv: [0x01; 8] },
        CekAlgorithm::Rc2Cbc {
            iv: [0x02; 8],
            effective_bits: 128,
        },
    ] {
        let cek = [0x22u8; 16];
        let payload = seed_payload(0x63, 0xFFFF_F0C0);
        let encrypted_key = rsa_wrap_cek(&key, &cek);
        let enc_oid = match algorithm {
            CekAlgorithm::Rc4 => RC4_OID,
            CekAlgorithm::TdeaCbc { .. } => DES_EDE3_CBC,
            CekAlgorithm::Rc2Cbc { .. } => RC2_OID,
            _ => AES128_CBC,
        };
        let param = match algorithm {
            CekAlgorithm::Rc4 => None,
            CekAlgorithm::TdeaCbc { iv } => Some(tlv(0x04, &iv)),
            CekAlgorithm::Rc2Cbc { iv, effective_bits } => {
                let mut b = integer_bytes(effective_bits as u64);
                b.extend_from_slice(&tlv(0x04, &iv));
                Some(tlv(0x30, &b))
            }
            _ => None,
        };
        let mut body = oid_bytes(DATA);
        body.extend_from_slice(&algorithm_identifier(enc_oid, param.as_deref()));
        body.extend_from_slice(&tlv(0x80, &content_encrypt(algorithm, &cek, &payload)));
        let eci = tlv(0x30, &body);
        let blob = enveloped_blob(
            &[key_trans_alg(
                &default_rid(),
                &encrypted_key,
                RSA_ENCRYPTION,
                Some(&[0x05, 0x00]),
            )],
            &eci,
        );
        let e = authenticate_public_key(
            &[&blob],
            &PubKeyCredential::rsa(rsa_to_pkcs8(&wrong)),
            128,
            false,
            true,
            &mut g,
        )
        .expect_err("wrong key must not open a legacy envelope");
        assert_eq!(e.code(), Code::RecipientNoMatch);
    }
}

/// An RC2 envelope whose DER parameters disagree with the CEK length is a
/// wrong-key path (None out of `unwrap_cek`), never a truncated key. Here the
/// payload is encrypted under a *valid* 24-byte TDEA key while the *transport*
/// wraps a 20-byte CEK, so `decrypt_payload` sees the oversized key first.
#[test]
fn legacy_cek_length_mismatch_is_refused() {
    let mut g = guard();
    let key = test_rsa_key(512);
    let payload = seed_payload(0x0f, 0xFFFF_F0C0);
    let short_cek = [0xABu8; 20];
    let good_cek = [0x77u8; 24];
    let encrypted_key = rsa_wrap_cek(&key, &short_cek);
    let content = content_encrypt(CekAlgorithm::TdeaCbc { iv: [0x07; 8] }, &good_cek, &payload);
    let mut body = oid_bytes(DATA);
    body.extend_from_slice(&algorithm_identifier(
        DES_EDE3_CBC,
        Some(&tlv(0x04, &[0x07; 8])),
    ));
    body.extend_from_slice(&tlv(0x80, &content));
    let blob = enveloped_blob(
        &[key_trans_alg(
            &default_rid(),
            &encrypted_key,
            RSA_ENCRYPTION,
            Some(&[0x05, 0x00]),
        )],
        &tlv(0x30, &body),
    );
    let e = authenticate_public_key(
        &[&blob],
        &PubKeyCredential::rsa(rsa_to_pkcs8(&key)),
        128,
        false,
        true,
        &mut g,
    )
    .expect_err("bad tdea cek");
    assert_eq!(e.code(), Code::RecipientNoMatch);
}

/// RSAES-OAEP key transport with the DER default parameters (absent), the
/// only-padded-params form, and MGF1/SHA-1 Ã¢â‚¬â€ a real CMS *write* path's read.
#[test]
fn rsa_oaep_transport_round_trip() {
    let mut g = guard();
    let key = test_rsa_key(512);
    let cek = [0x5au8; 16];
    let payload = seed_payload(0x63, 0xFFFF_F0C0);
    let encrypted_key = rsa_oaep_wrap(&key, &cek, OaepHash::Sha1);
    let blob = oaep_envelope(
        &encrypted_key,
        &cek,
        CekAlgorithm::Aes128Cbc { iv: [0x07; 16] },
        &payload,
        None,
    );
    let auth = authenticate_public_key(
        &[&blob],
        &PubKeyCredential::rsa(rsa_to_pkcs8(&key)),
        128,
        false,
        true,
        &mut g,
    )
    .expect("oaep opens");
    assert_eq!(auth.permissions, 0xFFFF_F0C0);
    assert_eq!(auth.matched_by, Matched::Structurally);
}

/// DER of an *implicitly tagged* `AlgorithmIdentifier` (the RFC 8017
/// `RSAES-OAEP-params` members are `[0]/[1]/[2]` on AlgorithmIdentifier, so
/// the SEQUENCE tag is replaced and the content is the bare OID + NULL).
fn implicit_algorithm(tag: u8, alg_oid: &[u64], params: Option<&[u8]>) -> Vec<u8> {
    let mut body = oid_bytes(alg_oid);
    if let Some(p) = params {
        body.extend_from_slice(p);
    } else {
        body.extend_from_slice(&[0x05, 0x00]);
    }
    tlv(tag, &body)
}

#[test]
fn rsa_oaep_sha256_and_explicit_params() {
    let mut g = guard();
    // OAEP-SHA-256 needs a modulus ≥ 2·32+2 = 66 bytes; 2048-bit keeps the
    // fixture well inside it (512-bit RSA in the other tests is only for v1.5).
    let key = test_rsa_key(2048);
    let cek = [0x5au8; 16];
    let payload = seed_payload(0x63, 0xFFFF_F0C0);
    let encrypted_key = rsa_oaep_wrap(&key, &cek, OaepHash::Sha256);
    // RSAES-OAEP-params ::= SEQUENCE { hashAlgorithm [0] SHA-256,
    // maskGenAlgorithm [1] MGF1 { [0] SHA-256 } }.
    let sha256: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 2, 1];
    let hash = implicit_algorithm(0xA0, sha256, None);
    let mut mgf_body = oid_bytes(&[1, 2, 840, 113_549, 1, 1, 8]); // id-MGF1
    mgf_body.extend_from_slice(&implicit_algorithm(0xA0, sha256, None));
    let mgf = tlv(0xA1, &mgf_body);
    let mut params_body = Vec::new();
    params_body.extend_from_slice(&hash);
    params_body.extend_from_slice(&mgf);
    let params = tlv(0x30, &params_body);
    let blob = oaep_envelope(
        &encrypted_key,
        &cek,
        CekAlgorithm::Aes128Cbc { iv: [0x08; 16] },
        &payload,
        Some(&params),
    );
    let key_pkcs8 = rsa_to_pkcs8(&key);
    let auth = authenticate_public_key(
        &[&blob],
        &PubKeyCredential::rsa(key_pkcs8.as_slice().to_vec()),
        128,
        false,
        true,
        &mut g,
    )
    .expect("oaep sha256 opens");
    assert_eq!(auth.permissions, 0xFFFF_F0C0);
}

#[test]
fn unsupported_oaep_variant_is_typed() {
    let mut g = guard();
    let key = test_rsa_key(512);
    let cek = [0x5au8; 16];
    let payload = seed_payload(0x63, 0xFFFF_F0C0);
    // The *wrapped* key uses a SHA-1-able size (a 512-bit modulus cannot hold
    // an OAEP-SHA-256 block); only the DER parameters matter to this test.
    let encrypted_key = rsa_oaep_wrap(&key, &cek, OaepHash::Sha1);
    // An unknown *label* hash is refused at parse time (not scanned away —
    // §5), even though the transport could otherwise be opened with the SHA-1
    // arm's `RecipientNoMatch`.
    let md5: &[u64] = &[1, 3, 6, 1, 4, 1, 2, 1, 14, 2, 5];
    let hash = implicit_algorithm(0xA0, md5, None);
    let params = tlv(0x30, &hash);
    let blob = oaep_envelope(
        &encrypted_key,
        &cek,
        CekAlgorithm::Aes128Cbc { iv: [0x07; 16] },
        &payload,
        Some(&params),
    );
    let e = parse_enveloped_data(&blob, &mut g).expect_err("md5 label refused");
    assert_eq!(e.code(), Code::EncryptUnsupported);
}

/// RFC 8017 A.2.1's OAEP parameter slots in all their shapes: the *absence* of
/// `pSourceAlgorithm` (the DEFAULT) and a present-but-empty label both
/// authenticate; a *non-empty* label is refused typed (§5: a labelled unwrap
/// cannot detect that the label differed), as is a pSource OID that is not
/// id-PSpecified.
#[test]
fn rsa_oaep_psource_param_variants() {
    let mut g = guard();
    let key = test_rsa_key(512);
    let cek = [0x5au8; 16];
    let payload = seed_payload(0x63, 0xFFFF_F0C0);
    let encrypted_key = rsa_oaep_wrap(&key, &cek, OaepHash::Sha1);
    let sha: &[u64] = &[1, 3, 14, 3, 2, 26];
    let mut digest_body = oid_bytes(sha);
    digest_body.extend_from_slice(&[0x05, 0x00]);
    let pspec: &[u64] = &[1, 2, 840, 113_549, 1, 1, 9];
    let mut empty_body = oid_bytes(pspec);
    empty_body.extend_from_slice(&tlv(0x04, &[]));
    let mut other_arcs = oid_bytes(&[1, 2, 840, 113_549, 1, 9, 16, 3, 9]);
    other_arcs.extend_from_slice(&tlv(0x04, &[]));
    let mut labelled = oid_bytes(pspec);
    labelled.extend_from_slice(&tlv(0x04, b"pdf!"));
    for (what, psource, opens) in [
        ("absent", Vec::new(), true),
        ("empty label", tlv(0xA2, &empty_body), true),
        ("non-empty label", tlv(0xA2, &labelled), false),
        ("foreign pSource OID", tlv(0xA2, &other_arcs), false),
    ] {
        let mut pr = tlv(0xA0, &digest_body);
        pr.extend_from_slice(&psource);
        let params = tlv(0x30, &pr);
        let blob = oaep_envelope(
            &encrypted_key,
            &cek,
            CekAlgorithm::Aes128Cbc { iv: [0x07; 16] },
            &payload,
            Some(&params),
        );
        if opens {
            let auth = authenticate_public_key(
                &[&blob],
                &PubKeyCredential::rsa(rsa_to_pkcs8(&key)),
                128,
                false,
                true,
                &mut g,
            )
            .unwrap_or_else(|e| panic!("oaep pSource {what} must authenticate: {:?}", e.code()));
            assert_eq!(auth.permissions, 0xFFFF_F0C0);
        } else {
            let e = parse_enveloped_data(&blob, &mut g)
                .expect_err("oaep pSource must be refused typed");
            assert_eq!(e.code(), Code::EncryptUnsupported, "oaep pSource {what}");
        }
    }
}
