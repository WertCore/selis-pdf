//! `xtask pubkey-fixtures` — SL-1.ENC.03's committed public-key fixtures.
//!
//! Generates the deterministic fixture PDFs in
//! `crates/selis-pdf-engine/tests/fixtures/` (or `--check`s them against
//! their generator, the SL-2.PERF.02 render-set pattern: regenerate in
//! memory and byte-compare).
//!
//! The generator is deliberately independent of `selis-crypto::pkcs7`: it
//! builds the CMS envelopes (RFC 5652), the self-signed X.509 certificates
//! that address the recipients (SL-1.ENC.07: real `issuerAndSerialNumber` /
//! `subjectKeyIdentifier` identifiers quoted from the certificates), and
//! derives the file key straight from the spec's Algorithm 1 primitives
//! (SHA-1/SHA-256 of seed ‖ recipients), so the engine DoD tests
//! cross-check the handler instead of trusting it. Determinism: RSAES-PKCS1
//! -v1_5 padding draws from a fixed xorshift RNG, and the fixture
//! certificates carry a fixed placeholder `signatureValue` — recipient
//! selection reads only identity fields and **never verifies certificate
//! signatures** (the design note §4.1; the unwrap itself proves key
//! possession) — so the fixture bytes reproduce exactly on every machine
//! without a signing code path.

use std::path::Path;

// ---------------------------------------------------------------------------
// A deterministic RNG for the v1.5 padding (fixture bytes must reproduce)
// ---------------------------------------------------------------------------

/// xorshift64* — not a CSPRNG, and deliberately so: the same seed must
/// produce the same fixture bytes on every machine.
struct FixedRng(u64);

impl FixedRng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

impl rsa::rand_core::CryptoRng for FixedRng {}

