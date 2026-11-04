//! A detached CMS signature that a viewer can check, written as an incremental
//! revision.
//!
//! The signature covers the document by `/ByteRange`, not by re-writing it:
//! the file's original bytes are the prefix of the output unchanged, so a
//! signature added now sits on top of every revision before it and does not
//! disturb an earlier one. That is the property that lets "sign an
//! already-signed document" be a supported flow rather than a corruption.
//!
//! The mechanics are the conventional ones (ISO 32000-2 §12.8.3.4):
//!
//! 1. write a signature dictionary whose `/ByteRange` and `/Contents` are
//!    fixed-width placeholders, one digit per byte the final values will need;
//! 2. patch `/ByteRange` in place, so the offsets describe the file that now
//!    exists;
//! 3. hash the two ranges and build the detached `SignedData` over that hash;
//! 4. replace the placeholder `/Contents` with the CMS bytes, same width, so
//!    every offset written in step 1 stays correct;
//! 5. re-open the produced file, find the field, and verify the signature
//!    before returning anything.
//!
//! A signature that does not read back as verified is an error, never a file.

use rsa::pkcs8::DecodePublicKey as _;
use rsa::traits::PublicKeyParts as _;
use selis_bytes::Bytes;
use selis_pdf_cos::doc_writer::write_incremental_update;
use selis_pdf_cos::{Doc, Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard};

use crate::visual::{content_stream, VisualLine, VisualOptions};
use crate::{
    bytes, carried_trailer, dict_get, dict_set, invalid, malformed, next_object_number, resolve,
    trailer, SignError, MAX_KEY_BYTES, MAX_TEXT_BYTES,
};

/// The signature dictionary's `/Filter` entry.
const FILTER: &str = "Adobe.PPKLite";
/// The detached CMS sub-filter this crate writes.
const SUB_FILTER: &str = "adbe.pkcs7.detached";
/// The substitution font every appearance uses.
const FONT_NAME: &str = "SelisSigF";
/// The shortest RSA key this build will sign with.
const MIN_RSA_BITS: usize = 2048;
/// The largest CMS blob a signature may produce, as a refusal bound.
const MAX_CMS_BYTES: usize = 1 << 20;
/// The fixed width of each `/ByteRange` number. Ten digits covers any file
/// under 10 GB, and the placeholder is patched to the same width so no byte
/// after it moves.
const RANGE_DIGITS: usize = 10;

/// What to sign, and with what.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DigitalOptions {
    /// 1-based page the signature field paints on.
    pub page: usize,
    /// The field's rectangle in display-space points, `[x0, y0, x1, y1]`.
    pub rect: [f64; 4],
    /// The field's fully qualified name.
    pub field_name: String,
    /// The name the signature dictionary asserts.
    pub signer_name: String,
    /// `/Reason`, when the signer gave one.
    pub reason: Option<String>,
    /// `/Location`, when the signer gave one.
    pub location: Option<String>,
    /// `YYYYMMDDHHMMSSZ`. The core has no clock; the caller asserts the
    /// moment, and a caller that cannot name one must refuse.
    pub signing_time: String,
    /// The signing key, as PEM (`PKCS#8` or `PKCS#1`). It is parsed, used, and
    /// never written anywhere.
    pub key_pem: String,
    /// The certificate chain, leaf first, as PEM blocks.
    pub certificates_pem: Vec<String>,
}

/// What was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigitalReport {
    /// The field's name.
    pub field: String,
    /// 1-based page the widget paints on.
    pub page: usize,
    /// The sub-filter written.
    pub sub_filter: String,
    /// The digest algorithm.
    pub digest: String,
    /// The certificate's subject common name.
    pub signer_certificate: String,
    /// The four `/ByteRange` values.
    pub byte_range: [u64; 4],
    /// Size of the CMS blob.
    pub cms_bytes: usize,
    /// Always `true`: the produced bytes were re-opened and verified.
    pub verified: bool,
}

