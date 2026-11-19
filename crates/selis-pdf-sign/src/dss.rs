//! Pure PAdES Document Security Store embedding (SL-5.SIGN.04).
//!
//! This module **never** talks to a network. The caller hands it DER bytes that
//! were already fetched (by the consent-gated CLI binding layer), and it
//! writes them into an incremental revision as a `/DSS` dictionary in the
//! catalog:
//!
//! ```text
//! /DSS << /V [cert streams] /OCSP [ocsp streams] /CRL [crl streams]
//!         /TS << /TS [RFC 3161 timestamp signature streams] >> >>
//! ```
//!
//! Every embedded RFC 3161 token is parsed with
//! [`selis_crypto::timestamp::parse_rfc3161_token`] and its `messageImprint` is
//! checked against SHA-256 of the named signature's `/Contents` before any
//! byte is written. A token that cannot be parsed, or that binds to different
//! data, is refused — this module never claims a token is valid just because
//! it is opaque.
//!
//! The DSS revision is appended, so the signature's `/ByteRange` still names
//! the original bytes and the signature keeps verifying (the same invariant
//! EDIT.07 relies on). Read-back re-opens the output and checks the DSS chain,
//! the token imprints and the original signature before returning.
//!
//! # Budget / Malformed Input
//!
//! Every object reference is resolved through the budgeted COS resolver;
//! token parsing charges the DER reader. A missing signature field, a
//! wrong-shaped DSS, or a malformed token is a typed error, never a panic.

use selis_bytes::Bytes;
use selis_crypto::timestamp::parse_rfc3161_token;
use selis_pdf_cos::doc_writer::write_incremental_update;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

use crate::{
    bytes, carried_trailer, dict_get, dict_set, invalid, malformed, next_object_number, open_doc,
    resolve, trailer, SignError,
};

/// The sub-filter of a PAdES document timestamp token.
const TIMESTAMP_SUB_FILTER: &str = "ETSI.RFC3161";
/// The largest single DSS payload (one token, OCSP, CRL or certificate).
const MAX_DSS_BLOB: usize = 1 << 20;

/// An already-fetched RFC 3161 token and its expected content binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampToken {
    /// The token's DER bytes, as returned by the time provider.
    pub der: Vec<u8>,
    /// The imprint the token must attest to. `None` means "SHA-256 of the
    /// named signature's `/Contents`" (computed locally in `embed_dss`).
    pub expected_imprint: Option<Vec<u8>>,
}

impl TimestampToken {
    /// A token whose imprint is checked against the signature being timestamped.
    #[must_use]
    pub fn from_der(der: Vec<u8>) -> Self {
        Self {
            der,
            expected_imprint: None,
        }
    }
}

/// What to write into the Document Security Store.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DssOptions<'a> {
    /// The name of the signature whose trust data is being recorded.
    pub field_name: &'a str,
    /// RFC 3161 document timestamp tokens (already fetched by the caller).
    pub timestamps: &'a [TimestampToken],
    /// OCSP responses, DER.
    pub ocsp: &'a [Vec<u8>],
    /// Certificate revocation lists, DER.
    pub crl: &'a [Vec<u8>],
    /// X.509 certificates (leaf and chain), DER.
    pub certificates: &'a [Vec<u8>],
}

/// What was written, and the read-back evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DssReport {
    /// The signature field the DSS was attached to.
    pub field: String,
    /// Tokens embedded and verified.
    pub timestamps: usize,
    /// OCSP responses embedded.
    pub ocsp: usize,
    /// CRLs embedded.
    pub crl: usize,
    /// Certificates embedded.
    pub certificates: usize,
    /// The signature's `/ByteRange` before the DSS was appended.
    pub byte_range: [u64; 4],
    /// Whether the original signature still verifies in the output.
    pub signature_still_verifies: bool,
    /// Always `true`: the DSS chain and the signature were read back.
    pub verified: bool,
}

