#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! Permission semantics as policy (SL-1.ENC.04, SL-1.ENC.09).
//!
//! The `/P` bits in a PDF trailer are the document's declared permissions.
//! We honour them by default and expose an explicit, logged override for the
//! owner-password case. Do not pretend the bits are security — they are
//! metadata, and treating them as enforcement is hostile to the user who owns
//! the file and has the owner password.
//!
//! The explicit, logged override is the defensible middle: honouring permissions
//! when the user has the owner password and legitimately owns the file is
//! user-hostile; ignoring them silently is the thing that gets a vendor sued.
//!
//! **SL-1.ENC.09 — the CMS recipient block.** A public-key (PKCS#7)
//! document carries a *per-recipient* 4-byte permission block inside the
//! unwrapped 24-byte payload (stronger semantics: the bits are bound to the
//! certificate-identity holder and are cryptographically attested by the
//! recipient-key unwrap, unlike `/P`). When such a receipt is the active
//! open, the enforced grant is the **intersection** ([`Permissions::restrict`])
//! of the PDF-level `/P` bits and the CMS bits: a CMS holder with a weaker
//! grant never gains the higher PDF-level permission.
//! [`check_permissions`] turns a content-touching [`ContentOp`] against that
//! effective grant into a typed `PERMISSION_DENIED_BY_CMS` refusal. The
//! owner-password path keeps the standard `/P`-only semantics of the
//! paragraph above (the owner owns the document; the recipient block belongs
//! to a *recipient*).
//!
//! **SL-5.JS.02/03 — scripting consent and admin origins (ADR-P0020).**
//! Document JavaScript is off by default. [`ConsentPolicy`] records an explicit
//! per-document reader consent; [`ApprovedOrigins`] is the admin list of
//! pre-approved origins/IDs that may auto-run; [`ScriptAuthorization`] is the
//! choke point an engine calls before it runs any script. Both default to deny.

use std::collections::{BTreeMap, BTreeSet};

/// The standard PDF permission bits (ISO 32000-2 Table 23).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// Raw `/P` value.
    bits: u32,
}

/// An override decision: whether to bypass a permission check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Override {
    /// Honour the permission (deny the action).
    Honour,
    /// Override the permission (allow the action). The override is recorded
    /// in the oplog with the given reason.
    Override,
}

impl Permissions {
    /// Parse from a `/P` integer.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self { bits }
    }

    /// Whether the user may print the document (bit 3).
    #[must_use]
    pub const fn print(&self) -> bool {
        (self.bits & (1 << 3)) != 0
    }

    /// Whether the user may modify the document (bit 4).
    #[must_use]
    pub const fn modify(&self) -> bool {
        (self.bits & (1 << 4)) != 0
    }

    /// Whether the user may copy text and graphics (bit 5).
    #[must_use]
    pub const fn copy(&self) -> bool {
        (self.bits & (1 << 5)) != 0
    }

    /// Whether the user may add or modify annotations (bit 6).
    #[must_use]
    pub const fn annotate(&self) -> bool {
        (self.bits & (1 << 6)) != 0
    }

    /// Whether the user may fill in form fields (bit 9).
    #[must_use]
    pub const fn fill_forms(&self) -> bool {
        (self.bits & (1 << 9)) != 0
    }

    /// Whether the user may extract text and graphics for accessibility (bit 10).
    #[must_use]
    pub const fn extract_accessibility(&self) -> bool {
        (self.bits & (1 << 10)) != 0
    }

    /// Whether the user may assemble the document (rotate, insert, delete) (bit 11).
    #[must_use]
    pub const fn assemble(&self) -> bool {
        (self.bits & (1 << 11)) != 0
    }

    /// Whether the user may print high quality (bit 12).
    #[must_use]
    pub const fn print_high(&self) -> bool {
        (self.bits & (1 << 12)) != 0
    }

    /// The raw bits.
    #[must_use]
    pub const fn bits(&self) -> u32 {
        self.bits
    }

    /// Parse the CMS public-key recipient permission block (the 4 bytes
    /// after the 20-byte seed in the unwrapped PKCS#7 payload —
    /// SL-1.ENC.09, ISO 32000-2 §7.6.6.4, Table 23 semantics).
    ///
    /// The little-endian block carries the *same* bit layout as `/P`; this is
    /// the cryptographically-bound, per-recipient grant that the policy edge
    /// intersects with the document's `/P`. It is never a raise: only bits
    /// already granted by `/P` survive the intersection.
    #[must_use]
    pub const fn from_cms_block(block: [u8; 4]) -> Self {
        Self {
            bits: u32::from_le_bytes(block),
        }
    }

    /// The intersection of two grants — a bit survives only if both allow it
    /// (SL-1.ENC.09: the weaker of the PDF `/P` and the CMS recipient block
    /// wins for every operation; a CMS grant never raises the PDF-level
    /// grant).
    #[must_use]
    pub const fn restrict(&self, other: &Self) -> Self {
        Self {
            bits: self.bits & other.bits,
        }
    }
}

