//! Tests for encrypted incremental save (SL-5.EDIT.06).
//!
//! For each of the three standard-handler ciphers we build an encrypted
//! fixture, append a `SetObject` mutation through [`save_incremental_encrypted`],
//! then prove all four DoD properties: the bytes parse, the prefix is intact,
//! the change decrypts with the password while every other object still
//! decrypts, and no plaintext copy of the changed value appears in the
//! appended revision. `qpdf --check` / `--decrypt` is the scriptable interop
//! oracle when qpdf is installed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use crate::incremental::{
    encrypted_save_key_from_parts, materialise, open_encrypted, open_encrypted_public_key,
    save_incremental, save_incremental_encrypted, EncryptedSaveKey, EncryptionPolicy, ObjectModel,
    ObjectMutation,
};
use selis_bytes::Bytes;
use selis_pdf_cos::doc_writer::{ContentBuilder, DocumentBuilder};
use selis_pdf_cos::encrypt::{DecryptPolicy, EncryptInfo, R6Material};
use selis_pdf_cos::{Obj, Ref, XrefEntry};
use selis_pdf_doc::Resolver;
use selis_sandbox::{Budget, BudgetGuard, FixedClock};

const TITLE_MARKER: &[u8] = b"EDIT06_PLAINTEXT_MARKER_5F8C21";
const CONTENT_MARKER: &[u8] = b"ORIGINAL_CONTENT_MARKER";

#[derive(Debug, Clone, Copy)]
enum HandlerCase {
    /// `/V 2 /R 3` — RC4-128.
    Rc4,
    /// `/V 4 /R 4` with `/CFM /AESV2` — AES-128.
    AesV2,
    /// `/V 5 /R 6` — AES-256.
    AesV3,
}

impl HandlerCase {
    const ALL: [Self; 3] = [Self::Rc4, Self::AesV2, Self::AesV3];

    fn label(self) -> &'static str {
        match self {
            Self::Rc4 => "RC4-128 (R3)",
            Self::AesV2 => "AESV2 (R4)",
            Self::AesV3 => "AESV3 (R6)",
        }
    }

    fn expected_rev(self) -> u8 {
        match self {
            Self::Rc4 => 3,
            Self::AesV2 => 4,
            Self::AesV3 => 6,
        }
    }

    fn expected_aes(self) -> bool {
        matches!(self, Self::AesV2 | Self::AesV3)
    }
}

/// Fixed document ID: revisions 2–4 hash `/ID[0]` into the file key, so the
/// fixture must write exactly the bytes its `/Encrypt` dictionary was built
/// against.
fn document_id() -> Vec<u8> {
    (1u8..=16).collect()
}

fn budget() -> Budget {
    Budget::unlimited()
}

fn guard(b: &Budget) -> BudgetGuard<'_> {
    b.guard_with(&FixedClock(0), selis_sandbox::CancelToken::new())
}

/// Build a one-page encrypted document: a catalog, page tree, content stream
/// carrying `CONTENT_MARKER`, and an `/Info` dictionary carrying the original
/// title. The empty user password authenticates every case.
fn encrypted_base(case: HandlerCase) -> Vec<u8> {
    let b = budget();
    let mut g = guard(&b);
    let id0 = document_id();

    let mut builder = DocumentBuilder::new();
    let mut content = ContentBuilder::new();
    content
        .begin_text()
        .set_font("Helvetica", 12.0)
        .text_at(10.0, 50.0)
        .show_text(std::str::from_utf8(CONTENT_MARKER).unwrap())
        .end_text();
    builder.add_page(100.0, 100.0, content.to_bytes().as_slice());
    builder.set_info(b"Title", "ORIGINAL_TITLE");
    builder.set_id(id0.clone(), id0.clone());

    let (info, file_key) = match case {
        HandlerCase::Rc4 => EncryptInfo::new_legacy(b"", b"owner", 0xFFFF_F0C0, 3, 2, 128, &id0)
            .expect("an RC4-128 /Encrypt dictionary"),
        HandlerCase::AesV2 => EncryptInfo::new_legacy(b"", b"owner", 0xFFFF_F0C0, 4, 4, 128, &id0)
            .expect("an AESV2 /Encrypt dictionary"),
        HandlerCase::AesV3 => {
            let material = R6Material::from_bytes(&[0x5Au8; 64]).expect("credential material");
            EncryptInfo::new_r6(b"", b"owner", 0xFFFF_F0C0, &material)
        }
    };
    builder.set_encrypt(info, file_key);
    builder.write(&b, &mut g).expect("write encrypted fixture")
}