impl rsa::rand_core::RngCore for FixedRng {
    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    fn next_u64(&mut self) -> u64 {
        self.next_u64()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(8) {
            let v = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rsa::rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DER writer (fixture building only — the shipped code is decrypt-only per
// ADR-P0019)
// ---------------------------------------------------------------------------

/// Encode a TLV with DER length octets.
fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = content.len();
    if n < 0x80 {
        out.push(n as u8);
    } else if n <= 0xFF {
        out.extend_from_slice(&[0x81, n as u8]);
    } else {
        out.extend_from_slice(&[0x82, (n >> 8) as u8, (n & 0xFF) as u8]);
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

/// An `AlgorithmIdentifier` with an optional pre-encoded parameter TLV.
fn algorithm_identifier(oid_arcs: &[u64], param: Option<&[u8]>) -> Vec<u8> {
    let mut body = oid_bytes(oid_arcs);
    if let Some(p) = param {
        body.extend_from_slice(p);
    }
    tlv(0x30, &body)
}

/// A `KeyTransRecipientInfo` (rsaEncryption) addressed by `rid` (the
/// pre-encoded `RecipientIdentifier` TLV — SL-1.ENC.07: a real certificate
/// identifier, not a placeholder).
fn key_trans_recipient_with_rid(rid: &[u8], encrypted_key: &[u8]) -> Vec<u8> {
    let mut body = integer_bytes(0); // version
    body.extend_from_slice(rid);
    body.extend_from_slice(&algorithm_identifier(
        &[1, 2, 840, 113_549, 1, 1, 1],
        Some(&[0x05, 0x00]), // NULL
    ));
    body.extend_from_slice(&tlv(0x04, encrypted_key));
    tlv(0x30, &body)
}

/// A `KeyAgreeRecipientInfo` ([1] arm) with one originator point and one
/// AES-KW-wrapped CEK, addressed by `rid`.
fn key_agree_recipient_with_rid(originator_point: &[u8], wrapped: &[u8], rid: &[u8]) -> Vec<u8> {
    let mut bit_string = vec![0u8]; // no unused bits
    bit_string.extend_from_slice(originator_point);
    let mut opk_body = algorithm_identifier(&[1, 2, 840, 10045, 2, 1], None); // id-ecPublicKey
    opk_body.extend_from_slice(&tlv(0x03, &bit_string));
    let originator = tlv(0xA1, &tlv(0x30, &opk_body)); // [1] originatorKey
    let originator_wrapper = tlv(0xA0, &originator); // [0] EXPLICIT CHOICE

    let mut body = integer_bytes(3); // version
    body.extend_from_slice(&originator_wrapper);
    body.extend_from_slice(&algorithm_identifier(
        // dhSinglePass-stdDH-sha256kdf-scheme (RFC 5753 §3.1)
        &[1, 3, 133, 16, 840, 63, 0, 4],
        None,
    ));
    let mut entry = rid.to_vec();
    entry.extend_from_slice(&tlv(0x04, wrapped));
    body.extend_from_slice(&tlv(0x30, &tlv(0x30, &entry)));
    tlv(0xA1, &tlv(0x30, &body))
}

/// A full `ContentInfo`/`EnvelopedData` blob.
fn enveloped_blob(recipients: &[Vec<u8>], content_algorithm: &[u64], content: &[u8]) -> Vec<u8> {
    let mut set_body = Vec::new();
    for r in recipients {
        set_body.extend_from_slice(r);
    }
    let mut alg = oid_bytes(content_algorithm);
    alg.extend_from_slice(&tlv(0x04, &AES_IV));
    let mut eci_body = oid_bytes(&[1, 2, 840, 113_549, 1, 7, 1]); // id-data
    eci_body.extend_from_slice(&tlv(0x30, &alg));
    eci_body.extend_from_slice(&tlv(0xA0, &tlv(0x04, content)));
    let mut env_body = integer_bytes(2);
    env_body.extend_from_slice(&tlv(0x31, &set_body));
    env_body.extend_from_slice(&tlv(0x30, &eci_body));
    let mut ci_body = oid_bytes(&[1, 2, 840, 113_549, 1, 7, 3]); // id-envelopedData
    ci_body.extend_from_slice(&tlv(0xA0, &tlv(0x30, &env_body)));
    tlv(0x30, &ci_body)
}

// ---------------------------------------------------------------------------
// Crypto fixture primitives
// ---------------------------------------------------------------------------

/// The fixed AES-CBC IV of the fixture envelopes (deterministic bytes).
const AES_IV: [u8; 16] = [0x5Au8; 16];

/// The 20-byte seed and 4-byte permission block of the fixtures.
const SEED: [u8; 20] = *b"Selis ENC.03 seed!!!";
const PERMISSIONS: u32 = 0xFFFF_F0C0;

/// SL-1.ENC.09 per-recipient grants (table-23 bit layout — the same
/// positions the shipped `selis_policy::Permissions` accessors read and
/// `ContentOp` gates on: bit 3 print, 4 modify, 5 copy/extract, 6
/// annotate, 9 fill, 10 extract-a11y, 11 assemble, 12 high-quality print;
/// the upper bits follow the all-set `/P -4` writer convention).
///
/// View-only deliberately clears every content-touching bit, including
/// fill (bit 9): a viewer holds the lowest grant the format can express.
const GRANT_ALL: u32 = 0xFFFF_FE78; // print|modify|copy|annotate|fill|a11y|assemble|hq
const GRANT_EXTRACT: u32 = 0xFFFF_F020; // copy/extract only (bit 5)
const GRANT_VIEW: u32 = 0xFFFF_F000; // no functional bits set

/// The page content stream of the fixtures (plaintext; encrypted per object).
const PAGE_CONTENT: &[u8] = b"0 0 1 rg 10 10 180 180 re f\n";

/// An AES block handle for the generator.
enum Cipher {
    A128(aes::Aes128),
    A256(aes::Aes256),
}

impl Cipher {
    fn new(key: &[u8]) -> Self {
        use aes::cipher::KeyInit;
        match key.len() {
            16 => Cipher::A128(aes::Aes128::new_from_slice(key).expect("key")),
            32 => Cipher::A256(aes::Aes256::new_from_slice(key).expect("key")),
            _ => panic!("fixture keys are 16 or 32 bytes"),
        }
    }

    fn encrypt_block(&self, block: &mut aes::Block) {
        use aes::cipher::BlockEncrypt;
        match self {
            Cipher::A128(c) => c.encrypt_block(block),
            Cipher::A256(c) => c.encrypt_block(block),
        }
    }
}

/// AES-128/256-CBC encrypt with PKCS#7 padding; IV from [`AES_IV`],
/// ciphertext without the IV (CMS keeps it in the algorithm parameters).
fn cbc_encrypt(cek: &[u8], payload: &[u8]) -> Vec<u8> {
    let cipher = Cipher::new(cek);
    let pad = 16 - (payload.len() % 16);
    let mut padded = payload.to_vec();
    padded.extend(std::iter::repeat_n(pad as u8, pad));
    let mut out = Vec::new();
    let mut prev = AES_IV;
    for chunk in padded.chunks(16) {
        let block_bytes: [u8; 16] = chunk.try_into().expect("block");
        let mut block = aes::Block::clone_from_slice(&block_bytes);
        for k in 0..16 {
            block[k] ^= prev[k];
        }
        cipher.encrypt_block(&mut block);
        out.extend_from_slice(block.as_slice());
        prev.copy_from_slice(block.as_slice());
    }
    out
}

/// RFC 3394 AES key wrap (fixture-side mirror of the reader under test).
fn aes_kw_wrap(kek: &[u8], key: &[u8]) -> Vec<u8> {
    let cipher = Cipher::new(kek);
    let n = key.len() / 8;
    let mut a: [u8; 8] = [0xA6; 8];
    let mut r: Vec<[u8; 8]> = Vec::new();
    for i in 0..n {
        r.push(key[i * 8..(i + 1) * 8].try_into().expect("block"));
    }
    for j in 0..6usize {
        for i in 0..n {
            let t = (j * n + i + 1) as u64;
            let mut input = [0u8; 16];
            input[..8].copy_from_slice(&a);
            input[8..].copy_from_slice(&r[i]);
            let mut block = aes::Block::clone_from_slice(&input);
            cipher.encrypt_block(&mut block);
            let t_bytes = t.to_be_bytes();
            for k in 0..8 {
                a[k] = block[k];
            }
            for k in 0..8 {
                a[k] ^= t_bytes[k];
            }
            r[i].copy_from_slice(&block[8..16]);
        }
    }
    let mut out = a.to_vec();
    for block in &r {
        out.extend_from_slice(block);
    }
    out
}

/// X9.63 KDF with SHA-256 and an empty SharedInfo (RFC 5753 §2.1.1).
fn x963_kdf_sha256(z: &[u8], out_len: usize) -> Vec<u8> {
    use sha2::Digest;
    let mut out = Vec::new();
    let mut counter: u32 = 1;
    while out.len() < out_len {
        let mut h = sha2::Sha256::new();
        h.update(z);
        h.update(counter.to_be_bytes());
        out.extend_from_slice(h.finalize().as_slice());
        counter += 1;
    }
    out.truncate(out_len);
    out
}

/// The per-object stream encryption of [`selis_crypto::encrypt_data`] with a
/// FIXED IV (the shipped function draws a fresh IV per call for
/// indistinguishability; the fixture must be byte-reproducible). Mirrors
/// `decrypt_data` exactly: r ≥ 5 uses the file key directly with AES-256-CBC
/// and an IV prefix; r 4 uses the MD5-salted key (`fileKey ‖ objnum_le24 ‖
/// gen_le16 ‖ "sAlT"`, truncated to 16) with AES-128-CBC and an IV prefix;
/// padding is PKCS#7.
fn encrypt_stream_fixture(
    file_key: &[u8],
    objnum: u32,
    gen: u16,
    data: &[u8],
    r: u8,
    iv: &[u8; 16],
) -> Vec<u8> {
    use md5::{Digest as _, Md5};
    let padded = {
        let pad = 16 - (data.len() % 16);
        let mut p = data.to_vec();
        p.extend(std::iter::repeat_n(pad as u8, pad));
        p
    };
    let (cipher, iv): (Cipher, &[u8; 16]) = if r >= 5 {
        (Cipher::new(&file_key[..file_key.len().min(32)]), iv)
    } else {
        let mut hasher = Md5::new();
        hasher.update(file_key);
        hasher.update(&objnum.to_le_bytes()[..3]);
        hasher.update(gen.to_le_bytes());
        hasher.update(b"sAlT");
        let mut obj_key = hasher.finalize().to_vec();
        obj_key.truncate(file_key.len().saturating_add(5).min(16));
        obj_key.resize(16, 0);
        (Cipher::new(&obj_key), iv)
    };
    let mut out = iv.to_vec();
    let mut prev: [u8; 16] = *iv;
    for chunk in padded.chunks(16) {
        let block_bytes: [u8; 16] = chunk.try_into().expect("block");
        let mut block = aes::Block::clone_from_slice(&block_bytes);
        for k in 0..16 {
            block[k] ^= prev[k];
        }
        cipher.encrypt_block(&mut block);
        out.extend_from_slice(block.as_slice());
        prev.copy_from_slice(block.as_slice());
    }
    out
}

/// The fixed per-object IV of the fixture streams (deterministic bytes).
const STREAM_IV: [u8; 16] = [0x3Du8; 16];

/// The AES-CBC OID for the fixture's `/V`.
fn aes_oid(v: u32) -> &'static [u64] {
    if v >= 5 {
        &[2, 16, 840, 1, 101, 3, 4, 1, 42] // aes256-CBC
    } else {
        &[2, 16, 840, 1, 101, 3, 4, 1, 2] // aes128-CBC
    }
}

// ---------------------------------------------------------------------------
// Self-signed fixture certificates (SL-1.ENC.07)
// ---------------------------------------------------------------------------

/// What kind of CMS `RecipientIdentifier` addresses a recipient.
#[derive(Clone, Copy)]
enum IdKind {
    /// `issuerAndSerialNumber` (what Acrobat writes for RSA recipients).
    IssuerSerial,
    /// `subjectKeyIdentifier [0]` (the SKI form).
    Ski,
}

/// One fixture recipient: which committed key, the grant, and the
/// identifier form.
#[derive(Clone, Copy)]
struct Recipient {
    key: KeySlot,
    perms: u32,
    id: IdKind,
}

/// Which committed test key a recipient slot uses.
#[derive(Clone, Copy)]
enum KeySlot {
    /// The committed RSA-2048 recipient key ("Alice").
    Rsa,
    /// The committed EC P-256 recipient key.
    Ec,
    /// The second committed RSA-2048 key ("Bob") — a throwaway that is a
    /// *wrong key* for the ENC.03 fixtures but a legitimate recipient of the
    /// ENC.09 permission fixture.
    RsaBob,
}

/// The certificate identity material of one fixture recipient: the cert DER
/// to commit, the DER content of its issuer `Name`, the DER content of its
/// serial, and its 20-byte SKI.
struct Cert {
    der: Vec<u8>,
    issuer: Vec<u8>,
    serial: Vec<u8>,
    ski: [u8; 20],
}

/// The committed fixture keys (PKCS#8 DER, test-only throwaways).
const RECIPIENT_RSA: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../crates/selis-pdf-engine/tests/fixtures/enc03-recipient-rsa2048.pk8.der"
));
const RECIPIENT_EC: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../crates/selis-pdf-engine/tests/fixtures/enc03-recipient-ecp256.pk8.der"
));
const RECIPIENT_RSA_BOB: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../crates/selis-pdf-engine/tests/fixtures/enc03-wrong-rsa2048.pk8.der"
));

