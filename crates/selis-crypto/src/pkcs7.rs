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
//! **Recipient selection (SL-1.ENC.07):** when the credential carries an
//! X.509 certificate chain, recipients are matched by their explicit
//! `RecipientIdentifier` — `issuerAndSerialNumber` or
//! `subjectKeyIdentifier` (RFC 5652 §6) — *before* any unwrap, exactly as
//! Acrobat/PDFium/qpdf/PDFBox select; [`MatchBy`] controls the policy
//! (`auto` prefers the identifier match with a structural fall-through,
//! `first_valid` is the draft's try-in-array-order scan, `certificate` is
//! spec-strict identity-only). A bare private key (no chain) always uses the
//! structural path — the draft's rationale still holds for it (design
//! note §4).
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
use crate::x509::CertIdentity;

/// The object identifiers of the implemented CMS subset.
mod oid {
    /// rsaEncryption (PKCS#1 v1.5 key transport).
    pub(crate) const RSA_ENCRYPTION: &[u64] = &[1, 2, 840, 113_549, 1, 1, 1];
    /// id-RSAES-OAEP (PKCS#1 v2.1 RSAES-OAEP key transport, RFC 8017 §A.2.3
    /// as RFC 5751 §… uses it in CMS `keyEncryptionAlgorithm`).
    pub(crate) const RSAES_OAEP: &[u64] = &[1, 2, 840, 113_549, 1, 1, 7];
    /// id-MGF1 (the mask generation function RSAES-OAEP parameters name).
    pub(crate) const MGF1: &[u64] = &[1, 2, 840, 113_549, 1, 1, 8];
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
    /// rc4 (`1.2.840.113549.3.4`, RFC 8635-era PKCS #5 table) — read-only
    /// legacy content, SL-1.ENC.08.
    pub(crate) const RC4: &[u64] = &[1, 2, 840, 113_549, 3, 4];
    /// rc2-cbc (parameters are `SEQUENCE { INTEGER rc2EffectiveKeyLength
    /// DEFAULT 0, OCTET STRING iv(8) }`).
    pub(crate) const RC2_CBC: &[u64] = &[1, 2, 840, 113_549, 3, 2];
    /// des-ede3-cbc (Triple DES in CBC, IV as an OCTET STRING).
    pub(crate) const DES_EDE3_CBC: &[u64] = &[1, 2, 840, 113_549, 3, 7];
    /// sha1 (`1.3.14.3.2.26`, the OAEP/MGF1 default digest).
    pub(crate) const SHA1: &[u64] = &[1, 3, 14, 3, 2, 26];
    /// sha256.
    pub(crate) const SHA256: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 2, 1];
    /// sha384.
    pub(crate) const SHA384: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 2, 2];
    /// sha512.
    pub(crate) const SHA512: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 2, 3];
    /// id-PSpecified (PKCS#1 `1.2.840.113549.1.1.9`; RFC 8017 §A.2.4 — NOT the
    /// SMIMEAlgs arc the ENC.08 task sketch floated; that is CMS *signing*
    /// capability ids). A non-empty label is refused — see
    /// `parse_rsa_oaep_params`.
    pub(crate) const P_SPECIFIED: &[u64] = &[1, 2, 840, 113_549, 1, 1, 9];
    /// dhSinglePass-stdDH-sha256kdf-scheme (RFC 5753 §3.1) + AES key wrap.
    pub(crate) const DH_STDDH_SHA256_KDF: &[u64] = &[1, 3, 133, 16, 840, 63, 0, 4];
}

/// The CMS content-encryption algorithm of an `EncryptedContentInfo`.
///
/// AES-CBC is the modern set (SL-1.ENC.03); RC4, TDEA-CBC and RC2-CBC are the
/// `adbe.pkcs7.s3`-era *reads* (SL-1.ENC.08 — see [`crate::legacy`] for the
/// hand-rolled ciphers and the reason they are decrypt-only).
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
    /// RC4, keyed by the unwrapped CEK (40-bit = 5 bytes, 128-bit = 16;
    /// `adbe.pkcs7.s3`-era documents). RC4 has no IV parameter.
    Rc4,
    /// Triple-DES (TDEA, two- or three-key) in CBC with the 8-byte IV.
    TdeaCbc {
        /// The 8-byte initial value.
        iv: [u8; 8],
    },
    /// RC2 in CBC with the 8-byte IV and the parameters'
    /// `rc2EffectiveKeyLength` in bits (`0` = whole key).
    Rc2Cbc {
        /// The 8-byte initial value.
        iv: [u8; 8],
        /// `rc2EffectiveKeyLength`, clamped against the CEK by expansion.
        effective_bits: usize,
    },
}

impl CekAlgorithm {
    /// The CEK length this algorithm needs, in bytes (RC2/RC4 have a range:
    /// the key material is whatever the key transport unwrapped, so
    /// `None` means "length is checked against the unwrapped CEK").
    #[must_use]
    pub fn cek_len(self) -> Option<usize> {
        match self {
            CekAlgorithm::Aes128Cbc { .. } => Some(16),
            CekAlgorithm::Aes192Cbc { .. } => Some(24),
            CekAlgorithm::Aes256Cbc { .. } => Some(32),
            CekAlgorithm::TdeaCbc { .. } => Some(24),
            CekAlgorithm::Rc2Cbc { effective_bits, .. } if effective_bits != 0 => {
                Some(effective_bits.div_ceil(8))
            }
            CekAlgorithm::Rc2Cbc { .. } | CekAlgorithm::Rc4 => None,
        }
    }

    /// Accept an unwrapped CEK whose length the algorithm permits.
    #[must_use]
    pub fn accepts_cek(self, cek: &[u8]) -> bool {
        let n = cek.len();
        match self {
            // RC2 keys run 5..=128 bytes (Adobe-era envelopes used 5 or 16).
            CekAlgorithm::Rc2Cbc { .. } => (5..=128).contains(&n),
            CekAlgorithm::Rc4 => (5..=16).contains(&n),
            CekAlgorithm::TdeaCbc { .. } => matches!(n, 16 | 24),
            other => {
                self.cek_len().is_some_and(|want| want == n)
                    || matches!(
                        (other, n),
                        (CekAlgorithm::Aes128Cbc { .. }, 16)
                            | (CekAlgorithm::Aes192Cbc { .. }, 24)
                            | (CekAlgorithm::Aes256Cbc { .. }, 32)
                    )
            }
        }
    }
}

