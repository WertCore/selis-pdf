//! `xtask pubkey-fixtures` — SL-1.ENC.03's committed public-key fixtures.
//!
//! Generates the three deterministic fixture PDFs in
//! `crates/selis-pdf-engine/tests/fixtures/` (or `--check`s them against
//! their generator, the SL-2.PERF.02 render-set pattern: regenerate in
//! memory and byte-compare).
//!
//! The generator is deliberately independent of `selis-crypto::pkcs7`: it
//! builds the CMS envelopes (RFC 5652) and derives the file key straight
//! from the spec's Algorithm 1 primitives (SHA-1/SHA-256 of seed ‖
//! recipients), so the engine DoD tests cross-check the handler instead of
//! trusting it. Determinism: RSAES-PKCS1-v1_5 padding draws from a fixed
//! xorshift RNG, so the fixture bytes are reproducible.

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

/// A `KeyTransRecipientInfo` (issuer-and-serial, rsaEncryption).
fn key_trans_recipient(encrypted_key: &[u8]) -> Vec<u8> {
    let mut rid_body = tlv(0x30, &[]); // issuer Name (empty RDNSequence)
    rid_body.extend_from_slice(&integer_bytes(1)); // serialNumber
    let rid = tlv(0x30, &rid_body);
    let mut body = integer_bytes(0); // version
    body.extend_from_slice(&rid);
    body.extend_from_slice(&algorithm_identifier(
        &[1, 2, 840, 113_549, 1, 1, 1],
        Some(&[0x05, 0x00]), // NULL
    ));
    body.extend_from_slice(&tlv(0x04, encrypted_key));
    tlv(0x30, &body)
}

/// A `KeyAgreeRecipientInfo` ([1] arm) with one originator point and one
/// AES-KW-wrapped CEK.
fn key_agree_recipient(originator_point: &[u8], wrapped: &[u8]) -> Vec<u8> {
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
    let mut entry = tlv(0x30, &[]); // rid placeholder
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
// Fixture assembly
// ---------------------------------------------------------------------------

/// Which recipient a fixture slot carries.
enum Recipient {
    /// The committed RSA-2048 recipient key.
    Rsa,
    /// The committed EC P-256 recipient key.
    Ec,
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

/// The fixed xorshift seed for the v1.5 padding (fixture determinism).
const FIXED_RNG_SEED: u64 = 0x5E1D_5E1D_5E1D_5E1D;

/// Build one public-key fixture PDF.
///
/// `v` is 4 (AESV2, RSA, direct `/Recipients`) or 5 (AESV3, crypt-filter
/// `/Recipients`).
fn build_fixture(v: u32, recipients: &[Recipient]) -> Vec<u8> {
    let (length_bits, cek_len) = if v >= 5 {
        (256usize, 32usize)
    } else {
        (128, 16)
    };
    let cek: Vec<u8> = (0..cek_len).map(|i| 0x10u8 + i as u8).collect();
    let mut payload = SEED.to_vec();
    payload.extend_from_slice(&PERMISSIONS.to_le_bytes());
    let content = cbc_encrypt(&cek, &payload);

    let mut blobs: Vec<Vec<u8>> = Vec::new();
    for kind in recipients {
        let blob = match kind {
            Recipient::Rsa => {
                use rsa::pkcs8::DecodePrivateKey;
                let key = rsa::RsaPrivateKey::from_pkcs8_der(RECIPIENT_RSA).expect("fixture key");
                let mut rng = FixedRng(FIXED_RNG_SEED);
                let encrypted = key
                    .to_public_key()
                    .encrypt(&mut rng, rsa::Pkcs1v15Encrypt, &cek)
                    .expect("rsa encrypt");
                enveloped_blob(&[key_trans_recipient(&encrypted)], aes_oid(v), &content)
            }
            Recipient::Ec => {
                use p256::elliptic_curve::sec1::ToEncodedPoint;
                use p256::pkcs8::DecodePrivateKey;
                let secret = p256::SecretKey::from_pkcs8_der(RECIPIENT_EC).expect("fixture key");
                let public = secret.public_key();
                let point = public.to_encoded_point(false);
                let shared =
                    p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), public.as_affine());
                let shared_bytes = *shared.raw_secret_bytes();
                let kek = x963_kdf_sha256(shared_bytes.as_slice(), cek_len);
                let wrapped = aes_kw_wrap(&kek, &cek);
                enveloped_blob(
                    &[key_agree_recipient(point.as_bytes(), &wrapped)],
                    aes_oid(v),
                    &content,
                )
            }
        };
        blobs.push(blob);
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
    // shape, hex-string recipients like Acrobat writes.
    let mut encrypt_dict = format!(
        "<< /Filter /Adobe.PPKLite /V {v} /Length {length_bits} /SubFilter /adbe.pkcs7.s{} /EncryptMetadata true",
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

/// The three fixtures, in commit order.
fn build_all() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("pubkey-rsa-s4", build_fixture(4, &[Recipient::Rsa])),
        ("pubkey-ec-s5", build_fixture(5, &[Recipient::Ec])),
        (
            "pubkey-multi-s5",
            build_fixture(5, &[Recipient::Ec, Recipient::Rsa]),
        ),
    ]
}

/// Write the fixtures into `dir` (regeneration path).
pub fn run(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (stem, pdf) in build_all() {
        let dest = dir.join(format!("{stem}.pdf"));
        std::fs::write(&dest, &pdf).map_err(|e| format!("{}: {e}", dest.display()))?;
        println!("wrote {}", dest.display());
    }
    Ok(())
}

/// Regenerate in memory and byte-compare against the committed fixtures.
pub fn check(dir: &Path) -> Result<(), String> {
    for (stem, pdf) in build_all() {
        let dest = dir.join(format!("{stem}.pdf"));
        let on_disk = std::fs::read(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        if on_disk != pdf {
            return Err(format!(
                "{stem}.pdf drifts from its generator ({} on disk, {} generated); \
                 rerun `xtask pubkey-fixtures` and commit the result",
                on_disk.len(),
                pdf.len()
            ));
        }
    }
    println!("pubkey-fixtures: 3 fixtures match their generator");
    Ok(())
}