/// Per-slot certificate parameters: CN label and serial (the SL-1.ENC.07
/// identifiers are quoted from these certificates).
fn cert_for(slot: KeySlot) -> Cert {
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    use rsa::pkcs1::EncodeRsaPublicKey;
    use rsa::pkcs8::DecodePrivateKey;
    use sha1::Digest as _;

    match slot {
        KeySlot::Rsa | KeySlot::RsaBob => {
            let bob = matches!(slot, KeySlot::RsaBob);
            let der = if bob {
                RECIPIENT_RSA_BOB
            } else {
                RECIPIENT_RSA
            };
            let key = rsa::RsaPrivateKey::from_pkcs8_der(der).expect("fixture key");
            let pkcs1 = key.to_public_key().to_pkcs1_der().expect("rsa spki");
            // SubjectPublicKeyInfo: rsaEncryption + NULL, BIT STRING.
            let mut bit = vec![0u8];
            bit.extend_from_slice(pkcs1.as_bytes());
            let spki = {
                let mut alg = oid_bytes(&[1, 2, 840, 113_549, 1, 1, 1]);
                alg.extend_from_slice(&[0x05, 0x00]);
                let mut b = tlv(0x30, &alg);
                b.extend_from_slice(&tlv(0x03, &bit));
                tlv(0x30, &b)
            };
            make_cert(
                if bob {
                    "Selis ENC.09 Bob (test)"
                } else {
                    "Selis ENC.03 Alice (test)"
                },
                if bob { 84 } else { 83 },
                spki,
                // sha1 over the BIT STRING key octets (RFC 5280 method 1),
                // carried as the certificate's subjectKeyIdentifier
                // extension — the arm the RSA-SKI recipient exercises.
                sha1::Sha1::digest(pkcs1.as_bytes()).as_slice(),
                true,
                false,
            )
        }
        KeySlot::Ec => {
            let secret = p256::SecretKey::from_pkcs8_der(RECIPIENT_EC).expect("fixture key");
            let point = secret.public_key().to_encoded_point(false);
            let spki = {
                let mut alg = oid_bytes(&[1, 2, 840, 10_045, 2, 1]); // id-ecPublicKey
                alg.extend_from_slice(&oid_bytes(&[1, 2, 840, 10_045, 3, 1, 7])); // P-256
                let mut b = tlv(0x30, &alg);
                let mut bit = vec![0u8];
                bit.extend_from_slice(point.as_bytes());
                b.extend_from_slice(&tlv(0x03, &bit));
                tlv(0x30, &b)
            };
            make_cert(
                "Selis ENC.03 EC (test)",
                91,
                spki,
                // The EC cert carries no SKI extension; consumers match the
                // method-1 SHA-1 over the BIT STRING content (the fallback
                // mainstream viewers apply).
                sha1::Sha1::digest(point.as_bytes()).as_slice(),
                false,
                true,
            )
        }
    }
}