/// How one recipient CEK is transported, together with the
/// `RecipientIdentifier` that addresses it (SL-1.ENC.07).
///
/// A `KeyAgreeRecipientInfo` with several `recipientEncryptedKeys` fans out
/// into one entry per addressed key — every one carries its own
/// `RecipientIdentifier` (RFC 5652 §6.2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyTransport<'a> {
    /// RSAES-PKCS1-v1_5: the CEK, RSA-encrypted to the recipient's public
    /// key (the transport Acrobat writes).
    RsaPkcs1v15 {
        /// The explicit recipient identifier of the `KeyTransRecipientInfo`.
        identifier: RecipientIdentifier<'a>,
        /// The RSA-encrypted CEK.
        encrypted_key: &'a [u8],
    },
    /// RSAES-OAEP (PKCS#1 v2.1, RFC 8017 §7.1.1): the modern CMS
    /// `keyEncryptionAlgorithm` for RSA transports — the MGF1 unwrap the
    /// `rsa` crate performs. `oaep` records the (hash, MGF1-hash) pair parsed
    /// from the algorithm parameters, so an envelope that asks for a digest
    /// we do not build refuses *typed* at parse time.
    RsaOaep {
        /// The explicit recipient identifier of the `KeyTransRecipientInfo`.
        identifier: RecipientIdentifier<'a>,
        /// The RSA-OAEP-encrypted CEK.
        encrypted_key: &'a [u8],
        /// The parsed parameter selection.
        oaep: OaepParams,
    },
    /// ECDH key agreement (static-stdDH, SHA-256 X9.63 KDF) with AES key
    /// wrap (RFC 5753 §2.1.1), one entry per `recipientEncryptedKey`.
    EcdhAesKw {
        /// The explicit recipient identifier of this `RecipientEncryptedKey`.
        identifier: RecipientIdentifier<'a>,
        /// The originator's EC point (`OriginatorPublicKey.publicKey`, the
        /// BIT STRING content without the unused-bits octet).
        originator: &'a [u8],
        /// The wrapped CEK.
        wrapped_key: &'a [u8],
    },
}

/// One OAEP digest choice of `RSAES-OAEP-params`. Exactly the set the `rsa`
/// crate can instantiate against our `sha1`/`sha2` backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OaepHash {
    /// SHA-1 (the DER default of both parameters).
    Sha1,
    /// SHA-256.
    Sha256,
    /// SHA-384.
    Sha384,
    /// SHA-512.
    Sha512,
}

/// `RSAES-OAEP-params ::= SEQUENCE { hashAlgorithm, maskGenAlgorithm,
/// pSourceAlgorithm }` with the RFC 8017 defaults: SHA-1 / MGF1-SHA-1 /
/// id-PSpecified-with-an-empty-label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OaepParams {
    /// The label-hash digest (`hashAlgorithm`).
    pub hash: OaepHash,
    /// The MGF1 hash (`maskGenAlgorithm`'s parameter).
    pub mgf_hash: OaepHash,
}

impl OaepParams {
    /// RFC 8017's OAEP default pair, used when the parameters are absent.
    pub(crate) const DEFAULT: Self = Self {
        hash: OaepHash::Sha1,
        mgf_hash: OaepHash::Sha1,
    };
}

/// The CMS `RecipientIdentifier` of one recipient (RFC 5652 §6.2): the
/// recipient's certificate addressed by issuer + serial, or by its
/// `subjectKeyIdentifier`. The bytes are the raw DER contents of the
/// identifier fields, matched against [`CertIdentity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipientIdentifier<'a> {
    /// `IssuerAndSerialNumber ::= SEQUENCE { issuer Name, serialNumber }`.
    IssuerAndSerialNumber {
        /// DER content of the issuer `Name` TLV.
        issuer: &'a [u8],
        /// DER content of the `serialNumber` INTEGER.
        serial: &'a [u8],
    },
    /// `subjectKeyIdentifier [0] OCTET STRING` (usually the 20-byte SKI of
    /// RFC 5280 §4.2.1.2).
    SubjectKeyIdentifier(&'a [u8]),
}

impl<'a> KeyTransport<'a> {
    /// The explicit recipient identifier addressing this transport.
    #[must_use]
    pub fn identifier(&self) -> &RecipientIdentifier<'a> {
        match self {
            KeyTransport::RsaPkcs1v15 { identifier, .. }
            | KeyTransport::RsaOaep { identifier, .. }
            | KeyTransport::EcdhAesKw { identifier, .. } => identifier,
        }
    }
}

/// The parsed `EnvelopedData` of one `/Recipients` blob — the subset the
/// public-key security handler needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopedData<'a> {
    /// The recipient CEK transports, in wire order. [`authenticate_public_key`]
    /// tries them in this order on the structural pass.
    pub recipients: Vec<KeyTransport<'a>>,
    /// The content-encryption algorithm and its parameters.
    pub cek_algorithm: CekAlgorithm,
    /// The encrypted content — the wrapped 24-byte seed+permission payload.
    /// `None` when the writer used detached content, which the PDF handler
    /// never does; [`authenticate_public_key`] refuses such blobs.
    pub encrypted_content: Option<&'a [u8]>,
}

/// The recipient-selection policy of a [`PubKeyCredential`] (SL-1.ENC.07,
/// design note §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MatchBy {
    /// Prefer the certificate-identifier match when the credential carries a
    /// chain; fall through to the structural decrypt when the exact
    /// identifier is absent from the blob list (or no chain is supplied).
    /// This is the Acrobat-compatible default.
    #[default]
    Auto,
    /// The draft policy: try every recipient's transport in `/Recipients`
    /// array order; the first structural decrypt wins. The right choice for
    /// a bare private key with no certificate (the unwrap's own integrity
    /// checks — RSA padding, AES-CBC padding + length, AES-KW's register —
    /// reject wrong keys with negligible false-accept probability).
    FirstValid,
    /// Spec-strict: select only recipients whose `RecipientIdentifier`
    /// matches the supplied chain; a credential whose chain matches nothing
    /// yields `RECIPIENT_NO_MATCH` even if its key material would have
    /// opened a differently-addressed recipient.
    Certificate,
}

/// How the successful recipient was selected (SL-1.ENC.07) — surfaced so
/// callers and tests can prove identity-first selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matched {
    /// The `issuerAndSerialNumber` matched a certificate in the chain.
    ByIssuerAndSerialNumber,
    /// The `subjectKeyIdentifier` matched a certificate in the chain.
    BySubjectKeyIdentifier,
    /// No certificate-identity match was used (bare key, `MatchBy::FirstValid`,
    /// or the `Auto` fall-through): the private key opened the transport.
    Structurally,
}

/// The recipient's private key material: PKCS#8 DER (from a `.der`/`.pem`
/// key file; PKCS#12 keystore extraction is out of scope for the read-only
/// draft).
#[derive(Clone)]
pub enum KeyMaterial {
    /// An RSA private key (2048-bit or larger; RFC 8017 key transport).
    Rsa(Vec<u8>),
    /// An EC P-256 private key (ECDH key agreement, RFC 5753).
    EcP256(Vec<u8>),
}

