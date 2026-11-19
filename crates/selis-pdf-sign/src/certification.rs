//! Certification signatures and the DocMDP transform chain (SL-5.SIGN.05).
//!
//! A certification signature is an ordinary detached-CMS signature plus a
//! `/Reference` entry that declares a document modification detection and
//! prevention (DocMDP) transform. ISO 32000-2 §12.8.2.2 defines three levels:
//!
//! * `/P 1` — no changes to the document shall be permitted;
//! * `/P 2` — form fill-in and signing only;
//! * `/P 3` — form fill-in, annotation creation/editing and signing.
//!
//! The structure written here is deliberately the exact shape
//! `selis-pdf-edit::signature_guard` parses:
//!
//! ```text
//! signature dict (Type /Sig, ByteRange, Contents, …)
//!   /Reference [ refDictRef ]
//! refDict: /Type /SigRef /TransformMethod /DocMDP
//!          /TransformParams paramsDictRef /Data signatureDictRef
//! paramsDict: /Type /TransformParams /P <level>
//! ```
//!
//! Creation and enforcement therefore agree on one object graph. The reader
//! re-opens the produced bytes, walks the graph, checks the `/P` integer and
//! re-runs the real signature report before anything is returned.

use selis_pdf_cos::Obj;
use selis_sandbox::{Budget, BudgetGuard};
use serde::Deserialize;

use crate::{dict_get, malformed, resolve, trailer, DigitalOptions, DigitalReport, SignError};

/// The `/P` transform level of a DocMDP certification signature.
///
/// The numbers are the PDF values, not an ordering of convenience: they are
/// written verbatim into `/TransformParams /P` and read back by the
/// `selis-pdf-edit` guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DocMdpLevel {
    /// `/P 1` — no changes permitted (further signing is still allowed).
    NoChanges,
    /// `/P 2` — form fill-in and signing only.
    FillForms,
    /// `/P 3` — form fill-in, annotations and signing.
    FillAndAnnotate,
}

impl DocMdpLevel {
    /// Interpret a `/P` integer, returning `None` for an unknown level.
    #[must_use]
    pub const fn from_p(p: i64) -> Option<Self> {
        match p {
            1 => Some(Self::NoChanges),
            2 => Some(Self::FillForms),
            3 => Some(Self::FillAndAnnotate),
            _ => None,
        }
    }

    /// The `/P` integer this level writes.
    #[must_use]
    pub const fn p(self) -> i64 {
        match self {
            Self::NoChanges => 1,
            Self::FillForms => 2,
            Self::FillAndAnnotate => 3,
        }
    }

    /// A short human label for error messages.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NoChanges => "no changes permitted",
            Self::FillForms => "fill forms and sign only",
            Self::FillAndAnnotate => "fill, annotate and sign",
        }
    }
}

/// What to certify, and with what key.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CertificationOptions {
    /// The DocMDP permission level the certification declares.
    pub doc_mdp: DocMdpLevel,
    /// The underlying detached signature options (page, rect, key, chain, …).
    #[serde(flatten)]
    pub signature: DigitalOptions,
}

/// What a certification write produced, verified by read-back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificationReport {
    /// The underlying detached-CMS signature report.
    pub signature: DigitalReport,
    /// The level that was written.
    pub level: DocMdpLevel,
    /// The `/P` integer read back from the produced file.
    pub p: i64,
    /// Object number of the `/TransformParams` dictionary that carries `/P`.
    pub params_object: u32,
    /// Always `true`: the bytes were re-opened and both the signature and the
    /// DocMDP chain verified.
    pub verified: bool,
}