/// A content-touching operation that the permission bits gate (SL-1.ENC.09).
///
/// The viewer-UI greying of these is Phase 4; the *API* gating lives here:
/// [`check_permissions`] turns an op whose required bit is clear in the
/// effective grant into a typed `PERMISSION_DENIED_BY_CMS`, and the engine
/// session is the single choke point that calls it (SL-1.ENC.09 plumbing,
/// design note §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentOp {
    /// Render page content as a printed page (needs `print`).
    Print,
    /// Copy or extract text/graphics from the document (needs `copy`).
    CopyText,
    /// Add or modify a text annotation / mark-up (needs `annotate`).
    Annotate,
    /// Redact content (a destructive edit — needs `modify`).
    Redact,
    /// Edit or assemble content (needs `modify`).
    Edit,
    /// Fill in a form field (needs `fill_forms`).
    FillForm,
}

impl ContentOp {
    /// Whether the given grant permits this operation.
    #[must_use]
    pub const fn allowed_by(self, perms: &Permissions) -> bool {
        match self {
            ContentOp::Print => perms.print(),
            ContentOp::CopyText => perms.copy(),
            ContentOp::Annotate => perms.annotate(),
            ContentOp::Redact => perms.modify(),
            ContentOp::Edit => perms.modify(),
            ContentOp::FillForm => perms.fill_forms(),
        }
    }
}

/// Enforce a content-touching operation against the effective grant
/// (SL-1.ENC.09), returning a typed refusal when the already-intersected
/// grant forbids it (`PERMISSION_DENIED_BY_CMS`). `is_owner` mirrors
/// [`check`]: an owner-password holder is on the standard `/P`-only path,
/// where the logged override applies; every public-key *recipient* opens
/// without `is_owner`, so the CMS-intersected grant binds without escape.
pub fn check_permissions(
    perms: &Permissions,
    op: ContentOp,
    is_owner: bool,
) -> selis_error::Result<()> {
    if op.allowed_by(perms) {
        return Ok(());
    }
    // The owner on the standard handler still gets the explicit, logged
    // override (SL-1.ENC.04); a non-owner, and every recipient on the
    // public-key handler, is bound by the effective bits.
    if is_owner {
        return Ok(());
    }
    Err(selis_error::err!(
        selis_error::Code::PermissionDeniedByCms,
        during = "policy",
        detail = "the recipient's permission block forbids this operation"
    ))
}

impl Default for Permissions {
    fn default() -> Self {
        // PDF 2.0 default: all permissions allowed.
        Self {
            bits: 0xffffffff_u32,
        }
    }
}

/// Check whether an action is permitted.
///
/// `is_owner`: whether the caller has the owner password. `override_`: the
/// caller's override preference (defaults to `Honour` for non-owner).
///
/// The override is recorded in the oplog when used. Logging is the caller's
/// responsibility — this function returns the decision.
pub fn check(
    perms: &Permissions,
    action: &dyn Fn(&Permissions) -> bool,
    is_owner: bool,
    override_: Option<Override>,
) -> Override {
    if action(perms) {
        return Override::Honour;
    }
    // Bit denies the action. Owner may override.
    if is_owner {
        override_.unwrap_or(Override::Override)
    } else {
        Override::Honour
    }
}

// ---------------------------------------------------------------------------
// SL-5.JS.02/03 — scripting consent + admin origin policy (ADR-P0020).
// ---------------------------------------------------------------------------

/// The identity a scripting decision is keyed on.
///
/// The caller supplies both strings; this type never derives identity from
/// document bytes. `id` should be a stable per-document identifier (for example
/// a content hash with volatile fields removed) and `origin` the place the
/// document came from (a URL origin, or `"local"` / `"memory"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DocumentIdentity {
    /// Stable per-document identifier.
    pub id: String,
    /// Where the document came from.
    pub origin: String,
}

