//! Local self-signed signing identities (the "Foxit-model" local option).
//!
//! A user who does not have a publicly trusted document-signing certificate
//! can still want a tamper-evidence seal: "did this file change since I signed
//! it?" This module generates a signing key and a self-signed X.509
//! certificate entirely on the device, with no CA, no network and no external
//! authority. It is deliberately labelled *local only* wherever it is shown:
//! a self-signed seal is trusted by Selis (and by a viewer that imports the
//! certificate as an anchor), NOT by Acrobat's default AATL/EUTL list.
//!
//! The writer reuses the hand-rolled DER/X.509 reader's mirror in
//! [`crate::der_write`] and the RSA signing primitive already relied on by CMS,
//! so there is no new crypto dependency and no `unsafe`. An RSA key is
//! generated with the caller's RNG (the native CLI passes the OS CSPRNG), the
//! `TBSCertificate` is built with `basicConstraints.cA`, `keyUsage` and a
//! subject key identifier, and the certificate is signed `sha256WithRSA`.
//! The output is checked by parsing it back with [`crate::cert::parse`] and
//! verifying its self-signature with [`crate::cert::verify_with_key`] before it
//! is returned — a writer bug cannot hide behind an opaque blob.
//!
//! # Trust scope
//!
//! [`SelfSignedIdentity::trust_note`] is the exact string shells must show
//! next to a verdict that used this certificate. It never claims public trust.

use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::{DecodePublicKey as _, EncodePrivateKey as _, EncodePublicKey as _};
use rsa::signature::{SignatureEncoding as _, Signer as _};
use selis_error::{err, Code, Result};
use selis_sandbox::{Budget, BudgetGuard};
use sha2::{Digest, Sha256};

use crate::cert::{DigestAlg, Scheme, SignatureAlgorithm};
use crate::der_write as w;

/// The smallest RSA key this generator will produce. Below this a local seal
/// is not even tamper-evidence worth the name.
pub const MIN_IDENTITY_BITS: usize = 2048;
/// The largest RSA key this generator will produce (a refusal bound).
pub const MAX_IDENTITY_BITS: usize = 4096;
/// How long a generated identity is valid, in days, when the caller does not
/// override it.
pub const DEFAULT_VALIDITY_DAYS: u32 = 3650;

/// The honesty string shown with a locally generated identity.
pub const LOCAL_TRUST_NOTE: &str =
    "Trusted by Selis (local self-signed identity) — not a publicly trusted / AATL identity";

/// Parameters for [`generate_self_signed`].
#[derive(Debug, Clone)]
pub struct IdentityParams<'a> {
    /// The subject common name, e.g. "Ada Lovelace (Selis local)".
    pub common_name: &'a str,
    /// An optional organization name attribute.
    pub organization: Option<&'a str>,
    /// RSA modulus size in bits: 2048..=4096.
    pub key_bits: usize,
    /// Validity in days from `not_before_unix`.
    pub validity_days: u32,
    /// `notBefore`, Unix seconds, asserted by the caller (this crate has no
    /// clock). The CLI passes its own wall clock.
    pub not_before_unix: i64,
    /// A 16-byte serial. The caller draws it from its RNG; passing the same
    /// serial twice with a different key is still a distinct certificate.
    pub serial: [u8; 16],
    /// Whether the certificate asserts `basicConstraints.cA = true` and
    /// `keyUsage.keyCertSign`. A per-user signing identity passes `false`.
    pub is_ca: bool,
}

/// A generated local identity: an RSA private key and its self-signed cert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfSignedIdentity {
    /// Unencrypted PKCS#8 private key, DER.
    pub key_der: Vec<u8>,
    /// Self-signed X.509 certificate, DER.
    pub cert_der: Vec<u8>,
    /// PKCS#8 private key as PEM.
    pub key_pem: String,
    /// Certificate as PEM.
    pub cert_pem: String,
    /// The honesty string for shells.
    pub trust_note: &'static str,
    /// Whether this identity asserts CA constraints.
    pub is_ca: bool,
}

impl SelfSignedIdentity {
    /// The exact trust-scope note to display with this identity.
    #[must_use]
    pub fn trust_note(&self) -> &'static str {
        self.trust_note
    }
}

