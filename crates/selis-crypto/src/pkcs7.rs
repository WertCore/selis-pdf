//! The PKCS#7/CMS public-key security handler, read side (SL-1.ENC.03,
//! ISO 32000-2 §7.6.6, ADR-P0019).
//!
//! PDF public-key encryption stores, per recipient, a CMS `ContentInfo` of
//! type `EnvelopedData` in the `/Encrypt` dictionary's `/Recipients` array.
//! Algorithm 1 of ISO 32000-1 §7.6.6 / ISO 32000-2 §7.6.6.4 recovers the file
//! encryption key:
//!
//! 1. Parse the recipient's blob as `ContentInfo` → `EnvelopedData`
//!    ([`parse_enveloped_data`]).
//! 2. Match a `RecipientInfo` and unwrap the content-encryption key (CEK)
//!    with the recipient's private key (RSA key transport; ECDH key
//!    agreement with AES key wrap for the EC path).
//! 3. Decrypt the `EncryptedContentInfo`'s content with the CEK. The payload
//!    is exactly 24 bytes: a 20-byte **seed** followed by the recipient's
//!    4 permission bytes.
//! 4. The file encryption key is `HASH(seed || every recipient blob
//!    [|| 0xFFFFFFFF])` truncated to `/Length`/8 — SHA-256 for `/V 5`
//!    (AESV3), SHA-1 for `/V 4` (AESV2). The hash spans *all* recipients so
//!    no single recipient can shorten another's key.
//!
//! This module is **decrypt-only** (ADR-P0019: the write side is AESV3-only
//! standard-handler). A wrong credential yields a typed
//! [`Code::RecipientNoMatch`] — the AES-CBC padding check plus the exact
//! 24-byte payload length give wrong-key detection with a false-accept
//! probability below 2⁻⁶⁰; there is never a partial decrypt.
//!
//! The DER reader is [`crate::der`]; the supported CMS subset is documented
//! on [`parse_enveloped_data`] and in `pdf-plan/31-ENC03-DESIGN-NOTE.md`.

use aes::cipher::{BlockDecrypt, KeyInit};
use aes::{Aes128, Aes192, Aes256, Block};
use rsa::pkcs8::DecodePrivateKey;
use selis_error::{err, Code, Result};
use selis_sandbox::alloc;
use selis_sandbox::BudgetGuard;
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::der::{self, Der, Tag};

/// The object identifiers of the implemented CMS subset.
mod oid {
    /// rsaEncryption (PKCS#1 v1.5 key transport).
    pub(crate) const RSA_ENCRYPTION: &[u64] = &[1, 2, 840, 113_549, 1, 1, 1];
    /// id-envelopedData (the ContentInfo type PDF uses).
    pub(crate) const ENVELOPED_DATA: &[u64] = &[1, 2, 840, 113_549, 1, 7, 3];
    /// id-data (the EncryptedContentInfo's content type).
    pub(crate) const DATA: &[u64] = &[1, 2, 840, 113_549, 1, 7, 1];
    /// aes128-CBC.
    pub(crate) const AES128_CBC: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 1, 2];
    /// aes192-CBC.
    pub(crate) const AES192_CBC: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 1, 22];
    /// aes256-CBC.
    pub(crate) const AES256_CBC: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 1, 42];
    /// dhSinglePass-stdDH-sha256kdf-scheme (RFC 5753 §3.1) + AES key wrap.
    pub(crate) const DH_STDDH_SHA256_KDF: &[u64] = &[1, 3, 133, 16, 840, 63, 0, 4];
}

/// The CMS content-encryption algorithm of an `EncryptedContentInfo`.
///
/// Only AES-CBC is implemented (see the module docs for why RC4 and 3DES are
/// refused rather than half-supported).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CekAlgorithm {
    /// AES-128-CBC with the given IV.
    Aes128Cbc {
        /// The 16-byte initial value.
        iv: [u8; 16],
    },
    /// AES-192-CBC with the given IV.
    Aes192Cbc {
        /// The 16-byte initial value.
        iv: [u8; 16],
    },
    /// AES-256-CBC with the given IV.
    Aes256Cbc {
        /// The 16-byte initial value.
        iv: [u8; 16],
    },
}

impl CekAlgorithm {
    /// The CEK length this algorithm needs, in bytes.
    #[must_use]
    pub fn cek_len(self) -> usize {
        match self {
            CekAlgorithm::Aes128Cbc { .. } => 16,
            CekAlgorithm::Aes192Cbc { .. } => 24,
            CekAlgorithm::Aes256Cbc { .. } => 32,
        }
    }
}

