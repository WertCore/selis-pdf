//! PKCS#12 / PFX as a signing key source (SL-5.SIGN.06).
//!
//! A `.p12` / `.pfx` is a password-protected *file* the caller chose. This
//! module reads the bytes handed to it (the crate never opens files itself),
//! opens the store locally with [`p12_keystore`], and converts the private key
//! and certificate chain to the PEM form [`crate::sign_digital`] already
//! consumes. No key byte is written to the PDF, to a request, or to a log
//! (ADR-P0019). Both modern PBES2/AES-256 and legacy RC2/3DES protected stores
//! are supported by the pinned `p12-keystore` dependency.
//!
//! # Budget / Malformed Input
//!
//! [`load_pkcs12`] charges the file's byte length to [`selis_sandbox::Resource::Bytes`]
//! and refuses data over [`MAX_P12_BYTES`]. An empty file, a wrong password, a
//! store with no private-key entry, or a non-PKCS#8 key is a typed error, never
//! a panic.

use p12_keystore::{KeyStore, Pkcs12ImportPolicy};
use selis_crypto::cert::{pem_encode, PrivateKeyKind};
use selis_sandbox::{Budget, BudgetGuard, Resource};

use crate::{invalid, DigitalOptions, SignError};

/// The largest PKCS#12 file this build will open, as a refusal bound.
pub const MAX_P12_BYTES: usize = 1 << 20;

/// A key and chain extracted from a local PKCS#12 file, in the PEM form the
/// signing engine consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkcs12Key {
    /// The private key as an unencrypted PKCS#8 PEM block.
    pub key_pem: String,
    /// The certificate chain, leaf first, as PEM `CERTIFICATE` blocks.
    pub certificates_pem: Vec<String>,
    /// The alias (friendly name) of the key entry in the store.
    pub alias: String,
}

impl Pkcs12Key {
    /// Put the extracted key and chain into a [`DigitalOptions`] value, leaving
    /// the page, rectangle, name and time fields untouched.
    #[must_use]
    pub fn into_options(self, mut options: DigitalOptions) -> DigitalOptions {
        options.key_pem = self.key_pem;
        options.certificates_pem = self.certificates_pem;
        options
    }
}