/// Generate a self-signed RSA identity on this device.
///
/// `rng` is supplied by the caller — the native CLI passes the OS CSPRNG — so
/// this function stays pure and works in any build. No document, key or
/// identifier leaves the process.
///
/// # Errors
///
/// A typed `SignatureInvalid` error if the key size is out of range, the name
/// is empty or non-ASCII-incompatible, RSA key generation fails, or the
/// produced certificate does not parse back and verify its own signature.
pub fn generate_self_signed<R: rsa::rand_core::CryptoRngCore>(
    params: &IdentityParams<'_>,
    rng: &mut R,
) -> Result<SelfSignedIdentity> {
    if !(MIN_IDENTITY_BITS..=MAX_IDENTITY_BITS).contains(&params.key_bits) {
        return Err(err!(
            Code::SignatureInvalid,
            during = "identity",
            detail = "RSA identity key size must be between 2048 and 4096 bits"
        ));
    }
    if params.common_name.trim().is_empty() {
        return Err(err!(
            Code::SignatureInvalid,
            during = "identity",
            detail = "a signing identity needs a common name"
        ));
    }
    if params.validity_days == 0 {
        return Err(err!(
            Code::SignatureInvalid,
            during = "identity",
            detail = "a signing identity needs a non-zero validity"
        ));
    }
    let not_after_unix = params
        .not_before_unix
        .checked_add(i64::from(params.validity_days).saturating_mul(86_400))
        .ok_or_else(|| {
            err!(
                Code::SignatureInvalid,
                during = "identity",
                detail = "the identity validity overflows the time range"
            )
        })?;

    let key = rsa::RsaPrivateKey::new(rng, params.key_bits).map_err(|_| {
        err!(
            Code::SignatureInvalid,
            during = "identity",
            detail = "RSA key generation failed"
        )
    })?;
    let spki = key
        .to_public_key()
        .to_public_key_der()
        .map_err(|_| {
            err!(
                Code::SignatureInvalid,
                during = "identity",
                detail = "the generated public key could not be encoded"
            )
        })?
        .as_bytes()
        .to_vec();

    let name = |cn: &str, org: Option<&str>| -> Vec<u8> {
        // RDNSequence: [ organizationName?, commonName ]. Each RDN is a SET OF
        // one AttributeTypeAndValue.
        let mut rdns = Vec::new();
        if let Some(org) = org {
            let atv = w::sequence(&[w::oid(&[2, 5, 4, 10]), w::utf8_string(org)]);
            rdns.push(w::set(&[atv]));
        }
        let cn_atv = w::sequence(&[w::oid(&[2, 5, 4, 3]), w::utf8_string(cn)]);
        rdns.push(w::set(&[cn_atv]));
        w::sequence(&rdns)
    };
    let subject = name(params.common_name, params.organization);
    // Self-signed: issuer == subject, byte for byte.
    let issuer = subject.clone();

    // Serial as a positive INTEGER, with a leading zero when the top bit is set.
    let mut serial_content = Vec::with_capacity(params.serial.len().saturating_add(1));
    if params.serial.first().is_some_and(|byte| byte & 0x80 != 0) {
        serial_content.push(0);
    }
    serial_content.extend_from_slice(&params.serial);

    let sig_alg = w::sequence(&[
        w::oid(
            SignatureAlgorithm {
                scheme: Scheme::RsaPkcs1,
                digest: DigestAlg::Sha256,
            }
            .oid(),
        ),
        w::null(),
    ]);

    // subjectKeyIdentifier identifies the key; RFC 5280 hashes the key bits.
    // SHA-256 over the full SPKI is a stable identifier (we avoid another
    // digest dependency, and the SKI is never a security decision).
    let ski = Sha256::digest(&spki).to_vec();

    let mut extensions = Vec::new();
    // basicConstraints: cA (and pathLen when a CA), critical.
    let mut bc_fields = Vec::new();
    if params.is_ca {
        bc_fields.push(w::boolean(true));
    }
    let bc = w::sequence(&bc_fields);
    extensions.push(w::sequence(&[
        w::oid(&[2, 5, 29, 19]),
        w::boolean(true),
        w::octet_string(&bc),
    ]));
    // keyUsage: digitalSignature (bit 0) always; keyCertSign (bit 5) for a CA.
    let usage: u16 = if params.is_ca { 0x8400 } else { 0x8000 };
    let ku_bits = w::tlv(w::BIT_STRING, &[0x00, (usage >> 8) as u8, usage as u8]);
    extensions.push(w::sequence(&[
        w::oid(&[2, 5, 29, 15]),
        w::boolean(true),
        w::octet_string(&ku_bits),
    ]));
    extensions.push(w::sequence(&[
        w::oid(&[2, 5, 29, 14]),
        w::octet_string(&w::octet_string(&ski)),
    ]));
    let extensions = w::tlv(0xA3, &w::sequence(&extensions));

    let tbs = w::sequence(&[
        w::tlv(0xA0, &w::integer(2)), // version v3
        w::tlv(w::INTEGER, &serial_content),
        sig_alg.clone(),
        issuer.clone(),
        w::sequence(&[
            w::signing_time(&unix_stamp(params.not_before_unix)).ok_or_else(|| {
                err!(
                    Code::SignatureInvalid,
                    during = "identity",
                    detail = "notBefore is out of the representable time range"
                )
            })?,
            w::signing_time(&unix_stamp(not_after_unix)).ok_or_else(|| {
                err!(
                    Code::SignatureInvalid,
                    during = "identity",
                    detail = "notAfter is out of the representable time range"
                )
            })?,
        ]),
        subject.clone(),
        spki_der_as_tlv(&spki)?,
        extensions,
    ]);
    let signature = SigningKey::<Sha256>::new(key.clone()).sign(&tbs).to_vec();
    let cert_der = w::sequence(&[tbs, sig_alg, w::bit_string(&signature)]);

    // Read back what we wrote: a writer bug must not ship as an opaque cert.
    let budget = Budget::profile(selis_sandbox::Surface::Fuzz);
    let mut g = budget.guard();
    verify_generated(&cert_der, &key, &mut g)?;

    let key_der = key
        .to_pkcs8_der()
        .map_err(|_| {
            err!(
                Code::SignatureInvalid,
                during = "identity",
                detail = "the generated private key could not be encoded"
            )
        })?
        .as_bytes()
        .to_vec();
    Ok(SelfSignedIdentity {
        key_pem: crate::cert::pem_encode("PRIVATE KEY", &key_der),
        cert_pem: crate::cert::pem_encode("CERTIFICATE", &cert_der),
        key_der,
        cert_der,
        trust_note: LOCAL_TRUST_NOTE,
        is_ca: params.is_ca,
    })
}