/// How one recipient's CEK is transported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyTransport<'a> {
    /// RSAES-PKCS1-v1_5: the CEK, RSA-encrypted to the recipient's public
    /// key (the transport Acrobat writes).
    RsaPkcs1v15 {
        /// The RSA-encrypted CEK.
        encrypted_key: &'a [u8],
    },
    /// ECDH key agreement (static-stdDH, SHA-256 X9.63 KDF) with AES key
    /// wrap (RFC 5753 §2.1.1), one wrapped CEK per `recipientEncryptedKey`.
    EcdhAesKw {
        /// The originator's EC point (`OriginatorPublicKey.publicKey`, the
        /// BIT STRING content without the unused-bits octet).
        originator: &'a [u8],
        /// The wrapped CEKs (one per `recipientEncryptedKey`).
        wrapped_keys: Vec<&'a [u8]>,
    },
}

/// One `RecipientInfo` of the `EnvelopedData`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmsRecipient<'a> {
    /// How this recipient's CEK is transported.
    pub transport: KeyTransport<'a>,
}

/// The parsed `EnvelopedData` of one `/Recipients` blob — the subset the
/// public-key security handler needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopedData<'a> {
    /// The recipients, in wire order. [`authenticate_public_key`] tries them
    /// in this order (first match wins).
    pub recipients: Vec<CmsRecipient<'a>>,
    /// The content-encryption algorithm and its parameters.
    pub cek_algorithm: CekAlgorithm,
    /// The encrypted content — the wrapped 24-byte seed+permission payload.
    /// `None` when the writer used detached content, which the PDF handler
    /// never does; [`authenticate_public_key`] refuses such blobs.
    pub encrypted_content: Option<&'a [u8]>,
}

/// A credential for the public-key security handler: the recipient's private
/// key in PKCS#8 DER (from a `.der`/`.pem` key file; PKCS#12 keystore
/// extraction is out of scope for the read-only draft).
pub enum PubKeyCredential {
    /// An RSA private key (2048-bit or larger; RFC 8017 key transport).
    Rsa(Vec<u8>),
    /// An EC P-256 private key (ECDH key agreement, RFC 5753).
    EcP256(Vec<u8>),
}

impl core::fmt::Debug for PubKeyCredential {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Key material must never leak through Debug (crash reports, logs).
        match self {
            PubKeyCredential::Rsa(_) => f.write_str("PubKeyCredential::Rsa([redacted])"),
            PubKeyCredential::EcP256(_) => f.write_str("PubKeyCredential::EcP256([redacted])"),
        }
    }
}

/// The successful authentication of a public-key document: the derived file
/// encryption key and the recipient's permission bits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubKeyAuth {
    /// The file encryption key (`/Length`/8 bytes).
    pub key: Vec<u8>,
    /// The recipient's permission bits from the CMS payload (the 4 bytes
    /// after the seed, little-endian). Surfaced honestly per SL-1.ENC.04;
    /// wiring them into the policy layer is a sign-off review point.
    pub permissions: u32,
}