/// A credential for the public-key security handler: the recipient's private
/// key plus, optionally, the X.509 certificate chain that identifies it
/// (DER certificates, leaf first — the SL-1.ENC.07 explicit-identifier
/// match), and the [`MatchBy`] selection policy.
#[derive(Clone)]
pub struct PubKeyCredential {
    /// The recipient's private key.
    pub key: KeyMaterial,
    /// Certificate identities, leaf first; empty = a bare private key, and
    /// the selection falls back to the structural pass.
    pub certificates: Vec<Vec<u8>>,
    /// Recipient-selection policy (default [`MatchBy::Auto`]).
    pub match_by: MatchBy,
}

impl PubKeyCredential {
    /// A bare RSA private key (PKCS#8 DER), no chain.
    #[must_use]
    pub fn rsa(pkcs8_der: Vec<u8>) -> Self {
        Self {
            key: KeyMaterial::Rsa(pkcs8_der),
            certificates: Vec::new(),
            match_by: MatchBy::Auto,
        }
    }

    /// A bare EC P-256 private key (PKCS#8 DER), no chain.
    #[must_use]
    pub fn ec_p256(pkcs8_der: Vec<u8>) -> Self {
        Self {
            key: KeyMaterial::EcP256(pkcs8_der),
            certificates: Vec::new(),
            match_by: MatchBy::Auto,
        }
    }

    /// Append an X.509 certificate (DER) to the identity chain (leaf first).
    #[must_use]
    pub fn with_certificate(mut self, der: Vec<u8>) -> Self {
        self.certificates.push(der);
        self
    }

    /// Override the recipient-selection policy.
    #[must_use]
    pub fn matching(mut self, match_by: MatchBy) -> Self {
        self.match_by = match_by;
        self
    }
}

impl core::fmt::Debug for PubKeyCredential {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Key material must never leak through Debug (crash reports, logs).
        // Certificates are public data but need not echo bytes either.
        let kind = match self.key {
            KeyMaterial::Rsa(_) => "Rsa",
            KeyMaterial::EcP256(_) => "EcP256",
        };
        write!(
            f,
            "PubKeyCredential::{kind}([redacted], {} certificate(s), match_by: {:?})",
            self.certificates.len(),
            self.match_by,
        )
    }
}

/// The successful authentication of a public-key document: the derived file
/// encryption key and the recipient's permission bits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubKeyAuth {
    /// The file encryption key (`/Length`/8 bytes).
    pub key: Vec<u8>,
    /// The recipient's permission bits from the CMS payload (the 4 bytes
    /// after the seed, little-endian). Surfaced honestly per SL-1.ENC.04
    /// and *enforced* through the policy layer by SL-1.ENC.09.
    pub permissions: u32,
    /// How this recipient was selected (SL-1.ENC.07).
    pub matched_by: Matched,
    /// The index of the winning `/Recipients` blob.
    pub recipient_index: usize,
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
        match tlv.tag {
            Tag::SEQUENCE => recipients.push(parse_key_trans_recipient(tlv.content, g)?),
            Tag::CTX_1_CONSTRUCTED => {
                recipients.extend(parse_key_agree_recipient(tlv.content, g)?);
            }
            _ => return Err(der::malformed(at, "unsupported RecipientInfo CHOICE")),
        }
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
) -> Result<KeyTransport<'a>> {
    let mut r = Der::new(data);
    let version = r.next_expect(Tag::INTEGER, g)?;
    if !matches!(integer_u64(version.content), Some(0)) {
        return Err(der::malformed(
            r.offset(),
            "unsupported KeyTransRecipientInfo version",
        ));
    }
    // recipientIdentifier: IssuerAndSerialNumber (SEQUENCE) or
    // SubjectKeyIdentifier ([0] Primitive) — parsed structurally so the
    // certificate-identity match can use it (SL-1.ENC.07).
    let at = r.offset();
    let rid = r.next(g)?;
    let identifier = match rid.tag {
        Tag::SEQUENCE => parse_issuer_and_serial(rid.content, g)?,
        Tag::CTX_0 => RecipientIdentifier::SubjectKeyIdentifier(rid.content),
        _ => return Err(der::malformed(at, "bad RecipientIdentifier")),
    };
    let alg = r.next_expect(Tag::SEQUENCE, g)?;
    let mut alg_reader = Der::new(alg.content);
    let algorithm = alg_reader.next_expect(Tag::OID, g)?;
    let transport = match der::oid_arcs(algorithm.content).as_deref() {
        // rsaEncryption: accept a NULL or an absent parameter octet (a few
        // writers omit it), then the encrypted key.
        Some(oid::RSA_ENCRYPTION) => {
            skip_algorithm_parameters(&mut alg_reader, g)?;
            KeyTransport::RsaPkcs1v15 {
                identifier,
                encrypted_key: r.next_expect(Tag::OCTET_STRING, g)?.content,
            }
        }
        // RSAES-OAEP: the parameters are an RFC 8017 `RSAES-OAEP-params`;
        // absent (or NULL) means the SHA-1/MGF1-SHA-1/empty-label defaults.
        Some(oid::RSAES_OAEP) => {
            let oaep = parse_rsa_oaep_params(&mut alg_reader, g)?;
            KeyTransport::RsaOaep {
                identifier,
                encrypted_key: r.next_expect(Tag::OCTET_STRING, g)?.content,
                oaep,
            }
        }
        _ => return Err(unsupported("key transport algorithm is not RSA")),
    };
    if !r.is_empty() {
        return Err(der::malformed(
            r.offset(),
            "trailing bytes in KeyTransRecipientInfo",
        ));
    }
    Ok(transport)
}

/// Tolerate the NULL/absent parameter octet of an `rsaEncryption`
/// `AlgorithmIdentifier` (and refuse *anything* substantive: a PKCS#5 PRF
/// parameter under an rsaEncryption OID is not a key transport).
fn skip_algorithm_parameters(alg_reader: &mut Der<'_>, g: &mut BudgetGuard<'_>) -> Result<()> {
    if !alg_reader.is_empty() {
        let at = alg_reader.offset();
        let params = alg_reader.next(g)?;
        if params.tag != Tag::NULL || !alg_reader.is_empty() {
            return Err(der::malformed(at, "unexpected rsaEncryption parameters"));
        }
    }
    Ok(())
}

