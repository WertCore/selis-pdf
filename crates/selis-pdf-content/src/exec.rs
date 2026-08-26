//! The execution pass: dispatch → display list (SL-2.CONT.05).
//!
//! Drives the `Interpreter` over a content stream, tracks the graphics state
//! and text state, and emits a `DisplayList` with positioned operations.
//! This is the bridge between the token-level dispatcher and the raster
//! backend that the engine uses.

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_geom::{Matrix, Point, Rect};
use selis_sandbox::BudgetGuard;

use crate::dispatch::{Dispatch, Interpreter, Operand};
use crate::display_list::{DisplayList, GlyphRun, Op, ResolvedState};
use crate::gstate::{GState, GStateStack};
use crate::path::{PaintOp, Path};
use crate::text::{self, TextState};

/// An XObject target resolved from a `Do` resource name.
#[derive(Debug, Clone, PartialEq)]
pub enum DoTarget {
    /// An image XObject's decoded straight-RGBA samples.
    Image {
        /// The image width in pixels.
        width: u32,
        /// The image height in pixels.
        height: u32,
        /// The decoded RGBA8 samples.
        rgba8: Bytes,
    },
}

/// Execute a content stream into a display list.
///
/// `font_width` resolves a glyph's advance width (1000/em units) for text
/// positioning; `resolve_do` resolves a `Do` resource name to an XObject.
/// The engine provides both from the document's resources.
pub fn execute(
    content: &[u8],
    font_width: &dyn Fn(&Bytes, u16) -> f64,
    resolve_do: &dyn Fn(&Bytes) -> Option<DoTarget>,
    g: &mut BudgetGuard<'_>,
) -> Result<DisplayList> {
    let mut interp = Interpreter::new(content);
    interp.run(g)?;
    let mut dl = DisplayList::new();
    let mut gstate = GState::new();
    let mut stack = GStateStack::new();
    let mut text_state = TextState::default();
    let mut current_path = Path::new();
    let mut paint_op = PaintOp::None;
    let mut clip_op = None;

    for dispatch in &interp.ops {
        let (op_name, operands) = match dispatch {
            Dispatch::Ignored { .. } => continue,
            Dispatch::Handled(op) => (&op.name, &op.operands),
        };
        match op_name.as_str() {
            // Graphics state.
            "q" => {
                stack.push(&gstate, g)?;
            }
            "Q" => {
                if let Ok(gs) = stack.pop() {
                    gstate = gs;
                }
            }
            "cm" => {
                let a = num(operands, 0);
                let b = num(operands, 1);
                let c = num(operands, 2);
                let d = num(operands, 3);
                let e = num(operands, 4);
                let f = num(operands, 5);
                gstate.ctm = gstate.ctm.then(Matrix::new(a, b, c, d, e, f));
            }
            "w" => gstate.line_width = num(operands, 0),
            "J" => gstate.line_cap = clamp_u8(num(operands, 0)),
            "j" => gstate.line_join = clamp_u8(num(operands, 0)),
            "M" => gstate.miter_limit = num(operands, 0),
            "d" => {
                // d dash_array dash_phase
                if let Some(Operand::Arr(items)) = operands.first() {
                    let dash: Vec<f64> = items
                        .iter()
                        .filter_map(|o| {
                            if let Operand::Num(v) = o {
                                Some(*v)
                            } else {
                                None
                            }
                        })
                        .collect();
                    let phase = num(operands, 1);
                    gstate.dash = (dash, phase);
                }
            }
            "ri" => {} // rendering intent: ignored
            "gs" => {
                // ExtGState name — the engine resolves it. Ignored here.
            }

            // Colour — device RGB.
            "G" => gstate.stroke_colour = [num(operands, 0), num(operands, 0), num(operands, 0)],
            "g" => gstate.fill_colour = [num(operands, 0), num(operands, 0), num(operands, 0)],
            "RG" => gstate.stroke_colour = [num(operands, 0), num(operands, 1), num(operands, 2)],
            "rg" => gstate.fill_colour = [num(operands, 0), num(operands, 1), num(operands, 2)],
            "K" => {
                let c = num(operands, 0);
                let m = num(operands, 1);
                let y = num(operands, 2);
                let k = num(operands, 3);
                gstate.stroke_colour = cmyk_to_rgb(c, m, y, k);
            }
            "k" => {
                let c = num(operands, 0);
                let m = num(operands, 1);
                let y = num(operands, 2);
                let k = num(operands, 3);
                gstate.fill_colour = cmyk_to_rgb(c, m, y, k);
            }
            "CS" | "cs" | "SC" | "SCN" | "sc" | "scn" => {
                // Colour space handling — deferred to SL-2.CONT.09.
            }

            // Path construction.
            "m" => {
                flush_path(&mut dl, &mut current_path, paint_op, clip_op, &gstate);
                paint_op = PaintOp::None;
                clip_op = None;
                current_path.move_to(Point::new(num(operands, 0), num(operands, 1)));
            }
            "l" => current_path.line_to(Point::new(num(operands, 0), num(operands, 1))),
            "c" => {
                current_path.cubic_to(
                    Point::new(num(operands, 0), num(operands, 1)),
                    Point::new(num(operands, 2), num(operands, 3)),
                    Point::new(num(operands, 4), num(operands, 5)),
                );
            }
            "v" => {
                current_path.cubic_first(
                    Point::new(num(operands, 0), num(operands, 1)),
                    Point::new(num(operands, 2), num(operands, 3)),
                );
            }
            "y" => {
                current_path.cubic_second(
                    Point::new(num(operands, 0), num(operands, 1)),
                    Point::new(num(operands, 2), num(operands, 3)),
                );
            }
            "h" => current_path.close(),
            "re" => {
                flush_path(&mut dl, &mut current_path, paint_op, clip_op, &gstate);
                paint_op = PaintOp::None;
                clip_op = None;
                current_path.rectangle(Rect::new(
                    num(operands, 0),
                    num(operands, 1),
                    num(operands, 0) + num(operands, 2),
                    num(operands, 1) + num(operands, 3),
                ));
            }

            // Path painting.
            "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" => {
                paint_op = match op_name.as_str() {
                    "S" => PaintOp::Stroke,
                    "s" => PaintOp::CloseStroke,
                    "f" | "F" => PaintOp::Fill,
                    "f*" => PaintOp::FillEvenOdd,
                    "B" => PaintOp::FillStroke,
                    "B*" => PaintOp::FillStrokeEvenOdd,
                    "b" => PaintOp::CloseFillStroke,
                    "b*" => PaintOp::CloseFillStrokeEvenOdd,
                    _ => PaintOp::None,
                };
                if !current_path.is_degenerate() {
                    flush_path(&mut dl, &mut current_path, paint_op, clip_op, &gstate);
                }
                current_path = Path::new();
                paint_op = PaintOp::None;
                clip_op = None;
            }
            "W" => clip_op = Some(false), // non-zero
            "W*" => clip_op = Some(true), // even-odd

            // Text.
            "BT" => text_state = TextState::default(),
            "ET" => {}
            n if text_op_names().contains(&n) => {
                let font = text_state.font.clone();
                let empty = Bytes::new();
                let font_slice: &Bytes = font.as_ref().unwrap_or(&empty);
                let glyphs = text::process(&mut text_state, n, operands, false, &|code| {
                    font_width(font_slice, code)
                });
                for glyph in glyphs {
                    let state = ResolvedState::from(&gstate);
                    // gstate.ctm is the CTM in user space; the text glyph's
                    // `at` is in user space (the text matrix maps text → user).
                    dl.push(Op::Text {
                        at: glyph.at,
                        state,
                        runs: vec![GlyphRun {
                            font: glyph.font,
                            size: glyph.size,
                            glyphs: vec![glyph.code],
                        }],
                    });
                    g.charge_one(selis_sandbox::Resource::Objects)?;
                }
            }

            // XObjects.
            "Do" => {
                if let Some(Operand::Name(name)) = operands.first() {
                    if let Some(DoTarget::Image {
                        width,
                        height,
                        rgba8,
                    }) = resolve_do(name)
                    {
                        // The image fills the unit square in user space,
                        // transformed by the CTM.
                        let p0 = gstate.ctm.apply(Point::new(0.0, 0.0));
                        let p1 = gstate.ctm.apply(Point::new(1.0, 1.0));
                        let state = ResolvedState::from(&gstate);
                        dl.push(Op::Image {
                            rgba8,
                            width,
                            height,
                            rect: Rect::new(p0.x, p0.y, p1.x, p1.y),
                            state,
                        });
                        g.charge_one(selis_sandbox::Resource::Objects)?;
                    }
                }
            }

            // Everything else — ignored.
            _ => {}
        }
    }
    // Flush any remaining path.
    flush_path(&mut dl, &mut current_path, paint_op, clip_op, &gstate);
    Ok(dl)
}