/// Parse one `/Recipients` blob as `ContentInfo` → `EnvelopedData`.
///
/// # Budget
///
/// The parse walks `data` once; every TLV charges its exact wire size to the
/// guard ([`Resource::Bytes`]). Exhaustion poisons the guard and fails the
/// parse typed; nothing is allocated beyond the (small) recipient vector,
/// whose growth is bounded by the already-resident input.
///
/// # Malformed Input
///
/// Any structural deviation — a wrong content type, an unknown OID, an
/// indefinite or lying length, a non-AES content algorithm, trailing bytes —
/// is a typed error ([`Code::EncryptMalformed`] for structural damage,
/// [`Code::EncryptUnsupported`] for a recognised but unimplemented
/// algorithm), never a panic and never a partial structure.
pub fn parse_enveloped_data<'a>(
    data: &'a [u8],
    g: &mut BudgetGuard<'_>,
) -> Result<EnvelopedData<'a>> {
    let mut outer = Der::new(data);
    let content_info = outer.next_expect(Tag::SEQUENCE, g)?;
    if !outer.is_empty() {
        return Err(der::malformed(
            outer.offset(),
            "trailing bytes after ContentInfo",
        ));
    }

    let mut ci = Der::new(content_info.content);
    let content_type = ci.next_expect(Tag::OID, g)?;
    if der::oid_arcs(content_type.content).as_deref() != Some(oid::ENVELOPED_DATA) {
        return Err(unsupported("ContentInfo type is not id-envelopedData"));
    }
    let content = ci.next_expect(Tag::CTX_0_CONSTRUCTED, g)?;
    if !ci.is_empty() {
        return Err(der::malformed(ci.offset(), "trailing bytes in ContentInfo"));
    }

    let mut env = Der::new(content.content);
    let enveloped = env.next_expect(Tag::SEQUENCE, g)?;
    if !env.is_empty() {
        return Err(der::malformed(
            env.offset(),
            "trailing bytes in [0] content",
        ));
    }

    let mut ed = Der::new(enveloped.content);
    let version = ed.next_expect(Tag::INTEGER, g)?;
    let version = integer_u64(version.content)
        .ok_or_else(|| der::malformed(ed.offset(), "bad EnvelopedData version"))?;
    if version > 3 {
        return Err(der::malformed(
            ed.offset(),
            "unsupported EnvelopedData version",
        ));
    }

    let recipient_infos = ed.next_expect(Tag::SET, g)?;
    let mut recipients = Vec::new();
    let mut set = Der::new(recipient_infos.content);
    while !set.is_empty() {
        let at = set.offset();
        let tlv = set.next(g)?;
        let recipient = match tlv.tag {
            Tag::SEQUENCE => parse_key_trans_recipient(tlv.content, g)?,
            Tag::CTX_1_CONSTRUCTED => parse_key_agree_recipient(tlv.content, g)?,
            _ => return Err(der::malformed(at, "unsupported RecipientInfo CHOICE")),
        };
        recipients.push(recipient);
    }

    let eci = ed.next_expect(Tag::SEQUENCE, g)?;
    let (cek_algorithm, encrypted_content) = parse_encrypted_content_info(eci.content, g)?;

    // [1] IMPLICIT unprotectedAttrs (rare) is tolerated and ignored.
    if !ed.is_empty() {
        let at = ed.offset();
        let tlv = ed.next(g)?;
        if tlv.tag != Tag::CTX_1_CONSTRUCTED {
            return Err(der::malformed(
                at,
                "unexpected entry after EncryptedContentInfo",
            ));
        }
        if !ed.is_empty() {
            return Err(der::malformed(
                ed.offset(),
                "trailing bytes in EnvelopedData",
            ));
        }
    }

    Ok(EnvelopedData {
        recipients,
        cek_algorithm,
        encrypted_content,
    })
}

/// `KeyTransRecipientInfo ::= SEQUENCE { version, recipientIdentifier,
/// keyEncryptionAlgorithm, encryptedKey }` (RFC 5652 §6.2.1).
fn parse_key_trans_recipient<'a>(
    data: &'a [u8],
    g: &mut BudgetGuard<'_>,
) -> Result<CmsRecipient<'a>> {
    let mut r = Der::new(data);
    let version = r.next_expect(Tag::INTEGER, g)?;
    if !matches!(integer_u64(version.content), Some(0)) {
        return Err(der::malformed(
            r.offset(),
            "unsupported KeyTransRecipientInfo version",
        ));
    }
    // recipientIdentifier: IssuerAndSerialNumber (SEQUENCE) or
    // SubjectKeyIdentifier ([0]). Certificate matching is not needed for the
    // try-all decryption policy — the identifier is validated structurally
    // and skipped.
    let rid = r.next(g)?;
    match rid.tag {
        Tag::SEQUENCE | Tag::CTX_0 => {}
        _ => return Err(der::malformed(r.offset(), "bad RecipientIdentifier")),
    }
    let alg = r.next_expect(Tag::SEQUENCE, g)?;
    let mut alg_reader = Der::new(alg.content);
    let algorithm = alg_reader.next_expect(Tag::OID, g)?;
    if der::oid_arcs(algorithm.content).as_deref() != Some(oid::RSA_ENCRYPTION) {
        return Err(unsupported("key transport algorithm is not rsaEncryption"));
    }
    let encrypted_key = r.next_expect(Tag::OCTET_STRING, g)?;
    if !r.is_empty() {
        return Err(der::malformed(
            r.offset(),
            "trailing bytes in KeyTransRecipientInfo",
        ));
    }
    Ok(CmsRecipient {
        transport: KeyTransport::RsaPkcs1v15 {
            encrypted_key: encrypted_key.content,
        },
    })
}