/// Resolve every object through the decryption policy, mirroring the
/// production read-back verifier so the test sees exactly what a reader sees.
fn materialise_decrypted(
    src: &[u8],
    key: &EncryptedSaveKey,
    b: &Budget,
    g: &mut BudgetGuard<'_>,
) -> ObjectModel {
    let startxref = selis_pdf_cos::xref::find_startxref(src, 4096).expect("startxref");
    let doc = selis_pdf_cos::parse_revisions(src, startxref, b, g).expect("parse revisions");
    let last = doc.len().checked_sub(1).expect("at least one revision");
    let view = doc.at_revision(last).expect("last view");
    let mut resolver = Resolver::new(&doc, src, b);
    resolver.set_key(DecryptPolicy::from_encrypt(&key.info, key.file_key.clone()));
    let mut model = ObjectModel::new();
    for (num, entry) in view.xref {
        if matches!(entry, XrefEntry::InUse { .. }) || matches!(entry, XrefEntry::Compressed { .. })
        {
            let value = resolver
                .resolve(Ref::new(num, entry.gen()), g)
                .unwrap_or_else(|e| panic!("object {num} must decrypt: {e:?}"));
            model.insert(num, value);
        }
    }
    model
}

fn object_with_title(model: &ObjectModel, title: &[u8]) -> u32 {
    *model
        .iter()
        .find(|(_, obj)| {
            matches!(obj, Obj::Dict(pairs) if pairs.iter().any(|(k, v)| {
                k.as_slice() == b"Title" && matches!(v, Obj::String(s) if s.as_slice() == title)
            }))
        })
        .map(|(num, _)| num)
        .unwrap_or_else(|| {
            panic!(
                "no object carries title {:?}",
                String::from_utf8_lossy(title)
            )
        })
}

fn object_with_stream_marker(model: &ObjectModel, marker: &[u8]) -> u32 {
    *model
        .iter()
        .find(|(_, obj)| {
            matches!(obj, Obj::Stream { data, .. } if data.windows(marker.len()).any(|w| w == marker))
        })
        .map(|(num, _)| num)
        .expect("the content stream carrying the marker exists")
}

fn expected_title(value: &[u8]) -> Obj {
    Obj::Dict(vec![(
        Bytes::copy_from_slice(b"Title"),
        Obj::String(Bytes::copy_from_slice(value)),
    )])
}

