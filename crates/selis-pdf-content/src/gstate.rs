//! Graphics state machine (SL-2.CONT.02).
//!
//! The full graphics state: CTM, stroke/fill colours, line width/cap/join/
//! miter/dash, rendering intent, flatness, smoothness, stroke adjustment,
//! blend mode, soft mask, alpha constants, alpha-is-shape, and the text state.
//! `q`/`Q` push and pop the stack with depth budgeting.

use selis_error::{err, Code, Result};
use selis_geom::Matrix;
use selis_sandbox::{Budget, BudgetGuard};

use crate::path::{ClipRule, Path};

/// The graphics state.
#[derive(Debug, Clone, PartialEq)]
pub struct GState {
    /// The current transformation matrix.
    pub ctm: Matrix,
    /// The current line width.
    pub line_width: f64,
    /// The current line cap style (0 butt, 1 round, 2 projecting).
    pub line_cap: u8,
    /// The current line join style (0 miter, 1 round, 2 bevel).
    pub line_join: u8,
    /// The current miter limit.
    pub miter_limit: f64,
    /// The dash pattern (array + phase).
    pub dash: (Vec<f64>, f64),
    /// The rendering intent (`0 RelativeColorimetric` …).
    pub rendering_intent: u8,
    /// The flatness tolerance.
    pub flatness: f64,
    /// The smoothness tolerance.
    pub smoothness: f64,
    /// Stroke adjustment (`false` = on).
    pub stroke_adjust: bool,
    /// The blend mode (PDF name, e.g. `Normal`).
    pub blend_mode: selis_bytes::Bytes,
    /// The soft mask (an `SMask` reference or `None`).
    pub soft_mask: Option<selis_bytes::Bytes>,
    /// The accumulated clip paths, innermost last (`W`/`W*`). Saved and
    /// restored with `q`/`Q` as part of the graphics state.
    pub clip: Vec<(Path, ClipRule)>,
    /// The constant alpha for stroking.
    pub alpha_stroke: f64,
    /// The constant alpha for non-stroking.
    pub alpha_fill: f64,
    /// Whether alpha is shape (true) or opacity (false).
    pub alpha_is_shape: bool,
    /// Overprint for stroking (`/OP`).
    pub overprint_stroke: bool,
    /// Overprint for non-stroking (`/op`).
    pub overprint_fill: bool,
    /// Overprint mode (`/OPM`); 0 or 1.
    pub overprint_mode: u8,
    /// The non-stroking colour (device RGB, 0..1).
    pub fill_colour: [f64; 3],
    /// The stroking colour (device RGB, 0..1).
    pub stroke_colour: [f64; 3],
    /// The non-stroking colour space name (`cs`; default `DeviceRGB`).
    pub fill_cs: selis_bytes::Bytes,
    /// The stroking colour space name (`CS`; default `DeviceRGB`).
    pub stroke_cs: selis_bytes::Bytes,
    /// The fill pattern resource name (from `scn` with a trailing name), if
    /// the current colour space is a pattern.
    pub fill_pattern: Option<selis_bytes::Bytes>,
    /// The stroke pattern resource name.
    pub stroke_pattern: Option<selis_bytes::Bytes>,
    /// Text state: the font resource name.
    pub text_font: Option<selis_bytes::Bytes>,
    /// Text size.
    pub text_size: f64,
    /// Character spacing.
    pub char_spacing: f64,
    /// Word spacing.
    pub word_spacing: f64,
    /// Horizontal scaling (percent, default 100).
    pub h_scale: f64,
    /// Leading (line spacing).
    pub leading: f64,
    /// Text rise.
    pub rise: f64,
    /// Text render mode (0 fill, 1 stroke, …).
    pub render_mode: u8,
}