/// The `SubjectPublicKeyInfo` from `rsa` is already a SEQUENCE TLV; return it
/// unchanged or fail with a typed error if `rsa` ever returns something else.
fn spki_der_as_tlv(spki: &[u8]) -> Result<Vec<u8>> {
    if spki.first() == Some(&w::SEQUENCE) {
        Ok(spki.to_vec())
    } else {
        Err(err!(
            Code::SignatureInvalid,
            during = "identity",
            detail = "the generated SPKI is not a DER SEQUENCE"
        ))
    }
}

fn verify_generated(
    cert_der: &[u8],
    key: &rsa::RsaPrivateKey,
    g: &mut BudgetGuard<'_>,
) -> Result<()> {
    let cert = crate::cert::parse(cert_der, g)?;
    if !cert.valid_at(cert.not_before) || !cert.valid_at(cert.not_after) {
        return Err(err!(
            Code::SignatureInvalid,
            during = "identity",
            detail = "the generated certificate validity is inconsistent"
        ));
    }
    let algorithm = SignatureAlgorithm {
        scheme: Scheme::RsaPkcs1,
        digest: DigestAlg::Sha256,
    };
    // `verify_with_key` expects the whole SubjectPublicKeyInfo for RSA; the
    // parsed certificate stores exactly that in `key`, so use it.
    crate::cert::verify_with_key(cert.key, algorithm, cert.tbs, cert.signature)?;
    let expected = rsa::RsaPublicKey::from(key);
    let cert_public = match cert.key {
        crate::cert::PublicKey::Rsa(spki) => {
            rsa::RsaPublicKey::from_public_key_der(spki).map_err(|_| {
                err!(
                    Code::SignatureInvalid,
                    during = "identity",
                    detail = "the generated SPKI did not parse back"
                )
            })?
        }
        crate::cert::PublicKey::P256(_) => {
            return Err(err!(
                Code::SignatureInvalid,
                during = "identity",
                detail = "a self-signed identity must carry an RSA key"
            ))
        }
    };
    if cert_public != expected {
        return Err(err!(
            Code::SignatureInvalid,
            during = "identity",
            detail = "the generated certificate key does not match the private key"
        ));
    }
    Ok(())
}

