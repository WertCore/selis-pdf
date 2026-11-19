//! SL-5.SIGN.04 end-to-end consent evidence for the CLI binding layer.
//!
//! The engine crates are network-free; these tests exercise the only place a
//! socket is opened. They prove three things:
//!
//! 1. plain `selis sign` opens no connection and prints no consent notice;
//! 2. `--connect-tsa` with no configured URL is a typed refusal before any
//!    network call;
//! 3. `--connect-tsa --tsa-url <local mock>` prints the consent line verbatim,
//!    sends only a 32-byte hash, and embeds the returned token as `/DSS /TS`
//!    while the original signature keeps verifying.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation
)]

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn selis() -> Command {
    Command::new(env!("CARGO_BIN_EXE_selis"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/selis-pdf-sign/tests/fixtures")
        .join(name)
}

fn temp(name: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("selis-tsa-{}-{n}-{name}", std::process::id()))
}

fn one_page_pdf(path: &PathBuf) {
    let objects: Vec<Vec<u8>> = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>\nendobj\n".to_vec(),
        b"4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n".to_vec(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for object in &objects {
        offsets.push(out.len());
        out.extend_from_slice(object);
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 5\n0000000000 65535 f \n").as_bytes());
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    std::fs::write(path, out).unwrap();
}

fn sign_args(input: &PathBuf, output: &PathBuf) -> Command {
    let mut command = selis();
    command
        .arg("sign")
        .arg(input)
        .arg("--output")
        .arg(output)
        .arg("--key")
        .arg(fixture("leaf.key"))
        .arg("--cert")
        .arg(fixture("leaf.crt"))
        .arg("--cert")
        .arg(fixture("root.crt"))
        .arg("--field")
        .arg("Signature1")
        .arg("--time")
        .arg("20261104120000Z");
    command
}

fn token(imprint: &[u8]) -> Vec<u8> {
    use selis_crypto::der_write::{
        explicit, integer, null, octet_string, oid, sequence, set, tlv, GENERALIZED_TIME,
    };
    let algorithm = sequence(&[oid(&[2, 16, 840, 1, 101, 3, 4, 2, 1]), null()]);
    let mi = sequence(&[algorithm, octet_string(imprint)]);
    let tst = sequence(&[
        integer(1),
        oid(&[1, 3, 6, 1, 4, 1, 99_999, 1]),
        mi,
        integer(9),
        tlv(GENERALIZED_TIME, b"20261104120000Z"),
    ]);
    let encap = sequence(&[
        oid(&[1, 2, 840, 113_549, 1, 9, 16, 1, 4]),
        explicit(0, &octet_string(&tst)),
    ]);
    let signed = sequence(&[integer(3), set(&[]), encap]);
    sequence(&[oid(&[1, 2, 840, 113_549, 1, 7, 2]), explicit(0, &signed)])
}

/// A one-shot local TSA that echoes the request's SHA-256 imprint back in a
/// structurally valid token. Records whether it was contacted.
fn mock_tsa(tx: mpsc::Sender<Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        stream.read_to_end(&mut request).unwrap();
        let _ = tx.send(request.clone());
        // The request contains the 32-byte hashedMessage after an 04 20 tag.
        let at = request.windows(2).position(|w| w == [0x04, 0x20]).unwrap() + 2;
        let imprint = request.get(at..at + 32).unwrap().to_vec();
        let body = token(&imprint);
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/timestamp-reply\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
    });
    format!("http://{address}/tsa")
}

#[test]
fn plain_sign_never_connects_and_prints_no_consent_notice() {
    let input = temp("in.pdf");
    let output = temp("out.pdf");
    one_page_pdf(&input);
    let (tx, rx) = mpsc::channel();
    let url = mock_tsa(tx);

    let mut command = sign_args(&input, &output);
    command.env("SELIS_TSA_URL", &url);
    let run = command.output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        rx.try_recv().is_err(),
        "no HTTP request was made without --connect-tsa"
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        !stderr.contains("Connecting to external time provider"),
        "{stderr}"
    );
    let pdf = std::fs::read(&output).unwrap();
    let text = String::from_utf8_lossy(&pdf);
    assert!(!text.contains("/DSS"), "offline signing writes no DSS");
    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}

#[test]
fn connect_tsa_without_a_url_is_a_typed_refusal() {
    let input = temp("in.pdf");
    let output = temp("out.pdf");
    one_page_pdf(&input);
    let mut command = sign_args(&input, &output);
    command.arg("--connect-tsa");
    command.env_remove("SELIS_TSA_URL");
    let run = command.output().unwrap();
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(stderr.contains("SELIS_TSA_URL"), "{stderr}");
    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}

#[test]
fn consented_sign_sends_a_hash_and_embeds_the_token() {
    let input = temp("in.pdf");
    let output = temp("out.pdf");
    one_page_pdf(&input);
    let (tx, rx) = mpsc::channel();
    let url = mock_tsa(tx);

    let run = sign_args(&input, &output)
        .arg("--connect-tsa")
        .arg("--tsa-url")
        .arg(&url)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let stderr = String::from_utf8_lossy(&run.stderr);
    let expected = format!("Connecting to external time provider {url} to timestamp this signature; a hash of the document will be sent. Continue?");
    assert!(
        stderr.contains(&expected),
        "consent line missing from {stderr}"
    );

    let request = rx.recv().unwrap();
    let request_text = String::from_utf8_lossy(&request);
    assert!(request_text.contains("application/timestamp-query"));
    assert!(
        !request_text.contains("Ada Lovelace"),
        "no document bytes were sent"
    );

    let pdf = std::fs::read(&output).unwrap();
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("/DSS"), "the DSS was embedded");
    assert!(
        text.contains("/ETSI.RFC3161"),
        "the RFC 3161 token was embedded"
    );
    assert!(text.contains("/TS"), "the timestamp store is present");

    // The original signature still verifies after the DSS revision.
    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let mut g = budget.guard();
    let report = selis_pdf_sign::signatures(&pdf, &[], 1_793_793_600, &budget, &mut g).unwrap();
    assert_eq!(report.signatures.len(), 1);
    assert!(
        report.signatures[0].verified,
        "signature survived DSS embedding"
    );
    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}

#[test]
fn tsa_hash_is_sha256_of_the_signature_not_the_document() {
    // Documents the exact ADR-P0017 claim with a direct computation: the mock
    // TSA echoes back the imprint it received, so the embedded token's
    // messageImprint must equal SHA-256 of the signature's /Contents.
    let input = temp("in.pdf");
    let output = temp("out.pdf");
    one_page_pdf(&input);
    let (tx, rx) = mpsc::channel();
    let url = mock_tsa(tx);
    let run = sign_args(&input, &output)
        .arg("--connect-tsa")
        .arg("--tsa-url")
        .arg(&url)
        .output()
        .unwrap();
    assert!(run.status.success());
    let request = rx.recv().unwrap();
    let at = request.windows(2).position(|w| w == [0x04, 0x20]).unwrap() + 2;
    let sent_hash = request.get(at..at + 32).unwrap();

    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Viewer);
    let mut g = budget.guard();
    let signed_only = temp("signed-only.pdf");
    let offline = sign_args(&input, &signed_only).output().unwrap();
    assert!(offline.status.success());
    let pdf = std::fs::read(&signed_only).unwrap();
    let cms = selis_pdf_sign::signature_contents(&pdf, "Signature1", &budget, &mut g).unwrap();
    assert_eq!(Sha256::digest(&cms).as_slice(), sent_hash);

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    let _ = std::fs::remove_file(&signed_only);
}
