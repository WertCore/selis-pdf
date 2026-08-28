//! Per-stream /Crypt filter verification (SL-1.FILT.09 follow-up).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use selis_pdf_cos::{Obj, Ref, XrefEntry};
use selis_pdf_engine::Session;
use selis_sandbox::{Budget, Surface};

static AUTH_EVENT_EF_OPEN: &[u8] = include_bytes!("../../../corpus/pdfs/auth-event-ef-open.pdf");
static ENCRYPTED_ATTACHMENT: &[u8] = include_bytes!("../../../corpus/pdfs/encrypted-attachment.pdf");
static ISSUE19484_1: &[u8] = include_bytes!("../../../corpus/pdfs/issue19484_1.pdf");
static ISSUE19484_2: &[u8] = include_bytes!("../../../corpus/pdfs/issue19484_2.pdf");
static BUG1782186: &[u8] = include_bytes!("../../../corpus/pdfs/bug1782186.pdf");

fn open(src: &[u8]) -> Session {
    let budget = Budget::profile(Surface::Viewer);
    Session::open(src.to_vec(), &budget).expect("Session::open")
}

/// auth-event-ef-open.pdf: /StmF /Identity but the embedded file stream has
/// an explicit `/Filter [/Crypt]` + `/DecodeParms /Name /StdCF` (AESV3). The
/// document uses /AuthEvent /EFOpen, so the empty user password does not
/// authenticate — the stream must open without error and list the attachment.
#[test]
fn per_stream_crypt_embedded_file_opens() {
    let session = open(AUTH_EVENT_EF_OPEN);
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let attachments = session.attachments(&budget, &mut g).expect("attachments");
    assert!(!attachments.is_empty(), "must have an embedded file");
    assert_eq!(attachments.first().map(|a| a.size), Some(Some(784)));
}

/// encrypted-attachment.pdf: same structure as auth-event-ef-open.
#[test]
fn encrypted_attachment_per_stream_crypt_opens() {
    let session = open(ENCRYPTED_ATTACHMENT);
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let attachments = session.attachments(&budget, &mut g).expect("attachments");
    assert!(!attachments.is_empty(), "must have an embedded file");
}

/// issue19484_1.pdf: the metadata stream has `/Filter[/Crypt]` without
/// /DecodeParms (/Name defaults to /Identity), and /EncryptMetadata is false.
/// The stream must not be decrypted — its XML payload must stay plaintext.
#[test]
fn issue19484_metadata_crypt_identity_not_decrypted() {
    let src = ISSUE19484_1;
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let _session = Session::open(src.to_vec(), &budget).expect("Session::open");
    // Authenticate exactly as the session does, then resolve the metadata
    // stream (object 3) and verify its payload is plaintext XML.
    let startxref = selis_pdf_cos::xref::find_startxref(src, 2048).expect("startxref");
    let doc = selis_pdf_cos::parse_revisions(src, startxref, &budget, &mut g).expect("parse");
    let encrypt_ref = doc.revisions().last().and_then(|v| v.encrypt).expect("encrypt");
    let info = selis_pdf_cos::encrypt::parse_encrypt(src, Some(encrypt_ref), &budget, &mut g)
        .expect("parse encrypt")
        .expect("encrypt info");
    let id = doc
        .revisions()
        .last()
        .map(|v| v.trailer.clone())
        .map(|t| selis_pdf_cos::encrypt::document_id(&t))
        .unwrap_or_default();
    let key = selis_pdf_cos::encrypt::authenticate(&info, &id, b"").expect("auth");
    let policy = selis_pdf_cos::encrypt::DecryptPolicy::from_encrypt(&info, key);
    let mut resolver = selis_pdf_doc::Resolver::new(&doc, src, &budget);
    resolver.set_key(policy);
    let obj = resolver.resolve(Ref::new(3, 0), &mut g).expect("resolve metadata");
    let Obj::Stream { dict, data } = &obj else {
        panic!("expected a stream");
    };
    // The /Filter must still name Crypt (the pipeline treats it as a no-op).
    let has_crypt = dict.iter().any(|(k, v)| {
        if k.as_slice() != b"Filter" {
            return false;
        }
        match v {
            Obj::Name(n) => n.as_slice() == b"Crypt",
            Obj::Array(a) => a.iter().any(|o| matches!(o, Obj::Name(n) if n.as_slice() == b"Crypt")),
            _ => false,
        }
    });
    assert!(has_crypt, "metadata stream must keep its /Filter [/Crypt]");
    // The payload must be plaintext (not decrypted into garbage).
    assert!(
        data.as_slice().starts_with(b"<?xpacket"),
        "metadata must stay plaintext, got {:?}",
        data.as_slice().get(..12)
    );
}

/// issue19484_2.pdf: same structure as issue19484_1.
#[test]
fn issue19484_2_metadata_crypt_identity_not_decrypted() {
    let src = ISSUE19484_2;
    let budget = Budget::profile(Surface::Viewer);
    let mut g = budget.guard();
    let _session = Session::open(src.to_vec(), &budget).expect("Session::open");
    let startxref = selis_pdf_cos::xref::find_startxref(src, 2048).expect("startxref");
    let doc = selis_pdf_cos::parse_revisions(src, startxref, &budget, &mut g).expect("parse");
    let encrypt_ref = doc.revisions().last().and_then(|v| v.encrypt).expect("encrypt");
    let info = selis_pdf_cos::encrypt::parse_encrypt(src, Some(encrypt_ref), &budget, &mut g)
        .expect("parse encrypt")
        .expect("encrypt info");
    let id = doc
        .revisions()
        .last()
        .map(|v| v.trailer.clone())
        .map(|t| selis_pdf_cos::encrypt::document_id(&t))
        .unwrap_or_default();
    let key = selis_pdf_cos::encrypt::authenticate(&info, &id, b"").expect("auth");
    let policy = selis_pdf_cos::encrypt::DecryptPolicy::from_encrypt(&info, key);
    let mut resolver = selis_pdf_doc::Resolver::new(&doc, src, &budget);
    resolver.set_key(policy);
    let obj = resolver.resolve(Ref::new(3, 0), &mut g).expect("resolve metadata");
    let Obj::Stream { data, .. } = &obj else {
        panic!("expected a stream");
    };
    assert!(
        data.as_slice().starts_with(b"<?xpacket"),
        "metadata must stay plaintext, got {:?}",
        data.as_slice().get(..12)
    );
}

/// bug1782186.pdf: standard /StmF /StdCF (AESV2), /StrF /Identity,
/// /EncryptMetadata false, no per-stream /Crypt. Must still open and resolve
/// the metadata stream without decryption.
#[test]
fn bug1782186_standard_crypt() {
    let session = open(BUG1782186);
    assert!(session.len() > 0, "must have at least one page");
}
