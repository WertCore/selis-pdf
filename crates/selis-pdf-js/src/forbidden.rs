//! Static deny-scan for Acrobat APIs that are intentionally absent.
//!
//! This is a **diagnostic aid**, not the security boundary. It turns a script
//! that names `app.launchURL` into a typed `JS_API_REFUSED` before execution.
//! Even a spelling the tokenizer misses is contained at runtime: the API was
//! never installed, and the embedded engine has no host capability to reach.

/// Identifiers that must never appear in a form script.
///
/// Every name here is either an Acrobat API outside the calculation/format
/// whitelist (ADR-P0020) or a host capability the sandbox does not provide.
pub const FORBIDDEN_ACROBAT_APIS: &[&str] = &[
    "app",
    "launchURL",
    "Net",
    "Collab",
    "submitForm",
    "exportDataObject",
    "importDataObject",
    "getURL",
    "require",
    "process",
    "WebAssembly",
    "fetch",
    "XMLHttpRequest",
    "WebSocket",
    "indexedDB",
    "importScripts",
    "postMessage",
    "Worker",
];

/// Return the first forbidden identifier found as a whole token, if any.
///
/// The scan is intentionally conservative: it tokenises ASCII identifier
/// characters and ignores string/comment context, so it may refuse a benign
/// script that mentions a name in prose. Safety favours the false positive.
#[must_use]
pub fn find_forbidden_api(script: &str) -> Option<&'static str> {
    let bytes = script.as_bytes();
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        if is_ident_byte(b) {
            let start = i;
            while let Some(&c) = bytes.get(i) {
                if is_ident_byte(c) {
                    i = i.saturating_add(1);
                } else {
                    break;
                }
            }
            if let Some(token) = script.get(start..i) {
                if let Some(found) = FORBIDDEN_ACROBAT_APIS
                    .iter()
                    .find(|candidate| **candidate == token)
                {
                    return Some(found);
                }
            }
        } else {
            i = i.saturating_add(1);
        }
    }
    None
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catches_dangerous_apis_as_whole_tokens() {
        for (src, expected) in [
            ("app.launchURL('x')", "app"),
            ("this.submitForm({cURL:'x'})", "submitForm"),
            ("this.exportDataObject({n:'x'})", "exportDataObject"),
            ("Net.HTTP.request({})", "Net"),
            ("Collab.getIcon('x')", "Collab"),
            ("fetch('https://evil')", "fetch"),
        ] {
            assert_eq!(find_forbidden_api(src), Some(expected), "{src}");
        }
    }

    #[test]
    fn does_not_match_substrings_or_clean_scripts() {
        assert_eq!(find_forbidden_api("application.value = 1"), None);
        assert_eq!(
            find_forbidden_api("event.value = this.getField('Subtotal').value * 2;"),
            None
        );
        assert_eq!(find_forbidden_api("var total = 0;"), None);
    }
}