/// `RSAES-OAEP-params ::= SEQUENCE { hashAlgorithm [0] HashAlgorithm
/// DEFAULT sha1, maskGenAlgorithm [1] MaskGenAlgorithm
/// DEFAULT mgf1SHA1, pSourceAlgorithm [2] PSourceAlgorithm
/// DEFAULT id-PSpecified("", ...) }` (RFC 8017 A.2.1, RFC 5751 §3). The
/// digests map onto what `rsa::Oaep` can instantiate; anything else — an
/// unknown OID, a digest we do not link, a *non-empty* label — is a typed
/// `Code::EncryptUnsupported` (recognised, refused, never silently dropped:
/// a labelled OAEP cannot be detected as a wrong key).
fn parse_rsa_oaep_params(alg_reader: &mut Der<'_>, g: &mut BudgetGuard<'_>) -> Result<OaepParams> {
    if alg_reader.is_empty() {
        return Ok(OaepParams::DEFAULT);
    }
    let at = alg_reader.offset();
    let params = alg_reader.next(g)?;
    if params.tag == Tag::NULL {
        return Ok(OaepParams::DEFAULT);
    }
    if params.tag != Tag::SEQUENCE || !alg_reader.is_empty() {
        return Err(der::malformed(at, "bad RSAES-OAEP-params"));
    }
    let mut p = Der::new(params.content);
    let mut hash = OaepHash::Sha1;
    let mut mgf = OaepHash::Sha1;
    while !p.is_empty() {
        let at = p.offset();
        let member = p.next(g)?;
        match member.tag {
            // [0] IMPLICIT HashAlgorithm
            Tag::CTX_0_CONSTRUCTED => {
                hash = parse_oaep_digest(member.content, g)?;
            }
            // [1] IMPLICIT MaskGenAlgorithm id-MGF1 { [0] HashAlgorithm }
            Tag::CTX_1_CONSTRUCTED => {
                mgf = parse_mgf1(member.content, g)?;
            }
            // [2] IMPLICIT PSourceAlgorithm id-PSpecified { OCTET STRING }
            Tag::CTX_2_CONSTRUCTED => {
                parse_psource(member.content, g)?;
            }
            _ => return Err(der::malformed(at, "unknown RSAES-OAEP-params member")),
        }
    }
    Ok(OaepParams {
        hash,
        mgf_hash: mgf,
    })
}

/// An `AlgorithmIdentifier { OID (digest), NULL | absent }` in the OAEP
/// parameter slots, mapped onto our `OaepHash` (only MD2/MD5/RIPEMD-160/SHA3
/// are refused here — everything we can build the padding with is accepted).
fn parse_oaep_digest(content: &[u8], g: &mut BudgetGuard<'_>) -> Result<OaepHash> {
    let mut alg = Der::new(content);
    let oid = alg.next_expect(Tag::OID, g)?;
    if !alg.is_empty() {
        alg.next(g)?;
        if !alg.is_empty() {
            return Err(der::malformed(
                alg.offset(),
                "trailing bytes in digest AlgorithmIdentifier",
            ));
        }
    }
    match der::oid_arcs(oid.content).as_deref() {
        Some(oid::SHA1) => Ok(OaepHash::Sha1),
        Some(oid::SHA256) => Ok(OaepHash::Sha256),
        Some(oid::SHA384) => Ok(OaepHash::Sha384),
        Some(oid::SHA512) => Ok(OaepHash::Sha512),
        _ => Err(unsupported("OAEP digest is not SHA-1/256/384/512")),
    }
}

/// `MaskGenAlgorithm ::= SEQUENCE { id-MGF1, [0] HashAlgorithm }` — a
/// different mask generator than MGF1 is refused typed.
fn parse_mgf1(content: &[u8], g: &mut BudgetGuard<'_>) -> Result<OaepHash> {
    let mut alg = Der::new(content);
    let oid = alg.next_expect(Tag::OID, g)?;
    if der::oid_arcs(oid.content).as_deref() != Some(oid::MGF1) {
        return Err(unsupported("OAEP mask generation function is not MGF1"));
    }
    if alg.is_empty() {
        return Ok(OaepHash::Sha1); // id-MGF1DB, SHA-1, and the DER default
    }
    let params = alg.next_expect(Tag::CTX_0_CONSTRUCTED, g)?;
    if !alg.is_empty() {
        return Err(der::malformed(
            alg.offset(),
            "trailing bytes in MaskGenAlgorithm",
        ));
    }
    parse_oaep_digest(params.content, g)
}

/// `PSourceAlgorithm ::= SEQUENCE { id-PSpecified, OCTET STRING }`. An
/// *empty* label (the RFC 8017 default `""`) is the supported case: the
/// label bytes would otherwise feed the MGF1 digest, and a *non-empty* label
/// is an OAEP variant no PDF writer emits (refused typed, §5).
/// `PSourceAlgorithm ::= SEQUENCE { PSourceAlgorithmID
/// OBJECT IDENTIFIER (id-pSourceAlgorithm), P OCTET STRING (SIZE (1|MAX)) }`
/// (RFC 8017 A.2.1), DEFAULT `id-PEmptySeq`/`P=""` — *any* other pSource is
/// refused typed because a non-empty label changes the MGF1/XOR algebra and a
/// *differently*-labelled-but-same-hash OAEP cannot be told apart from a wrong
/// key at this layer (§5). The OID itself is checked structurally: an
/// unrecognised `id-pSourceAlgorithm` is `ENCRYPT_UNSUPPORTED`.
fn parse_psource(content: &[u8], g: &mut BudgetGuard<'_>) -> Result<()> {
    let mut alg = Der::new(content);
    let oid = alg.next_expect(Tag::OID, g)?;
    if der::oid_arcs(oid.content).is_none_or(|arcs| {
        // Only id-PSpecified (`1.2.840.113549.1.1.9`, the sole pSource
        // algorithm PKCS#1 defines; RFC 8017 A.2.4) tolerates the empty-label
        // check below. Anything else cannot be reasoned about, so refuse it.
        arcs != oid::P_SPECIFIED
    }) {
        return Err(unsupported("OAEP pSourceAlgorithm is not id-PSpecified"));
    }
    if alg.is_empty() {
        return Ok(());
    }
    let label = alg.next_expect(Tag::OCTET_STRING, g)?;
    if !alg.is_empty() {
        return Err(der::malformed(
            alg.offset(),
            "trailing bytes in PSourceAlgorithm",
        ));
    }
    if label.content.is_empty() {
        Ok(())
    } else {
        Err(unsupported("OAEP with a non-empty label"))
    }
}

/// `IssuerAndSerialNumber ::= SEQUENCE { issuer RDNSequence, serialNumber
/// CertificateSerialNumber }` (RFC 5652 §10.2.3, quoting RFC 5280).
fn parse_issuer_and_serial<'a>(
    content: &'a [u8],
    g: &mut BudgetGuard<'_>,
) -> Result<RecipientIdentifier<'a>> {
    let mut rid = Der::new(content);
    let issuer = rid.next_expect(Tag::SEQUENCE, g)?;
    let serial = rid.next_expect(Tag::INTEGER, g)?;
    if serial.content.is_empty() || !rid.is_empty() {
        return Err(der::malformed(
            rid.offset(),
            "malformed IssuerAndSerialNumber",
        ));
    }
    Ok(RecipientIdentifier::IssuerAndSerialNumber {
        issuer: issuer.content,
        serial: serial.content,
    })
}

