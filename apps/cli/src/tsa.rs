//! Consent-gated external time / revocation providers (SL-5.SIGN.04).
//!
//! This file is the **only** place in the workspace that opens a socket for
//! signing. The engines (`selis-pdf-sign`, `selis-pdf-edit`, `selis-crypto`) are
//! network-free. Nothing here runs unless the user passes `--connect-tsa`
//! (and `--connect-aia`) and a URL is configured by `SELIS_TSA_URL` /
//! `SELIS_OCSP_URL`. There is no hardcoded third party.
//!
//! Before any connection the exact consent line is printed to **stderr**:
//!
//! > Connecting to external time provider &lt;URL&gt; to timestamp this
//! > signature; a hash of the document will be sent. Continue?
//!
//! Only a **hash of the signature** is sent to the TSA (ADR-P0017): no PDF
//! bytes, no key material, no document text leaves the device. The returned
//! RFC 3161 token is handed to the engine's pure `embed_dss` for local
//! validation and embedding.
//!
//! # Honest limits
//!
//! * `http://` only. HTTPS needs a TLS stack that is deliberately not linked
//!   into this binary; a `https://` URL is refused before connecting.
//! * OCSP/CRL fetching is from an explicitly **configured responder URL**, not
//!   from the certificate's AIA extension: `selis-crypto` has no X.509
//!   extension reader, so auto-discovering an arbitrary URL from a document
//!   would be an unaudited SSRF surface. `--connect-aia` is the opt-in and the
//!   URL must still come from the environment.
//! * A token's TSA signature/trust chain is **not** verified here; the engine
//!   parses its structure and imprint binding and says so honestly.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::CliError;

/// The consent line, with `{url}` replaced by the configured provider.
pub(crate) const CONSENT_LINE: &str = "Connecting to external time provider {url} to timestamp this signature; a hash of the document will be sent. Continue?";

/// The revocation-data consent line (same rule: hash only, opt-in only).
pub(crate) const AIA_CONSENT_LINE: &str = "Connecting to external time provider {url} to fetch revocation data for this signature; an identifier derived from the certificate will be sent. Continue?";

/// The environment variable that configures the RFC 3161 TSA URL.
pub(crate) const TSA_URL_ENV: &str = "SELIS_TSA_URL";
/// The environment variable that configures the OCSP/CRL responder URL.
pub(crate) const AIA_URL_ENV: &str = "SELIS_OCSP_URL";
/// Largest provider response accepted, as a refusal bound.
const MAX_RESPONSE: usize = 4 << 20;
/// SHA-256 object identifier (RFC 5754).
const OID_SHA256: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 2, 1];

/// The user's explicit, revocable consent for one network operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NetworkConsent<'a> {
    /// `--connect-tsa` / `--connect-aia`: without this, no socket is opened.
    pub connect: bool,
    /// An optional per-invocation URL override; otherwise the env var is used.
    pub url: Option<&'a str>,
}

/// Typed provider refusals. Every one of these happens before or without a
/// network connection, except `Io`/`Http` which name a real transport failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TsaError {
    /// The user did not opt in. Default state: no network.
    ConsentRequired,
    /// Opt-in was given but no URL was configured.
    UrlRequired(&'static str),
    /// Only `http://` is supported; `https://` is refused before connecting.
    UnsupportedScheme(String),
    /// The URL could not be parsed as `http://host[:port]/path`.
    InvalidUrl(String),
    /// Socket read/write failed.
    Io(String),
    /// The provider returned a non-200 or unsupported HTTP shape.
    Http(String),
    /// The returned bytes are not a parseable RFC 3161 token.
    InvalidToken(String),
    /// The token's messageImprint does not bind to the local hash.
    ImprintMismatch,
}

impl std::fmt::Display for TsaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConsentRequired => f.write_str(
                "external time/revocation services are disabled; pass --connect-tsa (or \
                 --connect-aia) after reading the consent notice to allow a network connection",
            ),
            Self::UrlRequired(env) => {
                write!(f, "a provider URL is required; set {env} or pass --tsa-url")
            }
            Self::UnsupportedScheme(url) => write!(
                f,
                "the provider URL {url:?} is not http://; this build has no TLS stack and will \
                 not connect"
            ),
            Self::InvalidUrl(detail) => write!(f, "invalid provider URL: {detail}"),
            Self::Io(detail) => write!(f, "provider connection failed: {detail}"),
            Self::Http(detail) => write!(f, "provider returned an unusable response: {detail}"),
            Self::InvalidToken(detail) => {
                write!(f, "the time provider returned no valid token: {detail}")
            }
            Self::ImprintMismatch => f.write_str(
                "the time provider's token does not attest to the hash of this signature",
            ),
        }
    }
}