impl DocumentIdentity {
    /// Construct an identity from an id and an origin.
    #[must_use]
    pub fn new(id: impl Into<String>, origin: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            origin: origin.into(),
        }
    }
}

/// How long a reader's explicit consent lasts, as recorded by the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentScope {
    /// Consent is valid only for the current open session.
    Session,
    /// Consent is persisted by the host for this document id.
    Persistent,
}

/// Reader consent, keyed per document (SL-5.JS.02).
///
/// A fresh policy grants nothing: a script never runs until the host calls
/// [`ConsentPolicy::grant`] with an explicit reader action for that document.
#[derive(Debug, Clone, Default)]
pub struct ConsentPolicy {
    grants: BTreeMap<String, ConsentScope>,
}

impl ConsentPolicy {
    /// A deny-all policy.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the reader has explicitly consented for this document.
    #[must_use]
    pub fn is_consented(&self, doc: &DocumentIdentity) -> bool {
        self.grants.contains_key(&doc.id)
    }

    /// The recorded scope for this document, if consented.
    #[must_use]
    pub fn scope(&self, doc: &DocumentIdentity) -> Option<ConsentScope> {
        self.grants.get(&doc.id).copied()
    }

    /// Record an explicit reader consent for one document.
    pub fn grant(&mut self, doc: &DocumentIdentity, scope: ConsentScope) {
        let _ = self.grants.insert(doc.id.clone(), scope);
    }

    /// Withdraw consent for one document. Returns whether a grant was removed.
    pub fn revoke(&mut self, doc: &DocumentIdentity) -> bool {
        self.grants.remove(&doc.id).is_some()
    }

    /// Withdraw every grant (for example when the reader enables a "block all
    /// scripts" setting).
    pub fn clear(&mut self) {
        self.grants.clear();
    }

    /// Number of documents with a recorded grant.
    #[must_use]
    pub fn grant_count(&self) -> usize {
        self.grants.len()
    }
}

/// Admin pre-approval list (SL-5.JS.03).
///
/// An empty list denies everything: no document may auto-run scripts unless an
/// administrator explicitly adds its origin or id.
#[derive(Debug, Clone, Default)]
pub struct ApprovedOrigins {
    origins: BTreeSet<String>,
    ids: BTreeSet<String>,
}

impl ApprovedOrigins {
    /// A deny-all list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-approve a document origin (for example `"https://forms.example"`).
    pub fn approve_origin(&mut self, origin: impl Into<String>) {
        let _ = self.origins.insert(origin.into());
    }

    /// Pre-approve one document id.
    pub fn approve_document(&mut self, id: impl Into<String>) {
        let _ = self.ids.insert(id.into());
    }

    /// Whether an admin pre-approved this document's origin or id.
    #[must_use]
    pub fn is_approved(&self, doc: &DocumentIdentity) -> bool {
        self.origins.contains(&doc.origin) || self.ids.contains(&doc.id)
    }

    /// Remove an origin from the list.
    pub fn revoke_origin(&mut self, origin: &str) -> bool {
        self.origins.remove(origin)
    }

    /// Remove a document id from the list.
    pub fn revoke_document(&mut self, id: &str) -> bool {
        self.ids.remove(id)
    }

    /// Total number of admin entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.origins.len().saturating_add(self.ids.len())
    }

    /// Whether the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The combined gate an engine calls before running any document script.
///
/// A run is allowed only when the reader consented for this document **or** an
/// administrator pre-approved its origin/id. Admin pre-approval is the explicit
/// ADR-P0020 exception to "no script without reader consent".
#[derive(Debug, Clone, Copy)]
pub struct ScriptAuthorization<'a> {
    /// Reader consent state.
    pub consent: &'a ConsentPolicy,
    /// Admin pre-approval list.
    pub origins: &'a ApprovedOrigins,
}

impl<'a> ScriptAuthorization<'a> {
    /// Build a gate over the two policies.
    #[must_use]
    pub const fn new(consent: &'a ConsentPolicy, origins: &'a ApprovedOrigins) -> Self {
        Self { consent, origins }
    }

