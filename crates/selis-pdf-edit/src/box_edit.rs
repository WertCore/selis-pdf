//! Box/run-level fallback editing (SL-5.TEXTEDIT.03).
//!
//! When paragraph reconstruction reports a confidence below the reflow
//! threshold (see `pdf-plan/23-EDIT-MODEL-SPEC.md`: `< 0.55` means box/run
//! editing only), a full reflow would be guesswork. The honest option is to
//! splice the replacement into a single line/run in place and **say that the
//! paragraph did not reflow**.
//!
//! The warning is typed ([`BoxEditWarning`]) and the contract is enforced by
//! the type system: [`edit_box`] returns a [`BoxEditPreview`] whose bytes are
//! private. The only way to obtain them is [`BoxEditPreview::commit`], which
//! requires a [`BoxAck`] obtained from that very warning's
//! [`acknowledge`](BoxEditWarning::acknowledge). There is no silent path.
//!
//! The splice itself is the same one [`crate::edit_text`] uses, with reflow
//! disabled, so the edit is an incremental update that preserves the original
//! bytes and any signature over them.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use selis_sandbox::{Budget, BudgetGuard};

use crate::{edit_text_impl, EditError, EditReport};

/// One id per warning, so an acknowledgement cannot be replayed against a
/// different edit. Monotonic and saturating; collisions are irrelevant because
/// a stale ack only ever comes from a warning the caller no longer holds.
static NEXT_WARNING_ID: AtomicU64 = AtomicU64::new(1);

/// A typed warning that a box-level edit did not reflow the paragraph.
///
/// This is the "explicit UI" the spec asks for, carried as data so a caller
/// cannot show the edit without first showing this. The message is phrased for
/// display verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoxEditWarning {
    /// A stable machine-readable code (`BOX_EDIT_NO_REFLOW`).
    pub code: &'static str,
    /// The user-facing message.
    pub message: String,
    /// Binds an acknowledgement to this specific warning.
    id: u64,
}

impl BoxEditWarning {
    /// The stable code every no-reflow warning carries.
    pub const CODE: &'static str = "BOX_EDIT_NO_REFLOW";

    /// Produce the acknowledgement this warning requires before its bytes may
    /// be taken.
    #[must_use]
    pub const fn acknowledge(&self) -> BoxAck {
        BoxAck { id: self.id }
    }

    /// The user-facing message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn new() -> Self {
        let id = NEXT_WARNING_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            code: Self::CODE,
            message:
                "this edit did not reflow the paragraph: it was spliced into a single line/run \
                 in place. Surrounding text may overlap or leave a gap; review the page before \
                 saving."
                    .to_string(),
            id,
        }
    }
}

impl fmt::Display for BoxEditWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Proof that the caller saw and accepted a specific [`BoxEditWarning`].
///
/// The only public constructor is [`BoxEditWarning::acknowledge`], so a caller
/// cannot take a box edit's bytes without first obtaining its warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxAck {
    id: u64,
}

/// Why a committed box edit could not be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoxEditError {
    /// The acknowledgement did not come from this preview's warning.
    WarningNotAcknowledged,
}

impl fmt::Display for BoxEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WarningNotAcknowledged => f.write_str(
                "this box edit did not reflow the paragraph; the warning must be acknowledged \
                 before its bytes can be used",
            ),
        }
    }
}

impl std::error::Error for BoxEditError {}

/// A box edit whose bytes are sealed until the warning is acknowledged.
#[derive(Debug, Clone)]
pub struct BoxEditPreview {
    bytes: Vec<u8>,
    report: EditReport,
    warning: BoxEditWarning,
}

impl BoxEditPreview {
    /// The warning that must be acknowledged to release the bytes.
    #[must_use]
    pub const fn warning(&self) -> &BoxEditWarning {
        &self.warning
    }

    /// An estimate of the edit's size before the bytes are released — safe to
    /// display while the warning is still on screen.
    #[must_use]
    pub const fn report(&self) -> &EditReport {
        &self.report
    }

    /// Release the bytes, but only against the acknowledgement derived from
    /// THIS preview's warning.
    ///
    /// # Errors
    ///
    /// [`BoxEditError::WarningNotAcknowledged`] when `ack` came from a different
    /// warning (or was never obtained). The preview is borrowed, so the caller
    /// can acknowledge correctly and retry.
    pub fn commit(&self, ack: BoxAck) -> Result<BoxEdit, BoxEditError> {
        if ack.id != self.warning.id {
            return Err(BoxEditError::WarningNotAcknowledged);
        }
        Ok(BoxEdit {
            bytes: self.bytes.clone(),
            report: self.report.clone(),
            warning: self.warning.clone(),
        })
    }
}

/// A committed box edit: the bytes plus the warning that authorised them.
///
/// The warning is kept on the result so the disclosure travels with the bytes
/// all the way to whatever writes the file.
#[derive(Debug, Clone)]
pub struct BoxEdit {
    /// The incrementally-updated document.
    pub bytes: Vec<u8>,
    /// What the splice did.
    pub report: EditReport,
    /// The no-reflow warning that was acknowledged.
    pub warning: BoxEditWarning,
}

/// Splice `replace` over the first `find` in a single line/run on page
/// `page_index` **without reflowing the paragraph**, and return a sealed
/// preview.
///
/// Use this when paragraph confidence is below the reflow threshold. The bytes
/// are not available until the caller acknowledges the returned warning, so a
/// box edit can never be committed silently (SL-5.TEXTEDIT.03).
///
/// # Errors
///
/// The same [`EditError`] variants as [`crate::edit_text`]: an invalid request,
/// an encrypted document, text not found in a single show-text operator, or a
/// page/font/stream shape this slice does not handle.
///
/// # Budget
///
/// Identical to [`crate::edit_text`]: the work is bounded by the decoded
/// content-stream length (`MAX_CONTENT_BYTES`). Building the warning is O(1).
///
/// # Malformed Input
///
/// A document whose content cannot be decoded or rewritten is refused with an
/// [`EditError`]; no preview (and therefore no bytes) is produced.
pub fn edit_box(
    src: &[u8],
    page_index: u32,
    find: &str,
    replace: &str,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<BoxEditPreview, EditError> {
    let (bytes, report) = edit_text_impl(src, page_index, find, replace, budget, g, false)?;
    Ok(BoxEditPreview {
        bytes,
        report,
        warning: BoxEditWarning::new(),
    })
}