impl std::error::Error for TsaError {}

impl From<TsaError> for CliError {
    fn from(error: TsaError) -> Self {
        CliError(error.to_string())
    }
}

/// The consent line for a URL, printed verbatim before connecting.
#[must_use]
pub(crate) fn consent_line(url: &str) -> String {
    CONSENT_LINE.replace("{url}", url)
}

/// The revocation consent line for a URL.
#[must_use]
pub(crate) fn aia_consent_line(url: &str) -> String {
    AIA_CONSENT_LINE.replace("{url}", url)
}

/// Resolve the configured URL. No network is touched here.
fn configured_url(consent: &NetworkConsent<'_>, env: &'static str) -> Result<String, TsaError> {
    if !consent.connect {
        return Err(TsaError::ConsentRequired);
    }
    if let Some(url) = consent.url {
        return Ok(url.to_string());
    }
    std::env::var(env)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(TsaError::UrlRequired(env))
}

/// An `http://` URL split into connect host, port and request path.
struct ParsedUrl {
    host: String,
    port: u16,
    path: String,
}

fn parse_http_url(url: &str) -> Result<ParsedUrl, TsaError> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        if url.starts_with("https://") {
            TsaError::UnsupportedScheme(url.to_string())
        } else {
            TsaError::InvalidUrl("expected an http:// URL".to_string())
        }
    })?;
    let (authority, path) = match rest.find('/') {
        Some(at) => (
            rest.get(..at)
                .ok_or_else(|| TsaError::InvalidUrl("bad authority".to_string()))?,
            rest.get(at..)
                .ok_or_else(|| TsaError::InvalidUrl("bad path".to_string()))?,
        ),
        None => (rest, "/"),
    };
    if authority.is_empty() || authority.contains('@') {
        return Err(TsaError::InvalidUrl(
            "empty or userinfo-bearing authority".to_string(),
        ));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host,
            port.parse::<u16>()
                .map_err(|_| TsaError::InvalidUrl("bad port".to_string()))?,
        ),
        None => (authority, 80u16),
    };
    if host.is_empty() {
        return Err(TsaError::InvalidUrl("empty host".to_string()));
    }
    Ok(ParsedUrl {
        host: host.to_string(),
        port,
        path: path.to_string(),
    })
}

/// A tiny HTTP/1.1 client: POST/GET one DER body, read one response.
fn exchange(
    url: &ParsedUrl,
    method: &str,
    content_type: Option<&str>,
    body: &[u8],
) -> Result<Vec<u8>, TsaError> {
    let address = (url.host.as_str(), url.port)
        .to_socket_addrs()
        .map_err(|e| TsaError::Io(format!("cannot resolve {}: {e}", url.host)))?
        .next()
        .ok_or_else(|| TsaError::Io(format!("{} has no address", url.host)))?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))
        .map_err(|e| TsaError::Io(e.to_string()))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

    let mut request = format!(
        "{method} {} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n",
        url.path, url.host, url.port
    )
    .into_bytes();
    if let Some(kind) = content_type {
        request.extend_from_slice(format!("Content-Type: {kind}\r\n").as_bytes());
    }
    request.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    request.extend_from_slice(body);
    stream
        .write_all(&request)
        .map_err(|e| TsaError::Io(e.to_string()))?;
    // `Connection: close`: a half-close tells the peer the request is complete
    // without dropping our read half, so a one-shot server can answer.
    let _ = stream.shutdown(Shutdown::Write);

    let mut response = Vec::new();
    stream
        .take(u64::try_from(MAX_RESPONSE).unwrap_or(u64::MAX))
        .read_to_end(&mut response)
        .map_err(|e| TsaError::Io(e.to_string()))?;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| TsaError::Http("no header/body separator".to_string()))?;
    let headers = String::from_utf8_lossy(response.get(..split).unwrap_or(&[]));
    let status_ok = headers
        .lines()
        .next()
        .is_some_and(|line| line.contains(" 200 ") || line.ends_with(" 200"));
    if !status_ok {
        return Err(TsaError::Http(
            headers
                .lines()
                .next()
                .unwrap_or("no status line")
                .to_string(),
        ));
    }
    if headers.lines().any(|line| {
        line.to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
    }) {
        return Err(TsaError::Http(
            "chunked responses are not supported".to_string(),
        ));
    }
    Ok(response
        .get(split.saturating_add(4)..)
        .unwrap_or(&[])
        .to_vec())
}