/// `KeyAgreeRecipientInfo ::= [1] EXPLICIT SEQUENCE { version, originator
/// [0] EXPLICIT, ukm [1] EXPLICIT OPTIONAL, keyEncryptionAlgorithm,
/// recipientEncryptedKeys }` (RFC 5652 §6.2.2).
fn parse_key_agree_recipient<'a>(
    data: &'a [u8],
    g: &mut BudgetGuard<'_>,
) -> Result<CmsRecipient<'a>> {
    let mut outer = Der::new(data);
    let kari = outer.next_expect(Tag::SEQUENCE, g)?;
    if !outer.is_empty() {
        return Err(der::malformed(
            outer.offset(),
            "trailing bytes in [1] content",
        ));
    }
    let mut r = Der::new(kari.content);
    let version = r.next_expect(Tag::INTEGER, g)?;
    if !matches!(integer_u64(version.content), Some(3)) {
        return Err(der::malformed(
            r.offset(),
            "unsupported KeyAgreeRecipientInfo version",
        ));
    }
    // originator [0] EXPLICIT CHOICE: only originatorKey [1] is implemented
    // (issuer/SKI originators cannot drive an ECDH derivation).
    let originator = r.next_expect(Tag::CTX_0_CONSTRUCTED, g)?;
    let mut originator_choice = Der::new(originator.content);
    let originator_key = originator_choice.next_expect(Tag::CTX_1_CONSTRUCTED, g)?;
    if !originator_choice.is_empty() {
        return Err(der::malformed(
            originator_choice.offset(),
            "trailing bytes in originator CHOICE",
        ));
    }
    let mut opk_reader = Der::new(originator_key.content);
    let opk = opk_reader.next_expect(Tag::SEQUENCE, g)?;
    if !opk_reader.is_empty() {
        return Err(der::malformed(
            opk_reader.offset(),
            "trailing bytes in OriginatorPublicKey",
        ));
    }
    let mut opk = Der::new(opk.content);
    let algorithm = opk.next_expect(Tag::SEQUENCE, g)?;
    validate_algorithm_identifier(algorithm.content, g)?;
    let point = opk.next_expect(Tag::BIT_STRING, g)?;
    if !opk.is_empty() {
        return Err(der::malformed(
            opk.offset(),
            "trailing bytes in OriginatorPublicKey",
        ));
    }
    // BIT STRING content: one unused-bits octet followed by the EC point.
    let originator_point = bit_string_bytes(point.content)
        .ok_or_else(|| der::malformed(opk.offset(), "bad originator BIT STRING"))?;

    // ukm [1] EXPLICIT UserKeyingMaterial: optional, and unused by the
    // static-stdDH schemes; tolerated and ignored when present (peek, do not
    // consume the keyEncryptionAlgorithm that may follow directly).
    if r.rest().first() == Some(&Tag::CTX_1_CONSTRUCTED.0) {
        r.next(g)?;
    }

    let alg = r.next_expect(Tag::SEQUENCE, g)?;
    let mut alg_reader = Der::new(alg.content);
    let algorithm = alg_reader.next_expect(Tag::OID, g)?;
    if der::oid_arcs(algorithm.content).as_deref() != Some(oid::DH_STDDH_SHA256_KDF) {
        return Err(unsupported("key agreement scheme is not stdDH-sha256kdf"));
    }

    let keys = r.next_expect(Tag::SEQUENCE, g)?;
    if !r.is_empty() {
        return Err(der::malformed(
            r.offset(),
            "trailing bytes in KeyAgreeRecipientInfo",
        ));
    }
    let mut wrapped_keys = Vec::new();
    let mut entries = Der::new(keys.content);
    while !entries.is_empty() {
        let entry = entries.next_expect(Tag::SEQUENCE, g)?;
        let mut entry = Der::new(entry.content);
        let _rid = entry.next(g)?; // IssuerAndSerialNumber or [0]; skipped
        let wrapped = entry.next_expect(Tag::OCTET_STRING, g)?;
        if !entry.is_empty() {
            return Err(der::malformed(
                entry.offset(),
                "trailing bytes in RecipientEncryptedKey",
            ));
        }
        wrapped_keys.push(wrapped.content);
    }
    if wrapped_keys.is_empty() {
        return Err(der::malformed(0, "empty recipientEncryptedKeys"));
    }
    Ok(CmsRecipient {
        transport: KeyTransport::EcdhAesKw {
            originator: originator_point,
            wrapped_keys,
        },
    })
}

