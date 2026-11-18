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
    encrypted_save_key_from_parts, materialise, open_encrypted, save_incremental,
    save_incremental_encrypted, EncryptedSaveKey, EncryptionPolicy, ObjectModel, ObjectMutation,
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

    // No consent: refused as a policy/consent error before any key work.
    let no_consent = open_encrypted(
        &src,
        b"",
        EncryptionPolicy::UpgradeToAesV3 {
            user_consent: false,
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

    // With consent: still refused, honestly, because an incremental save
    // cannot re-key the untouched prefix. The message names the rewrite path.
    let consent = open_encrypted(
        &src,
        b"",
        EncryptionPolicy::UpgradeToAesV3 { user_consent: true },
        &b,
        &mut g,
    )
    .expect_err("consent does not make an incremental re-key possible");
    assert_eq!(consent.code(), selis_error::Code::EncryptUnsupported);
    let detail = consent.ctx().detail.as_deref().unwrap_or("").to_string();
    assert!(detail.contains("not implemented"), "{detail}");
    assert!(detail.contains("save_rewritten"), "{detail}");

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
        EncryptionPolicy::UpgradeToAesV3 { user_consent: true },
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
        let candidate = std::path::PathBuf::from(r"C:\Program Files\qpdf 12.4.2\bin\qpdf.exe");
        candidate.is_file().then_some(candidate)
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