/// Build a minimal RFC 3161 `TimeStampReq` for a 32-byte SHA-256 imprint.
fn timestamp_request(hash: &[u8]) -> Vec<u8> {
    use selis_crypto::der_write::{explicit, integer, null, octet_string, oid, sequence};
    let algorithm = sequence(&[oid(OID_SHA256), null()]);
    let imprint = sequence(&[algorithm, octet_string(hash)]);
    // version, messageImprint, [2] IMPLICIT certReq TRUE.
    sequence(&[integer(1), imprint, explicit(2, &[0xFF])])
}

/// Ask a consented RFC 3161 TSA to timestamp the hash of a signature.
///
/// `emit` receives the consent line before the socket is opened; tests capture
/// it to prove the user is told. The CLI passes a stderr writer.
pub(crate) fn timestamp_signature(
    signature_cms: &[u8],
    consent: &NetworkConsent<'_>,
    emit: &mut dyn FnMut(&str),
) -> Result<Vec<u8>, TsaError> {
    use sha2::{Digest, Sha256};
    let url = configured_url(consent, TSA_URL_ENV)?;
    let parsed = parse_http_url(&url)?;

    // The consent notice is printed BEFORE any connection attempt.
    emit(&consent_line(&url));

    let hash = Sha256::digest(signature_cms);
    let request = timestamp_request(&hash);
    let response = exchange(
        &parsed,
        "POST",
        Some("application/timestamp-query"),
        &request,
    )?;

    let budget = selis_sandbox::Budget::profile(selis_sandbox::Surface::Fuzz);
    let mut g = budget.guard();
    let token = selis_crypto::timestamp::parse_rfc3161_token(&response, &mut g)
        .map_err(|e| TsaError::InvalidToken(e.to_string()))?;
    if token.imprint != hash.as_slice() {
        return Err(TsaError::ImprintMismatch);
    }
    Ok(response)
}

/// Revocation data fetched from a consented, configured responder URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LtvData {
    /// An OCSP response (DER) to embed in the DSS `/OCSP` array.
    pub ocsp: Vec<u8>,
}

