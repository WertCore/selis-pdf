//! Native OS-keystore abstraction (SL-5.SIGN.06) — honest capability surface.
//!
//! This module exists on native targets only (`cfg(not(wasm32))`); a browser
//! chunk never compiles it. It models the three OS keystores the plan names —
//! Windows CNG / CryptoAPI, macOS Keychain and the Linux secret service — and
//! gives callers a stable enum, a key reference and a typed error for asking a
//! store to sign a digest.
//!
//! # Why signing is a typed "not available" today
//!
//! A genuine keystore signature must call the platform API with a **non-
//! exportable** key handle (NCryptSignHash / SecKeyCreateSignature). The only
//! Rust bindings for those APIs in this workspace are raw FFI (`windows`,
//! `security-framework-sys`) whose entry points are `unsafe`. This crate
//! `#![forbid(unsafe_code)]` by policy (ADR-P0019), and a C ABI forced into the
//! signing path would break that guarantee and the browser build alike. There
//! is no safe, maintained crate that exposes CNG/Keychain signing here, so we
//! do **not** pretend to: [`sign_digest`] returns the typed
//! [`SignError::OsKeyStoreUnavailable`] naming the store and the reason. This
//! is a deliberate, documented gap, not a silent fallback to exporting a key.
//!
//! Adding a safe backend later is an additive change to [`sign_digest`]; the
//! public surface and the "keys never leave the device" rule stay the same.
//! PKCS#12 keys (which are files, and therefore exportable by construction)
//! are supported for real in [`crate::p12`].

use crate::SignError;

/// The native keystores the plan names (SL-5.SIGN.06).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsKeyStore {
    /// Windows CNG / CryptoAPI (`NCrypt*`).
    WindowsCng,
    /// macOS Keychain / Security framework (`SecKey*`).
    MacOsKeychain,
    /// Linux D-Bus secret service.
    LinuxSecretService,
}

impl OsKeyStore {
    /// A short, stable name used in errors and reports.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::WindowsCng => "Windows CNG",
            Self::MacOsKeychain => "macOS Keychain",
            Self::LinuxSecretService => "Linux secret-service",
        }
    }

    /// Why no safe signing backend is compiled in for this store.
    #[must_use]
    pub const fn unavailability_reason(self) -> &'static str {
        match self {
            Self::WindowsCng => {
                "CNG NCryptSignHash bindings are raw FFI and this build forbids unsafe code; \
                 no safe maintained Rust backend is available"
            }
            Self::MacOsKeychain => {
                "Keychain SecKeyCreateSignature bindings are raw FFI and this build forbids \
                 unsafe code; no safe maintained Rust backend is available"
            }
            Self::LinuxSecretService => {
                "the secret-service stores secrets, not non-exportable signing keys; signing a \
                 digest needs a platform key handle this safe build does not expose"
            }
        }
    }
}

/// A key held by a native OS keystore, identified by the store and a
/// caller-supplied identifier (a CNG key name, a Keychain generic-item label,
/// or a secret-service alias). No key bytes are carried in this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OsKeyRef<'a> {
    /// Which native store holds the key.
    pub store: OsKeyStore,
    /// The store's identifier for the key (never the key material).
    pub identifier: &'a str,
}

impl<'a> OsKeyRef<'a> {
    /// Build a reference to a key in `store`.
    #[must_use]
    pub const fn new(store: OsKeyStore, identifier: &'a str) -> Self {
        Self { store, identifier }
    }
}

/// The keystores this build could target on the current platform.
///
/// This is a capability list, not a claim that signing works: see
/// [`sign_digest`]. A store absent from this list is unsupported on this OS.
#[must_use]
pub fn available_stores() -> &'static [OsKeyStore] {
    #[cfg(target_os = "windows")]
    {
        &[OsKeyStore::WindowsCng]
    }
    #[cfg(target_os = "macos")]
    {
        &[OsKeyStore::MacOsKeychain]
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        &[OsKeyStore::LinuxSecretService]
    }
}

/// Whether `store` is a recognised keystore on this platform.
#[must_use]
pub fn is_supported(store: OsKeyStore) -> bool {
    available_stores().contains(&store)
}

/// Ask a native OS keystore to sign a SHA-256 digest with a key handle.
///
/// # Errors
///
/// Always returns [`SignError::OsKeyStoreUnavailable`] in this build: the
/// detail names `key.store` and the precise reason from
/// [`OsKeyStore::unavailability_reason`]. A store not supported on this OS is
/// refused before any backend is consulted.
///
/// # Budget
///
/// Constant work and no allocation beyond the error string; no key material
/// is copied, uploaded or written anywhere.
pub fn sign_digest(key: &OsKeyRef<'_>, digest: &[u8; 32]) -> Result<Vec<u8>, SignError> {
    let _ = digest; // no backend consumes it yet; kept in the signature for the safe future one.
    let store = key.store;
    if !is_supported(store) {
        return Err(SignError::OsKeyStoreUnavailable(format!(
            "{} is not a keystore on this platform",
            store.name()
        )));
    }
    Err(SignError::OsKeyStoreUnavailable(format!(
        "{} signing for key {:?} is not available: {}",
        store.name(),
        key.identifier,
        store.unavailability_reason()
    )))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stderr
    )]

    use super::{available_stores, is_supported, sign_digest, OsKeyRef, OsKeyStore};
    use crate::SignError;

    #[test]
    fn the_platform_has_at_least_one_modelled_store() {
        let stores = available_stores();
        assert!(!stores.is_empty());
        for store in stores {
            assert!(is_supported(*store));
            assert!(!store.name().is_empty());
            assert!(!store.unavailability_reason().is_empty());
        }
    }

    /// A real keystore key is not present in CI. This test deliberately skips
    /// the signing assertion when none is available and instead proves the
    /// typed, precise refusal — so the module cannot silently claim support.
    #[test]
    fn signing_without_a_native_backend_is_a_precise_typed_gap() {
        let Some(store) = available_stores().first().copied() else {
            eprintln!("skip: this platform models no OS keystore");
            return;
        };
        let key = OsKeyRef::new(store, "selis-test-signing-key");
        let digest = [0xABu8; 32];
        match sign_digest(&key, &digest) {
            Err(SignError::OsKeyStoreUnavailable(detail)) => {
                assert!(detail.contains(store.name()));
                assert!(detail.contains("unsafe") || detail.contains("non-exportable"));
            }
            other => panic!("expected a typed OS-keystore gap, got {other:?}"),
        }
    }

    #[test]
    fn a_store_from_another_platform_is_refused() {
        let wrong = if is_supported(OsKeyStore::WindowsCng) {
            OsKeyStore::MacOsKeychain
        } else {
            OsKeyStore::WindowsCng
        };
        assert!(!is_supported(wrong));
        let key = OsKeyRef::new(wrong, "x");
        assert!(matches!(
            sign_digest(&key, &[0u8; 32]),
            Err(SignError::OsKeyStoreUnavailable(_))
        ));
    }
}