/// The core EDIT.06 DoD, run once per handler.
fn roundtrip(case: HandlerCase) {
    let b = budget();
    let src = encrypted_base(case);
    let mut g = guard(&b);

    let key = open_encrypted(&src, b"", EncryptionPolicy::KeepExisting, &b, &mut g)
        .expect("authenticate")
        .expect("encrypted fixture has a key");
    assert_eq!(
        key.handler_revision(),
        case.expected_rev(),
        "{} keeps /R",
        case.label()
    );
    assert_eq!(
        key.is_aes(),
        case.expected_aes(),
        "{} keeps /CFM",
        case.label()
    );

    let model = materialise_decrypted(&src, &key, &b, &mut g);
    let info_num = object_with_title(&model, b"ORIGINAL_TITLE");
    let content_num = object_with_stream_marker(&model, CONTENT_MARKER);

    let mutations = vec![ObjectMutation::SetObject {
        num: info_num,
        value: expected_title(TITLE_MARKER),
    }];
    let (out, report) =
        save_incremental_encrypted(&src, &model, &mutations, &key, &b, &mut g).expect("save");
    assert!(report.prefix_preserved, "{}: I3 prefix", case.label());
    assert_eq!(
        report.objects_changed,
        1,
        "{}: one object changed",
        case.label()
    );
    assert_eq!(
        report.revisions,
        2,
        "{}: one appended revision",
        case.label()
    );
    assert!(
        out.len() > src.len(),
        "{}: append grows the file",
        case.label()
    );

    // No plaintext object: the marker is ciphertext in the appended bytes.
    let appended = &out[src.len()..];
    assert!(
        !appended
            .windows(TITLE_MARKER.len())
            .any(|window| window == TITLE_MARKER),
        "{}: appended revision must not contain the plaintext marker",
        case.label()
    );

    // Re-open the OUTPUT with the password: same handler, change readable.
    let mut g2 = guard(&b);
    let out_key = open_encrypted(&out, b"", EncryptionPolicy::KeepExisting, &b, &mut g2)
        .expect("re-authenticate output")
        .expect("output is still encrypted");
    assert_eq!(out_key.handler_revision(), key.handler_revision());
    assert_eq!(out_key.is_aes(), key.is_aes());
    let reopened = materialise_decrypted(&out, &out_key, &b, &mut g2);

    assert_eq!(
        reopened.get(&info_num),
        Some(&expected_title(TITLE_MARKER)),
        "{}: the change decrypts to the expected value",
        case.label()
    );
    assert_eq!(
        reopened.keys().copied().collect::<Vec<_>>(),
        model.keys().copied().collect::<Vec<_>>(),
        "{}: no object was dropped",
        case.label()
    );
    let content = reopened.get(&content_num).expect("content object survived");
    match content {
        Obj::Stream { data, .. } => assert!(
            data.windows(CONTENT_MARKER.len())
                .any(|window| window == CONTENT_MARKER),
            "{}: an untouched object still decrypts",
            case.label()
        ),
        other => panic!("{}: content is no longer a stream: {other:?}", case.label()),
    }

    // The engine (parse + decrypt) opens the saved file with the same empty
    // user password and finds the page tree.
    let session = selis_pdf_engine::Session::open(out.clone(), &b, &FixedClock(0))
        .unwrap_or_else(|e| panic!("{}: engine open failed: {e:?}", case.label()));
    assert_eq!(session.len(), 1, "{}: page count", case.label());
}

#[test]
fn rc4_incremental_save_roundtrips() {
    roundtrip(HandlerCase::Rc4);
}

#[test]
fn aesv2_incremental_save_roundtrips() {
    roundtrip(HandlerCase::AesV2);
}

#[test]
fn aesv3_incremental_save_roundtrips() {
    roundtrip(HandlerCase::AesV3);
}

#[test]
fn no_key_refuses_encrypted_source() {
    let b = budget();
    let src = encrypted_base(HandlerCase::AesV2);
    let mut g = guard(&b);
    let model = materialise(&src, &b, &mut g).expect("clear model still resolves structurally");
    let mutations = vec![ObjectMutation::SetObject {
        num: 5,
        value: expected_title(TITLE_MARKER),
    }];
    let err = save_incremental(&src, &model, &mutations, &b, &mut g)
        .expect_err("a key-less save must refuse an encrypted document");
    assert_eq!(err.code(), selis_error::Code::MutationInvalid);
    assert!(
        err.ctx()
            .detail
            .as_deref()
            .unwrap_or("")
            .contains("save_incremental_encrypted"),
        "the refusal must point at the encrypted API: {err:?}"
    );
}

#[test]
fn wrong_password_is_refused() {
    let b = budget();
    let src = encrypted_base(HandlerCase::Rc4);
    let mut g = guard(&b);
    let err = open_encrypted(
        &src,
        b"not-the-password",
        EncryptionPolicy::KeepExisting,
        &b,
        &mut g,
    )
    .expect_err("wrong password must not authenticate");
    assert_eq!(err.code(), selis_error::Code::WrongPassword);
}