/// Assemble one v3 self-signed certificate with `spki` as the
/// SubjectPublicKeyInfo and `ski` (20 bytes) as the certificate's
/// `subjectKeyIdentifier` extension value (when `extension`) and the CMS
/// identifier source. The signature value is a fixed deterministic BIT
/// STRING: **nothing in Selis verifies certificate signatures** — recipient
/// selection reads only identity fields (RFC 5280 §4.2.1.2, design note
/// §4.1), and the unwrap itself proves key possession.
fn make_cert(
    label: &str,
    serial: u64,
    spki: Vec<u8>,
    ski: &[u8],
    extension: bool,
    ec: bool,
) -> Cert {
    let subject = rdn_sequence(label);
    let issuer = subject.clone();
    let serial_tlv = integer_bytes(serial);

    let sig_alg = {
        let mut a = if ec {
            oid_bytes(&[1, 2, 840, 10_045, 4, 3, 2]) // ecdsa-with-SHA256
        } else {
            oid_bytes(&[1, 2, 840, 113_549, 1, 1, 11]) // sha256WithRSAEncryption
        };
        if !ec {
            a.extend_from_slice(&[0x05, 0x00]); // ECDSA identifiers carry no params
        }
        tlv(0x30, &a)
    };
    let validity = tlv(
        0x30,
        &[
            tlv(0x17, b"260101000000Z"), // notBefore (UTCTime)
            tlv(0x17, b"991231235959Z"), // notAfter
        ]
        .concat(),
    );
    let mut tbs = tlv(0xA0, &integer_bytes(2)); // version v3
    tbs.extend_from_slice(&serial_tlv);
    tbs.extend_from_slice(&sig_alg);
    tbs.extend_from_slice(&issuer);
    tbs.extend_from_slice(&validity);
    tbs.extend_from_slice(&subject);
    tbs.extend_from_slice(&spki);
    if extension {
        // subjectKeyIdentifier ::= OCTET STRING, wrapped in its extnValue.
        let ext = {
            let mut b = oid_bytes(&[2, 5, 29, 14]);
            b.extend_from_slice(&tlv(0x04, &tlv(0x04, ski)));
            tlv(0x30, &b)
        };
        tbs.extend_from_slice(&tlv(0xA3, &tlv(0x30, &ext)));
    }
    let tbs = tlv(0x30, &tbs);

    let mut cert = tbs;
    cert.extend_from_slice(&sig_alg);
    let mut sig = vec![0u8];
    sig.extend_from_slice(b"Selis test cert: signature not verified");
    cert.extend_from_slice(&tlv(0x03, &sig));
    let der = tlv(0x30, &cert);

    // The issuer Name TLV content and the serial INTEGER content, exactly
    // as the identity parser will find them.
    let issuer_content = issuer.get(2..).map(<[u8]>::to_vec).unwrap_or_default();
    let serial_content = serial_tlv.get(2..).map(<[u8]>::to_vec).unwrap_or_default();
    let mut ski_bytes = [0u8; 20];
    ski_bytes[..ski.len().min(20)].copy_from_slice(&ski[..ski.len().min(20)]);
    Cert {
        der,
        issuer: issuer_content,
        serial: serial_content,
        ski: ski_bytes,
    }
}