/// `KeyAgreeRecipientInfo ::= [1] EXPLICIT SEQUENCE { version, originator
/// [0] EXPLICIT, ukm [1] EXPLICIT OPTIONAL, keyEncryptionAlgorithm,
/// recipientEncryptedKeys }` (RFC 5652 §6.2.2) — one
/// [`KeyTransport::EcdhAesKw`] entry per `RecipientEncryptedKey`.
fn parse_key_agree_recipient<'a>(
    data: &'a [u8],
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<KeyTransport<'a>>> {
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
    let mut transports = Vec::new();
    let mut entries = Der::new(keys.content);
    while !entries.is_empty() {
        let entry = entries.next_expect(Tag::SEQUENCE, g)?;
        let mut entry = Der::new(entry.content);
        // RecipientEncryptedKey ::= SEQUENCE { rid RecipientIdentifier,
        // encryptedContent OCTET STRING } — the rid is parsed for the
        // certificate-identity match (SL-1.ENC.07).
        let rid_at = entry.offset();
        let rid = entry.next(g)?;
        let identifier = match rid.tag {
            Tag::SEQUENCE => parse_issuer_and_serial(rid.content, g)?,
            Tag::CTX_0 => RecipientIdentifier::SubjectKeyIdentifier(rid.content),
            _ => return Err(der::malformed(rid_at, "bad RecipientIdentifier")),
        };
        let wrapped = entry.next_expect(Tag::OCTET_STRING, g)?;
        if !entry.is_empty() {
            return Err(der::malformed(
                entry.offset(),
                "trailing bytes in RecipientEncryptedKey",
            ));
        }
        transports.push(KeyTransport::EcdhAesKw {
            identifier,
            originator: originator_point,
            wrapped_key: wrapped.content,
        });
    }
    if transports.is_empty() {
        return Err(der::malformed(0, "empty recipientEncryptedKeys"));
    }
    Ok(transports)
}

/// `EncryptedContentInfo ::= SEQUENCE { contentType,
/// contentEncryptionAlgorithm, encryptedContent [0] IMPLICIT OCTET STRING
/// OPTIONAL }`.
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
        // ── SL-1.ENC.08 legacy *read* paths (module docs) ────────────────
        Some(oid::RC4) => {
            // PKCS #5's RC4 carries no IV; Acrobat-era envelopes emit `NULL`
            // or nothing at all — both are accepted, other parameters are not.
            if !alg_reader.is_empty() {
                let at = alg_reader.offset();
                let params = alg_reader.next(g)?;
                if params.tag != Tag::NULL || !params.content.is_empty() || !alg_reader.is_empty() {
                    return Err(der::malformed(at, "unexpected RC4 parameters"));
                }
            }
            CekAlgorithm::Rc4
        }
        Some(oid::DES_EDE3_CBC) => CekAlgorithm::TdeaCbc {
            iv: cbc_iv8(&mut alg_reader, g)?,
        },
        Some(oid::RC2_CBC) => {
            // RC2-CBC-Parameter ::= SEQUENCE { rc2EffectiveKeyLength
            // INTEGER (0..1024) DEFAULT 0, iv OCTET STRING }
            let (effective_bits, iv) = rc2_params(&mut alg_reader, g)?;
            CekAlgorithm::Rc2Cbc { iv, effective_bits }
        }
        // PasswordBasedRecipientInfo, AES key wrap, …: a typed refusal.
        _ => return Err(unsupported("unsupported content-encryption algorithm")),
    };
    let encrypted_content = if eci.is_empty() {
        None
    } else {
        let at = eci.offset();
        // RFC 5652 says `[0] IMPLICIT OCTET STRING`, i.e. a *primitive*
        // `ctx 0`; the CMS writers PDF actually met also emit the explicit
        // `[0] { OCTET STRING }` form. Accept both — they decode the same
        // octet string.
        let tlv = eci.next(g)?;
        let content = match tlv.tag {
            Tag::CTX_0 => tlv.content,
            Tag::CTX_0_CONSTRUCTED => {
                let mut inner = Der::new(tlv.content);
                let octets = inner.next_expect(Tag::OCTET_STRING, g)?;
                if !inner.is_empty() {
                    return Err(der::malformed(at, "trailing bytes in encryptedContent"));
                }
                octets.content
            }
            _ => return Err(der::malformed(at, "bad encryptedContent tag")),
        };
        if !eci.is_empty() {
            return Err(der::malformed(at, "trailing bytes in encryptedContent"));
        }
        Some(content)
    };
    Ok((cek_algorithm, encrypted_content))
}

/// Read the 8-byte IV of an RC2/TDEA `AlgorithmIdentifier`.
fn cbc_iv8(alg_reader: &mut Der<'_>, g: &mut BudgetGuard<'_>) -> Result<[u8; 8]> {
    let iv = iv_octet(alg_reader, g, 8)?;
    let bytes = <[u8; 8]>::try_from(iv).unwrap_or([0u8; 8]);
    Ok(bytes)
}

/// A fixed-length OCTET STRING parameter of an `AlgorithmIdentifier`, used by
/// the AES-CBC (16) and RC2/TDEA (8) IV slots.
fn iv_octet<'a>(
    alg_reader: &'a mut Der<'_>,
    g: &mut BudgetGuard<'_>,
    len: usize,
) -> Result<&'a [u8]> {
    if alg_reader.is_empty() {
        return Err(der::malformed(
            alg_reader.offset(),
            "missing CBC IV parameter",
        ));
    }
    let iv = alg_reader.next_expect(Tag::OCTET_STRING, g)?;
    let iv = iv
        .content
        .get(..len)
        .filter(|_| iv.content.len() == len)
        .ok_or_else(|| der::malformed(alg_reader.offset(), "CBC IV has the wrong length"))?;
    if !alg_reader.is_empty() {
        return Err(der::malformed(
            alg_reader.offset(),
            "trailing bytes in AlgorithmIdentifier",
        ));
    }
    Ok(iv)
}