#[test]
fn aesv3_upgrade_requires_explicit_consent() {
    let b = budget();
    let src = encrypted_base(HandlerCase::Rc4);
    let mut g = guard(&b);

    // No consent: refused as a policy/consent error before any key work, even
    // though the correct OLD password was supplied.
    let no_consent = open_encrypted(
        &src,
        b"",
        EncryptionPolicy::UpgradeToAesV3 {
            user_consent: false,
            new_user_password: b"new-user".to_vec(),
            new_owner_password: b"new-owner".to_vec(),
        },
        &b,
        &mut g,
    )
    .expect_err("no consent -> refusal");
    assert_eq!(no_consent.code(), selis_error::Code::PermissionDenied);
    assert!(
        no_consent
            .ctx()
            .detail
            .as_deref()
            .unwrap_or("")
            .contains("explicit user consent"),
        "{no_consent:?}"
    );

    // The default path on the same bytes still works: no silent upgrade.
    let key = open_encrypted(&src, b"", EncryptionPolicy::KeepExisting, &b, &mut g)
        .expect("default")
        .expect("key");
    assert_eq!(
        key.handler_revision(),
        3,
        "RC4 handler is reused, not upgraded"
    );
    assert!(!key.is_aes());
}

/// With consent, the upgrade is a full rewrite under a fresh AESV3 handler.
/// The source authenticates with the OLD (empty) password; the output opens
/// with the NEW password and the old one no longer works. The mutation still
/// decrypts and every untouched object survives.
#[test]
fn aesv3_upgrade_with_consent_rewrites_and_rotates_password() {
    let b = budget();
    let src = encrypted_base(HandlerCase::Rc4);
    let mut g = guard(&b);
    let new_user = b"new-user";
    let new_owner = b"new-owner";

    let key = open_encrypted(
        &src,
        b"", // the OLD user password authenticates the source
        EncryptionPolicy::UpgradeToAesV3 {
            user_consent: true,
            new_user_password: new_user.to_vec(),
            new_owner_password: new_owner.to_vec(),
        },
        &b,
        &mut g,
    )
    .expect("consented upgrade authenticates with the old password")
    .expect("encrypted source has a key");
    // The key still describes the OLD handler: it was only used to decrypt.
    assert_eq!(key.handler_revision(), 3);
    assert!(!key.is_aes());

    let model = materialise_decrypted(&src, &key, &b, &mut g);
    let info_num = object_with_title(&model, b"ORIGINAL_TITLE");
    let content_num = object_with_stream_marker(&model, CONTENT_MARKER);
    let mutations = vec![ObjectMutation::SetObject {
        num: info_num,
        value: expected_title(TITLE_MARKER),
    }];

    let (out, report) =
        save_incremental_encrypted(&src, &model, &mutations, &key, &b, &mut g).expect("upgrade");
    assert!(
        !report.prefix_preserved,
        "a handler change cannot keep the old bytes as a prefix"
    );
    assert_eq!(report.revisions, 1, "the rewrite is a single revision");
    assert_ne!(out, src, "the upgraded bytes differ from the source");
    assert!(
        !out.windows(TITLE_MARKER.len()).any(|w| w == TITLE_MARKER),
        "the edited value must be ciphertext in the rewritten file"
    );

    // The NEW password opens the output as AESV3/R6.
    let mut g2 = guard(&b);
    let out_key = open_encrypted(&out, new_user, EncryptionPolicy::KeepExisting, &b, &mut g2)
        .expect("new password opens the upgraded file")
        .expect("output is still encrypted");
    assert_eq!(out_key.handler_revision(), 6, "installed AESV3/R6");
    assert!(out_key.is_aes());
    assert_eq!(out_key.handler_name(), "AESV3 (R6)");

    // The OLD password no longer opens it.
    let mut g3 = guard(&b);
    let old = open_encrypted(&out, b"", EncryptionPolicy::KeepExisting, &b, &mut g3)
        .expect_err("the old password must not open the upgraded file");
    assert_eq!(old.code(), selis_error::Code::WrongPassword);

    // The owner password of the new handler also authenticates (owner keys
    // open the file too), and the decrypted model matches the edit.
    let mut g4 = guard(&b);
    let owner_key = open_encrypted(&out, new_owner, EncryptionPolicy::KeepExisting, &b, &mut g4)
        .expect("new owner password opens the upgraded file")
        .expect("owner key");
    assert_eq!(owner_key.handler_revision(), 6);

    let reopened = materialise_decrypted(&out, &out_key, &b, &mut g2);
    assert_eq!(
        reopened.get(&info_num),
        Some(&expected_title(TITLE_MARKER)),
        "the mutation decrypts under the new key"
    );
    let content = reopened.get(&content_num).expect("content survived");
    match content {
        Obj::Stream { data, .. } => assert!(
            data.windows(CONTENT_MARKER.len())
                .any(|w| w == CONTENT_MARKER),
            "an untouched stream still decrypts"
        ),
        other => panic!("content is no longer a stream: {other:?}"),
    }

    // The engine tolerantly opens the rewritten file (its page-tree structure
    // is plaintext) and sees one page — the full rewrite is well-formed.
    let session = selis_pdf_engine::Session::open(out.clone(), &b, &FixedClock(0))
        .expect("engine opens the upgraded file");
    assert_eq!(session.len(), 1, "page count survives the rewrite");
}

