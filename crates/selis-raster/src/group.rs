//! Transparency groups (SL-2.RAST.04).
//!
//! The hardest correctness problem in PDF rendering. A transparency group is
//! a temporary canvas onto which a set of objects is drawn, then composited
//! back. The parameters:
//! * **isolated** — the group's backdrop is transparent (initialised to
//!   transparent black) rather than the current backdrop;
//! * **knockout** — objects in the group do not composite with each other;
//! * **group colour space** — the space the group is composited in.
//!
//! This module owns the group semantics and the correct compositing formula
//! (§11.3.1); the per-group raster lives on the backend (push/pop layer).

use selis_color::{BlendMode, Rgba};

/// A transparency group's parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroupParams {
    /// Isolated: the group starts from transparent black.
    pub isolated: bool,
    /// Knockout: objects in the group do not composite with each other.
    pub knockout: bool,
    /// The blend mode used to composite the group back.
    pub blend: BlendMode,
    /// The group's alpha (composited with the accumulated alpha).
    pub alpha: f64,
}

impl Default for GroupParams {
    fn default() -> Self {
        Self {
            isolated: false,
            knockout: false,
            blend: BlendMode::Normal,
            alpha: 1.0,
        }
    }
}

/// The initial backdrop of a group.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Backdrop {
    /// The accumulated colour.
    pub colour: Rgba,
}

impl Backdrop {
    /// The page's initial backdrop: white.
    #[must_use]
    pub fn page() -> Self {
        Self {
            colour: Rgba::new(1.0, 1.0, 1.0, 1.0),
        }
    }

    /// A transparent backdrop (for an isolated group).
    #[must_use]
    pub fn transparent() -> Self {
        Self {
            colour: Rgba::TRANSPARENT,
        }
    }
}

/// The compositing operation: `B(Cb, Cs)` blended then source-over onto the
/// backdrop.
///
/// The group's result is `Co = (1 − αs/αo)·Cb + (αs/αo)·((1−αb)·Cs + αb·B)`.
/// When the group is isolated, `Cb` is transparent black.
#[must_use]
pub fn composite(backdrop: Rgba, source: Rgba, blend: BlendMode) -> Rgba {
    blend.blend(backdrop, source)
}

/// The group semantics decision table.
///
/// Returns the backdrop a group's contents are drawn against.
#[must_use]
pub fn group_backdrop(current: Backdrop, params: GroupParams) -> Backdrop {
    if params.isolated {
        Backdrop::transparent()
    } else {
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_group_uses_transparent_backdrop() {
        let current = Backdrop::page();
        let group = GroupParams {
            isolated: true,
            ..GroupParams::default()
        };
        assert_eq!(group_backdrop(current, group), Backdrop::transparent());
    }

    #[test]
    fn non_isolated_group_uses_current_backdrop() {
        let current = Backdrop::page();
        let group = GroupParams {
            isolated: false,
            ..GroupParams::default()
        };
        assert_eq!(group_backdrop(current, group), current);
    }

    #[test]
    fn page_backdrop_is_white() {
        let page = Backdrop::page();
        assert!((page.colour.rgb.r - 1.0).abs() < 1e-9);
        assert!((page.colour.a - 1.0).abs() < 1e-9);
    }

    #[test]
    fn composite_matches_the_blend() {
        // Normal blend over an opaque backdrop is source-over.
        let out = composite(
            Rgba::new(1.0, 0.0, 0.0, 1.0),
            Rgba::new(0.0, 0.0, 1.0, 0.5),
            BlendMode::Normal,
        );
        assert!((out.rgb.r - 0.5).abs() < 1e-9);
        assert!((out.rgb.b - 0.5).abs() < 1e-9);
    }

    #[test]
    fn knockout_flag_is_carried() {
        let group = GroupParams {
            knockout: true,
            ..GroupParams::default()
        };
        assert!(group.knockout);
        assert!(!GroupParams::default().knockout);
    }
}