/// `EncryptedContentInfo ::= SEQUENCE { contentType,
/// contentEncryptionAlgorithm, encryptedContent [0] EXPLICIT OPTIONAL }`.
fn parse_encrypted_content_info<'a>(
    data: &'a [u8],
    g: &mut BudgetGuard<'_>,
) -> Result<(CekAlgorithm, Option<&'a [u8]>)> {
    let mut eci = Der::new(data);
    let content_type = eci.next_expect(Tag::OID, g)?;
    if der::oid_arcs(content_type.content).as_deref() != Some(oid::DATA) {
        return Err(unsupported("EncryptedContentInfo type is not id-data"));
    }
    let alg = eci.next_expect(Tag::SEQUENCE, g)?;
    let mut alg_reader = Der::new(alg.content);
    let algorithm = alg_reader.next_expect(Tag::OID, g)?;
    let cek_algorithm = match der::oid_arcs(algorithm.content).as_deref() {
        Some(oid::AES128_CBC) => CekAlgorithm::Aes128Cbc {
            iv: cbc_iv(&mut alg_reader, g)?,
        },
        Some(oid::AES192_CBC) => CekAlgorithm::Aes192Cbc {
            iv: cbc_iv(&mut alg_reader, g)?,
        },
        Some(oid::AES256_CBC) => CekAlgorithm::Aes256Cbc {
            iv: cbc_iv(&mut alg_reader, g)?,
        },
        // RC4/3DES/RC2 and anything else: see the module docs — refusing a
        // cipher without wrong-key detection is a typed error by design.
        _ => return Err(unsupported("content-encryption algorithm is not AES-CBC")),
    };
    let encrypted_content = if eci.is_empty() {
        None
    } else {
        let at = eci.offset();
        let explicit = eci.next_expect(Tag::CTX_0_CONSTRUCTED, g)?;
        let mut inner = Der::new(explicit.content);
        let octets = inner.next_expect(Tag::OCTET_STRING, g)?;
        if !inner.is_empty() || !eci.is_empty() {
            return Err(der::malformed(at, "trailing bytes in encryptedContent"));
        }
        Some(octets.content)
    };
    Ok((cek_algorithm, encrypted_content))
}