/// `RC2-CBC-Parameter ::= SEQUENCE { rc2EffectiveKeyLength INTEGER (0..1024)
/// DEFAULT 0, iv OCTET STRING (SIZE(8)) }`. Both members are optional in
/// practice; a writer that supplies only the IV gets `effective_bits = 0`
/// ("the whole key").
fn rc2_params(alg_reader: &mut Der<'_>, g: &mut BudgetGuard<'_>) -> Result<(usize, [u8; 8])> {
    let at = alg_reader.offset();
    if alg_reader.is_empty() {
        return Err(der::malformed(at, "missing RC2-CBC-Parameter"));
    }
    let params = alg_reader.next_expect(Tag::SEQUENCE, g)?;
    if !alg_reader.is_empty() {
        return Err(der::malformed(
            alg_reader.offset(),
            "trailing bytes in AlgorithmIdentifier",
        ));
    }
    let mut p = Der::new(params.content);
    let mut effective_bits = 0usize;
    let mut iv: Option<[u8; 8]> = None;
    while !p.is_empty() {
        let at = p.offset();
        let tlv = p.next(g)?;
        match tlv.tag {
            Tag::INTEGER => {
                if iv.is_some() {
                    return Err(der::malformed(at, "RC2 parameters out of order"));
                }
                let bits = integer_u64(tlv.content)
                    .and_then(|v| usize::try_from(v).ok())
                    .filter(|v: &usize| *v <= 1024)
                    .ok_or_else(|| der::malformed(at, "bad rc2EffectiveKeyLength"))?;
                effective_bits = bits;
            }
            Tag::OCTET_STRING => {
                let iv_bytes = <[u8; 8]>::try_from(
                    tlv.content
                        .get(..8)
                        .filter(|b| b.len() == 8)
                        .ok_or_else(|| der::malformed(at, "RC2 IV must be 8 bytes"))?,
                )
                .map_err(|_| der::malformed(at, "RC2 IV must be 8 bytes"))?;
                iv = Some(iv_bytes);
            }
            _ => return Err(der::malformed(at, "unexpected RC2-CBC-Parameter member")),
        }
    }
    Ok((
        effective_bits,
        iv.ok_or_else(|| der::malformed(at, "RC2 missing IV"))?,
    ))
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
    let iv = iv_octet(alg_reader, g, 16)?;
    let bytes = <[u8; 16]>::try_from(iv).unwrap_or([0u8; 16]);
    Ok(bytes)
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
    // DER positive integers carry a leading 0x00 exactly when the first
    // content octet would otherwise read as a negative two's-complement
    // value. So reject a first octet with the sign bit set (that is a
    // negative integer), then read the remaining octets as an unsigned
    // magnitude.
    if content.is_empty() || content.first().is_some_and(|b| b & 0x80 != 0) {
        return None;
    }
    let mut s = content;
    while s.first() == Some(&0x00) && s.len() > 1 {
        s = &s[1..];
    }
    let mut value: u64 = 0;
    for &b in s {
        value = value.checked_mul(256)?.checked_add(u64::from(b))?;
    }
    Some(value)
}

/// Canonicalise a DER INTEGER content for value equality: strip exactly the
/// sign-extension octets DER minimality forbids, then compare bytes. Two
/// well-formed DER encodings compare equal by content already; this also
/// tolerates a BER-ish writer that padded the serial.
fn serial_eq(a: &[u8], b: &[u8]) -> bool {
    /// Strip redundant leading 0x00 (positive) / 0xFF (negative) padding.
    fn trim(content: &[u8]) -> &[u8] {
        let mut s = content;
        while let Some((&first, rest)) = s.split_first() {
            let redundant = match rest.first() {
                Some(next) => {
                    (first == 0x00 && next & 0x80 == 0) || (first == 0xFF && next & 0x80 != 0)
                }
                None => false,
            };
            if !redundant {
                break;
            }
            s = rest;
        }
        s
    }
    let (a, b) = (trim(a), trim(b));
    // An all-padding sequence trims to empty — treat as the zero INTEGER.
    let a_empty = a.is_empty();
    let b_empty = b.is_empty();
    if a_empty || b_empty {
        return a_empty && b_empty;
    }
    a == b
}

/// Does the certificate `ident` carry the address a recipient is named by?
/// `pub` so tests and the fuzz targets can re-verify a selection result
/// against the blob's identifiers independently of the selection loop.
#[must_use]
pub fn identifier_matches(
    ident: &CertIdentity<'_>,
    rid: &RecipientIdentifier<'_>,
) -> Option<Matched> {
    match rid {
        RecipientIdentifier::IssuerAndSerialNumber { issuer, serial } => {
            if ident.issuer == *issuer && serial_eq(ident.serial, serial) {
                Some(Matched::ByIssuerAndSerialNumber)
            } else {
                None
            }
        }
        RecipientIdentifier::SubjectKeyIdentifier(key_id) => {
            if key_id.is_empty() {
                return None;
            }
            if ident.extension_ski.is_some_and(|ski| ski == *key_id)
                || ident
                    .computed_ski
                    .is_some_and(|ski| ski.as_slice() == *key_id)
            {
                Some(Matched::BySubjectKeyIdentifier)
            } else {
                None
            }
        }
    }
}

/// The 24-byte CMS payload format: a 20-byte seed plus this recipient's
/// 4 permission bytes (little-endian, Table-23 bit semantics — enforced by
/// the policy layer per SL-1.ENC.09).
pub const CMS_PAYLOAD_LEN: usize = 24;

/// Decode the 4-byte permission block of a decrypted CMS payload.
///
/// The payload must be **exactly** [`CMS_PAYLOAD_LEN`] bytes; anything else
/// is a failed wrong-key detection or a damaged file, never a silent
/// zero-grant: `None` is the caller's "this is not a valid payload" signal,
/// and the selection loop treats it exactly like a failed decrypt (try next
/// recipient / [`Code::RecipientNoMatch`]) — no bypass, no default grant.
#[must_use]
pub fn decode_permission_block(payload: &[u8]) -> Option<u32> {
    if payload.len() != CMS_PAYLOAD_LEN {
        return None;
    }
    let block: [u8; 4] = payload.get(20..24)?.try_into().ok()?;
    Some(u32::from_le_bytes(block))
}