/// A `Name` (RDNSequence) holding one `Cn=label` attribute.
fn rdn_sequence(label: &str) -> Vec<u8> {
    let atv = {
        let mut b = oid_bytes(&[2, 5, 4, 3]); // cn
        b.extend_from_slice(&tlv(0x13, label.as_bytes())); // PrintableString
        tlv(0x30, &b)
    };
    tlv(0x30, &tlv(0x31, &atv))
}

/// The CMS `RecipientIdentifier` TLV for a fixture recipient.
fn recipient_rid(cert: &Cert, kind: IdKind) -> Vec<u8> {
    match kind {
        IdKind::IssuerSerial => {
            let mut body = tlv(0x30, &cert.issuer);
            body.extend_from_slice(&tlv(0x02, &cert.serial));
            tlv(0x30, &body)
        }
        IdKind::Ski => tlv(0x80, &cert.ski),
    }
}

/// The fixed xorshift seed for the v1.5 padding (fixture determinism).
const FIXED_RNG_SEED: u64 = 0x5E1D_5E1D_5E1D_5E1D;

/// One recipient's `/Recipients` blob for revision `v` (shared `cek`).
/// Also used by [`fuzz_seeds`] to emit the SL-1.ENC.07 fuzz inputs.
fn recipient_blob(v: u32, cek: &[u8], r: &Recipient) -> Vec<u8> {
    let cert = cert_for(r.key);
    let rid = recipient_rid(&cert, r.id);
    let mut payload = SEED.to_vec();
    payload.extend_from_slice(&r.perms.to_le_bytes());
    let content = cbc_encrypt(cek, &payload);
    match r.key {
        KeySlot::Rsa | KeySlot::RsaBob => {
            use rsa::pkcs8::DecodePrivateKey;
            let der = match r.key {
                KeySlot::RsaBob => RECIPIENT_RSA_BOB,
                _ => RECIPIENT_RSA,
            };
            let key = rsa::RsaPrivateKey::from_pkcs8_der(der).expect("fixture key");
            let mut rng = FixedRng(FIXED_RNG_SEED);
            let encrypted = key
                .to_public_key()
                .encrypt(&mut rng, rsa::Pkcs1v15Encrypt, cek)
                .expect("rsa encrypt");
            enveloped_blob(
                &[key_trans_recipient_with_rid(&rid, &encrypted)],
                aes_oid(v),
                &content,
            )
        }
        KeySlot::Ec => {
            use p256::elliptic_curve::sec1::ToEncodedPoint;
            use p256::pkcs8::DecodePrivateKey;
            let secret = p256::SecretKey::from_pkcs8_der(RECIPIENT_EC).expect("fixture key");
            let public = secret.public_key();
            let point = public.to_encoded_point(false);
            let shared = p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), public.as_affine());
            let shared_bytes = *shared.raw_secret_bytes();
            let kek = x963_kdf_sha256(shared_bytes.as_slice(), cek.len());
            let wrapped = aes_kw_wrap(&kek, cek);
            enveloped_blob(
                &[key_agree_recipient_with_rid(
                    point.as_bytes(),
                    &wrapped,
                    &rid,
                )],
                aes_oid(v),
                &content,
            )
        }
    }
}