/// Sign one page as a certification signature with a DocMDP `/P` level.
///
/// # Errors
///
/// * every error [`crate::sign_digital`] can return (same validation, key
///   checks, CMS build and signature read-back);
/// * [`SignError::ReadBack`] if the `/Reference → /TransformParams → /P` chain
///   cannot be followed in the produced bytes or its `/P` differs from the
///   requested level.
///
/// # Budget
///
/// The budget charges of [`crate::sign_digital`] plus one resolver charge per
/// object on the catalog → field → signature → reference → params walk.
///
/// # Malformed Input
///
/// A missing or wrong-shaped entry at any step of the read-back walk is a
/// typed `ReadBack` error; no byte is indexed without a checked bounds read.
#[allow(clippy::too_many_arguments)]
pub fn sign_certification(
    src: &[u8],
    options: &CertificationOptions,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(Vec<u8>, CertificationReport), SignError> {
    let level = options.doc_mdp;
    let (out, signature) =
        crate::digital::sign_impl(src, &options.signature, Some(level), budget, g)?;
    let read = read_back(&out, &options.signature.field_name, budget, g)?;
    if read.p != level.p() {
        return Err(SignError::ReadBack(format!(
            "the written DocMDP /P is {} but {} was requested",
            read.p,
            level.p()
        )));
    }
    let verified = signature.verified;
    Ok((
        out,
        CertificationReport {
            signature,
            level,
            p: read.p,
            params_object: read.params_object,
            verified,
        },
    ))
}

/// The `/P` value and params object number found in a produced file.
struct ReadBack {
    p: i64,
    params_object: u32,
}

/// Walk the produced file's object graph from the catalog to the signature
/// field named `field_name`, then through its DocMDP reference chain.
fn read_back(
    src: &[u8],
    field_name: &str,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<ReadBack, SignError> {
    let doc = crate::open_doc(src, budget, g)?;
    let root = trailer(&doc)?
        .root
        .ok_or_else(|| malformed("the trailer has no /Root"))?;
    let catalog = resolve(src, &doc, Obj::Ref(root), budget, g)?;
    let Obj::Dict(catalog_dict) = catalog else {
        return Err(malformed("the catalog is not a dictionary"));
    };
    let acro = dict_get(&catalog_dict, b"AcroForm")
        .ok_or_else(|| SignError::ReadBack("the catalog has no /AcroForm".to_string()))?;
    let acro = resolve(src, &doc, acro.clone(), budget, g)?;
    let Obj::Dict(acro_dict) = acro else {
        return Err(SignError::ReadBack(
            "the /AcroForm is not a dictionary".to_string(),
        ));
    };
    let fields = dict_get(&acro_dict, b"Fields")
        .ok_or_else(|| SignError::ReadBack("the /AcroForm has no /Fields".to_string()))?;
    let fields = resolve(src, &doc, fields.clone(), budget, g)?;
    let Obj::Array(fields) = fields else {
        return Err(SignError::ReadBack(
            "the /Fields is not an array".to_string(),
        ));
    };

    for field in &fields {
        let field_obj = resolve(src, &doc, field.clone(), budget, g)?;
        let Obj::Dict(field_dict) = &field_obj else {
            continue;
        };
        let name_matches = dict_get(field_dict, b"T").is_some_and(|name| {
            matches!(
                resolve(src, &doc, name.clone(), budget, g),
                Ok(Obj::String(value)) if value.as_slice() == field_name.as_bytes()
            )
        });
        if !name_matches {
            continue;
        }
        let v = dict_get(field_dict, b"V")
            .ok_or_else(|| SignError::ReadBack("the signature field has no /V".to_string()))?;
        let sig = resolve(src, &doc, v.clone(), budget, g)?;
        let Obj::Dict(sig_dict) = sig else {
            return Err(SignError::ReadBack(
                "the /V is not a signature dictionary".to_string(),
            ));
        };
        let reference = dict_get(&sig_dict, b"Reference").ok_or_else(|| {
            SignError::ReadBack("the signature has no DocMDP /Reference".to_string())
        })?;
        let reference = resolve(src, &doc, reference.clone(), budget, g)?;
        let Obj::Array(entries) = reference else {
            return Err(SignError::ReadBack(
                "the /Reference is not an array".to_string(),
            ));
        };
        for entry in &entries {
            let entry = resolve(src, &doc, entry.clone(), budget, g)?;
            let Obj::Dict(entry_dict) = entry else {
                continue;
            };
            let is_docmdp = dict_get(&entry_dict, b"TransformMethod")
                .and_then(|method| match method {
                    Obj::Name(name) => Some(name.as_slice() == b"DocMDP".as_slice()),
                    _ => None,
                })
                .unwrap_or(false);
            if !is_docmdp {
                continue;
            }
            let params_ref = dict_get(&entry_dict, b"TransformParams").ok_or_else(|| {
                SignError::ReadBack("the DocMDP reference has no /TransformParams".to_string())
            })?;
            let params_object = match params_ref {
                Obj::Ref(reference) => reference.num,
                _ => {
                    return Err(SignError::ReadBack(
                        "the /TransformParams is not an indirect object".to_string(),
                    ))
                }
            };
            let params = resolve(src, &doc, params_ref.clone(), budget, g)?;
            let Obj::Dict(params_dict) = params else {
                return Err(SignError::ReadBack(
                    "the /TransformParams is not a dictionary".to_string(),
                ));
            };
            if let Some(Obj::Int(p)) = dict_get(&params_dict, b"P") {
                return Ok(ReadBack {
                    p: *p,
                    params_object,
                });
            }
        }
    }
    Err(SignError::ReadBack(format!(
        "no DocMDP /P was found for the field {field_name:?}"
    )))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{CertificationOptions, DocMdpLevel};
    use crate::DigitalOptions;

    #[test]
    fn docmdp_levels_round_trip_through_their_p_values() {
        for level in [
            DocMdpLevel::NoChanges,
            DocMdpLevel::FillForms,
            DocMdpLevel::FillAndAnnotate,
        ] {
            assert_eq!(DocMdpLevel::from_p(level.p()), Some(level));
            assert!(!level.label().is_empty());
        }
        assert_eq!(DocMdpLevel::from_p(0), None);
        assert_eq!(DocMdpLevel::from_p(4), None);
    }

    #[test]
    fn certification_options_deserialise_with_a_flat_signature_block() {
        let json = r#"{
            "docMdp": "noChanges",
            "page": 1,
            "rect": [0,0,100,100],
            "fieldName": "Cert",
            "signerName": "Ada",
            "reason": null,
            "location": null,
            "signingTime": "20261104120000Z",
            "keyPem": "KEY",
            "certificatesPem": ["CERT"]
        }"#;
        let options: CertificationOptions = serde_json::from_str(json).expect("deserialises");
        assert_eq!(options.doc_mdp, DocMdpLevel::NoChanges);
        assert_eq!(options.signature.field_name, "Cert");
        let _: &DigitalOptions = &options.signature;
    }
}