/// The set of text operator names.
fn text_op_names() -> &'static [&'static str] {
    &[
        "Tj", "TJ", "'", "\"", "Td", "TD", "Tm", "T*", "Tf", "Tz", "Tc", "Tw", "TL", "Ts", "Tr",
    ]
}

/// Flush the current path (if non-degenerate and has a paint op).
fn flush_path(
    dl: &mut DisplayList,
    path: &mut Path,
    paint: PaintOp,
    clip: Option<bool>,
    gs: &GState,
) {
    if path.is_degenerate() || !paint.paints() {
        return;
    }
    let path = std::mem::take(path);
    let state = ResolvedState {
        ctm: gs.ctm,
        line_width: gs.line_width,
        line_cap: gs.line_cap,
        line_join: gs.line_join,
        fill: gs.fill_colour,
        stroke: gs.stroke_colour,
        alpha_fill: gs.alpha_fill,
        alpha_stroke: gs.alpha_stroke,
    };
    let op = match paint {
        PaintOp::Fill | PaintOp::FillEvenOdd => Op::Fill { path, state },
        PaintOp::Stroke | PaintOp::CloseStroke => Op::Stroke { path, state },
        PaintOp::FillStroke
        | PaintOp::FillStrokeEvenOdd
        | PaintOp::CloseFillStroke
        | PaintOp::CloseFillStrokeEvenOdd => Op::FillStroke { path, state },
        _ => return,
    };
    dl.push(op);
}