/// Sign one page with a detached CMS signature.
///
/// # Errors
///
/// * [`SignError::Unsupported`] for an EC key, an encrypted PEM, an RSA key
///   under 2048 bits, or a certificate whose key does not match the private
///   key;
/// * [`SignError::Invalid`] for a page outside the document, a degenerate
///   rectangle, an unparseable signing time, an empty field name, or a
///   certificate that does not allow digital signatures;
/// * [`SignError::Encrypted`] for a document whose dictionaries are ciphertext;
/// * [`SignError::ReadBack`] when the produced bytes do not verify.
#[allow(clippy::too_many_arguments)]
pub fn sign_digital(
    src: &[u8],
    options: &DigitalOptions,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, DigitalReport), SignError> {
    validate(options)?;

    let doc = crate::open_doc(src, budget, g)?;
    if crate::is_encrypted(&doc) {
        return Err(SignError::Encrypted);
    }
    let all_pages = crate::pages(src, &doc, budget, g)?;
    let target = options
        .page
        .checked_sub(1)
        .and_then(|index| all_pages.get(index))
        .copied()
        .ok_or_else(|| {
            invalid(format!(
                "page {} does not exist; the document has {}",
                options.page,
                all_pages.len()
            ))
        })?;
    let (display_width, display_height) = target.display_size();
    let rect = clamp_rect(options.rect, display_width, display_height)?;

    let key = parse_private_key(&options.key_pem)?;
    if key.size().saturating_mul(8) < MIN_RSA_BITS {
        return Err(SignError::Unsupported(format!(
            "this build signs with RSA keys of at least {MIN_RSA_BITS} bits (this key is {})",
            key.size().saturating_mul(8)
        )));
    }
    let chain = parse_certificates(&options.certificates_pem)?;
    let leaf = chain
        .first()
        .ok_or_else(|| invalid("a digital signature needs at least one certificate"))?;
    let leaf_cert = {
        let chain_budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Fuzz);
        let mut chain_guard = chain_budget.guard();
        selis_crypto::cert::parse(leaf, &mut chain_guard).map_err(|e| {
            SignError::Invalid(format!("the signing certificate does not parse: {e}"))
        })?
    };
    if !leaf_cert.allows_digital_signature() {
        return Err(SignError::Invalid(
            "the signing certificate's key usage does not allow digital signatures".to_string(),
        ));
    }
    let cert_key = match leaf_cert.key {
        selis_crypto::cert::PublicKey::Rsa(spki) => rsa::RsaPublicKey::from_public_key_der(spki)
            .map_err(|_| {
                SignError::Invalid("the certificate's RSA key does not parse".to_string())
            })?,
        selis_crypto::cert::PublicKey::P256(_) => {
            return Err(SignError::Unsupported(
                "this build signs with RSA keys; that certificate carries an EC key".to_string(),
            ))
        }
    };
    if cert_key != rsa::RsaPublicKey::from(&key) {
        return Err(SignError::Invalid(
            "the private key does not match the signing certificate".to_string(),
        ));
    }

    // A first CMS build with a zero digest measures the exact length: every
    // field around the digest has a fixed width, so the real signature is the
    // same size and the `/Contents` placeholder can be exact.
    let digest_alg = selis_crypto::cert::DigestAlg::Sha256;
    let probe = selis_crypto::cms::build_detached_signed_data(
        &key,
        &chain,
        &vec![0u8; digest_alg.output_len()],
        &options.signing_time,
        digest_alg,
    )
    .map_err(|e| SignError::Unsupported(format!("the signature could not be built: {e}")))?;
    let cms_len = probe.len();
    if cms_len == 0 || cms_len > MAX_CMS_BYTES {
        return Err(SignError::Limit(format!(
            "a signature of {cms_len} bytes is not signable by this build"
        )));
    }

    // Display-space rectangle mapped into the page's own user space; the
    // widget's /Rect and the appearance's /BBox then agree for any rotation.
    let (ax, ay) = target.display_to_user(rect[0], rect[1]);
    let (bx, by) = target.display_to_user(rect[2], rect[3]);
    let ux0 = ax.min(bx);
    let uy0 = ay.min(by);
    let user_width = (bx - ax).abs();
    let user_height = (by - ay).abs();

    let lines = appearance_lines(options);
    let appearance_options = VisualOptions {
        page: options.page,
        rect: [0.0, 0.0, rect[2] - rect[0], rect[3] - rect[1]],
        lines,
        strokes: Vec::new(),
        color: [0.05, 0.05, 0.12],
        stroke_width: 1.0,
        font_size: 9.0,
        border: true,
    };
    // The synthetic page reproduces the real page's rotation but with the
    // annotation's own user-space extent, so `content_stream` emits
    // annotation-local coordinates.
    let local_page = crate::Page {
        reference: target.reference,
        width: user_width,
        height: user_height,
        rotation: target.rotation,
    };
    let appearance = content_stream(
        &appearance_options,
        local_page,
        [0.0, 0.0, rect[2] - rect[0], rect[3] - rect[1]],
    )?;

    let base = next_object_number(&doc);
    let signature_num = base;
    let appearance_num = base.saturating_add(1);
    let field_num = base.saturating_add(2);
    let new_acroform_num = base.saturating_add(3);

    let signature_ref = Obj::Ref(Ref::new(signature_num, 0));
    let appearance_ref = Obj::Ref(Ref::new(appearance_num, 0));
    let field_ref = Obj::Ref(Ref::new(field_num, 0));

    // `/ByteRange` placeholders: four ten-digit integers. The values are
    // patched after the revision is written, to the same width.
    let placeholder = 9_999_999_999i64;
    let mut signature_dict = vec![
        (bytes(b"Type"), Obj::Name(bytes(b"Sig"))),
        (bytes(b"Filter"), Obj::Name(bytes(FILTER.as_bytes()))),
        (bytes(b"SubFilter"), Obj::Name(bytes(SUB_FILTER.as_bytes()))),
        (
            bytes(b"ByteRange"),
            Obj::Array(vec![
                Obj::Int(placeholder),
                Obj::Int(placeholder),
                Obj::Int(placeholder),
                Obj::Int(placeholder),
            ]),
        ),
        (
            bytes(b"Contents"),
            Obj::HexString(Bytes::copy_from_slice(&vec![0u8; cms_len])),
        ),
        (
            bytes(b"M"),
            Obj::String(bytes(pdf_date(&options.signing_time).as_bytes())),
        ),
        (
            bytes(b"Name"),
            Obj::String(bytes(options.signer_name.as_bytes())),
        ),
    ];
    if let Some(reason) = options.reason.as_deref().filter(|text| !text.is_empty()) {
        signature_dict.push((bytes(b"Reason"), Obj::String(bytes(reason.as_bytes()))));
    }
    if let Some(location) = options.location.as_deref().filter(|text| !text.is_empty()) {
        signature_dict.push((bytes(b"Location"), Obj::String(bytes(location.as_bytes()))));
    }

    let appearance_dict = vec![
        (bytes(b"Type"), Obj::Name(bytes(b"XObject"))),
        (bytes(b"Subtype"), Obj::Name(bytes(b"Form"))),
        (
            bytes(b"BBox"),
            Obj::Array(vec![
                Obj::Int(0),
                Obj::Int(0),
                crate::visual::coordinate(user_width),
                crate::visual::coordinate(user_height),
            ]),
        ),
        (
            bytes(b"Resources"),
            Obj::Dict(vec![(
                bytes(b"Font"),
                Obj::Dict(vec![(
                    bytes(FONT_NAME.as_bytes()),
                    Obj::Dict(vec![
                        (bytes(b"Type"), Obj::Name(bytes(b"Font"))),
                        (bytes(b"Subtype"), Obj::Name(bytes(b"Type1"))),
                        (bytes(b"BaseFont"), Obj::Name(bytes(b"Helvetica"))),
                        (bytes(b"Encoding"), Obj::Name(bytes(b"WinAnsiEncoding"))),
                    ]),
                )]),
            )]),
        ),
    ];

    let field_dict = vec![
        (bytes(b"Type"), Obj::Name(bytes(b"Annot"))),
        (bytes(b"Subtype"), Obj::Name(bytes(b"Widget"))),
        (bytes(b"FT"), Obj::Name(bytes(b"Sig"))),
        (
            bytes(b"T"),
            Obj::String(bytes(options.field_name.as_bytes())),
        ),
        (
            bytes(b"Rect"),
            Obj::Array(vec![
                real_obj(ux0),
                real_obj(uy0),
                real_obj(ux0 + user_width),
                real_obj(uy0 + user_height),
            ]),
        ),
        (bytes(b"F"), Obj::Int(4)),
        (bytes(b"P"), Obj::Ref(target.reference)),
        (bytes(b"V"), signature_ref),
        (bytes(b"AP"), Obj::Dict(vec![(bytes(b"N"), appearance_ref)])),
    ];

    // The page gains one annotation; its other entries are copied verbatim.
    let page_obj = resolve(src, &doc, Obj::Ref(target.reference), budget, g)?;
    let Obj::Dict(mut page_dict) = page_obj else {
        return Err(malformed("the target page is not a dictionary"));
    };
    let mut annots = match dict_get(&page_dict, b"Annots").cloned() {
        Some(Obj::Ref(reference)) => match resolve(src, &doc, Obj::Ref(reference), budget, g)? {
            Obj::Array(items) => items,
            _ => Vec::new(),
        },
        Some(Obj::Array(items)) => items,
        _ => Vec::new(),
    };
    annots.push(field_ref.clone());
    dict_set(&mut page_dict, b"Annots", Obj::Array(annots));

    // The form gains one field and the document-level /SigFlags 3 (both
    // SignaturesExist and AppendOnly).
    let revision = trailer(&doc)?;
    let catalog_ref = revision
        .root
        .ok_or_else(|| malformed("the trailer has no /Root"))?;
    let catalog = resolve(src, &doc, Obj::Ref(catalog_ref), budget, g)?;
    let Obj::Dict(mut catalog_dict) = catalog else {
        return Err(malformed("the catalog is not a dictionary"));
    };
    let acroform = selis_pdf_cos::reconcile::catalog_entry(src, &doc, b"AcroForm", budget, g)
        .ok()
        .flatten();
    let mut objects: Vec<(u32, Obj)> = vec![
        (signature_num, Obj::Dict(signature_dict)),
        (
            appearance_num,
            Obj::Stream {
                dict: appearance_dict,
                data: Bytes::copy_from_slice(&appearance),
            },
        ),
        (field_num, Obj::Dict(field_dict)),
        (target.reference.num, Obj::Dict(page_dict)),
    ];
    match acroform {
        Some(Obj::Ref(reference)) => {
            let form = resolve(src, &doc, Obj::Ref(reference), budget, g)?;
            let Obj::Dict(mut form_dict) = form else {
                return Err(malformed("the /AcroForm is not a dictionary"));
            };
            append_field(src, &doc, &mut form_dict, &field_ref, budget, g)?;
            dict_set(&mut form_dict, b"SigFlags", Obj::Int(3));
            objects.push((reference.num, Obj::Dict(form_dict)));
        }
        Some(Obj::Dict(mut form_dict)) => {
            append_field(src, &doc, &mut form_dict, &field_ref, budget, g)?;
            dict_set(&mut form_dict, b"SigFlags", Obj::Int(3));
            dict_set(&mut catalog_dict, b"AcroForm", Obj::Dict(form_dict));
            objects.push((catalog_ref.num, Obj::Dict(catalog_dict)));
        }
        _ => {
            let form = Obj::Dict(vec![
                (bytes(b"Fields"), Obj::Array(vec![field_ref.clone()])),
                (bytes(b"SigFlags"), Obj::Int(3)),
            ]);
            dict_set(
                &mut catalog_dict,
                b"AcroForm",
                Obj::Ref(Ref::new(new_acroform_num, 0)),
            );
            objects.push((new_acroform_num, form));
            objects.push((catalog_ref.num, Obj::Dict(catalog_dict)));
        }
    }

    let carried = carried_trailer(revision);
    let out = write_incremental_update(src, &objects, &carried, budget, g)
        .map_err(|e| malformed(format!("the incremental revision failed: {e}")))?;

    let (patched, byte_range) = patch_byte_range(&out)?;
    let mut out = patched;
    let digest = selis_crypto::cms::digest_spans(
        digest_alg,
        &[
            out.get(..usize::try_from(byte_range[1]).unwrap_or(0))
                .unwrap_or(&[]),
            out.get(
                usize::try_from(byte_range[2]).unwrap_or(0)
                    ..usize::try_from(byte_range[2].saturating_add(byte_range[3]))
                        .unwrap_or(usize::MAX),
            )
            .unwrap_or(&[]),
        ],
    );
    let cms = selis_crypto::cms::build_detached_signed_data(
        &key,
        &chain,
        &digest,
        &options.signing_time,
        digest_alg,
    )
    .map_err(|e| SignError::Unsupported(format!("the signature could not be built: {e}")))?;
    if cms.len() != cms_len {
        return Err(SignError::Limit(format!(
            "the CMS length changed between measurement ({cms_len}) and signing ({})",
            cms.len()
        )));
    }
    patch_contents(&mut out, byte_range, &cms)?;

    // Read-back: the produced file must carry a signature that verifies.
    let now = unix_from_stamp(&options.signing_time).unwrap_or(0);
    let report = crate::report::signatures(&out, &[], now, budget, g)?;
    let written = report
        .signatures
        .iter()
        .find(|signature| signature.field == options.field_name)
        .ok_or_else(|| {
            SignError::ReadBack(format!(
                "the field {:?} was not found in the output",
                options.field_name
            ))
        })?;
    if !written.verified {
        return Err(SignError::ReadBack(written.error.clone().unwrap_or_else(
            || "the written signature did not verify".to_string(),
        )));
    }
    if !written.covers_whole_document {
        return Err(SignError::ReadBack(
            "the written /ByteRange does not cover the whole document".to_string(),
        ));
    }

    Ok((
        out,
        DigitalReport {
            field: options.field_name.clone(),
            page: options.page,
            sub_filter: SUB_FILTER.to_string(),
            digest: digest_alg.name().to_string(),
            signer_certificate: leaf_cert.subject_common_name(),
            byte_range,
            cms_bytes: cms_len,
            verified: true,
        },
    ))
}