/// Validate an `AlgorithmIdentifier`'s structure (any algorithm).
fn validate_algorithm_identifier(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<()> {
    let mut alg = Der::new(data);
    let _oid = alg.next_expect(Tag::OID, g)?;
    if !alg.is_empty() {
        alg.next(g)?;
        if !alg.is_empty() {
            return Err(der::malformed(
                alg.offset(),
                "trailing bytes in AlgorithmIdentifier",
            ));
        }
    }
    Ok(())
}

/// Read the 16-byte IV parameter of an AES-CBC `AlgorithmIdentifier`.
fn cbc_iv(alg_reader: &mut Der<'_>, g: &mut BudgetGuard<'_>) -> Result<[u8; 16]> {
    if alg_reader.is_empty() {
        return Err(der::malformed(
            alg_reader.offset(),
            "missing AES-CBC IV parameter",
        ));
    }
    let iv = alg_reader.next_expect(Tag::OCTET_STRING, g)?;
    let iv: [u8; 16] = iv
        .content
        .get(..16)
        .and_then(|s| <[u8; 16]>::try_from(s).ok())
        .ok_or_else(|| der::malformed(alg_reader.offset(), "AES-CBC IV must be 16 bytes"))?;
    if !alg_reader.is_empty() {
        return Err(der::malformed(
            alg_reader.offset(),
            "trailing bytes in AlgorithmIdentifier",
        ));
    }
    Ok(iv)
}

/// The bytes of a BIT STRING's content (one unused-bits octet + data).
fn bit_string_bytes(content: &[u8]) -> Option<&[u8]> {
    let unused = *content.first()?;
    if unused != 0 {
        return None;
    }
    content.get(1..)
}

/// A non-negative DER INTEGER's value, when it fits a `u64`.
fn integer_u64(content: &[u8]) -> Option<u64> {
    let mut value: u64 = 0;
    let mut started = false;
    for &b in content {
        if !started {
            if b == 0 && content.len() > 1 {
                continue; // DER minimality padding
            }
            if b & 0x80 != 0 {
                return None; // negative
            }
            started = true;
        }
        value = value.checked_mul(256)?.checked_add(u64::from(b))?;
    }
    Some(value)
}

/// An unimplemented-algorithm error for the CMS subset.
fn unsupported(detail: &'static str) -> selis_error::Error {
    err!(Code::EncryptUnsupported, during = "pkcs7", detail = detail)
}

/// Authenticate a public-key document and derive the file encryption key
/// (ISO 32000-1 §7.6.6 / ISO 32000-2 §7.6.6.4, Algorithm 1).
///
/// `recipients` is the document's `/Recipients` array (every blob — the
/// derivation hashes all of them); `length_bits` is the `/Encrypt` `/Length`
/// (128 for AESV2/`/V 4`, 256 for AESV3/`/V 5`); `sha256` selects the
/// Algorithm-1 hash (SHA-256 for `/V 5`, SHA-1 otherwise); `encrypt_metadata`
/// is `/EncryptMetadata` (false appends the 4×`0xFF` marker to the hash).
///
/// Recipients are tried in array order with the supplied credential; the
/// first blob that decrypts to a valid 24-byte payload wins (the
/// certificate-selection policy of the draft — see the design note §4).
///
/// # Budget
///
/// The recipient parses charge their wire bytes via the guard; the
/// seed-hash input is a bounded concatenation (seed + already-resident
/// recipient blobs) charged through [`alloc::vec_with_capacity`] before any
/// allocation. A hostile blob set terminates within its own byte budget.
///
/// # Malformed Input
///
/// A structurally damaged blob is [`Code::EncryptMalformed`]; a recognised
/// but unimplemented algorithm is [`Code::EncryptUnsupported`]; a credential
/// that opens no recipient (wrong key, wrong credential type) is
/// [`Code::RecipientNoMatch`]. There is no partial result: the key is
/// returned only when the full derivation succeeded.
pub fn authenticate_public_key(
    recipients: &[&[u8]],
    credential: &PubKeyCredential,
    length_bits: usize,
    sha256: bool,
    encrypt_metadata: bool,
    g: &mut BudgetGuard<'_>,
) -> Result<PubKeyAuth> {
    if recipients.is_empty() {
        return Err(der::malformed(0, "no /Recipients blobs"));
    }
    if !(40..=256).contains(&length_bits) || length_bits % 8 != 0 {
        return Err(err!(
            Code::EncryptMalformed,
            during = "pkcs7",
            detail = "implausible /Length"
        ));
    }
    let key_len = length_bits / 8;

    // Decode the credential once, outside the recipient loop.
    let rsa_key = match credential {
        PubKeyCredential::Rsa(der_bytes) => {
            let key = rsa::RsaPrivateKey::from_pkcs8_der(der_bytes).map_err(|_| {
                err!(
                    Code::EncryptMalformed,
                    during = "pkcs7",
                    detail = "bad RSA PKCS#8 key"
                )
            })?;
            Some(key)
        }
        PubKeyCredential::EcP256(_) => None,
    };
    let ec_key = match credential {
        PubKeyCredential::EcP256(der_bytes) => {
            let key = p256::SecretKey::from_pkcs8_der(der_bytes).map_err(|_| {
                err!(
                    Code::EncryptMalformed,
                    during = "pkcs7",
                    detail = "bad EC PKCS#8 key"
                )
            })?;
            Some(key)
        }
        PubKeyCredential::Rsa(_) => None,
    };

    for blob in recipients {
        let enveloped = parse_enveloped_data(blob, g)?;
        let Some(encrypted_content) = enveloped.encrypted_content else {
            return Err(unsupported("detached CMS content"));
        };
        for recipient in &enveloped.recipients {
            let transport = recipient.transport.clone();
            let Some(cek) = unwrap_cek(transport, credential, rsa_key.as_ref(), ec_key.as_ref())
            else {
                continue; // wrong credential for this transport; try the next
            };
            let Some(payload) =
                decrypt_payload(cek.as_slice(), enveloped.cek_algorithm, encrypted_content)
            else {
                continue; // padding/payload check failed: wrong key, try the next
            };
            // The payload is exactly seed(20) + permission bytes(4).
            if payload.len() != 24 {
                continue;
            }
            let seed = payload.get(..20).unwrap_or(&[]);
            let permissions = u32::from_le_bytes(
                payload
                    .get(20..24)
                    .and_then(|s| <[u8; 4]>::try_from(s).ok())
                    .unwrap_or([0; 4]),
            );
            let key = derive_file_key(seed, recipients, key_len, sha256, encrypt_metadata, g)?;
            return Ok(PubKeyAuth { key, permissions });
        }
    }
    Err(err!(
        Code::RecipientNoMatch,
        during = "pkcs7",
        detail = "no recipient opened with the supplied credential"
    ))
}

/// Unwrap a CEK from one recipient under the supplied credential.
///
/// Returns `None` when the credential's type does not match the transport or
/// the unwrap fails (wrong key) — both are "try the next recipient", not
/// errors.
fn unwrap_cek(
    transport: KeyTransport<'_>,
    credential: &PubKeyCredential,
    rsa_key: Option<&rsa::RsaPrivateKey>,
    ec_key: Option<&p256::SecretKey>,
) -> Option<Vec<u8>> {
    match (transport, credential) {
        (KeyTransport::RsaPkcs1v15 { encrypted_key }, PubKeyCredential::Rsa(_)) => {
            let key = rsa_key?;
            // RSAES-PKCS1-v1_5 decrypt: a wrong key fails the padding check
            // (RUSTSEC-2023-0071 residual accepted for local decryption —
            // design note §6).
            key.decrypt(rsa::Pkcs1v15Encrypt, encrypted_key).ok()
        }
        (
            KeyTransport::EcdhAesKw {
                originator,
                wrapped_keys,
            },
            PubKeyCredential::EcP256(_),
        ) => {
            let secret = ec_key?;
            let public = p256::PublicKey::from_sec1_bytes(originator).ok()?;
            let shared = p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), public.as_affine());
            let shared_bytes = *shared.raw_secret_bytes();
            for wrapped in wrapped_keys {
                // The KEK length follows the wrapped key: RFC 3394 output is
                // key + 8 bytes, so a 24-byte blob wraps a 16-byte CEK
                // (AES-128) and a 40-byte blob a 32-byte CEK (AES-256).
                if wrapped.len() != 24 && wrapped.len() != 40 {
                    continue;
                }
                let kek = kdf_x963_sha256(shared_bytes.as_slice(), wrapped.len() - 8);
                if let Some(cek) = aes_kw_unwrap(&kek, wrapped) {
                    return Some(cek);
                }
            }
            None
        }
        _ => None,
    }
}