/// Open a password-protected PKCS#12/PFX blob and return its signing key and
/// chain as PEM, ready for [`crate::sign_digital`] or
/// [`crate::sign_certification`].
///
/// # Errors
///
/// * [`SignError::Limit`] if `der` exceeds [`MAX_P12_BYTES`];
/// * [`SignError::Invalid`] for an empty blob, a wrong password, a corrupt
///   store, or a store with no private-key/certificate entry;
/// * [`SignError::Unsupported`] for an EC key (this build signs RSA only).
///
/// # Budget
///
/// Charges `der.len()` bytes to the guard, then does bounded local parsing
/// inside `p12-keystore`; no network and no filesystem access.
pub fn load_pkcs12(
    der: &[u8],
    password: &str,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Pkcs12Key, SignError> {
    let _ = budget; // p12-keystore does its own local work; byte charge is the bound.
    if der.is_empty() {
        return Err(invalid("the PKCS#12 file is empty"));
    }
    if der.len() > MAX_P12_BYTES {
        return Err(SignError::Limit(format!(
            "a PKCS#12 file may be at most {MAX_P12_BYTES} bytes"
        )));
    }
    g.charge(
        Resource::Bytes,
        u64::try_from(der.len()).unwrap_or(u64::MAX),
    )
    .map_err(|e| SignError::Limit(format!("the PKCS#12 byte budget was exhausted: {e}")))?;

    let store = KeyStore::from_pkcs12(der, password, Pkcs12ImportPolicy::Strict).map_err(|e| {
        SignError::Invalid(format!(
            "the PKCS#12 file could not be opened (wrong password or corrupt data): {e}"
        ))
    })?;
    let (alias, chain) = store
        .private_key_chain()
        .ok_or_else(|| invalid("the PKCS#12 file carries no private-key entry to sign with"))?;

    // `p12-keystore` hands back PKCS#8 DER. Wrapping it as PEM routes the key
    // through the exact same parser the existing PEM path uses, so there is
    // one place that decides which algorithms this build signs.
    let key_pem = pem_encode("PRIVATE KEY", chain.key().as_der());
    if selis_crypto::cert::private_key_kind(&key_pem) == PrivateKeyKind::Ec {
        return Err(SignError::Unsupported(
            "this build signs with RSA keys only; the PKCS#12 file holds an EC key".to_string(),
        ));
    }

    let certificates_pem: Vec<String> = chain
        .certs()
        .iter()
        .map(|cert| pem_encode("CERTIFICATE", cert.as_der()))
        .collect();
    if certificates_pem.is_empty() {
        return Err(invalid(
            "the PKCS#12 private-key entry carries no certificate",
        ));
    }
    Ok(Pkcs12Key {
        key_pem,
        certificates_pem,
        alias: alias.to_string(),
    })
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

    use p12_keystore::{Certificate, KeyStore, KeyStoreEntry, PrivateKey, PrivateKeyChain};

    use super::{load_pkcs12, DigitalOptions, SignError};
    use crate::signatures;
    use crate::tests::{base_pdf, digital_options, EC_KEY, LEAF, LEAF_KEY, NOW, ROOT};
    use selis_crypto::cert::pem_body;
    use selis_sandbox::{Budget, Surface};

    const PASSWORD: &str = "correct horse battery staple";

    /// Build a PKCS#12 blob in-test from the committed RSA fixtures. This is a
    /// real, encrypted store, not a hand-rolled mock of one.
    fn make_pkcs12(key_pem: &str, certs: &[&str], password: &str) -> Vec<u8> {
        let key_der = pem_body(key_pem, "PRIVATE KEY").expect("a PKCS#8 key fixture");
        let key = PrivateKey::from_der(&key_der).expect("the key parses as PKCS#8");
        let certs: Vec<Certificate> = certs
            .iter()
            .map(|pem| {
                let der = pem_body(pem, "CERTIFICATE").expect("a certificate fixture");
                Certificate::from_der(&der).expect("the certificate parses")
            })
            .collect();
        let mut store = KeyStore::new();
        store.add_entry(
            "signer",
            KeyStoreEntry::PrivateKeyChain(PrivateKeyChain::new("local-id", key, certs)),
        );
        store
            .writer(password)
            .write()
            .expect("the store is written")
    }

    #[test]
    fn a_pkcs12_round_trip_signs_and_never_embeds_the_private_key() {
        let src = base_pdf(None);
        let p12 = make_pkcs12(LEAF_KEY, &[LEAF, ROOT], PASSWORD);
        let budget = Budget::profile(Surface::Viewer);
        let mut g = budget.guard();
        let key = load_pkcs12(&p12, PASSWORD, &budget, &mut g).expect("the store opens");
        assert_eq!(key.alias, "signer");
        assert!(key.key_pem.starts_with("-----BEGIN PRIVATE KEY-----"));
        assert_eq!(key.certificates_pem.len(), 2);

        let options = key.into_options(digital_options("Signature1"));
        let (out, report) =
            crate::sign_digital(&src, &options, &budget, &mut g).expect("signs from the p12 key");
        assert!(out.starts_with(&src));
        assert!(report.verified);

        let read = signatures(
            &out,
            &[pem_body(ROOT, "CERTIFICATE").unwrap()],
            NOW,
            &budget,
            &mut g,
        )
        .expect("reports");
        assert_eq!(read.signatures.len(), 1);
        assert!(read.signatures[0].verified);
        assert!(read.signatures[0].covers_whole_document);

        // The private key DER must not appear anywhere in the produced PDF.
        let key_der = pem_body(LEAF_KEY, "PRIVATE KEY").unwrap();
        let needle = &key_der[8..40];
        assert!(
            !out.windows(needle.len()).any(|window| window == needle),
            "private key bytes leaked into the output PDF"
        );
    }

    #[test]
    fn a_wrong_pkcs12_password_is_refused() {
        let p12 = make_pkcs12(LEAF_KEY, &[LEAF, ROOT], PASSWORD);
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        match load_pkcs12(&p12, "not the password", &budget, &mut g) {
            Err(SignError::Invalid(detail)) => assert!(detail.contains("wrong password")),
            other => panic!("expected a wrong-password refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_ec_key_in_a_pkcs12_is_refused_by_name() {
        let p12 = make_pkcs12(EC_KEY, &[LEAF], PASSWORD);
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        match load_pkcs12(&p12, PASSWORD, &budget, &mut g) {
            Err(SignError::Unsupported(detail)) => assert!(detail.contains("EC")),
            other => panic!("expected an EC refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_non_pkcs12_blob_and_an_empty_input_are_refused() {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        assert!(matches!(
            load_pkcs12(b"", PASSWORD, &budget, &mut g),
            Err(SignError::Invalid(_))
        ));
        assert!(matches!(
            load_pkcs12(b"this is not a PKCS#12 file", PASSWORD, &budget, &mut g),
            Err(SignError::Invalid(_))
        ));
    }

    #[test]
    fn a_pkcs12_blob_over_the_size_bound_is_refused() {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let huge = vec![0u8; super::MAX_P12_BYTES.saturating_add(1)];
        assert!(matches!(
            load_pkcs12(&huge, PASSWORD, &budget, &mut g),
            Err(SignError::Limit(_))
        ));
    }

    // Keep the import used when the `into_options` signature changes.
    #[allow(dead_code)]
    fn _options_type(_: DigitalOptions) {}
}