/// Build one public-key fixture PDF (and the certificates it addresses).
///
/// `v` is 4 (AESV2, RSA, direct `/Recipients`) or 5 (AESV3, crypt-filter
/// `/Recipients`). All recipients share the 20-byte seed (one file key for
/// the document); each carries its own 4-byte permission block, and the s5
/// fixtures declare an all-allowing `/P -4` so the ENC.09 intersection is
/// the only thing that can deny an operation.
fn build_fixture(v: u32, recipients: &[Recipient]) -> Vec<u8> {
    let length_bits = if v >= 5 { 256usize } else { 128 };
    let cek_len = if v >= 5 { 32usize } else { 16 };
    let cek: Vec<u8> = (0..cek_len).map(|i| 0x10u8 + i as u8).collect();

    let mut blobs: Vec<Vec<u8>> = Vec::new();
    for r in recipients {
        blobs.push(recipient_blob(v, &cek, r));
    }

    // Algorithm 1's file key: HASH(seed || every recipient blob), truncated
    // to /Length/8 — computed independently of selis-crypto::pkcs7.
    let mut input = SEED.to_vec();
    for blob in &blobs {
        input.extend_from_slice(blob);
    }
    let file_key: Vec<u8> = if v >= 5 {
        use sha2::Digest;
        sha2::Sha256::digest(&input).to_vec()[..32].to_vec()
    } else {
        use sha1::Digest as _;
        sha1::Sha1::digest(&input).to_vec()[..16].to_vec()
    };

    // The page content stream, encrypted with the per-object algorithm of
    // the handler revision (4 → salted AES-128, 5 → direct AES-256), using
    // the fixed IV (see [`encrypt_stream_fixture`]).
    let encrypted_stream =
        encrypt_stream_fixture(&file_key, 4, 0, PAGE_CONTENT, v as u8, &STREAM_IV);

    // The /Encrypt dictionary, s4 (direct /Recipients) or s5 (crypt filter)
    // shape, hex-string recipients like Acrobat writes. /P -4 is the
    // all-allowing permission set (bits 1–2 are the spec's must-be-zero
    // bits): every denial the SL-1.ENC.09 tests observe therefore comes from
    // the recipient's own CMS block, never from a missing /P grant.
    let mut encrypt_dict = format!(
        "<< /Filter /Adobe.PPKLite /V {v} /Length {length_bits} /P -4 /SubFilter /adbe.pkcs7.s{} /EncryptMetadata true",
        if v >= 5 { "5" } else { "4" }
    );
    if v >= 5 {
        encrypt_dict.push_str(" /StmF /DefaultCryptFilter /StrF /DefaultCryptFilter");
        encrypt_dict.push_str(&format!(
            " /CF << /DefaultCryptFilter << /CFM /AESV3 /Length {length_bits} /AuthEvent /DocOpen /Recipients [{}]",
            hex_array(&blobs)
        ));
        encrypt_dict.push_str(" >> >>");
    } else {
        encrypt_dict.push_str(&format!(" /Recipients [{}]", hex_array(&blobs)));
    }
    encrypt_dict.push_str(" >>");

    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R >>"
            .to_vec(),
        format!("<< /Length {} >>", encrypted_stream.len()).into_bytes(),
        encrypt_dict.into_bytes(),
    ];

    assemble_pdf(&objects, &encrypted_stream)
}

/// The hex-string array of the /Recipients entry (`<hex>` per blob).
fn hex_array(blobs: &[Vec<u8>]) -> String {
    blobs
        .iter()
        .map(|b| {
            let hex: String = b.iter().map(|byte| format!("{byte:02X}")).collect();
            format!("<{hex}>")
        })
        .collect()
}

