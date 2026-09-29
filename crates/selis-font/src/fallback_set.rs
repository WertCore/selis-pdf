//! Runtime-resident standard-14 fallback faces.
//!
//! **The counterpart to the built-in subset.** `fallback_bytes` answers from
//! whatever is compiled into the module; this holds the faces that were not,
//! fetched on demand. The split is a *delivery* question only — which face a
//! name maps to is decided once, by `fallback::substitute`, and is the same on
//! every target. That separation is the whole reason desktop and web can ship
//! different bytes without ever disagreeing about which face a `/Times-Bold`
//! wants.
//!
//! ## Why `None` is never "this document has no standard-14 font"
//!
//! A caller that gets `None` from `fallback_bytes` cannot conclude anything
//! about the *document* any more. It means this particular face is not
//! resident. Answering that with `fallback::is_standard14` and then asking this
//! set for the face is the sequence, and getting it backwards is how a page
//! that uses Helvetica ends up painting nothing: the engine takes the
//! no-program path, which is correct for a document with no standard-14 font
//! and wrong for one whose standard-14 font simply has not arrived yet.
//!
//! ## Sizing
//!
//! A face is ~50 kB brotli, so a blocking fetch is imperceptible and this set
//! is expected to fill on first use rather than up front. Nothing here decides
//! *when* to fetch — that is the shell's, per ADR-P0043 §3.

use std::collections::BTreeMap;

use selis_bytes::Bytes;

/// The fallback faces this set can hold, addressed by the same
/// `family + style` name `fallback::substitute` produces.
#[derive(Debug, Clone, Default)]
pub struct FallbackFontSet {
    faces: BTreeMap<&'static str, Bytes>,
    /// Faces a caller asked for that are not here yet. The shell drains this to
    /// decide what to fetch, and clears what it successfully delivered.
    requested: std::collections::BTreeSet<&'static str>,
    /// Faces the payload does not carry at all, so a missing face is not
    /// re-requested forever. Distinct from `requested`: that is "not yet",
    /// this is "never".
    unavailable: std::collections::BTreeSet<&'static str>,
}

impl FallbackFontSet {
    /// A set holding nothing; every non-built-in face is absent.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes for a `family + style` face, or `None` when it is not resident.
    ///
    /// Only consulted for faces the build does not embed, so this never shadows
    /// the built-in subset.
    #[must_use]
    pub fn face(&self, name: &str) -> Option<&Bytes> {
        self.faces.get(name)
    }

    /// Whether a face is resident, without handing out the bytes.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.faces.contains_key(name)
    }

    /// The face names resident right now, sorted.
    #[must_use]
    pub fn loaded(&self) -> Vec<&'static str> {
        self.faces.keys().copied().collect()
    }

    /// Note that a face is wanted. Idempotent, and a face already present is
    /// not re-requested — a request for something already resident is a bug
    /// upstream, and honouring it would fetch the same font twice.
    pub fn request(&mut self, name: &'static str) {
        if self.faces.contains_key(name) || self.unavailable.contains(name) {
            return;
        }
        self.requested.insert(name);
    }

    /// The faces the shell still owes us, sorted.
    #[must_use]
    pub fn pending(&self) -> Vec<&'static str> {
        self.requested.iter().copied().collect()
    }

    /// Install a delivered face, clearing any request for it.
    ///
    /// Replacing an existing face is allowed and is what a payload update
    /// looks like; the caller has already verified the bytes against the
    /// manifest digest, which is the only thing that makes a font from the
    /// network as trustworthy as one that was compiled in.
    pub fn install(&mut self, name: &'static str, bytes: Bytes) {
        self.faces.insert(name, bytes);
        self.requested.remove(name);
        self.unavailable.remove(name);
    }

    /// Record that a face will never arrive, so it stops being requested.
    pub fn mark_unavailable(&mut self, name: &'static str) {
        self.faces.remove(name);
        self.requested.remove(name);
        self.unavailable.insert(name);
    }

    /// Whether a face is known to be unobtainable.
    #[must_use]
    pub fn is_unavailable(&self, name: &str) -> bool {
        self.unavailable.contains(name)
    }
}

/// An immutable view of the resident faces, taken for one render walk.
///
/// The mirror of `cjk::CjkSnapshot`, and for the same reason: the walk must
/// see a fixed set of bytes so that rendering the same page twice produces
/// identical pixels (ADR-P0012), and so a concurrent `provide` cannot change
/// the ground under a render in progress.
///
/// The request queue is deliberately **not** carried. A render is a pure
/// reader; it asks for nothing and mutates nothing, so handing it the queue
/// would only invite a write from a read-only path.
#[derive(Debug, Clone, Default)]
pub struct FallbackSnapshot {
    faces: BTreeMap<&'static str, Bytes>,
}

impl FallbackSnapshot {
    /// The bytes for a face, or `None` when it is not resident.
    #[must_use]
    pub fn face(&self, name: &str) -> Option<&Bytes> {
        self.faces.get(name)
    }

    /// Whether a face is resident right now.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.faces.contains_key(name)
    }
}

impl FallbackFontSet {
    /// An immutable view of the faces resident right now, for a render walk.
    #[must_use]
    pub fn snapshot(&self) -> FallbackSnapshot {
        FallbackSnapshot {
            faces: self.faces.clone(),
        }
    }
}

