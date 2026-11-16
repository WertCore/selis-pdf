//! The re-flow: the text that follows an edited run on the same line is
//! pushed along by the width the edit added.
//!
//! The edit replaces a run of text with a run of text. When the new run is
//! wider, the text that follows it on the same line would be drawn over it —
//! the edit would be a smudge, not a change.
//!
//! The push-along is a translation of the text matrix, so it is written as a
//! translation:
//!
//! * after the edited show operation, `delta 0 Td` — the line matrix carries
//!   the shift from there on, so everything that follows on the same line
//!   moves with it;
//! * the first positioning operator that starts a NEW line takes the shift
//!   back out of it, so the lines below stay where they were.
//!
//! A `Tm` is absolute, so it is told about the shift directly (its `e` grows
//! by the delta) when it continues the edited line, and ignored when it
//! starts a new one — an absolute position discards the shift by itself.
//!
//! Nothing else in the stream moves: the shift is a number written into the
//! stream, and the arithmetic is the text matrix's own.

use std::ops::Range;

use crate::{copy_to, is_regular_start, Cursor};

/// How far apart two baselines must be before they are different lines.
const EPSILON: f64 = 1e-6;

/// Apply the re-flow to a content stream.
///
/// `span` is the edited operand's byte range in `content`, and `operand` is
/// its replacement. `delta` is the shift, in unscaled text space units: the
/// width the edit added, measured in the page's own font.
///
/// # Budget
///
/// The stream is read once; the output is the input with the replacement and
/// a handful of numbers written into it, so the work is bounded by the
/// stream's length.
///
/// # Malformed Input
///
/// A stream whose positioning operators cannot be read is copied through as
/// it stands: the re-flow writes a translation, but does not guess where an
/// unreadable operator puts the text.
pub(crate) fn apply(content: &[u8], span: Range<usize>, operand: &[u8], delta: f64) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(content.len().saturating_add(32));
    let mut cur = Cursor::new(content);
    // How much of the input has been copied out.
    let mut copied = 0usize;
    // Where the pending operand run starts in `out`, so the operands of a
    // positioning operator can be rewritten.
    let mut operand_start: Option<usize> = None;
    let mut numbers: Vec<f64> = Vec::new();
    // The line matrix's translation: where the current line starts.
    let mut line_x = 0.0f64;
    let mut line_y = 0.0f64;
    let mut leading = 0.0f64;
    // The shift, once the edited show operation has been written.
    let mut shift: Option<Shift> = None;
    // The edited operand has been replaced; the next operator draws it.
    let mut edited = false;
    // Whether the edited operand has been replaced yet.
    let mut replaced = false;

    loop {
        cur.skip_ws_and_comments();
        let Some(byte) = cur.peek() else { break };
        if !replaced && cur.pos() == span.start {
            // The edited operand: the replacement goes in its place, and the
            // original's bytes are stepped over so they are never copied.
            // The whitespace that separated it from its operator is still
            // un-copied, and belongs before the replacement.
            if span.start > copied {
                out.extend_from_slice(content.get(copied..span.start).unwrap_or_default());
            }
            out.extend_from_slice(operand);
            cur.seek(span.end);
            copied = cur.pos();
            replaced = true;
            // The next operator draws it.
            edited = true;
            continue;
        }
        match byte {
            b'(' => {
                let _ = cur.read_literal_string();
                copy_to(&mut out, &mut cur, &mut copied);
            }
            b'<' => {
                if cur.peek_at(1) == Some(b'<') {
                    cur.skip_dict();
                } else {
                    let _ = cur.read_hex_string();
                }
                copy_to(&mut out, &mut cur, &mut copied);
            }
            b'[' | b']' => {
                cur.bump();
                copy_to(&mut out, &mut cur, &mut copied);
            }
            b'/' => {
                let _ = cur.read_name_token();
                copy_to(&mut out, &mut cur, &mut copied);
            }
            b'%' => {
                cur.skip_comment();
                copy_to(&mut out, &mut cur, &mut copied);
            }
            b if b.is_ascii_digit() || b == b'+' || b == b'-' || b == b'.' => {
                let value = cur.read_number();
                if operand_start.is_none() {
                    operand_start = Some(out.len());
                }
                numbers.push(value);
                copy_to(&mut out, &mut cur, &mut copied);
            }
            b if is_regular_start(b) => {
                let word = cur.read_word();
                let op = String::from_utf8_lossy(&word).to_string();
                match op.as_str() {
                    "BT" => {
                        line_x = 0.0;
                        line_y = 0.0;
                        shift = None;
                    }
                    "TL" => {
                        leading = numbers.first().copied().unwrap_or(0.0);
                    }
                    "Td" | "TD" => {
                        let tx = numbers.first().copied().unwrap_or(0.0);
                        let ty = numbers.get(1).copied().unwrap_or(0.0);
                        if op == "TD" {
                            leading = -ty;
                        }
                        let new_y = line_y + ty;
                        // A `Td` that stays on the line keeps the shift (the
                        // line matrix already carries it); a `Td` that starts a
                        // new line takes it back out, so the line below starts
                        // where it did before the edit.
                        if let Some(active) = shift {
                            if (new_y - active.baseline).abs() > EPSILON {
                                rewrite_operands(&mut out, operand_start, &[tx - active.delta, ty]);
                                line_x = line_x + tx - active.delta;
                                shift = None;
                            } else {
                                line_x += tx;
                            }
                        } else {
                            line_x += tx;
                        }
                        line_y = new_y;
                    }
                    "Tm" => {
                        let m = [
                            numbers.first().copied().unwrap_or(1.0),
                            numbers.get(1).copied().unwrap_or(0.0),
                            numbers.get(2).copied().unwrap_or(0.0),
                            numbers.get(3).copied().unwrap_or(1.0),
                            numbers.get(4).copied().unwrap_or(0.0),
                            numbers.get(5).copied().unwrap_or(0.0),
                        ];
                        // An absolute position on the SAME line carries the
                        // shift; one on a different line discards it.
                        let same_line = shift.is_some_and(|shift| {
                            (m[5] - shift.baseline).abs() <= EPSILON
                                && m[1].abs() <= EPSILON
                                && m[2].abs() <= EPSILON
                        });
                        if same_line {
                            if let Some(shift) = shift {
                                let mut moved = m;
                                moved[4] += shift.delta;
                                rewrite_operands(&mut out, operand_start, &moved);
                                line_x = moved[4];
                                line_y = moved[5];
                            }
                        } else {
                            shift = None;
                            line_x = m[4];
                            line_y = m[5];
                        }
                    }
                    _ => {}
                }
                copy_to(&mut out, &mut cur, &mut copied);
                // The edited show operation: the shift goes after it.
                if edited {
                    emit_shift(&mut out, delta);
                    shift = Some(Shift {
                        delta,
                        baseline: line_y,
                    });
                    edited = false;
                }
                // The operators that move to the next line take the shift back
                // out of it, so the lines below stay where they were.
                if matches!(op.as_str(), "T*" | "'" | "\"") {
                    if let Some(active) = shift {
                        emit_shift(&mut out, -active.delta);
                        line_x -= active.delta;
                        shift = None;
                    }
                    line_y -= leading;
                }
                if op == "ET" {
                    // The text object ends, and with it the line matrix.
                    shift = None;
                }
                operand_start = None;
                numbers.clear();
            }
            _ => {
                cur.bump();
                copy_to(&mut out, &mut cur, &mut copied);
            }
        }
    }
    out
}

/// The shift, once it is live.
#[derive(Clone, Copy)]
struct Shift {
    /// How far the text that follows moves, in unscaled text space units.
    delta: f64,
    /// The baseline of the edited line, in unscaled text space units.
    baseline: f64,
}

/// Write a horizontal translation into the stream.
fn emit_shift(out: &mut Vec<u8>, delta: f64) {
    out.extend_from_slice(format!(" {} 0 Td", format_number(delta)).as_bytes());
}

/// Replace the operands of the operator that is about to be written.
fn rewrite_operands(out: &mut Vec<u8>, start: Option<usize>, values: &[f64]) {
    let Some(start) = start else { return };
    if start > out.len() {
        return;
    }
    out.truncate(start);
    let text: Vec<String> = values.iter().map(|value| format_number(*value)).collect();
    out.extend_from_slice(format!(" {}", text.join(" ")).as_bytes());
}

/// A number, as the stream writes it.
fn format_number(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    let mut text = format!("{value:.4}");
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    if text == "-0" {
        "0".to_string()
    } else {
        text
    }
}