#[test]
fn unencrypted_source_needs_no_key_and_refuses_upgrade() {
    let b = budget();
    let src = {
        let mut builder = DocumentBuilder::new();
        let mut content = ContentBuilder::new();
        content.begin_text().show_text("plain").end_text();
        builder.add_page(100.0, 100.0, content.to_bytes().as_slice());
        builder.write(&b, &mut guard(&b)).expect("write plain")
    };
    let mut g = guard(&b);
    assert!(
        open_encrypted(&src, b"", EncryptionPolicy::KeepExisting, &b, &mut g)
            .expect("plain source")
            .is_none(),
        "no key material for a clear document"
    );
    let err = open_encrypted(
        &src,
        b"",
        EncryptionPolicy::UpgradeToAesV3 {
            user_consent: true,
            new_user_password: b"new-user".to_vec(),
            new_owner_password: b"new-owner".to_vec(),
        },
        &b,
        &mut g,
    )
    .expect_err("adding protection needs a full rewrite");
    assert_eq!(err.code(), selis_error::Code::EncryptUnsupported);
}

#[test]
fn key_from_parts_rejects_public_key_and_empty_keys() {
    let b = budget();
    let src = encrypted_base(HandlerCase::AesV3);
    let mut g = guard(&b);
    let key = open_encrypted(&src, b"", EncryptionPolicy::KeepExisting, &b, &mut g)
        .expect("auth")
        .expect("key");
    let rebuilt = encrypted_save_key_from_parts(key.info.clone(), key.file_key.clone())
        .expect("a standard-handler key round-trips");
    assert_eq!(rebuilt.handler_revision(), 6);
    assert!(encrypted_save_key_from_parts(key.info.clone(), Vec::new()).is_none());
    let mut pubkey = key.info.clone();
    pubkey.handler = selis_pdf_cos::encrypt::Handler::PubKey;
    assert!(encrypted_save_key_from_parts(pubkey, key.file_key.clone()).is_none());
}

/// The first ordinary content stream (not an object/xref stream) in a model.
fn first_content_stream(model: &ObjectModel) -> u32 {
    *model
        .iter()
        .find(|(_, obj)| {
            matches!(obj, Obj::Stream { dict, .. } if !dict.iter().any(|(k, v)| {
                k.as_slice() == b"Type"
                    && matches!(v, Obj::Name(n) if n.as_slice() == b"ObjStm" || n.as_slice() == b"XRef")
            }))
        })
        .map(|(num, _)| num)
        .expect("the fixture has a content stream")
}