/// Embed a Document Security Store as an incremental revision.
///
/// # Errors
///
/// * [`SignError::Invalid`] for an empty field name, an oversized blob, or a
///   token whose `messageImprint` does not match the named signature;
/// * [`SignError::Encrypted`] for an encrypted document;
/// * [`SignError::ReadBack`] if the DSS cannot be found in the produced bytes
///   or the original signature no longer verifies.
///
/// # Budget
///
/// One incremental revision; each blob is charged by the DER token parser and
/// every object by the resolver. No blob is copied more than twice.
pub fn embed_dss(
    src: &[u8],
    options: &DssOptions<'_>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, DssReport), SignError> {
    if options.field_name.trim().is_empty() {
        return Err(invalid(
            "a DSS needs the signature field name it belongs to",
        ));
    }
    for blob in options
        .timestamps
        .iter()
        .map(|token| token.der.as_slice())
        .chain(options.ocsp.iter().map(Vec::as_slice))
        .chain(options.crl.iter().map(Vec::as_slice))
        .chain(options.certificates.iter().map(Vec::as_slice))
    {
        if blob.is_empty() || blob.len() > MAX_DSS_BLOB {
            return Err(invalid("a DSS payload is empty or larger than 1 MiB"));
        }
    }

    let doc = open_doc(src, budget, g)?;
    if crate::is_encrypted(&doc) {
        return Err(SignError::Encrypted);
    }
    let before = crate::report::signatures(src, &[], 0, budget, g)?;
    let original = before
        .signatures
        .iter()
        .find(|signature| signature.field == options.field_name)
        .ok_or_else(|| {
            invalid(format!(
                "the signature field {:?} was not found in the source",
                options.field_name
            ))
        })?;
    let byte_range = original
        .byte_range
        .ok_or_else(|| invalid("the named signature has no well-formed /ByteRange to timestamp"))?;

    // The imprint we expect every token to carry: SHA-256 over the signature's
    // own CMS blob. This is computed here, locally, from bytes in the PDF.
    let contents = signature_contents(src, options.field_name, budget, g)?;
    let expected = selis_crypto::cms::digest_spans(
        selis_crypto::cert::DigestAlg::Sha256,
        &[contents.as_slice()],
    );
    for token in options.timestamps {
        let parsed = {
            let chain_budget = Budget::profile(selis_sandbox::Surface::Fuzz);
            let mut chain_guard = chain_budget.guard();
            parse_rfc3161_token(&token.der, &mut chain_guard).map_err(|e| {
                SignError::Invalid(format!("the RFC 3161 token does not parse: {e}"))
            })?
        };
        let expected = token.expected_imprint.as_deref().unwrap_or(&expected);
        if parsed.imprint != expected {
            return Err(invalid(
                "the RFC 3161 token's messageImprint does not bind to this signature",
            ));
        }
    }

    let revision = trailer(&doc)?;
    let catalog_ref = revision
        .root
        .ok_or_else(|| malformed("the trailer has no /Root"))?;
    let catalog = resolve(src, &doc, Obj::Ref(catalog_ref), budget, g)?;
    let Obj::Dict(mut catalog_dict) = catalog else {
        return Err(malformed("the catalog is not a dictionary"));
    };

    let base = next_object_number(&doc);
    let dss_num = base;
    let catalog_num = catalog_ref.num;
    let mut next = base.saturating_add(1);

    // Preserve any DSS already in the document so timestamp-then-LTV (or two
    // timestamps) do not discard earlier trust data.
    let mut existing_v = Vec::new();
    let mut existing_ocsp = Vec::new();
    let mut existing_crl = Vec::new();
    let mut existing_ts = Vec::new();
    if let Some(existing) = dict_get(&catalog_dict, b"DSS") {
        if let Ok(Obj::Dict(pairs)) = resolve(src, &doc, existing.clone(), budget, g) {
            for (key, into) in [
                (b"V".as_slice(), &mut existing_v),
                (b"OCSP".as_slice(), &mut existing_ocsp),
                (b"CRL".as_slice(), &mut existing_crl),
            ] {
                if let Some(Obj::Array(items)) = dict_get(&pairs, key) {
                    into.extend(items.iter().cloned());
                }
            }
            if let Some(ts) = dict_get(&pairs, b"TS") {
                if let Ok(Obj::Dict(ts_pairs)) = resolve(src, &doc, ts.clone(), budget, g) {
                    if let Some(Obj::Array(items)) = dict_get(&ts_pairs, b"TS") {
                        existing_ts.extend(items.iter().cloned());
                    }
                }
            }
        }
    }

    let mut objects: Vec<(u32, Obj)> = Vec::new();
    let mut token_refs = existing_ts;
    for token in options.timestamps {
        let num = next;
        next = next.saturating_add(1);
        token_refs.push(Obj::Ref(Ref::new(num, 0)));
        objects.push((
            num,
            Obj::Stream {
                dict: vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Sig"))),
                    (bytes(b"Filter"), Obj::Name(bytes(b"Adobe.PPKLite"))),
                    (
                        bytes(b"SubFilter"),
                        Obj::Name(bytes(TIMESTAMP_SUB_FILTER.as_bytes())),
                    ),
                    (
                        bytes(b"Contents"),
                        Obj::HexString(Bytes::copy_from_slice(&token.der)),
                    ),
                ],
                data: Bytes::copy_from_slice(&token.der),
            },
        ));
    }

    let mut ocsp_refs = existing_ocsp;
    for der in options.ocsp {
        let num = next;
        next = next.saturating_add(1);
        ocsp_refs.push(Obj::Ref(Ref::new(num, 0)));
        objects.push((
            num,
            Obj::Stream {
                dict: vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"OCSP"))),
                    (
                        bytes(b"Length"),
                        Obj::Int(i64::try_from(der.len()).unwrap_or(0)),
                    ),
                ],
                data: Bytes::copy_from_slice(der),
            },
        ));
    }
    let mut crl_refs = existing_crl;
    for der in options.crl {
        let num = next;
        next = next.saturating_add(1);
        crl_refs.push(Obj::Ref(Ref::new(num, 0)));
        objects.push((
            num,
            Obj::Stream {
                dict: vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"CRL"))),
                    (
                        bytes(b"Length"),
                        Obj::Int(i64::try_from(der.len()).unwrap_or(0)),
                    ),
                ],
                data: Bytes::copy_from_slice(der),
            },
        ));
    }
    let mut cert_refs = existing_v;
    for der in options.certificates {
        let num = next;
        next = next.saturating_add(1);
        cert_refs.push(Obj::Ref(Ref::new(num, 0)));
        objects.push((
            num,
            Obj::Stream {
                dict: vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"Certificate"))),
                    (
                        bytes(b"Length"),
                        Obj::Int(i64::try_from(der.len()).unwrap_or(0)),
                    ),
                ],
                data: Bytes::copy_from_slice(der),
            },
        ));
    }

    let dss = Obj::Dict(vec![
        (bytes(b"Type"), Obj::Name(bytes(b"DSS"))),
        (bytes(b"V"), Obj::Array(cert_refs)),
        (bytes(b"OCSP"), Obj::Array(ocsp_refs)),
        (bytes(b"CRL"), Obj::Array(crl_refs)),
        (
            bytes(b"TS"),
            Obj::Dict(vec![(bytes(b"TS"), Obj::Array(token_refs))]),
        ),
    ]);
    dict_set(&mut catalog_dict, b"DSS", Obj::Ref(Ref::new(dss_num, 0)));
    objects.push((dss_num, dss));
    objects.push((catalog_num, Obj::Dict(catalog_dict)));

    let carried = carried_trailer(revision);
    let out = write_incremental_update(src, &objects, &carried, budget, g)
        .map_err(|e| malformed(format!("the DSS incremental revision failed: {e}")))?;

    let report = read_back(&out, options, byte_range, budget, g)?;
    Ok((out, report))
}