    /// Decide whether a script may run, returning a typed policy refusal.
    ///
    /// # Errors
    ///
    /// `JS_CONSENT_REQUIRED` when neither the reader nor the admin approved
    /// this document. The caller must not run the script on `Err`.
    pub fn authorize(&self, doc: &DocumentIdentity) -> selis_error::Result<()> {
        if self.consent.is_consented(doc) || self.origins.is_approved(doc) {
            return Ok(());
        }
        Err(selis_error::err!(
            selis_error::Code::JsConsentRequired,
            during = "scripting-consent",
            detail = "no reader consent and no admin origin approval"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perms_all() -> Permissions {
        Permissions::from_bits(0xffffffff_u32)
    }

    fn perms_none() -> Permissions {
        Permissions::from_bits(0)
    }

    #[test]
    fn all_permissions_when_every_bit_is_set() {
        let p = perms_all();
        assert!(p.print());
        assert!(p.modify());
        assert!(p.copy());
        assert!(p.annotate());
        assert!(p.fill_forms());
        assert!(p.extract_accessibility());
        assert!(p.assemble());
        assert!(p.print_high());
    }

    #[test]
    fn no_permissions_when_no_bits_are_set() {
        let p = perms_none();
        assert!(!p.print());
        assert!(!p.modify());
    }

    #[test]
    fn owner_can_override_a_denied_action() {
        let p = perms_none();
        assert_eq!(check(&p, &|pp| pp.print(), true, None), Override::Override);
    }

    #[test]
    fn non_owner_cannot_override() {
        let p = perms_none();
        assert_eq!(
            check(&p, &|pp| pp.print(), false, Some(Override::Override)),
            Override::Honour
        );
    }

    #[test]
    fn allowed_action_returns_honour() {
        let p = perms_all();
        assert_eq!(check(&p, &|pp| pp.print(), false, None), Override::Honour);
    }

    // ---- SL-1.ENC.09: the CMS recipient block as policy ----

    /// `/P`-allows, CMS-denies → the operation fails typed; a weaker CMS
    /// grant never raises the PDF-level grant.
    #[test]
    fn cms_block_intersects_pdf_bits() {
        let pdf = Permissions::from_bits(0xFFFF_FFFF); // /P allows everything
                                                       // Extract-only recipient: copy (bit 5) set, annotate (bit 6) clear.
        let cms = Permissions::from_cms_block([0x20, 0x00, 0xF0, 0xFF]);
        // (0xFFFF_F020 little-endian: copy allowed, annotate/print/modify denied)
        let eff = pdf.restrict(&cms);
        assert!(eff.copy());
        assert!(!eff.annotate());
        assert!(!eff.print());
        assert_eq!(
            check_permissions(&eff, ContentOp::Annotate, false)
                .err()
                .map(|e| e.code()),
            Some(selis_error::Code::PermissionDeniedByCms),
            "extract-only cannot annotate even though /P allows"
        );
        assert!(check_permissions(&eff, ContentOp::CopyText, false).is_ok());
        // Redact/edit rides the modify bit — also denied here.
        assert!(check_permissions(&eff, ContentOp::Redact, false).is_err());
        assert!(check_permissions(&eff, ContentOp::Edit, false).is_err());
    }

    /// A view-only recipient can open and view, but can neither extract nor
    /// annotate (or print).
    #[test]
    fn view_only_recipient_is_locked_down() {
        let cms = Permissions::from_cms_block([0x00, 0x00, 0xF0, 0xFF]); // no functional bits
        let eff = Permissions::from_bits(0xFFFF_FFFF).restrict(&cms);
        assert!(check_permissions(&eff, ContentOp::CopyText, false).is_err());
        assert!(check_permissions(&eff, ContentOp::Annotate, false).is_err());
        assert!(check_permissions(&eff, ContentOp::Print, false).is_err());
    }

    /// The AND is symmetric: a *stronger* CMS block than `/P` also cannot
    /// raise above the document grant.
    #[test]
    fn cms_never_raises_above_pdf() {
        let pdf = Permissions::from_cms_block([0x08, 0x00, 0xF0, 0xFF]); // print only
        let cms = Permissions::from_bits(0xFFFF_FFFF); // recipient allows all
        let eff = pdf.restrict(&cms);
        assert!(eff.print());
        assert!(!eff.copy());
        assert!(check_permissions(&eff, ContentOp::CopyText, false).is_err());
    }

    /// The owner-password path keeps standard `/P` semantics (override per
    /// SL-1.ENC.04); the CMS intersection belongs to recipients only — the
    /// owner opens without a CMS block, so the engine never sets `is_owner`
    /// and `perms` is the plain `/P`.
    #[test]
    fn owner_path_is_pdf_semantics_only() {
        let pdf_lacks_modify = Permissions::from_cms_block([0x08, 0x00, 0xF0, 0xFF]);
        assert!(check_permissions(&pdf_lacks_modify, ContentOp::Edit, true).is_ok());
        assert_eq!(
            check_permissions(&pdf_lacks_modify, ContentOp::Edit, false)
                .err()
                .map(|e| e.code()),
            Some(selis_error::Code::PermissionDeniedByCms)
        );
    }

    #[test]
    fn from_cms_block_is_little_endian() {
        let cms = Permissions::from_cms_block([0x28, 0x00, 0x00, 0x00]);
        assert!(cms.print()); // bit 3
        assert!(cms.copy()); // bit 5
        assert!(!cms.modify()); // bit 4 clear
    }

    // ---- SL-5.JS.02/03: scripting consent + admin origins ----

    fn doc() -> DocumentIdentity {
        DocumentIdentity::new("doc-123", "https://forms.example")
    }

    /// A fresh policy grants nothing: scripts never auto-run (ADR-P0020).
    #[test]
    fn consent_defaults_to_deny() {
        let consent = ConsentPolicy::new();
        assert!(!consent.is_consented(&doc()));
        assert_eq!(consent.scope(&doc()), None);
        assert_eq!(consent.grant_count(), 0);
    }

    /// Consent is keyed on the document id: granting for one document does not
    /// enable another, even at the same origin.
    #[test]
    fn consent_is_per_document() {
        let mut consent = ConsentPolicy::new();
        let a = doc();
        let b = DocumentIdentity::new("doc-456", "https://forms.example");
        consent.grant(&a, ConsentScope::Session);
        assert!(consent.is_consented(&a));
        assert_eq!(consent.scope(&a), Some(ConsentScope::Session));
        assert!(!consent.is_consented(&b));
        assert_eq!(consent.grant_count(), 1);
    }

    #[test]
    fn consent_can_be_revoked_and_cleared() {
        let mut consent = ConsentPolicy::new();
        let d = doc();
        consent.grant(&d, ConsentScope::Persistent);
        assert!(consent.revoke(&d));
        assert!(!consent.revoke(&d));
        consent.grant(&d, ConsentScope::Session);
        consent.clear();
        assert!(!consent.is_consented(&d));
    }

    /// The admin list is deny-all until an entry is added.
    #[test]
    fn approved_origins_default_to_deny() {
        let origins = ApprovedOrigins::new();
        assert!(origins.is_empty());
        assert!(!origins.is_approved(&doc()));
    }

    #[test]
    fn admin_can_approve_an_origin_or_a_single_document() {
        let mut origins = ApprovedOrigins::new();
        let d = doc();
        origins.approve_document("doc-123");
        assert!(origins.is_approved(&d));
        assert_eq!(origins.len(), 1);

        let other = DocumentIdentity::new("doc-999", "https://other.example");
        assert!(!origins.is_approved(&other));
        origins.approve_origin("https://other.example");
        assert!(origins.is_approved(&other));
        assert_eq!(origins.len(), 2);

        assert!(origins.revoke_document("doc-123"));
        assert!(!origins.is_approved(&d));
        assert!(origins.revoke_origin("https://other.example"));
        assert!(!origins.is_approved(&other));
    }

    /// The gate is the choke point: with neither grant it returns a typed
    /// refusal and the caller must not run the script.
    #[test]
    fn gate_denies_by_default() {
        let consent = ConsentPolicy::new();
        let origins = ApprovedOrigins::new();
        let gate = ScriptAuthorization::new(&consent, &origins);
        let e = gate.authorize(&doc()).expect_err("default deny");
        assert_eq!(e.code(), selis_error::Code::JsConsentRequired);
    }

    #[test]
    fn gate_allows_reader_consent_or_admin_approval() {
        let d = doc();
        let mut consent = ConsentPolicy::new();
        let origins = ApprovedOrigins::new();
        consent.grant(&d, ConsentScope::Session);
        assert!(ScriptAuthorization::new(&consent, &origins)
            .authorize(&d)
            .is_ok());

        let consent = ConsentPolicy::new();
        let mut origins = ApprovedOrigins::new();
        origins.approve_origin("https://forms.example");
        assert!(ScriptAuthorization::new(&consent, &origins)
            .authorize(&d)
            .is_ok());
    }
}