/// The font program to render a standard-14 name with: the built-in face when
/// this build embeds one, otherwise the delivered face.
///
/// **This is the whole delivery seam, and it is one function on purpose.** The
/// name a document asks for is decided by `fallback::substitute`, identically
/// on every target; *where the bytes come from* is the only thing that varies,
/// so it varies in exactly one place. Getting the order wrong is the bug worth
/// naming: the built-in subset must win, because it is already parsed, already
/// warm in the cache, and already what the previous build shipped. A delivered
/// full face for the same name must not displace it — that would silently
/// change which glyphs render (the subset covers Latin-1 plus typographic
/// punctuation; the full face covers more) and therefore the metrics.
#[must_use]
pub fn resolve_face(name: &str, resident: Option<&FallbackSnapshot>) -> Option<Vec<u8>> {
    if let Some(b) = crate::fallback::fallback_bytes(name) {
        return Some(b.to_vec());
    }
    // The snapshot is keyed by *face* name (`LiberationSans-Regular`), but the
    // caller has a PDF *font* name (`Helvetica`, possibly subset-tagged). Those
    // are different namespaces, and looking one up in the other is a silent
    // miss that reads as "the face was never delivered".
    let face = crate::fallback::substitute_for(name)?;
    resident
        .and_then(|r| r.face(face))
        .map(|b| b.as_ref().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(n: u8) -> Bytes {
        Bytes::from(vec![n; 4])
    }

    #[test]
    fn a_new_set_holds_nothing() {
        let set = FallbackFontSet::new();
        assert!(!set.has("LiberationSans-Regular"));
        assert!(set.face("LiberationSans-Regular").is_none());
        assert!(set.loaded().is_empty());
        assert!(set.pending().is_empty());
    }

    /// The request/pending/install round trip is the whole protocol: the shell
    /// learns what is missing, fetches it, and hands it over.
    #[test]
    fn request_then_install_makes_a_face_resident() {
        let mut set = FallbackFontSet::new();
        set.request("LiberationSans-Regular");
        assert_eq!(set.pending(), vec!["LiberationSans-Regular"]);
        assert!(!set.has("LiberationSans-Regular"));

        set.install("LiberationSans-Regular", bytes(1));
        assert!(set.has("LiberationSans-Regular"));
        assert_eq!(set.face("LiberationSans-Regular"), Some(&bytes(1)));
        // Installing must clear the request, or the shell refetches forever.
        assert!(set.pending().is_empty());
    }

    /// Requesting something already resident is an upstream bug; honouring it
    /// would buy a second copy of a font we already hold.
    #[test]
    fn requesting_a_resident_face_is_a_no_op() {
        let mut set = FallbackFontSet::new();
        set.install("LiberationSans-Regular", bytes(1));
        set.request("LiberationSans-Regular");
        assert!(set.pending().is_empty());
    }

    /// Requests are deduplicated, because the engine will ask again for every
    /// text run that needs the face.
    #[test]
    fn repeated_requests_are_deduplicated() {
        let mut set = FallbackFontSet::new();
        set.request("LiberationSans-Regular");
        set.request("LiberationSans-Regular");
        set.request("LiberationSerif-Italic");
        assert_eq!(
            set.pending(),
            vec!["LiberationSans-Regular", "LiberationSerif-Italic"]
        );
    }

    /// A face the payload will never carry must stop being requested. This is
    /// what keeps a document that asks for a missing face from producing a
    /// fetch attempt on every single page render.
    #[test]
    fn an_unavailable_face_stops_being_requested() {
        let mut set = FallbackFontSet::new();
        set.request("LiberationSans-Regular");
        set.mark_unavailable("LiberationSans-Regular");
        assert!(set.is_unavailable("LiberationSans-Regular"));
        assert!(set.pending().is_empty());
        set.request("LiberationSans-Regular");
        assert!(
            set.pending().is_empty(),
            "a 'never' face must not re-request"
        );
    }

    /// A payload update replaces the face and revives it, because 'unavailable'
    /// described the payload we had, not the face itself.
    #[test]
    fn installing_revives_an_unavailable_face() {
        let mut set = FallbackFontSet::new();
        set.mark_unavailable("LiberationSans-Regular");
        set.install("LiberationSans-Regular", bytes(2));
        assert!(!set.is_unavailable("LiberationSans-Regular"));
        assert!(set.has("LiberationSans-Regular"));
    }

    /// Replacing a resident face is how a payload update looks.
    #[test]
    fn installing_replaces_an_existing_face() {
        let mut set = FallbackFontSet::new();
        set.install("LiberationSans-Regular", bytes(1));
        set.install("LiberationSans-Regular", bytes(9));
        assert_eq!(set.face("LiberationSans-Regular"), Some(&bytes(9)));
    }

    /// `loaded` is sorted so a caller can log or diff the set deterministically.
    #[test]
    fn loaded_is_sorted() {
        let mut set = FallbackFontSet::new();
        set.install("LiberationSerif-Italic", bytes(1));
        set.install("LiberationMono-Bold", bytes(2));
        set.install("LiberationSans-Regular", bytes(3));
        assert_eq!(
            set.loaded(),
            vec![
                "LiberationMono-Bold",
                "LiberationSans-Regular",
                "LiberationSerif-Italic"
            ]
        );
    }

    /// Marking a face unavailable must also evict any bytes it had, so a stale
    /// copy is never served under a name that is supposed to be gone.
    #[test]
    fn marking_unavailable_evicts_the_bytes() {
        let mut set = FallbackFontSet::new();
        set.install("LiberationSans-Regular", bytes(1));
        set.mark_unavailable("LiberationSans-Regular");
        assert!(!set.has("LiberationSans-Regular"));
        assert!(set.loaded().is_empty());
    }
}