/// SL-1.ENC.03 read/write interop for the `/Adobe.PPKLite` handler: an
/// incremental save into a public-key-encrypted fixture re-encrypts the
/// changed stream under the SAME file key, so the unchanged `/Recipients`
/// blobs still open it with the recipient private key. Both the RSA s4
/// (AESV2) and EC s5 (AESV3) shapes are covered.
#[test]
fn public_key_incremental_save_roundtrips() {
    fn run(label: &str, pdf: &[u8], key_der: &[u8], ec: bool) {
        let b = budget();
        let credential = if ec {
            selis_crypto::pkcs7::PubKeyCredential::ec_p256(key_der.to_vec())
        } else {
            selis_crypto::pkcs7::PubKeyCredential::rsa(key_der.to_vec())
        };
        let mut g = guard(&b);
        let (key, auth) = open_encrypted_public_key(pdf, &credential, &b, &mut g)
            .unwrap_or_else(|e| panic!("{label}: public-key open failed: {e:?}"))
            .unwrap_or_else(|| panic!("{label}: fixture is encrypted"));
        assert_eq!(key.info.handler, selis_pdf_cos::encrypt::Handler::PubKey);
        assert!(!key.file_key.is_empty(), "{label}: unwrapped a file key");
        let _ = auth; // the receipt is the caller's to enforce (SL-1.ENC.09)

        let model = materialise_decrypted(pdf, &key, &b, &mut g);
        let stream_num = first_content_stream(&model);
        let marker: &[u8] = b"% EDIT06 PUBLIC-KEY MARKER\n";
        let replacement = match model.get(&stream_num).expect("stream object") {
            Obj::Stream { dict, .. } => Obj::Stream {
                dict: dict.clone(),
                data: Bytes::copy_from_slice(marker),
            },
            other => panic!("{label}: not a stream: {other:?}"),
        };
        let mutations = vec![ObjectMutation::SetObject {
            num: stream_num,
            value: replacement,
        }];

        let (out, report) = save_incremental_encrypted(pdf, &model, &mutations, &key, &b, &mut g)
            .unwrap_or_else(|e| panic!("{label}: public-key save failed: {e:?}"));
        assert!(
            report.prefix_preserved,
            "{label}: original bytes are the prefix"
        );
        assert_eq!(report.objects_changed, 1, "{label}: one object changed");
        let appended = &out[pdf.len()..];
        assert!(
            !appended.windows(marker.len()).any(|w| w == marker),
            "{label}: the marker must be ciphertext in the appended revision"
        );

        let mut g2 = guard(&b);
        let (out_key, _) = open_encrypted_public_key(&out, &credential, &b, &mut g2)
            .unwrap_or_else(|e| panic!("{label}: reopen failed: {e:?}"))
            .expect("output is still public-key encrypted");
        let reopened = materialise_decrypted(&out, &out_key, &b, &mut g2);
        match reopened.get(&stream_num).expect("stream survived") {
            Obj::Stream { data, .. } => {
                assert_eq!(
                    data.as_slice(),
                    marker,
                    "{label}: edit decrypts for the recipient"
                )
            }
            other => panic!("{label}: not a stream on reopen: {other:?}"),
        }
    }

    run(
        "RSA s4 / AESV2",
        include_bytes!("../../selis-pdf-engine/tests/fixtures/pubkey-rsa-s4.pdf"),
        include_bytes!("../../selis-pdf-engine/tests/fixtures/enc03-recipient-rsa2048.pk8.der"),
        false,
    );
    run(
        "EC s5 / AESV3",
        include_bytes!("../../selis-pdf-engine/tests/fixtures/pubkey-ec-s5.pdf"),
        include_bytes!("../../selis-pdf-engine/tests/fixtures/enc03-recipient-ecp256.pk8.der"),
        true,
    );
}