/// Decrypt the 24-byte payload with the CEK, strictly validating the AES-CBC
/// padding (the wrong-key detector).
fn decrypt_payload(cek: &[u8], algorithm: CekAlgorithm, content: &[u8]) -> Option<Vec<u8>> {
    let (key_len, iv) = match algorithm {
        CekAlgorithm::Aes128Cbc { iv } => (16usize, iv),
        CekAlgorithm::Aes192Cbc { iv } => (24usize, iv),
        CekAlgorithm::Aes256Cbc { iv } => (32usize, iv),
    };
    if cek.len() != key_len {
        return None; // CEK size mismatch: wrong recipient path
    }
    // One IV block plus at least one data block; exact block multiple.
    if content.len() < 32 || content.len() % 16 != 0 {
        return None;
    }
    let cipher = AesCipher::new(key_len, cek)?;
    let plain = cbc_decrypt_strict(&cipher, &iv, content)?;
    // Strict PKCS#7: the pad value must be 1..=16 and every pad byte equal.
    let pad = *plain.last()?;
    if pad == 0 || pad > 16 {
        return None;
    }
    let (body, padding) = plain.split_at_checked(plain.len() - usize::from(pad))?;
    if padding.iter().any(|&b| b != pad) {
        return None;
    }
    Some(body.to_vec())
}

/// An owned AES block-cipher handle (the three key sizes share the CBC walk).
enum AesCipher {
    /// AES-128.
    A128(Aes128),
    /// AES-192.
    A192(Aes192),
    /// AES-256.
    A256(Aes256),
}

impl AesCipher {
    fn new(key_len: usize, key: &[u8]) -> Option<Self> {
        match key_len {
            16 => Some(AesCipher::A128(Aes128::new_from_slice(key).ok()?)),
            24 => Some(AesCipher::A192(Aes192::new_from_slice(key).ok()?)),
            32 => Some(AesCipher::A256(Aes256::new_from_slice(key).ok()?)),
            _ => None,
        }
    }

    fn decrypt_block(&self, block: &mut Block) {
        match self {
            AesCipher::A128(c) => c.decrypt_block(block),
            AesCipher::A192(c) => c.decrypt_block(block),
            AesCipher::A256(c) => c.decrypt_block(block),
        }
    }

    /// Test-side mirror for fixture building.
    #[cfg(test)]
    fn encrypt_block(&self, block: &mut Block) {
        use aes::cipher::BlockEncrypt;
        match self {
            AesCipher::A128(c) => c.encrypt_block(block),
            AesCipher::A192(c) => c.encrypt_block(block),
            AesCipher::A256(c) => c.encrypt_block(block),
        }
    }
}

/// AES-CBC decrypt with strict structure: exact block multiples, the IV
/// copied in with a checked length (SL-1.ENC.06 discipline).
fn cbc_decrypt_strict(cipher: &AesCipher, iv: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() || data.len() % 16 != 0 {
        return None;
    }
    let mut out = Vec::new();
    let mut prev = *iv;
    for chunk in data.chunks(16) {
        let block_bytes: [u8; 16] = <[u8; 16]>::try_from(chunk).ok()?;
        let mut block = Block::clone_from_slice(&block_bytes);
        cipher.decrypt_block(&mut block);
        for k in 0..16 {
            let b = block[k] ^ prev[k];
            out.push(b);
        }
        prev = block_bytes;
    }
    Some(out)
}