fn validate(options: &DigitalOptions) -> Result<(), SignError> {
    if options.field_name.trim().is_empty() {
        return Err(invalid("the signature field needs a name"));
    }
    if options.field_name.len() > 256 {
        return Err(SignError::Limit(
            "a field name may be at most 256 bytes".to_string(),
        ));
    }
    if options.signer_name.len() > MAX_TEXT_BYTES {
        return Err(SignError::Limit(format!(
            "the signer name may be at most {MAX_TEXT_BYTES} bytes"
        )));
    }
    if options.reason.as_deref().map_or(0, str::len) > MAX_TEXT_BYTES
        || options.location.as_deref().map_or(0, str::len) > MAX_TEXT_BYTES
    {
        return Err(SignError::Limit(format!(
            "the reason and location may be at most {MAX_TEXT_BYTES} bytes each"
        )));
    }
    if options.key_pem.len() > MAX_KEY_BYTES {
        return Err(SignError::Limit(format!(
            "the key may be at most {MAX_KEY_BYTES} bytes of PEM"
        )));
    }
    if options.certificates_pem.is_empty() {
        return Err(invalid("a digital signature needs a certificate"));
    }
    if options.certificates_pem.len() > 16 {
        return Err(SignError::Limit(
            "a certificate chain may carry at most 16 certificates".to_string(),
        ));
    }
    if unix_from_stamp(&options.signing_time).is_none() {
        return Err(invalid(
            "the signing time must be fourteen digits followed by Z (YYYYMMDDHHMMSSZ)",
        ));
    }
    if !(options.rect[0].is_finite()
        && options.rect[1].is_finite()
        && options.rect[2].is_finite()
        && options.rect[3].is_finite())
    {
        return Err(invalid("the field rectangle has a non-finite coordinate"));
    }
    Ok(())
}