impl Default for GState {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            line_width: 1.0,
            line_cap: 0,
            line_join: 0,
            miter_limit: 10.0,
            dash: (Vec::new(), 0.0),
            rendering_intent: 0,
            flatness: 1.0,
            smoothness: 0.0,
            stroke_adjust: false,
            blend_mode: selis_bytes::Bytes::copy_from_slice(b"Normal"),
            soft_mask: None,
            clip: Vec::new(),
            alpha_stroke: 1.0,
            alpha_fill: 1.0,
            alpha_is_shape: false,
            overprint_stroke: false,
            overprint_fill: false,
            overprint_mode: 0,
            fill_colour: [0.0, 0.0, 0.0],
            stroke_colour: [0.0, 0.0, 0.0],
            fill_cs: selis_bytes::Bytes::copy_from_slice(b"DeviceRGB"),
            stroke_cs: selis_bytes::Bytes::copy_from_slice(b"DeviceRGB"),
            fill_pattern: None,
            stroke_pattern: None,
            text_font: None,
            text_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            h_scale: 100.0,
            leading: 0.0,
            rise: 0.0,
            render_mode: 0,
        }
    }
}

impl GState {
    /// The identity state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the CTM to `ctm` (the `cm` operator).
    pub fn set_ctm(&mut self, ctm: Matrix) {
        self.ctm = ctm;
    }

    /// Concatenate `m` onto the CTM (`cm` semantics: CTM' = CTM × m).
    pub fn concat_ctm(&mut self, m: Matrix) {
        self.ctm = self.ctm.then(m);
    }

    /// Set the line width.
    pub fn set_line_width(&mut self, w: f64) {
        self.line_width = w;
    }

    /// Merge `/ExtGState` dictionary keys into the state.
    ///
    /// Unknown keys are ignored (per spec, unrecognised keys are ignored).
    pub fn merge_ext_gstate(&mut self, pairs: &[(selis_bytes::Bytes, crate::dispatch::Operand)]) {
        for (key, val) in pairs {
            match key.as_slice() {
                b"LW" => {
                    if let crate::dispatch::Operand::Num(v) = val {
                        self.line_width = *v;
                    }
                }
                b"LC" => {
                    if let crate::dispatch::Operand::Num(v) = val {
                        self.line_cap = cap_join(*v);
                    }
                }
                b"LJ" => {
                    if let crate::dispatch::Operand::Num(v) = val {
                        self.line_join = cap_join(*v);
                    }
                }
                b"ML" => {
                    if let crate::dispatch::Operand::Num(v) = val {
                        self.miter_limit = *v;
                    }
                }
                b"D" => {
                    if let crate::dispatch::Operand::Arr(items) = val {
                        self.dash = (dash_array(items), self.dash.1);
                    }
                }
                b"CA" => {
                    if let crate::dispatch::Operand::Num(v) = val {
                        self.alpha_stroke = *v;
                    }
                }
                b"ca" => {
                    if let crate::dispatch::Operand::Num(v) = val {
                        self.alpha_fill = *v;
                    }
                }
                b"BM" => {
                    if let crate::dispatch::Operand::Name(n) = val {
                        self.blend_mode = n.clone();
                    }
                }
                b"SMask" => {
                    if let crate::dispatch::Operand::Name(n) = val {
                        self.soft_mask = Some(n.clone());
                    }
                }
                b"AIS" => {
                    if let crate::dispatch::Operand::Bool(_) = val {
                        self.alpha_is_shape = true;
                    }
                }
                b"OP" => {
                    if let crate::dispatch::Operand::Bool(b) = val {
                        self.overprint_stroke = *b;
                    }
                }
                b"op" => {
                    if let crate::dispatch::Operand::Bool(b) = val {
                        self.overprint_fill = *b;
                    }
                }
                b"OPM" => {
                    if let crate::dispatch::Operand::Num(v) = val {
                        self.overprint_mode = if *v >= 1.0 { 1 } else { 0 };
                    }
                }
                _ => {} // unrecognised key: ignored per spec
            }
        }
    }
}

fn dash_array(items: &[crate::dispatch::Operand]) -> Vec<f64> {
    items
        .iter()
        .filter_map(|o| match o {
            crate::dispatch::Operand::Num(v) => Some(*v),
            _ => None,
        })
        .collect()
}

/// A line cap/join value (0..=2); clamp then truncate, exact in range.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn cap_join(v: f64) -> u8 {
    v.clamp(0.0, 2.0) as u8
}

/// The `q`/`Q` graphics state stack, with a depth budget.
#[derive(Debug)]
pub struct GStateStack {
    /// The stack of saved states, outermost first.
    stack: Vec<GState>,
    /// The maximum stack depth.
    max_depth: u64,
}

