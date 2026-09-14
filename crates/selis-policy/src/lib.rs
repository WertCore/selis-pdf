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
}