fn clamp_rect(rect: [f64; 4], width: f64, height: f64) -> Result<[f64; 4], SignError> {
    let [mut x0, mut y0, mut x1, mut y1] = rect;
    if x1 < x0 {
        std::mem::swap(&mut x0, &mut x1);
    }
    if y1 < y0 {
        std::mem::swap(&mut y0, &mut y1);
    }
    if x1 - x0 < 8.0 || y1 - y0 < 8.0 {
        return Err(invalid("the signature box is smaller than 8 by 8 points"));
    }
    if x1 <= 0.0 || y1 <= 0.0 || x0 >= width || y0 >= height {
        return Err(invalid(
            "the signature box is entirely outside the displayed page",
        ));
    }
    Ok([x0.max(0.0), y0.max(0.0), x1.min(width), y1.min(height)])
}

/// Append the field reference to `/Fields`, resolving an indirect array first.
fn append_field(
    src: &[u8],
    doc: &Doc,
    form: &mut Vec<(Bytes, Obj)>,
    field: &Obj,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(), SignError> {
    let mut fields = match dict_get(form, b"Fields").cloned() {
        Some(Obj::Ref(reference)) => match resolve(src, doc, Obj::Ref(reference), budget, g)? {
            Obj::Array(items) => items,
            _ => Vec::new(),
        },
        Some(Obj::Array(items)) => items,
        _ => Vec::new(),
    };
    fields.push(field.clone());
    dict_set(form, b"Fields", Obj::Array(fields));
    Ok(())
}

fn appearance_lines(options: &DigitalOptions) -> Vec<VisualLine> {
    let mut lines = vec![
        VisualLine::new("Digitally signed"),
        VisualLine::new(options.signer_name.clone()),
        VisualLine::new(format!("Date: {}", human_time(&options.signing_time))),
    ];
    if let Some(reason) = options.reason.as_deref().filter(|text| !text.is_empty()) {
        lines.push(VisualLine::new(format!("Reason: {reason}")));
    }
    if let Some(location) = options.location.as_deref().filter(|text| !text.is_empty()) {
        lines.push(VisualLine::new(format!("Location: {location}")));
    }
    lines
}

/// `YYYYMMDDHHMMSSZ` → `YYYY-MM-DD HH:MM:SS UTC`, for the visible block only.
fn human_time(stamp: &str) -> String {
    let digits = stamp.strip_suffix('Z').unwrap_or(stamp);
    match digits {
        d if d.len() == 14 => format!(
            "{}-{}-{} {}:{}:{} UTC",
            d.get(0..4).unwrap_or(""),
            d.get(4..6).unwrap_or(""),
            d.get(6..8).unwrap_or(""),
            d.get(8..10).unwrap_or(""),
            d.get(10..12).unwrap_or(""),
            d.get(12..14).unwrap_or("")
        ),
        _ => stamp.to_string(),
    }
}

/// `YYYYMMDDHHMMSSZ` → the PDF date string `/M` carries.
fn pdf_date(stamp: &str) -> String {
    let digits = stamp.strip_suffix('Z').unwrap_or(stamp);
    format!("D:{digits}+00'00'")
}

/// Convert a `YYYYMMDDHHMMSSZ` stamp to Unix seconds, for the certificate
/// window in the read-back report.
fn unix_from_stamp(stamp: &str) -> Option<i64> {
    let digits = stamp.strip_suffix('Z')?;
    if digits.len() != 14 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let year: i64 = digits.get(0..4)?.parse().ok()?;
    let month: i64 = digits.get(4..6)?.parse().ok()?;
    let day: i64 = digits.get(6..8)?.parse().ok()?;
    let hour: i64 = digits.get(8..10)?.parse().ok()?;
    let minute: i64 = digits.get(10..12)?.parse().ok()?;
    let second: i64 = digits.get(12..14)?.parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn parse_private_key(pem: &str) -> Result<rsa::RsaPrivateKey, SignError> {
    use rsa::pkcs1::DecodeRsaPrivateKey as _;
    use rsa::pkcs8::DecodePrivateKey as _;
    if pem.contains("BEGIN ENCRYPTED PRIVATE KEY") {
        return Err(SignError::Unsupported(
            "this build cannot open an encrypted private key; export an unencrypted PKCS#8 key \
             for this device"
                .to_string(),
        ));
    }
    if pem.contains("BEGIN EC PRIVATE KEY") || pem.contains("BEGIN EC PARAMETERS") {
        return Err(SignError::Unsupported(
            "this build signs with RSA keys only; an EC key would need a nonce source the \
             browser chunk does not have"
                .to_string(),
        ));
    }
    if selis_crypto::cert::private_key_kind(pem) == selis_crypto::cert::PrivateKeyKind::Ec {
        return Err(SignError::Unsupported(
            "this build signs with RSA keys only; an EC key would need a nonce source the \
             browser chunk does not have"
                .to_string(),
        ));
    }
    if let Ok(key) = rsa::RsaPrivateKey::from_pkcs8_pem(pem) {
        return Ok(key);
    }
    if let Ok(key) = rsa::RsaPrivateKey::from_pkcs1_pem(pem) {
        return Ok(key);
    }
    Err(SignError::Invalid(
        "the private key is not an RSA PKCS#8 or PKCS#1 PEM block".to_string(),
    ))
}

fn parse_certificates(pems: &[String]) -> Result<Vec<Vec<u8>>, SignError> {
    let mut out = Vec::with_capacity(pems.len());
    for (index, pem) in pems.iter().enumerate() {
        let der = selis_crypto::cert::pem_body(pem, "CERTIFICATE").ok_or_else(|| {
            SignError::Invalid(format!(
                "certificate {} is not a PEM CERTIFICATE block",
                index.saturating_add(1)
            ))
        })?;
        if der.len() > MAX_KEY_BYTES {
            return Err(SignError::Limit(format!(
                "certificate {} is larger than {MAX_KEY_BYTES} bytes",
                index.saturating_add(1)
            )));
        }
        out.push(der);
    }
    Ok(out)
}

fn real_obj(value: f64) -> Obj {
    crate::visual::coordinate(value)
}

/// Locate the `/ByteRange` placeholder, patch the real values into the same
/// width, and return them.
fn patch_byte_range(out: &[u8]) -> Result<(Vec<u8>, [u64; 4]), SignError> {
    let placeholder = format!(
        "/ByteRange [{}]",
        vec![format!("{:010}", 9_999_999_999u64); 4].join(" ")
    );
    let at = find(out, placeholder.as_bytes())
        .ok_or_else(|| malformed("the /ByteRange placeholder is missing from the output"))?;
    let bracket = at
        .checked_add("/ByteRange ".len())
        .ok_or_else(|| malformed("the /ByteRange offset overflowed"))?;
    // The `/Contents` string follows the array.
    let contents_start = find_from(out, b'<', bracket)
        .ok_or_else(|| malformed("the /Contents placeholder is missing from the output"))?;
    let contents_end = find_from(out, b'>', contents_start)
        .and_then(|end| end.checked_add(1))
        .ok_or_else(|| malformed("the /Contents placeholder is unterminated"))?;
    let total = u64::try_from(out.len()).unwrap_or(u64::MAX);
    let first_end = u64::try_from(contents_start).unwrap_or(u64::MAX);
    let second_start = u64::try_from(contents_end).unwrap_or(u64::MAX);
    let second_length = total
        .checked_sub(second_start)
        .ok_or_else(|| malformed("the /Contents placeholder runs past the file"))?;
    if first_end >= 10u64.pow(RANGE_DIGITS as u32)
        || second_start >= 10u64.pow(RANGE_DIGITS as u32)
        || second_length >= 10u64.pow(RANGE_DIGITS as u32)
    {
        return Err(SignError::Limit(
            "the document is too large for this build's fixed-width /ByteRange".to_string(),
        ));
    }
    let ranges = [0u64, first_end, second_start, second_length];
    let text = format!(
        "[{:010} {:010} {:010} {:010}]",
        ranges[0], ranges[1], ranges[2], ranges[3]
    );
    if text.len() != RANGE_DIGITS.saturating_mul(4).saturating_add(5) {
        return Err(malformed("the /ByteRange placeholder width is wrong"));
    }
    let mut out = out.to_vec();
    let start = bracket;
    let end = bracket
        .checked_add(text.len())
        .ok_or_else(|| malformed("the /ByteRange span overflowed"))?;
    let span = out
        .get_mut(start..end)
        .ok_or_else(|| malformed("the /ByteRange span is outside the file"))?;
    span.copy_from_slice(text.as_bytes());
    Ok((out, ranges))
}

/// Replace the `/Contents` placeholder's hex digits with the CMS blob.
fn patch_contents(out: &mut [u8], ranges: [u64; 4], cms: &[u8]) -> Result<(), SignError> {
    let start = usize::try_from(ranges[1]).unwrap_or(usize::MAX);
    let end = usize::try_from(ranges[2]).unwrap_or(usize::MAX);
    let opening = out
        .get(start)
        .copied()
        .ok_or_else(|| malformed("the /Contents span starts outside the file"))?;
    if opening != b'<' {
        return Err(malformed("the /Contents span does not start at '<'"));
    }
    let closing = end
        .checked_sub(1)
        .and_then(|index| out.get(index).copied())
        .ok_or_else(|| malformed("the /Contents span ends outside the file"))?;
    if closing != b'>' {
        return Err(malformed("the /Contents span does not end at '>'"));
    }
    let hex_len = end
        .checked_sub(start)
        .and_then(|length| length.checked_sub(2))
        .ok_or_else(|| malformed("the /Contents span is too short"))?;
    if hex_len != cms.len().saturating_mul(2) {
        return Err(SignError::Limit(format!(
            "the /Contents placeholder holds {hex_len} hex digits but the CMS blob needs {}",
            cms.len().saturating_mul(2)
        )));
    }
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for (index, byte) in cms.iter().enumerate() {
        let at = start
            .checked_add(1)
            .and_then(|base| base.checked_add(index.saturating_mul(2)))
            .ok_or_else(|| malformed("the /Contents offset overflowed"))?;
        let hi = usize::from(byte >> 4);
        let lo = usize::from(byte & 0x0f);
        if let (Some(&h), Some(&l), Some(slot)) = (
            HEX.get(hi),
            HEX.get(lo),
            out.get_mut(at..at.saturating_add(2)),
        ) {
            slot.copy_from_slice(&[h, l]);
        }
    }
    Ok(())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn find_from(haystack: &[u8], byte: u8, from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .iter()
        .position(|candidate| *candidate == byte)
        .and_then(|offset| offset.checked_add(from))
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

    #[test]
    fn the_signing_stamp_converts_to_a_pdf_date() {
        assert_eq!(pdf_date("20261104120000Z"), "D:20261104120000+00'00'");
        assert_eq!(human_time("20261104120000Z"), "2026-11-04 12:00:00 UTC");
    }

    #[test]
    fn a_stamp_converts_to_unix_seconds() {
        // 2026-11-04T12:00:00Z
        assert_eq!(unix_from_stamp("20261104120000Z"), Some(1_793_793_600));
        assert!(unix_from_stamp("2026-11-04T12:00:00Z").is_none());
    }

    #[test]
    fn the_byte_range_placeholder_is_patched_to_its_own_width() {
        let source =
            b"x /ByteRange [9999999999 9999999999 9999999999 9999999999] <0000> tail".to_vec();
        let contents_at = source.windows(1).position(|window| window == b"<").unwrap();
        let (patched, ranges) = patch_byte_range(&source).unwrap();
        assert_eq!(patched.len(), source.len());
        assert_eq!(ranges[0], 0);
        assert_eq!(ranges[1], u64::try_from(contents_at).unwrap());
        assert!(patched.windows(11).any(|window| window == b"[0000000000"));
    }

    #[test]
    fn a_cms_blob_is_written_into_the_contents_span() {
        let mut out = b"<0000>".to_vec();
        patch_contents(&mut out, [0, 0, 6, 0], &[0xDE, 0xAD]).unwrap();
        assert_eq!(out, b"<DEAD>");
    }

    #[test]
    fn a_key_too_small_is_measured_in_bits() {
        assert_eq!(256usize.saturating_mul(8), 2048);
    }
}