/// Assemble the objects into a PDF with a classic xref table. Object 4's
/// stream body is spliced from `stream_body` (the dict's /Length was written
/// to match).
fn assemble_pdf(objects: &[Vec<u8>], stream_body: &[u8]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(&format!("{} 0 obj\n", i + 1).into_bytes());
        out.extend_from_slice(obj);
        if i == 3 {
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(stream_body);
            out.extend_from_slice(b"\nendstream");
        }
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_at = out.len();
    out.extend_from_slice(&format!("xref\n0 {}\n", objects.len() + 1).into_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(&format!("{offset:010} 00000 n \n").into_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Encrypt 5 0 R /ID [<0123456789abcdef0123456789abcdef> <0123456789abcdef0123456789abcdef>] >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            xref_at
        )
        .as_bytes(),
    );
    out
}

/// The fixtures: four deterministic public-key PDFs plus the three
/// self-signed certificates that address their recipients (the
/// `enc03-*` stems match the committed key stems). Entries are
/// `(filename, bytes)`.
fn build_all() -> Vec<(&'static str, Vec<u8>)> {
    let rsa_alice = Recipient {
        key: KeySlot::Rsa,
        perms: PERMISSIONS,
        id: IdKind::IssuerSerial,
    };
    let ec = Recipient {
        key: KeySlot::Ec,
        perms: PERMISSIONS,
        id: IdKind::Ski,
    };
    vec![
        ("pubkey-rsa-s4.pdf", build_fixture(4, &[rsa_alice])),
        ("pubkey-ec-s5.pdf", build_fixture(5, &[ec])),
        (
            "pubkey-multi-s5.pdf",
            build_fixture(
                5,
                &[
                    Recipient {
                        key: KeySlot::Ec,
                        perms: PERMISSIONS,
                        id: IdKind::Ski,
                    },
                    rsa_alice,
                ],
            ),
        ),
        // SL-1.ENC.09: three recipients, three different grants, all-allowing
        // /P — every denial must come from the recipient's own CMS block.
        (
            "pubkey-perms-s5.pdf",
            build_fixture(
                5,
                &[
                    Recipient {
                        key: KeySlot::Rsa,
                        perms: GRANT_ALL,
                        id: IdKind::IssuerSerial,
                    },
                    Recipient {
                        key: KeySlot::RsaBob,
                        perms: GRANT_EXTRACT,
                        id: IdKind::Ski,
                    },
                    Recipient {
                        key: KeySlot::Ec,
                        perms: GRANT_VIEW,
                        id: IdKind::IssuerSerial,
                    },
                ],
            ),
        ),
        // The SL-1.ENC.07 chains (identity fields quoted by the blobs above).
        (
            "enc03-recipient-rsa2048.x509.der",
            cert_for(KeySlot::Rsa).der,
        ),
        (
            "enc03-wrong-rsa2048.x509.der",
            cert_for(KeySlot::RsaBob).der,
        ),
        ("enc03-recipient-ecp256.x509.der", cert_for(KeySlot::Ec).der),
    ]
}

/// Write the fixtures into `dir` (regeneration path).
pub fn run(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, bytes) in build_all() {
        let dest = dir.join(name);
        std::fs::write(&dest, &bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
        println!("wrote {}", dest.display());
    }
    Ok(())
}