/// Generate a self-signed identity using the device's OS CSPRNG.
///
/// The native convenience wrapper for [`generate_self_signed`]; the engine
/// crates stay clock-free (the caller supplies `not_before_unix`) and this is
/// the only place entropy is drawn. Available on native builds only — a WASM
/// shell must pass its own injected key material to [`generate_self_signed`].
#[cfg(all(feature = "rng", not(target_arch = "wasm32")))]
pub fn generate_local_identity(params: &IdentityParams<'_>) -> Result<SelfSignedIdentity> {
    let mut os = rsa::rand_core::OsRng;
    generate_self_signed(params, &mut os)
}

/// `YYYYMMDDHHMMSSZ` from Unix seconds, without a clock in this crate.
fn unix_stamp(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    let hour = secs / 3_600;
    let minute = (secs % 3_600) / 60;
    let second = secs % 60;
    format!("{y:04}{m:02}{d:02}{hour:02}{minute:02}{second:02}Z")
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::arithmetic_side_effects
    )]

    use super::*;

    fn rng() -> rsa::rand_core::OsRng {
        rsa::rand_core::OsRng
    }

    fn params<'a>(cn: &'a str, bits: usize, is_ca: bool) -> IdentityParams<'a> {
        IdentityParams {
            common_name: cn,
            organization: Some("Selis Test"),
            key_bits: bits,
            validity_days: 30,
            not_before_unix: 1_700_000_000,
            serial: [0x42; 16],
            is_ca,
        }
    }

    #[test]
    fn a_generated_identity_parses_verifies_and_is_local_only() {
        let mut rng = rng();
        let id = generate_self_signed(&params("Ada Lovelace (Selis local)", 2048, false), &mut rng)
            .expect("generates");
        assert!(!id.is_ca);
        assert_eq!(id.trust_note(), LOCAL_TRUST_NOTE);
        assert!(id.trust_note().contains("not a publicly trusted"));
        assert!(id.key_pem.starts_with("-----BEGIN PRIVATE KEY-----"));
        assert!(id.cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert_eq!(
            crate::cert::pem_body(&id.cert_pem, "CERTIFICATE"),
            Some(id.cert_der.clone())
        );
    }

    #[test]
    fn a_generated_identity_chains_only_to_itself_as_an_anchor() {
        let mut rng = rng();
        let id = generate_self_signed(&params("Local signer", 2048, false), &mut rng)
            .expect("generates");
        let budget = Budget::profile(selis_sandbox::Surface::Fuzz);
        let mut g = budget.guard();
        let not_before = crate::cert::parse(&id.cert_der, &mut g)
            .map(|c| c.not_before)
            .unwrap_or(0);
        let anchored = crate::cert::validate_chain(
            &id.cert_der,
            &[],
            &[id.cert_der.as_slice()],
            not_before,
            &mut g,
        )
        .expect("validates");
        assert!(
            anchored.trusted,
            "a self-signed cert is trusted only as an anchor"
        );
        let unanchored = crate::cert::validate_chain(&id.cert_der, &[], &[], 1_700_000_000, &mut g)
            .expect("reports");
        assert!(!unanchored.trusted, "with no anchors nothing is trusted");
        assert!(unanchored
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("self-signed")));
    }

    #[test]
    fn oversized_small_and_empty_names_are_refused() {
        let mut rng = rng();
        assert!(generate_self_signed(&params("ok", 1024, false), &mut rng).is_err());
        assert!(generate_self_signed(&params("ok", 8192, false), &mut rng).is_err());
        assert!(generate_self_signed(&params("", 2048, false), &mut rng).is_err());
    }

    #[test]
    fn ca_identities_assert_the_ca_bits() {
        let mut rng = rng();
        let ca = generate_self_signed(&params("Selis Test CA", 2048, true), &mut rng)
            .expect("generates a CA");
        assert!(ca.is_ca);
        let budget = Budget::profile(selis_sandbox::Surface::Fuzz);
        let mut g = budget.guard();
        let cert = crate::cert::parse(&ca.cert_der, &mut g).expect("parses");
        assert_eq!(cert.is_ca, Some(true));
        assert!(cert.allows_certificate_signing());
        assert!(cert.allows_digital_signature());
    }
}