impl GStateStack {
    /// A stack with the default viewer depth budget.
    #[must_use]
    pub fn new() -> Self {
        let budget = Budget::profile(selis_sandbox::Surface::Viewer);
        Self {
            stack: Vec::new(),
            max_depth: budget.limit(selis_sandbox::Resource::Depth),
        }
    }

    /// A stack with an explicit depth budget (tests use a tight one).
    #[must_use]
    pub fn with_max_depth(max_depth: u64) -> Self {
        Self {
            stack: Vec::new(),
            max_depth,
        }
    }

    /// Push the current state (`q`).
    ///
    /// # Errors
    ///
    /// `BUDGET_DEPTH` when the stack exceeds the depth budget.
    pub fn push(&mut self, state: &GState, g: &mut BudgetGuard<'_>) -> Result<()> {
        if self.stack.len() >= usize::try_from(self.max_depth).unwrap_or(usize::MAX) {
            return Err(err!(
                Code::BudgetDepth,
                during = "gstate-stack",
                detail = "too many nested q"
            ));
        }
        self.stack.push(state.clone());
        Ok(())
    }

    /// Pop the saved state (`Q`).
    ///
    /// # Errors
    ///
    /// `BUDGET_DEPTH` on underflow (`Q` with no matching `q`).
    pub fn pop(&mut self) -> Result<GState> {
        self.stack.pop().ok_or_else(|| {
            err!(
                Code::BudgetDepth,
                during = "gstate-stack",
                detail = "Q without matching q"
            )
        })
    }

    /// The current depth.
    #[must_use]
    pub fn len(&self) -> usize {
        self.stack.len()
    }

    /// Whether the stack is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }
}

impl Default for GStateStack {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]

    use super::*;
    use selis_sandbox::{CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    #[test]
    fn push_pop_roundtrip() {
        let mut g = guard();
        let mut stack = GStateStack::new();
        let mut state = GState::new();
        state.set_line_width(5.0);
        stack.push(&state, &mut g).expect("push");
        // Modify the current state after pushing.
        state.set_line_width(1.0);
        let popped = stack.pop().expect("pop");
        assert_eq!(popped.line_width, 5.0);
    }

    #[test]
    fn underflow_is_a_typed_error() {
        let mut stack = GStateStack::new();
        let e = stack.pop().expect_err("underflow");
        assert_eq!(e.code(), Code::BudgetDepth);
    }

    #[test]
    fn depth_budget_is_enforced() {
        let mut g = guard();
        let mut stack = GStateStack::with_max_depth(2);
        let state = GState::new();
        stack.push(&state, &mut g).expect("push 1");
        stack.push(&state, &mut g).expect("push 2");
        let e = stack.push(&state, &mut g).expect_err("depth");
        assert_eq!(e.code(), Code::BudgetDepth);
    }

    #[test]
    fn ext_gstate_merge() {
        let mut state = GState::new();
        let pairs = vec![
            (
                selis_bytes::Bytes::copy_from_slice(b"LW"),
                crate::dispatch::Operand::Num(2.5),
            ),
            (
                selis_bytes::Bytes::copy_from_slice(b"CA"),
                crate::dispatch::Operand::Num(0.5),
            ),
        ];
        state.merge_ext_gstate(&pairs);
        assert_eq!(state.line_width, 2.5);
        assert_eq!(state.alpha_stroke, 0.5);
    }

    #[test]
    fn ext_gstate_overprint_is_parsed() {
        let mut state = GState::new();
        let pairs = vec![
            (
                selis_bytes::Bytes::copy_from_slice(b"OP"),
                crate::dispatch::Operand::Bool(true),
            ),
            (
                selis_bytes::Bytes::copy_from_slice(b"op"),
                crate::dispatch::Operand::Bool(false),
            ),
            (
                selis_bytes::Bytes::copy_from_slice(b"OPM"),
                crate::dispatch::Operand::Num(1.0),
            ),
        ];
        state.merge_ext_gstate(&pairs);
        assert!(state.overprint_stroke);
        assert!(!state.overprint_fill);
        assert_eq!(state.overprint_mode, 1);
    }
}
