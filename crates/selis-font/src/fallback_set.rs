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
    /// The invalidation counter. **A change is the repaint signal**
    /// (ADR-P0043 §3), mirroring `CjkFontSet::revision`. Named `rev` so the
    /// `revision()` accessor does not collide with the field it reads.
    rev: u64,
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
        // Adopting a face always changes the picture: either a new face becomes
        // resident, or an existing one is replaced, or a face previously marked
        // permanently missing turns out to be available. Bumping unconditionally
        // is therefore correct, and cheaper to reason about than proving
        // "nothing changed" — a redundant repaint is a wasted frame, a missed
        // one is a stale page.
        let _ = self.unavailable.remove(name);
        self.rev = self.rev.saturating_add(1);
    }

    /// Record that a face will never arrive, so it stops being requested.
    pub fn mark_unavailable(&mut self, name: &'static str) {
        // Evicting resident bytes *is* a pixel change, so that bumps the
        // revision. Merely recording a face that was never here is not a change
        // in the picture, and must not signal a repaint — otherwise every
        // render of a document naming a face this payload lacks would repaint
        // the world for nothing.
        if self.faces.remove(name).is_some() {
            self.rev = self.rev.saturating_add(1);
        }
        self.requested.remove(name);
        let _ = self.unavailable.insert(name);
    }

    /// Release a resident face's bytes and put it back on the request queue.
    ///
    /// The resident-set half of the eviction lever, and the mirror of
    /// [`CjkFontSet::evict`](crate::CjkFontSet::evict) — with **one deliberate
    /// difference, and it is the difference between a face and a chunk.**
    ///
    /// A missing chunk is missing *ink* at the right place, so a CJK eviction
    /// can leave the queue alone and let the next render re-mark it. A missing
    /// face is not missing ink: `resolve_face` falls through to a substitution
    /// or to nothing, and substituting a font changes every advance width on
    /// the line. So a face that was released goes straight back onto
    /// [`requested`](Self::pending), and the state a shell reads afterwards
    /// names it as still owed rather than looking like a payload that needs
    /// nothing. Without that, "evicted" and "this document needs no fallback at
    /// all" are the same wire value, and a shell that reads it as the latter
    /// paints a wrong-metrics page and never finds out why.
    ///
    /// The one thing it must **not** do is call
    /// [`mark_unavailable`](Self::mark_unavailable): a face that is not here
    /// yet and a face that will never arrive are different states, and
    /// collapsing them turns a recoverable eviction into permanent, silent
    /// text corruption.
    ///
    /// Eviction **bumps the revision**, exactly as [`install`](Self::install)
    /// does, and for the same reason: the pixels a render produces from this set
    /// have changed, so a repaint is owed. A face that is not resident returns
    /// `false` and does **not** bump it, so "nothing to release" is not a
    /// repaint.
    pub fn release(&mut self, name: &'static str) -> bool {
        if self.faces.remove(name).is_none() {
            return false;
        }
        self.rev = self.rev.saturating_add(1);
        // Inserted directly rather than through `request`: that one is a no-op
        // for a resident face, and this face is by definition not resident any
        // more. `unavailable` cannot contain it — `mark_unavailable` evicts, so
        // an unavailable face was never here to release.
        self.requested.insert(name);
        true
    }

    /// The set's invalidation revision. A change is the repaint signal.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.rev
    }

    /// The faces the payload will never carry, sorted.
    #[must_use]
    pub fn unavailable(&self) -> Vec<&'static str> {
        self.unavailable.iter().copied().collect()
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

    /// A released face is gone from the resident set and owed again, and the
    /// revision moved because the pixels a render would produce have changed.
    ///
    /// **The re-arm is the load-bearing half.** Leaving the face off the queue
    /// would make "evicted" and "this document needs no fallback at all" the
    /// same wire value, and a shell reading it as the latter paints a page in
    /// the wrong metrics and never learns why.
    #[test]
    fn a_released_face_is_gone_and_owed_again() {
        let mut set = FallbackFontSet::new();
        set.install("LiberationSans-Regular", bytes(1));
        let before = set.revision();

        assert!(set.release("LiberationSans-Regular"));
        assert!(!set.has("LiberationSans-Regular"));
        assert!(set.face("LiberationSans-Regular").is_none());
        assert!(set.loaded().is_empty(), "the bytes are given back");
        assert_eq!(
            set.pending(),
            vec!["LiberationSans-Regular"],
            "a released face is 'not here yet', not 'never'"
        );
        assert!(!set.is_unavailable("LiberationSans-Regular"));
        assert!(
            set.revision() > before,
            "a repaint is owed: the set changed under the renderer"
        );
    }

    /// A release that released nothing is not a change, so it must not move the
    /// revision — a polling shell would otherwise repaint the world every time
    /// it asked about a face that was not there.
    #[test]
    fn releasing_a_face_that_is_not_resident_is_a_no_op() {
        let mut set = FallbackFontSet::new();
        set.request("LiberationSans-Regular");
        let before = set.revision();
        assert!(!set.release("LiberationSans-Regular"), "nothing was held");
        assert!(!set.release("LiberationSerif-Italic"), "no such face");
        assert_eq!(set.revision(), before, "still no repaint owed");
        assert_eq!(
            set.pending(),
            vec!["LiberationSans-Regular"],
            "a queued face stays queued, and is not doubled"
        );
    }

    /// Eviction must be a round trip, not a one-way door: the same face can be
    /// installed again afterwards, which is what "the session is still armed"
    /// means at this level.
    #[test]
    fn a_released_face_can_be_installed_again() {
        let mut set = FallbackFontSet::new();
        set.install("LiberationSans-Regular", bytes(1));
        let _ = set.release("LiberationSans-Regular");
        set.install("LiberationSans-Regular", bytes(9));
        assert!(set.has("LiberationSans-Regular"));
        assert_eq!(set.face("LiberationSans-Regular"), Some(&bytes(9)));
        assert!(set.pending().is_empty(), "installing clears the request");
    }
}