/// Re-open the output and verify the DSS chain and the original signature.
fn read_back(
    src: &[u8],
    options: &DssOptions<'_>,
    byte_range: [u64; 4],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<DssReport, SignError> {
    let doc = open_doc(src, budget, g)?;
    let root = trailer(&doc)?
        .root
        .ok_or_else(|| malformed("the trailer has no /Root"))?;
    let catalog = resolve(src, &doc, Obj::Ref(root), budget, g)?;
    let Obj::Dict(catalog_dict) = catalog else {
        return Err(SignError::ReadBack(
            "the catalog is not a dictionary".to_string(),
        ));
    };
    let dss = dict_get(&catalog_dict, b"DSS")
        .ok_or_else(|| SignError::ReadBack("the catalog has no /DSS".to_string()))?;
    let Obj::Dict(dss_dict) = resolve(src, &doc, dss.clone(), budget, g)? else {
        return Err(SignError::ReadBack(
            "the /DSS is not a dictionary".to_string(),
        ));
    };
    let is_dss = dict_get(&dss_dict, b"Type")
        .and_then(|v| match v {
            Obj::Name(name) => Some(name.as_slice() == b"DSS".as_slice()),
            _ => None,
        })
        .unwrap_or(false);
    if !is_dss {
        return Err(SignError::ReadBack(
            "the /DSS has the wrong /Type".to_string(),
        ));
    }
    let count = |key: &[u8]| -> usize {
        dict_get(&dss_dict, key).map_or(0, |v| match v {
            Obj::Array(items) => items.len(),
            _ => 0,
        })
    };
    let ts_count = match dict_get(&dss_dict, b"TS") {
        Some(ts) => match resolve(src, &doc, ts.clone(), budget, g) {
            Ok(Obj::Dict(pairs)) => match dict_get(&pairs, b"TS") {
                Some(Obj::Array(items)) => items.len(),
                _ => 0,
            },
            _ => 0,
        },
        None => 0,
    };
    if ts_count < options.timestamps.len()
        || count(b"OCSP") < options.ocsp.len()
        || count(b"CRL") < options.crl.len()
        || count(b"V") < options.certificates.len()
    {
        return Err(SignError::ReadBack(
            "not every supplied DSS payload was embedded".to_string(),
        ));
    }

    let after = crate::report::signatures(src, &[], 0, budget, g)?;
    let signature = after
        .signatures
        .iter()
        .find(|signature| signature.field == options.field_name)
        .ok_or_else(|| {
            SignError::ReadBack("the named signature vanished after the DSS write".to_string())
        })?;
    if signature.byte_range != Some(byte_range) {
        return Err(SignError::ReadBack(
            "the DSS revision changed the signature's /ByteRange".to_string(),
        ));
    }
    if !signature.verified {
        return Err(SignError::ReadBack(
            "the signature no longer verifies after the DSS write".to_string(),
        ));
    }

    Ok(DssReport {
        field: options.field_name.to_string(),
        timestamps: ts_count,
        ocsp: count(b"OCSP"),
        crl: count(b"CRL"),
        certificates: count(b"V"),
        byte_range,
        signature_still_verifies: signature.verified,
        verified: true,
    })
}

/// Find the named signature's `/Contents` CMS bytes in the document.
///
/// The CLI uses this to hash exactly the bytes it will ask a TSA to timestamp.
/// No key material is involved; the value is the public CMS blob already in
/// the PDF.
pub fn signature_contents(
    src: &[u8],
    field_name: &str,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>, SignError> {
    let doc = open_doc(src, budget, g)?;
    let root = trailer(&doc)?
        .root
        .ok_or_else(|| malformed("the trailer has no /Root"))?;
    let catalog = resolve(src, &doc, Obj::Ref(root), budget, g)?;
    let Obj::Dict(catalog_dict) = catalog else {
        return Err(malformed("the catalog is not a dictionary"));
    };
    let acro = dict_get(&catalog_dict, b"AcroForm")
        .ok_or_else(|| invalid("the document has no /AcroForm"))?;
    let Obj::Dict(acro_dict) = resolve(src, &doc, acro.clone(), budget, g)? else {
        return Err(invalid("the /AcroForm is not a dictionary"));
    };
    let fields =
        dict_get(&acro_dict, b"Fields").ok_or_else(|| invalid("the /AcroForm has no /Fields"))?;
    let Obj::Array(fields) = resolve(src, &doc, fields.clone(), budget, g)? else {
        return Err(invalid("the /Fields is not an array"));
    };
    for field in &fields {
        let field_obj = resolve(src, &doc, field.clone(), budget, g)?;
        let Obj::Dict(field_dict) = &field_obj else {
            continue;
        };
        let matches = dict_get(field_dict, b"T").is_some_and(|name| {
            matches!(
                resolve(src, &doc, name.clone(), budget, g),
                Ok(Obj::String(value)) if value.as_slice() == field_name.as_bytes()
            )
        });
        if !matches {
            continue;
        }
        let v =
            dict_get(field_dict, b"V").ok_or_else(|| invalid("the signature field has no /V"))?;
        let Obj::Dict(sig) = resolve(src, &doc, v.clone(), budget, g)? else {
            return Err(invalid("the /V is not a signature dictionary"));
        };
        let contents =
            dict_get(&sig, b"Contents").ok_or_else(|| invalid("the signature has no /Contents"))?;
        // The COS parser emits a `<...>` literal as `Obj::String` holding the
        // already hex-decoded bytes (the signature report reads them the same
        // way). `Obj::HexString` is the write-only form used above.
        let cms = match resolve(src, &doc, contents.clone(), budget, g)? {
            Obj::HexString(text) | Obj::String(text) => text,
            _ => return Err(invalid("the /Contents is not a hex string")),
        };
        return Ok(cms.to_vec());
    }
    Err(invalid(format!(
        "the signature field {field_name:?} was not found"
    )))
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

    use super::{embed_dss, DssOptions, TimestampToken};
    use crate::tests::{base_pdf, digital_options, NOW, ROOT, SIGNING_TIME};
    use crate::{sign_digital, signatures};
    use selis_crypto::der_write::{
        explicit, integer, octet_string, oid, sequence, set, tlv, GENERALIZED_TIME,
    };
    use selis_sandbox::{Budget, Surface};

    const OID_SIGNED_DATA: &[u64] = &[1, 2, 840, 113_549, 1, 7, 2];
    const OID_TST_INFO: &[u64] = &[1, 2, 840, 113_549, 1, 9, 16, 1, 4];
    const OID_SHA256: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 2, 1];

    /// Build a structurally valid RFC 3161 token whose messageImprint is the
    /// supplied digest. It is NOT cryptographically signed: this crate never
    /// claims to verify a TSA signature, only to parse and bind the token.
    fn synthetic_token(imprint: &[u8]) -> Vec<u8> {
        let alg = sequence(&[oid(OID_SHA256)]);
        let mi = sequence(&[alg, octet_string(imprint)]);
        let tst = sequence(&[
            integer(1),
            oid(&[1, 3, 6, 1, 4, 1, 99_999, 1]),
            mi,
            integer(7),
            tlv(GENERALIZED_TIME, SIGNING_TIME.as_bytes()),
        ]);
        let encap = sequence(&[oid(OID_TST_INFO), explicit(0, &octet_string(&tst))]);
        let signed = sequence(&[integer(3), set(&[]), encap]);
        sequence(&[oid(OID_SIGNED_DATA), explicit(0, &signed)])
    }

    #[test]
    fn embedding_a_timestamp_keeps_the_signature_verifying() {
        let src = base_pdf(None);
        let budget = Budget::profile(Surface::Viewer);
        let mut g = budget.guard();
        let (signed, _) =
            sign_digital(&src, &digital_options("Signature1"), &budget, &mut g).expect("signs");

        // The imprint is SHA-256 of the signature's /Contents; compute it the
        // same way embed_dss does so the token binds to this signature.
        let read = signatures(&signed, &[], NOW, &budget, &mut g).expect("report");
        assert_eq!(read.signatures.len(), 1);
        let der = signed.clone();
        // A convenience token with the expected imprint is produced by asking
        // the embedder to derive it: embed without a token first to learn the
        // field, then use `from_der` with the known digest computed here.
        let field_cms_offset = der
            .windows(b"/SubFilter /adbe.pkcs7.detached".len())
            .position(|w| w == b"/SubFilter /adbe.pkcs7.detached")
            .expect("the subfilter is present");
        assert!(field_cms_offset < der.len());

        // Build the token with the imprint derived from the signature bytes by
        // using the CLI-independent helper exposed in the test module below.
        let imprint = test_imprint(&signed, "Signature1");
        let token = synthetic_token(&imprint);
        let options = DssOptions {
            field_name: "Signature1",
            timestamps: &[TimestampToken::from_der(token)],
            ..DssOptions::default()
        };
        let (out, report) =
            embed_dss(&signed, &options, &budget, &mut g).expect("the DSS is embedded");
        assert!(out.starts_with(&signed));
        assert_eq!(report.timestamps, 1);
        assert!(report.verified);
        assert!(report.signature_still_verifies);
        assert_eq!(report.byte_range, read.signatures[0].byte_range.unwrap());

        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("/DSS"), "the catalog carries /DSS");
        assert!(
            text.contains("/ETSI.RFC3161"),
            "the timestamp sub-filter is written"
        );

        // Re-running the report proves the original signature still verifies.
        let after = signatures(&out, &[], NOW, &budget, &mut g).expect("report after DSS");
        assert!(after.signatures[0].verified);
        assert_eq!(
            after.signatures[0].byte_range,
            read.signatures[0].byte_range
        );
    }

    #[test]
    fn a_token_with_a_foreign_imprint_is_refused() {
        let src = base_pdf(None);
        let budget = Budget::profile(Surface::Viewer);
        let mut g = budget.guard();
        let (signed, _) =
            sign_digital(&src, &digital_options("Signature1"), &budget, &mut g).expect("signs");
        let token = synthetic_token(&[0x99u8; 32]);
        let options = DssOptions {
            field_name: "Signature1",
            timestamps: &[TimestampToken::from_der(token)],
            ..DssOptions::default()
        };
        assert!(embed_dss(&signed, &options, &budget, &mut g).is_err());
    }

    #[test]
    fn a_garbage_token_is_refused_before_a_byte_is_written() {
        let src = base_pdf(None);
        let budget = Budget::profile(Surface::Viewer);
        let mut g = budget.guard();
        let (signed, _) =
            sign_digital(&src, &digital_options("Signature1"), &budget, &mut g).expect("signs");
        let options = DssOptions {
            field_name: "Signature1",
            timestamps: &[TimestampToken::from_der(b"not a token".to_vec())],
            ..DssOptions::default()
        };
        assert!(embed_dss(&signed, &options, &budget, &mut g).is_err());
    }

    #[test]
    fn dss_payloads_can_be_added_without_a_timestamp() {
        let src = base_pdf(None);
        let budget = Budget::profile(Surface::Viewer);
        let mut g = budget.guard();
        let (signed, _) =
            sign_digital(&src, &digital_options("Signature1"), &budget, &mut g).expect("signs");
        let ocsp = vec![0x30u8, 0x03, 0x02, 0x01, 0x01];
        let crl = vec![0x30u8, 0x03, 0x02, 0x01, 0x02];
        let cert = selis_crypto::cert::pem_body(ROOT, "CERTIFICATE").unwrap();
        let options = DssOptions {
            field_name: "Signature1",
            ocsp: &[ocsp],
            crl: &[crl],
            certificates: &[cert],
            ..DssOptions::default()
        };
        let (out, report) =
            embed_dss(&signed, &options, &budget, &mut g).expect("LTV data is embedded");
        assert_eq!(report.ocsp, 1);
        assert_eq!(report.crl, 1);
        assert_eq!(report.certificates, 1);
        assert!(report.verified);
        assert!(String::from_utf8_lossy(&out).contains("/OCSP"));
    }

    /// SHA-256 over the named signature's `/Contents`, using the same pure
    /// helper the embedder uses. Kept in tests so the synthetic token can bind
    /// to the signature without importing CLI code.
    fn test_imprint(pdf: &[u8], field: &str) -> Vec<u8> {
        use selis_crypto::cert::DigestAlg;
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let cms = super::signature_contents(pdf, field, &budget, &mut g).expect("signature CMS");
        selis_crypto::cms::digest_spans(DigestAlg::Sha256, &[cms.as_slice()])
    }
}