/// Regenerate in memory and byte-compare against the committed fixtures.
pub fn check(dir: &Path) -> Result<(), String> {
    for (name, bytes) in build_all() {
        let dest = dir.join(name);
        let on_disk = std::fs::read(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        if on_disk != bytes {
            return Err(format!(
                "{name} drifts from its generator ({} on disk, {} generated); \
                 rerun `xtask pubkey-fixtures` and commit the result",
                on_disk.len(),
                bytes.len()
            ));
        }
    }
    println!("pubkey-fixtures: 4 fixtures and 3 certificates match their generator");
    Ok(())
}

/// Write the SL-1.ENC.07/09 fuzz seed corpora under `root`
/// (`cargo xtask pubkey-fuzz-seeds`): CMS blobs addressed by real
/// certificate identities — including the wrong-serial and
/// duplicate-identifier inputs the selection fuzz must never resolve to a
/// wrong recipient — the X.509 chains with corruptions, and exact/edge-size
/// permission payloads for the decoder target. Deterministic: the same
/// generators that build the committed fixtures.
pub fn fuzz_seeds(root: &Path) -> Result<(), String> {
    let cek: Vec<u8> = (0..32u8).map(|i| 0x10u8 + i).collect();
    let alice = cert_for(KeySlot::Rsa);
    let bob = cert_for(KeySlot::RsaBob);
    let carol = cert_for(KeySlot::Ec);

    // --- pkcs7_cms / selection inputs -------------------------------------
    let r = |key: KeySlot, id: IdKind, perms: u32| Recipient { key, id, perms };
    let mut cms: Vec<(&str, Vec<u8>)> = Vec::new();
    cms.push((
        "cms-alice-isn",
        recipient_blob(5, &cek, &r(KeySlot::Rsa, IdKind::IssuerSerial, PERMISSIONS)),
    ));
    cms.push((
        "cms-bob-ski",
        recipient_blob(5, &cek, &r(KeySlot::RsaBob, IdKind::Ski, GRANT_EXTRACT)),
    ));
    cms.push((
        "cms-carol-ec-isn",
        recipient_blob(5, &cek, &r(KeySlot::Ec, IdKind::IssuerSerial, PERMISSIONS)),
    ));

    // Wrong serial: Alice's issuer quoted with a one-off serial number. The
    // identifier parses structurally but matches no chain the harness holds,
    // and the wrapped key is real — selection must behave by mode (typed in
    // Certificate, structural fall-back in Auto), never open as *someone
    // else's* identity.
    {
        use rsa::pkcs8::DecodePrivateKey;
        let mut flipped = alice.serial.clone();
        if let Some(last) = flipped.last_mut() {
            *last ^= 0x01;
        }
        let mut rid_body = tlv(0x30, &alice.issuer);
        rid_body.extend_from_slice(&tlv(0x02, &flipped));
        let rid = tlv(0x30, &rid_body);
        let key = rsa::RsaPrivateKey::from_pkcs8_der(RECIPIENT_RSA).expect("fixture key");
        let mut rng = FixedRng(FIXED_RNG_SEED);
        let enc = key
            .to_public_key()
            .encrypt(&mut rng, rsa::Pkcs1v15Encrypt, &cek)
            .expect("rsa encrypt");
        let mut payload = SEED.to_vec();
        payload.extend_from_slice(&PERMISSIONS.to_le_bytes());
        cms.push((
            "cms-wrong-serial",
            enveloped_blob(
                &[key_trans_recipient_with_rid(&rid, &enc)],
                aes_oid(5),
                &cbc_encrypt(&cek, &payload),
            ),
        ));
    }

    // Duplicate identifier: the same Bob-SKI `RecipientInfo` written twice
    // into one SET — every identifier candidate must be tried, deterministically.
    {
        use rsa::pkcs8::DecodePrivateKey;
        let rid = recipient_rid(&bob, IdKind::Ski);
        let key = rsa::RsaPrivateKey::from_pkcs8_der(RECIPIENT_RSA_BOB).expect("fixture key");
        let mut rng = FixedRng(FIXED_RNG_SEED);
        let enc = key
            .to_public_key()
            .encrypt(&mut rng, rsa::Pkcs1v15Encrypt, &cek)
            .expect("rsa encrypt");
        let mut payload = SEED.to_vec();
        payload.extend_from_slice(&GRANT_EXTRACT.to_le_bytes());
        let ktri = key_trans_recipient_with_rid(&rid, &enc);
        cms.push((
            "cms-duplicate-rid",
            enveloped_blob(
                &[ktri.clone(), ktri],
                aes_oid(5),
                &cbc_encrypt(&cek, &payload),
            ),
        ));
    }
    write_seeds(root, "pkcs7_cms", &cms)?;

    // --- x509_identity inputs ----------------------------------------------
    let mut certs: Vec<(&str, Vec<u8>)> = Vec::new();
    certs.push(("cert-rsa", alice.der.clone()));
    certs.push(("cert-rsa-bob", bob.der.clone()));
    certs.push(("cert-ec", carol.der.clone()));
    let mut truncated = alice.der.clone();
    truncated.truncate(truncated.len() / 2);
    certs.push(("cert-rsa-truncated", truncated));
    let mut flipped = alice.der.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0xFF;
    flipped[mid - 1] ^= 0x81;
    certs.push(("cert-rsa-corrupted", flipped));
    certs.push(("cert-empty-seq", vec![0x30, 0x00]));
    certs.push(("cert-length-lie", vec![0x30, 0x7F, 0x02, 0x01, 0x00]));
    write_seeds(root, "x509_identity", &certs)?;

    // --- permission decoder inputs -------------------------------------------
    let mut valid = SEED.to_vec();
    valid.extend_from_slice(&PERMISSIONS.to_le_bytes());
    let mut long = valid.clone();
    long.push(0x00);
    let perms: Vec<(&str, Vec<u8>)> = vec![
        ("perms-exact-24", valid.clone()),
        ("perms-short-23", valid[..23].to_vec()),
        ("perms-long-25", long),
        ("perms-empty", Vec::new()),
        ("perms-zero-24", vec![0u8; 24]),
        ("perms-max-24", {
            let mut v = SEED.to_vec();
            v.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
            v
        }),
    ];
    write_seeds(root, "pkcs7_perms", &perms)?;
    println!(
        "pubkey-fuzz-seeds: wrote seed corpora under {}",
        root.display()
    );
    Ok(())
}

fn write_seeds(root: &Path, target: &str, seeds: &[(&str, Vec<u8>)]) -> Result<(), String> {
    let dir = root.join("fuzz").join("seeds").join(target);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, bytes) in seeds {
        let dest = dir.join(name);
        std::fs::write(&dest, bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    Ok(())
}