/// The 20-byte seed of a decrypted CMS payload (see
/// [`decode_permission_block`] for the strictness rationale).
#[must_use]
pub fn payload_seed(payload: &[u8]) -> Option<&[u8]> {
    if payload.len() != CMS_PAYLOAD_LEN {
        return None;
    }
    payload.get(..20)
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
/// Recipient selection follows the credential's [`MatchBy`] policy
/// (SL-1.ENC.07): with a certificate chain, recipients are first selected by
/// explicit `RecipientIdentifier` (issuer/serial or SKI match) and only
/// those get unwrapped; no identifier match falls through to the structural
/// first-valid scan (`Auto`) or fails typed (`Certificate`). A bare private
/// key always uses the structural scan — the key material's own integrity
/// checks are the selector (design note §4.1).
///
/// # Budget
///
/// The recipient parses charge their wire bytes via the guard; the seed-hash
/// input is a bounded concatenation (seed + already-resident recipient
/// blobs) charged through [`alloc::vec_with_capacity`] before any
/// allocation. A hostile blob set terminates within its own byte budget.
///
/// # Malformed Input
///
/// A structurally damaged blob is [`Code::EncryptMalformed`]; a recognised
/// but unimplemented algorithm is [`Code::EncryptUnsupported`]; a credential
/// that opens no recipient (wrong key, wrong credential type, or no
/// certificate-identity match under [`MatchBy::Certificate`]) is
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
    let rsa_key = match &credential.key {
        KeyMaterial::Rsa(der_bytes) => {
            let key = rsa::RsaPrivateKey::from_pkcs8_der(der_bytes).map_err(|_| {
                err!(
                    Code::EncryptMalformed,
                    during = "pkcs7",
                    detail = "bad RSA PKCS#8 key"
                )
            })?;
            Some(key)
        }
        KeyMaterial::EcP256(_) => None,
    };
    let ec_key = match &credential.key {
        KeyMaterial::EcP256(der_bytes) => {
            let key = p256::SecretKey::from_pkcs8_der(der_bytes).map_err(|_| {
                err!(
                    Code::EncryptMalformed,
                    during = "pkcs7",
                    detail = "bad EC PKCS#8 key"
                )
            })?;
            Some(key)
        }
        KeyMaterial::Rsa(_) => None,
    };

    // Parse every blob once: the file-key derivation needs all of them, and
    // the certificate-identity pass needs each transport's identifier.
    // The vector is bounded by the (already-resident) recipient array.
    let mut parsed = Vec::new();
    for blob in recipients {
        let enveloped = parse_enveloped_data(blob, g)?;
        if enveloped.encrypted_content.is_none() {
            return Err(unsupported("detached CMS content"));
        }
        parsed.push(enveloped);
    }

    // SL-1.ENC.07: certificate identities, leaf first. A damaged chain
    // certificate is malformed input — never silently dropped.
    let chain = if credential.match_by == MatchBy::FirstValid {
        Vec::new()
    } else {
        let mut chain = Vec::new();
        for cert in &credential.certificates {
            chain.push(crate::x509::parse_identity(cert, g)?);
        }
        chain
    };

    // Identity-first pass (only with a chain): unwrap strictly the
    // identifier-matched transports; then — under `Auto` — fall through to
    // the draft's structural first-valid scan. `Certificate` never falls
    // through; `FirstValid` never looks at identifiers.
    if !chain.is_empty() && credential.match_by != MatchBy::FirstValid {
        if let Some(auth) = open_pass(
            &parsed,
            recipients,
            key_len,
            sha256,
            encrypt_metadata,
            Some(&chain),
            rsa_key.as_ref(),
            ec_key.as_ref(),
            g,
        )? {
            return Ok(auth);
        }
    }
    if credential.match_by != MatchBy::Certificate {
        if let Some(auth) = open_pass(
            &parsed,
            recipients,
            key_len,
            sha256,
            encrypt_metadata,
            None,
            rsa_key.as_ref(),
            ec_key.as_ref(),
            g,
        )? {
            return Ok(auth);
        }
    }
    let detail = if credential.match_by == MatchBy::Certificate {
        "the supplied certificate chain addresses no recipient (MatchBy::Certificate)"
    } else if chain.is_empty() {
        "no recipient opened with the supplied credential"
    } else {
        "no certificate-identity match fell through to a structural opening match"
    };
    Err(err!(
        Code::RecipientNoMatch,
        during = "pkcs7",
        detail = detail
    ))
}