/// A numeric operand, defaulting to 0.
fn num(operands: &[Operand], i: usize) -> f64 {
    match operands.get(i) {
        Some(Operand::Num(v)) => *v,
        _ => 0.0,
    }
}

/// Clamp a value to a byte.
fn clamp_u8(v: f64) -> u8 {
    if !v.is_finite() || v < 0.0 {
        return 0;
    }
    let t = v.trunc();
    if t >= f64::from(u8::MAX) {
        return u8::MAX;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        t as u8
    }
}

/// Simple CMYK → RGB conversion (inverted: 1-v).
fn cmyk_to_rgb(c: f64, m: f64, y: f64, k: f64) -> [f64; 3] {
    let r = (1.0 - c) * (1.0 - k);
    let g = (1.0 - m) * (1.0 - k);
    let b = (1.0 - y) * (1.0 - k);
    [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    fn const_width(_font: &Bytes, _code: u16) -> f64 {
        500.0
    }

    fn no_do(_name: &Bytes) -> Option<DoTarget> {
        None
    }

    #[test]
    fn a_path_and_fill_produces_a_fill_op() {
        let mut g = guard();
        let dl = execute(
            b"0 0 m 0 100 l 100 100 l 100 0 l h 0 g f",
            &const_width,
            &no_do,
            &mut g,
        )
        .expect("execute");
        assert_eq!(dl.ops.len(), 1);
        assert!(matches!(dl.ops[0], Op::Fill { .. }));
    }

    #[test]
    fn set_colour_modifies_the_fill_state() {
        let mut g = guard();
        let dl = execute(
            b"0 0 m 0 100 l 100 100 l 100 0 l h 0.5 0.3 0.1 rg f",
            &const_width,
            &no_do,
            &mut g,
        )
        .expect("execute");
        if let Op::Fill { state, .. } = &dl.ops[0] {
            assert!((state.fill[0] - 0.5).abs() < 0.01);
            assert!((state.fill[1] - 0.3).abs() < 0.01);
        } else {
            panic!("expected fill");
        }
    }

    #[test]
    fn text_produces_a_text_op() {
        let mut g = guard();
        // BT /F1 12 Tf 0 0 Td (A) Tj ET
        let dl = execute(
            b"BT /F1 12 Tf 0 0 Td (A) Tj ET",
            &const_width,
            &no_do,
            &mut g,
        )
        .expect("execute");
        assert_eq!(dl.ops.len(), 1);
        if let Op::Text { runs, .. } = &dl.ops[0] {
            assert_eq!(runs.len(), 1);
            assert_eq!(runs[0].glyphs, vec![65]);
            assert_eq!(&runs[0].font.as_slice()[..], b"F1");
        } else {
            panic!("expected text");
        }
    }

    #[test]
    fn q_and_q_restore_state() {
        let mut g = guard();
        // q 0.5 0 0 0.5 0 0 cm Q 0 0 m 0 100 l 100 100 l 100 0 l h 0 g f
        let dl = execute(
            b"q 0.5 0 0 0.5 0 0 cm Q 0 0 m 0 100 l 100 100 l 100 0 l h 0 g f",
            &const_width,
            &no_do,
            &mut g,
        )
        .expect("execute");
        // After Q, the CTM is restored to identity.
        if let Op::Fill { state, .. } = &dl.ops[0] {
            assert!((state.ctm.a - 1.0).abs() < 0.01);
        } else {
            panic!("expected fill");
        }
    }
}