/// X9.63 KDF with SHA-256 and an empty SharedInfo (RFC 5753 §2.1.1 static
/// stdDH): `T(i) = SHA256(Z || be32(i))`, concatenated and truncated.
fn kdf_x963_sha256(z: &[u8], out_len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut counter: u32 = 1;
    while out.len() < out_len {
        let mut h = Sha256::new();
        h.update(z);
        h.update(counter.to_be_bytes());
        out.extend_from_slice(h.finalize().as_slice());
        counter = counter.saturating_add(1);
    }
    out.truncate(out_len);
    out
}

/// AES key unwrap (RFC 3394 §2.2.2). Returns `None` when the integrity check
/// register does not match — the wrong-key detector of the ECDH path (false
/// accept ≈ 2⁻⁶⁴).
fn aes_kw_unwrap(kek: &[u8], wrapped: &[u8]) -> Option<Vec<u8>> {
    // CEKs are 16/24/32 bytes; anything larger is not a key.
    if wrapped.is_empty() || wrapped.len() % 8 != 0 || wrapped.len() > 64 {
        return None;
    }
    let kek_len = kek.len();
    let cipher = AesCipher::new(kek_len, kek)?;
    // A = C[0]; R[i] = C[i]. Unwrap: 5 rounds of
    // B = DEC(KEK, A | R[i]); A = MSB64(B) ^ t; R[i] = LSB64(B).
    let n = wrapped.len() / 8 - 1;
    if n == 0 {
        return None;
    }
    let mut a: [u8; 8] = <[u8; 8]>::try_from(wrapped.get(..8)?).ok()?;
    let mut r: Vec<[u8; 8]> = Vec::new();
    for i in 0..n {
        let start = (i + 1) * 8;
        let end = start + 8;
        r.push(<[u8; 8]>::try_from(wrapped.get(start..end)?).ok()?);
    }
    // j ≤ 5, i ≤ n ≤ 7: the tag counter t stays ≤ 42 — no overflow.
    // Six rounds (j = 5..=0), the mirror of the wrap.
    for j in (0..=5usize).rev() {
        for i in (1..=n).rev() {
            // B = AES-1(KEK, (A ^ t) | R[i]); A = MSB64(B); R[i] = LSB64(B).
            let t = (j * n + i) as u64;
            let t_bytes = t.to_be_bytes();
            let mut input = [0u8; 16];
            for k in 0..8 {
                input[k] = a[k] ^ t_bytes[k];
            }
            input[8..].copy_from_slice(r.get(i - 1)?);
            let mut block = Block::clone_from_slice(&input);
            cipher.decrypt_block(&mut block);
            for k in 0..8 {
                a[k] = block[k];
            }
            let slot = r.get_mut(i - 1)?;
            for k in 0..8 {
                slot[k] = block[8 + k];
            }
        }
    }
    // The integrity check register.
    const IV_A6: [u8; 8] = [0xA6, 0xA6, 0xA6, 0xA6, 0xA6, 0xA6, 0xA6, 0xA6];
    if a != IV_A6 {
        return None;
    }
    let mut out = Vec::new();
    for block in &r {
        out.extend_from_slice(block);
    }
    Some(out)
}

/// Algorithm 1, final step: `HASH(seed || every recipient blob [|| FFFFFFFF])`
/// truncated to the key length. The hash spans all recipient blobs, so the
/// key depends on the whole array (not just the matching entry).
fn derive_file_key(
    seed: &[u8],
    recipients: &[&[u8]],
    key_len: usize,
    sha256: bool,
    encrypt_metadata: bool,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let total: usize = seed
        .len()
        .checked_add(recipients.iter().map(|r| r.len()).sum::<usize>())
        .and_then(|t| t.checked_add(usize::from(!encrypt_metadata) * 4))
        .ok_or_else(|| der::malformed(0, "key derivation length overflow"))?;
    // Bounded by already-resident bytes; charged before allocation.
    let mut input: Vec<u8> = alloc::vec_with_capacity(g, total)?;
    input.extend_from_slice(seed);
    for blob in recipients {
        input.extend_from_slice(blob);
    }
    if !encrypt_metadata {
        input.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    }
    let digest: Vec<u8> = if sha256 {
        Sha256::digest(&input).to_vec()
    } else {
        Sha1::digest(&input).to_vec()
    };
    let key = digest
        .get(..key_len.min(digest.len()))
        .unwrap_or(&[])
        .to_vec();
    if key.len() != key_len {
        return Err(err!(
            Code::EncryptMalformed,
            during = "pkcs7",
            detail = "hash shorter than /Length"
        ));
    }
    Ok(key)
}

#[cfg(test)]
mod tests;
