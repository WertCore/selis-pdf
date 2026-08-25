//! Permission semantics as policy (SL-1.ENC.04).
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
}