/// Ask a consented responder for an OCSP response.
///
/// Only the configured URL is contacted; the certificate's AIA extension is
/// not parsed (honest gap, see module docs). A DER structure check is done so
/// an HTML error page is never embedded as "OCSP data".
pub(crate) fn fetch_revocation(
    consent: &NetworkConsent<'_>,
    emit: &mut dyn FnMut(&str),
) -> Result<LtvData, TsaError> {
    let url = configured_url(consent, AIA_URL_ENV)?;
    let parsed = parse_http_url(&url)?;
    emit(&aia_consent_line(&url));
    let response = exchange(&parsed, "GET", None, &[])?;
    if response.first() != Some(&0x30) {
        return Err(TsaError::InvalidToken(
            "the responder did not return DER (an ASN.1 SEQUENCE was expected)".to_string(),
        ));
    }
    Ok(LtvData { ocsp: response })
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

    use super::{
        aia_consent_line, consent_line, fetch_revocation, timestamp_signature, NetworkConsent,
        TsaError, AIA_CONSENT_LINE, CONSENT_LINE,
    };
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn token(imprint: &[u8]) -> Vec<u8> {
        use selis_crypto::der_write::{
            explicit, integer, octet_string, oid, sequence, set, tlv, GENERALIZED_TIME,
        };
        let alg = sequence(&[oid(&[2, 16, 840, 1, 101, 3, 4, 2, 1])]);
        let mi = sequence(&[alg, octet_string(imprint)]);
        let tst = sequence(&[
            integer(1),
            oid(&[1, 3, 6, 1, 4, 1, 99_999, 1]),
            mi,
            integer(1),
            tlv(GENERALIZED_TIME, b"20261104120000Z"),
        ]);
        let encap = sequence(&[
            oid(&[1, 2, 840, 113_549, 1, 9, 16, 1, 4]),
            explicit(0, &octet_string(&tst)),
        ]);
        let signed = sequence(&[integer(3), set(&[]), encap]);
        sequence(&[oid(&[1, 2, 840, 113_549, 1, 7, 2]), explicit(0, &signed)])
    }

    /// Start a one-shot local HTTP server. The handler receives the raw
    /// request and chooses the response bytes.
    fn mock_server(response: Vec<u8>, capture: std::sync::mpsc::Sender<Vec<u8>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local listener");
        let address = listener.local_addr().expect("a local address");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("one connection");
            let mut request = Vec::new();
            stream.read_to_end(&mut request).expect("the request");
            let _ = capture.send(request);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/timestamp-reply\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            );
            stream.write_all(header.as_bytes()).expect("header");
            stream.write_all(&response).expect("body");
        });
        format!("http://{address}/tsa")
    }

    #[test]
    fn default_without_consent_makes_no_connection() {
        let mut printed = String::new();
        let result = timestamp_signature(
            b"signature-bytes",
            &NetworkConsent {
                connect: false,
                url: Some("http://127.0.0.1:1/tsa"),
            },
            &mut |line| printed.push_str(line),
        );
        assert_eq!(result, Err(TsaError::ConsentRequired));
        assert!(printed.is_empty(), "no consent line without opt-in");
    }

    #[test]
    fn opt_in_without_a_url_is_refused() {
        let result = timestamp_signature(
            b"signature-bytes",
            &NetworkConsent {
                connect: true,
                url: None,
            },
            &mut |_| {},
        );
        assert_eq!(result, Err(TsaError::UrlRequired(super::TSA_URL_ENV)));
    }

    #[test]
    fn https_is_refused_before_connecting() {
        let result = timestamp_signature(
            b"signature-bytes",
            &NetworkConsent {
                connect: true,
                url: Some("https://example.test/tsa"),
            },
            &mut |_| {},
        );
        assert!(matches!(result, Err(TsaError::UnsupportedScheme(_))));
    }

    #[test]
    fn consented_request_sends_only_the_hash_and_embeds_the_token() {
        let signature = b"the signature CMS blob that must not leave the device";
        let hash = Sha256::digest(signature);
        let (tx, rx) = std::sync::mpsc::channel();
        let url = mock_server(token(&hash), tx);
        let mut printed = Vec::new();
        let returned = timestamp_signature(
            signature,
            &NetworkConsent {
                connect: true,
                url: Some(&url),
            },
            &mut |line| printed.push(line.to_string()),
        )
        .expect("a token comes back");

        assert_eq!(returned, token(&hash));
        let request = rx.recv().expect("the mock server saw a request");
        let request_text = String::from_utf8_lossy(&request);
        assert!(request_text.starts_with("POST /tsa HTTP/1.1"));
        assert!(request_text.contains("application/timestamp-query"));
        // The signature itself and its bytes never appear in the request.
        assert!(!request_text.contains("the signature CMS blob"));
        // ...but the 32-byte hash does (DER-encoded in the ASN.1 request).
        assert!(
            request
                .windows(hash.len())
                .any(|window| window == hash.as_slice()),
            "the hash of the signature was sent to the TSA"
        );
        assert!(
            rx.try_recv().is_err(),
            "one-shot server handled exactly one request"
        );

        assert_eq!(printed.len(), 1);
        assert_eq!(printed[0], CONSENT_LINE.replace("{url}", &url));
    }

    #[test]
    fn the_consent_messages_name_the_provider_verbatim() {
        assert_eq!(
            consent_line("http://tsa.example/"),
            "Connecting to external time provider http://tsa.example/ to timestamp this signature; a hash of the document will be sent. Continue?"
        );
        assert!(AIA_CONSENT_LINE.contains("{url}"));
        assert_eq!(
            aia_consent_line("http://ocsp.example/"),
            "Connecting to external time provider http://ocsp.example/ to fetch revocation data for this signature; an identifier derived from the certificate will be sent. Continue?"
        );
    }

    #[test]
    fn a_foreign_imprint_token_is_refused() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let url = mock_server(token(&[0x77u8; 32]), tx);
        let result = timestamp_signature(
            b"some signature",
            &NetworkConsent {
                connect: true,
                url: Some(&url),
            },
            &mut |_| {},
        );
        assert_eq!(result, Err(TsaError::ImprintMismatch));
    }

    #[test]
    fn revocation_fetch_is_also_consent_gated_and_checks_der() {
        let mut printed = String::new();
        let result = fetch_revocation(
            &NetworkConsent {
                connect: false,
                url: Some("http://127.0.0.1:1/ocsp"),
            },
            &mut |line| printed.push_str(line),
        );
        assert_eq!(result, Err(TsaError::ConsentRequired));
        assert!(printed.is_empty());

        let (tx, _rx) = std::sync::mpsc::channel();
        let url = mock_server(vec![0x30, 0x03, 0x02, 0x01, 0x01], tx);
        let data = fetch_revocation(
            &NetworkConsent {
                connect: true,
                url: Some(&url),
            },
            &mut |line| printed.push_str(line),
        )
        .expect("DER comes back");
        assert_eq!(data.ocsp, vec![0x30, 0x03, 0x02, 0x01, 0x01]);
        assert!(printed.contains(&aia_consent_line(&url)));
    }

    #[test]
    fn a_plain_url_helper_rejects_non_http_schemes() {
        assert!(matches!(
            super::parse_http_url("ftp://example/x"),
            Err(TsaError::InvalidUrl(_))
        ));
        let parsed = super::parse_http_url("http://example.test:8080/a").expect("parses");
        assert_eq!(parsed.host, "example.test");
        assert_eq!(parsed.port, 8080);
        assert_eq!(parsed.path, "/a");
    }
}