/// The public-key opener refuses a standard-handler `/Encrypt` with a typed,
/// precise error — the caller must use the password path instead.
#[test]
fn public_key_open_refuses_standard_handler() {
    let b = budget();
    let src = encrypted_base(HandlerCase::AesV3);
    let mut g = guard(&b);
    let credential = selis_crypto::pkcs7::PubKeyCredential::rsa(vec![0u8; 8]);
    let e = open_encrypted_public_key(&src, &credential, &b, &mut g)
        .expect_err("a password handler is not a public-key document");
    assert_eq!(e.code(), selis_error::Code::EncryptUnsupported);
    assert!(
        e.ctx()
            .detail
            .as_deref()
            .unwrap_or("")
            .contains("standard handler"),
        "{e:?}"
    );
}

/// qpdf interop oracle: every output must pass `--check` and decrypt with the
/// empty user password. Skipped with a note when qpdf is not installed.
#[test]
fn qpdf_interop_oracle() {
    fn qpdf_path() -> Option<std::path::PathBuf> {
        if let Ok(p) = std::env::var("QPDF") {
            let p = std::path::PathBuf::from(p);
            if p.is_file() {
                return Some(p);
            }
        }
        for candidate in [
            r"E:\softwares\pdf-tools\qpdf\qpdf.exe",
            r"C:\Program Files\qpdf 12.4.2\bin\qpdf.exe",
        ] {
            let p = std::path::PathBuf::from(candidate);
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }

    let Some(qpdf) = qpdf_path() else {
        eprintln!("qpdf not found; interop oracle skipped (set QPDF to its path)");
        return;
    };
    for case in HandlerCase::ALL {
        let b = budget();
        let src = encrypted_base(case);
        let mut g = guard(&b);
        let key = open_encrypted(&src, b"", EncryptionPolicy::KeepExisting, &b, &mut g)
            .expect("auth")
            .expect("key");
        let model = materialise_decrypted(&src, &key, &b, &mut g);
        let info_num = object_with_title(&model, b"ORIGINAL_TITLE");
        let mutations = vec![ObjectMutation::SetObject {
            num: info_num,
            value: expected_title(TITLE_MARKER),
        }];
        let (out, _) =
            save_incremental_encrypted(&src, &model, &mutations, &key, &b, &mut g).expect("save");

        let dir = std::env::temp_dir();
        let pdf = dir.join(format!(
            "selis-edit06-{}.pdf",
            case.label().replace(' ', "-")
        ));
        let dec = dir.join(format!(
            "selis-edit06-{}-dec.pdf",
            case.label().replace(' ', "-")
        ));
        std::fs::write(&pdf, &out).expect("write temp PDF");

        let checked = std::process::Command::new(&qpdf)
            .arg("--check")
            .arg("--password=")
            .arg(&pdf)
            .output()
            .expect("run qpdf --check");
        assert!(
            checked.status.success(),
            "{}: qpdf --check failed: {}",
            case.label(),
            String::from_utf8_lossy(&checked.stderr)
        );

        let decrypted = std::process::Command::new(&qpdf)
            .arg("--password=")
            .arg("--decrypt")
            .arg(&pdf)
            .arg(&dec)
            .output()
            .expect("run qpdf --decrypt");
        assert!(
            decrypted.status.success(),
            "{}: qpdf --decrypt failed: {}",
            case.label(),
            String::from_utf8_lossy(&decrypted.stderr)
        );
        let plain = std::fs::read(&dec).expect("read decrypted PDF");
        assert!(
            plain
                .windows(TITLE_MARKER.len())
                .any(|window| window == TITLE_MARKER),
            "{}: qpdf's decrypted output carries the edited title",
            case.label()
        );
        let _ = std::fs::remove_file(&pdf);
        let _ = std::fs::remove_file(&dec);
    }
}