/// One selection pass over the parsed blobs.
///
/// `chain = Some(..)` selects recipients strictly by certificate identity
/// (SL-1.ENC.07) and reports how each was matched; `None` is the draft's
/// structural scan — try each transport in array order, the first valid
/// 24-byte payload wins. `Ok(None)` means no recipient opened (the caller
/// decides the fall-through); budget/derivation errors on an *opened*
/// payload propagate.
#[allow(clippy::too_many_arguments)]
fn open_pass(
    parsed: &[EnvelopedData<'_>],
    recipients: &[&[u8]],
    key_len: usize,
    sha256: bool,
    encrypt_metadata: bool,
    chain: Option<&[CertIdentity<'_>]>,
    rsa_key: Option<&rsa::RsaPrivateKey>,
    ec_key: Option<&p256::SecretKey>,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<PubKeyAuth>> {
    for (blob_idx, enveloped) in parsed.iter().enumerate() {
        let Some(encrypted_content) = enveloped.encrypted_content else {
            continue; // refused by the caller, but the borrow checker is content.
        };
        for transport in &enveloped.recipients {
            let matched_by = match chain {
                Some(chain) => {
                    let Some(matched) = chain
                        .iter()
                        .find_map(|ident| identifier_matches(ident, transport.identifier()))
                    else {
                        continue; // no chain certificate addresses this one
                    };
                    matched
                }
                None => Matched::Structurally,
            };
            let Some(cek) = unwrap_cek(transport, rsa_key, ec_key) else {
                continue; // wrong credential for this transport; try the next
            };
            let Some(payload) =
                decrypt_payload(cek.as_slice(), enveloped.cek_algorithm, encrypted_content)
            else {
                continue; // padding/payload check failed: wrong key, try the next
            };
            // The payload is exactly seed(20) + permission bytes(4); a
            // structurally invalid payload continues the scan (never a
            // grant) — the same wrong-key detector the draft relied on,
            // now behind decode_permission_block (SL-1.ENC.09: never a
            // silent bypass).
            let seed = match payload_seed(&payload) {
                Some(seed) => seed,
                None => continue,
            };
            let permissions = match decode_permission_block(&payload) {
                Some(permissions) => permissions,
                None => continue,
            };
            let key = derive_file_key(seed, recipients, key_len, sha256, encrypt_metadata, g)?;
            return Ok(Some(PubKeyAuth {
                key,
                permissions,
                matched_by,
                recipient_index: blob_idx,
            }));
        }
    }
    Ok(None)
}

/// Unwrap a CEK from one recipient transport under the supplied key material.
///
/// Returns `None` when the key's type does not match the transport or the
/// unwrap fails (wrong key) — both are "try the next recipient", not errors.
fn unwrap_cek(
    transport: &KeyTransport<'_>,
    rsa_key: Option<&rsa::RsaPrivateKey>,
    ec_key: Option<&p256::SecretKey>,
) -> Option<Vec<u8>> {
    match transport {
        KeyTransport::RsaPkcs1v15 { encrypted_key, .. } => {
            let key = rsa_key?;
            // RSAES-PKCS1-v1_5 decrypt: a wrong key fails the padding check
            // (RUSTSEC-2023-0071 residual accepted for local decryption —
            // design note §6).
            key.decrypt(rsa::Pkcs1v15Encrypt, encrypted_key).ok()
        }
        KeyTransport::RsaOaep {
            encrypted_key,
            oaep,
            ..
        } => {
            let key = rsa_key?;
            // RSAES-OAEP (MGF1) unwrap: the hash/mgf pair came from the
            // algorithm parameters at parse time. Unlike the v1.5 arm there is
            // no Marvin-style padding oracle concern here (the whole OAEP
            // decode is constant-time inside `rsa`), and a wrong key simply
            // fails the hash check.
            use rsa::Oaep;
            match (oaep.hash, oaep.mgf_hash) {
                (OaepHash::Sha1, OaepHash::Sha1) => {
                    key.decrypt(Oaep::new::<sha1::Sha1>(), encrypted_key).ok()
                }
                (OaepHash::Sha1, OaepHash::Sha256) => key
                    .decrypt(
                        Oaep::new_with_mgf_hash::<sha1::Sha1, sha2::Sha256>(),
                        encrypted_key,
                    )
                    .ok(),
                (OaepHash::Sha256, OaepHash::Sha1) => key
                    .decrypt(
                        Oaep::new_with_mgf_hash::<sha2::Sha256, sha1::Sha1>(),
                        encrypted_key,
                    )
                    .ok(),
                (OaepHash::Sha256, OaepHash::Sha256) => {
                    key.decrypt(Oaep::new::<sha2::Sha256>(), encrypted_key).ok()
                }
                (OaepHash::Sha384, OaepHash::Sha384) => {
                    key.decrypt(Oaep::new::<sha2::Sha384>(), encrypted_key).ok()
                }
                (OaepHash::Sha512, OaepHash::Sha512) => {
                    key.decrypt(Oaep::new::<sha2::Sha512>(), encrypted_key).ok()
                }
                // An MGF hash other than the label hash (MGF1-SHA-384/512
                // combos) is *not* something PDF writers emit; refuse by
                // continuing the scan (the transport simply never matches, and
                // the caller reports `RecipientNoMatch`).
                _ => None,
            }
        }
        KeyTransport::EcdhAesKw {
            originator,
            wrapped_key,
            ..
        } => {
            let secret = ec_key?;
            let public = p256::PublicKey::from_sec1_bytes(originator).ok()?;
            let shared = p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), public.as_affine());
            let shared_bytes = *shared.raw_secret_bytes();
            // The KEK length follows the wrapped key: RFC 3394 output is key
            // + 8 bytes, so a 24-byte blob wraps a 16-byte CEK (AES-128) and
            // a 40-byte blob a 32-byte CEK (AES-256).
            if wrapped_key.len() != 24 && wrapped_key.len() != 40 {
                return None;
            }
            let kek = kdf_x963_sha256(&shared_bytes, wrapped_key.len() - 8);
            aes_kw_unwrap(&kek, wrapped_key)
        }
    }
}

/// Decrypt the payload with the CEK, strictly validating the block cipher's
/// PKCS#7 padding (the wrong-key detector) plus the exact 24-byte payload
/// rule. The honest "a wrong CEK cannot slip silently" story (SL-1.ENC.08),
/// per design note §5:
///
/// * The AES-TDEA-RC2 arms run in CBC with padding, so a wrong CEK must
///   survive a PKCS#7 check *and* yield a 24-byte plaintext.
/// * RC4 has no padding, so its *only* gate is the key-transport unwrap
///   (`unwrap_cek`) having run before this function: the RC4 key is the bytes
///   the RSAES-PKCS1-v1_5/RSAES-OAEP padding check authenticated (or the
///   `rc4_3072`-style export length it recovered), and the AES path above runs
///   the same check as a *bonus*, which makes the legacy RC4 arm exactly as
///   strict as the *pre-ENC.08* claim the sign-off already accepted about the
///   AES arm's unwrap. A wrong RSA key therefore still ends the scan with
///   [`Code::RecipientNoMatch`], never a garbage file key.
fn decrypt_payload(cek: &[u8], algorithm: CekAlgorithm, content: &[u8]) -> Option<Vec<u8>> {
    if !algorithm.accepts_cek(cek) {
        return None; // CEK size mismatch: wrong recipient path
    }
    match algorithm {
        CekAlgorithm::Rc4 => {
            // No padding: CMS's RC4 seals *exactly* the payload length, so the
            // 24-byte rule carries the structural check.
            if content.len() != CMS_PAYLOAD_LEN {
                return None;
            }
            Some(crate::rc4(cek, content))
        }
        CekAlgorithm::Aes128Cbc { iv }
        | CekAlgorithm::Aes192Cbc { iv }
        | CekAlgorithm::Aes256Cbc { iv } => {
            let cipher = AesCipher::new(cek.len(), cek)?;
            payload_cbc(&cipher, &iv, content)
        }
        CekAlgorithm::TdeaCbc { iv } => {
            let cipher = crate::legacy::Tdes::new(cek)?;
            payload_cbc(&cipher, &iv, content)
        }
        CekAlgorithm::Rc2Cbc { iv, effective_bits } => {
            let cipher = crate::legacy::Rc2::new(cek, effective_bits)?;
            payload_cbc(&cipher, &iv, content)
        }
    }
}

/// A payload block cipher: 16-byte AES or the 8-byte legacy modes.
trait PayloadBlock {
    const BLOCK: usize;
    fn transform(&self, block: &mut [u8]);
}

impl PayloadBlock for AesCipher {
    const BLOCK: usize = 16;
    fn transform(&self, block: &mut [u8]) {
        if let Ok(arr) = <[u8; 16]>::try_from(&*block) {
            let mut arr: Block = arr.into();
            self.decrypt_block(&mut arr);
            block.copy_from_slice(&arr);
        }
    }
}

impl PayloadBlock for crate::legacy::Tdes {
    const BLOCK: usize = 8;
    fn transform(&self, block: &mut [u8]) {
        if let Ok(mut arr) = <[u8; 8]>::try_from(&mut *block) {
            self.decrypt_block(&mut arr);
            block.copy_from_slice(&arr);
        }
    }
}

impl PayloadBlock for crate::legacy::Rc2 {
    const BLOCK: usize = 8;
    fn transform(&self, block: &mut [u8]) {
        if let Ok(mut arr) = <[u8; 8]>::try_from(&mut *block) {
            self.decrypt_block(&mut arr);
            block.copy_from_slice(&arr);
        }
    }
}

/// One CBC payload decrypt: exact block multiples, IV from the algorithm
/// parameters, strict PKCS#7 (pad in 1..=block, every pad byte equal), and a
/// plaintext of exactly [`CMS_PAYLOAD_LEN`] bytes — the *wrong-key detector*
/// every arm leans on (RC4's is above).
fn payload_cbc<C: PayloadBlock>(cipher: &C, iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    if iv.len() != C::BLOCK || data.is_empty() || data.len() % C::BLOCK != 0 {
        return None;
    }
    let mut out = Vec::new();
    let mut prev: &[u8] = iv;
    for chunk in data.chunks(C::BLOCK) {
        let mut block = chunk.to_vec();
        cipher.transform(&mut block);
        for k in 0..C::BLOCK {
            out.push(block[k] ^ prev[k]);
        }
        prev = chunk;
    }
    let pad = usize::from(*out.last()?);
    if pad == 0 || pad > C::BLOCK || out.len() < pad {
        return None;
    }
    let (body, padding) = out.split_at(out.len() - pad);
    if padding.iter().any(|&b| usize::from(b) != pad) || body.len() != CMS_PAYLOAD_LEN {
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

/// X9.63 KDF with SHA-256 and an empty SharedInfo (RFC 5753 §2.1.1 static
/// stdDH): `T(i) = SHA256(Z || be32(i))`, concatenated and truncated.
fn kdf_x963_sha256(z: &[u8], out_len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut counter: u32 = 1;
    while out.len() < out_len {
        let mut h = Sha256::new();
        h.update(z);
        h.update(counter.to_be_bytes());
        out.extend_from_slice(&h.finalize());
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
            let mut block: Block = input.into();
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
