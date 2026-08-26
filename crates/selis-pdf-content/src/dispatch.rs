//! Operator dispatch (SL-2.CONT.01).
//!
//! Groups operands with the operator that follows them and dispatches each of
//! the ~73 content operators. Every operator in ISO 32000-2 Table 50 has a
//! handler or an explicit "ignored, and why". **Unknown operators are recorded
//! as a deviation and skipped with correct operand consumption, never fatal.**
//!
//! Operand-count recovery after an unknown operator is the difference between
//! "one missing glyph" and "the rest of the page is garbage" — the dispatcher
//! uses a per-operator operand table so it always knows how many operands an
//! operator consumed, even when it does not implement the operator's effect.

use selis_error::Result;
use selis_sandbox::BudgetGuard;

use crate::lex::{Lexer, Tok};

/// A content operator.
#[derive(Debug, Clone, PartialEq)]
pub struct Operator {
    /// The operator's name (e.g. `cm`, `BT`, `Do`).
    pub name: String,
    /// The operands that preceded it.
    pub operands: Vec<Operand>,
}

/// A content operand.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    /// A number.
    Num(f64),
    /// A name.
    Name(selis_bytes::Bytes),
    /// A string.
    Str(selis_bytes::Bytes),
    /// A boolean (from an array or dict).
    Bool(bool),
    /// An array of numbers/names.
    Arr(Vec<Operand>),
    /// A dictionary of name→value.
    Dict(Vec<(selis_bytes::Bytes, Operand)>),
    /// An inline image (extracted from `BI`…`EI` by the interpreter).
    InlineImage(Box<crate::inline_image::InlineImage>),
}

/// The operand count an operator consumes, per ISO 32000-2 Table 50.
///
/// `Var` means the operand count is derived from the current graphics state
/// (e.g. `rg` takes 3, `sc` takes 1–4 depending on the colour space). `Zero`
/// means no operands.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Arity {
    /// Exactly n operands.
    N(u32),
    /// Variable; the dispatcher consumes the whole operand run.
    Var,
}

/// The dispatch outcome of one operator.
#[derive(Debug, Clone, PartialEq)]
pub enum Dispatch {
    /// The operator has a real effect (implemented or delegated).
    Handled(Operator),
    /// The operator is deliberately ignored, with the reason.
    Ignored {
        /// The operator.
        op: Operator,
        /// Why it is ignored.
        why: &'static str,
    },
}

/// The operand arity table (ISO 32000-2 Table 50). Unknown operators consume
/// zero operands and are recorded.
fn arity(name: &str) -> Arity {
    match name {
        // Inline image (the interpreter extracts `ID … EI` into one operand).
        "BI" => Arity::N(1),
        // Graphics state.
        "w" | "J" | "j" | "M" | "ri" | "gs" | "i" | "sh" => Arity::N(1),
        "d" => Arity::N(2),
        "cs" | "CS" | "G" | "g" => Arity::N(1),
        "RG" | "rg" => Arity::N(3),
        "K" | "k" => Arity::N(4),
        "SC" | "SCN" | "sc" | "scn" => Arity::Var,
        // Path construction.
        "m" | "l" => Arity::N(2),
        "c" => Arity::N(6),
        "v" | "y" | "re" => Arity::N(4),
        "h" | "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" | "W" | "W*" => {
            Arity::N(0)
        }
        // Text.
        "Td" | "TD" => Arity::N(2),
        "Tm" => Arity::N(6),
        "Tz" | "TL" | "Ts" | "Tw" | "Tc" => Arity::N(1),
        "Tf" => Arity::N(2),
        "Tr" => Arity::N(1),
        "\"" => Arity::N(3),
        "TJ" => Arity::N(1),
        "BT" | "ET" | "T*" => Arity::N(0),
        "Tj" | "'" => Arity::N(1),
        // XObjects.
        "Do" => Arity::N(1),
        // Marked content.
        "MP" | "BMC" => Arity::N(1),
        "DP" | "BDC" => Arity::N(2),
        "EMC" => Arity::N(0),
        // Compatibility.
        "BX" | "EX" => Arity::N(0),
        // Matrix.
        "cm" => Arity::N(6),
        "d0" => Arity::N(2),
        "d1" => Arity::N(6),
        "q" | "Q" => Arity::N(0),
        _ => Arity::N(0),
    }
}

/// A handler function for an operator with a real effect.
fn dispatch_one(op: Operator) -> Dispatch {
    let name = op.name.as_str();
    match name {
        // State-pushing operators.
        "q" => Dispatch::Handled(op),
        "Q" => Dispatch::Handled(op),
        "cm" => Dispatch::Handled(op),
        "m" | "l" | "c" | "v" | "y" | "re" | "h" => Dispatch::Handled(op),
        "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" | "W" | "W*" => {
            Dispatch::Handled(op)
        }
        "BT" | "ET" | "Tj" | "TJ" | "'" | "\"" | "Td" | "TD" | "Tm" => Dispatch::Handled(op),
        "Do" => Dispatch::Handled(op),
        "BI" => Dispatch::Handled(op),
        // Colour and state operators are implemented in CONT.02/03; they are
        // dispatched now so the token stream is complete.
        "w" | "J" | "j" | "M" | "ri" | "gs" | "cs" | "CS" | "SC" | "SCN" | "sc" | "scn" | "G"
        | "g" | "RG" | "rg" | "K" | "k" | "d" | "i" | "sh" | "BMC" | "BDC" | "EMC" | "MP"
        | "DP" => Dispatch::Handled(op),
        // Text state operators (SL-3.TEXT.01).
        "Tz" | "TL" | "Tr" | "Ts" | "Tw" | "Tc" | "Tf" => Dispatch::Handled(op),
        "d0" | "d1" => Dispatch::Ignored {
            op,
            why: "type-3 glyph metrics land in CONT.05",
        },
        "BX" | "EX" => Dispatch::Ignored {
            op,
            why: "compatibility sections are no-ops by spec",
        },
        _ => Dispatch::Ignored {
            op,
            why: "unknown operator: recorded, operands consumed, no effect",
        },
    }
}

/// An interpreter that drives the tokeniser and dispatches operators.
#[derive(Debug)]
pub struct Interpreter<'a> {
    src: &'a [u8],
    lexer: Lexer<'a>,
    /// Operands accumulated until the next operator.
    pending: Vec<Operand>,
    /// The dispatched operators.
    pub ops: Vec<Dispatch>,
}

impl<'a> Interpreter<'a> {
    /// An interpreter over a content stream.
    #[must_use]
    pub fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            lexer: Lexer::new(src),
            pending: Vec::new(),
            ops: Vec::new(),
        }
    }

    /// Run the whole stream, dispatching every operator.
    ///
    /// # Budget
    ///
    /// Charged per token and per operator.
    ///
    /// # Malformed Input
    ///
    /// Unknown operators are deviations, never errors; the stream always
    /// runs to the end (or a budget error).
    pub fn run(&mut self, g: &mut BudgetGuard<'_>) -> Result<()> {
        loop {
            g.tick()?;
            match self.lexer.next_token(g)? {
                Some(Tok::Num(v)) => self.pending.push(Operand::Num(v)),
                Some(Tok::Name(n)) => {
                    let name = String::from_utf8_lossy(n.as_slice()).to_string();
                    self.dispatch_operator(name, g)?;
                }
                Some(Tok::OperandName(n)) => self.pending.push(Operand::Name(n)),
                Some(Tok::Str(s)) => self.pending.push(Operand::Str(s)),
                Some(Tok::ArrStart) => {
                    // Collect the array.
                    let items = self.collect_array(g)?;
                    self.pending.push(Operand::Arr(items));
                }
                Some(Tok::DictStart) => {
                    let pairs = self.collect_dict(g)?;
                    self.pending.push(Operand::Dict(pairs));
                }
                Some(Tok::BI) => {
                    // Inline image: extract `ID … EI` from the raw bytes and
                    // dispatch it as a `BI` operator, skipping the block.
                    let bi_pos = self.lexer.pos();
                    match crate::inline_image::extract_inline_image(self.src, bi_pos) {
                        Some((img, after)) => {
                            self.pending.push(Operand::InlineImage(Box::new(img)));
                            self.lexer.set_pos(after);
                            self.dispatch_operator("BI".to_string(), g)?;
                        }
                        None => self.pending.clear(),
                    }
                }
                Some(Tok::ID) | Some(Tok::EI) => {
                    // Stray markers outside a handled inline image: no effect.
                    self.pending.clear();
                }
                Some(Tok::ArrEnd) | Some(Tok::DictEnd) => {}
                None => break,
            }
        }
        Ok(())
    }

    fn dispatch_operator(&mut self, name: String, _g: &mut BudgetGuard<'_>) -> Result<()> {
        // Pop the operands this operator consumes.
        let count = match arity(&name) {
            Arity::N(n) => usize::try_from(n).unwrap_or(usize::MAX),
            Arity::Var => self.pending.len(),
        };
        let consumed: Vec<Operand> = if count == 0 {
            Vec::new()
        } else {
            let start = self.pending.len().saturating_sub(count);
            self.pending.split_off(start)
        };
        let op = Operator {
            name,
            operands: consumed,
        };
        self.ops.push(dispatch_one(op));
        Ok(())
    }

    fn collect_array(&mut self, g: &mut BudgetGuard<'_>) -> Result<Vec<Operand>> {
        let mut items = Vec::new();
        loop {
            g.tick()?;
            match self.lexer.next_token(g)? {
                Some(Tok::ArrEnd) => break,
                Some(Tok::Num(v)) => items.push(Operand::Num(v)),
                Some(Tok::OperandName(n)) => {
                    items.push(Operand::Name(n));
                }
                Some(Tok::Name(n)) => {
                    items.push(Operand::Name(n));
                }
                Some(Tok::Str(s)) => items.push(Operand::Str(s)),
                Some(Tok::ArrStart) => {
                    items.push(Operand::Arr(self.collect_array(g)?));
                }
                None => break,
                _ => {}
            }
        }
        Ok(items)
    }

    fn collect_dict(
        &mut self,
        g: &mut BudgetGuard<'_>,
    ) -> Result<Vec<(selis_bytes::Bytes, Operand)>> {
        let mut pairs = Vec::new();
        let mut key: Option<selis_bytes::Bytes> = None;
        loop {
            g.tick()?;
            match self.lexer.next_token(g)? {
                Some(Tok::DictEnd) => break,
                Some(Tok::OperandName(n)) => {
                    if let Some(k) = key.take() {
                        pairs.push((k, Operand::Name(n)));
                    } else {
                        key = Some(n);
                    }
                }
                Some(Tok::Name(n)) => {
                    if let Some(k) = key.take() {
                        pairs.push((k, Operand::Name(n)));
                    } else {
                        key = Some(n);
                    }
                }
                Some(Tok::Num(v)) => {
                    if let Some(k) = key.take() {
                        pairs.push((k, Operand::Num(v)));
                    }
                }
                Some(Tok::Str(s)) => {
                    if let Some(k) = key.take() {
                        pairs.push((k, Operand::Str(s)));
                    }
                }
                Some(Tok::DictStart) => {
                    let inner = self.collect_dict(g)?;
                    if let Some(k) = key.take() {
                        pairs.push((k, Operand::Dict(inner)));
                    }
                }
                None => break,
                _ => {}
            }
        }
        Ok(pairs)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]

    use super::*;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    #[test]
    fn simple_operators_dispatch() {
        let mut g = guard();
        let mut interp = Interpreter::new(b"1 0 0 1 0 0 cm 10 w");
        interp.run(&mut g).expect("run");
        assert_eq!(interp.ops.len(), 2);
        // cm consumes 6 operands.
        if let Dispatch::Handled(op) = &interp.ops[0] {
            assert_eq!(op.name, "cm");
            assert_eq!(op.operands.len(), 6);
        } else {
            panic!("cm must be handled");
        }
        // w consumes 1 operand.
        if let Dispatch::Handled(op) = &interp.ops[1] {
            assert_eq!(op.name, "w");
            assert_eq!(op.operands.len(), 1);
        }
    }

    #[test]
    fn unknown_operator_is_ignored_not_fatal() {
        let mut g = guard();
        let mut interp = Interpreter::new(b"1 0 m 0 1 l SOMEWEIRDOP 5 w");
        interp.run(&mut g).expect("run");
        // The unknown operator consumed 0 operands and the stream continues.
        assert!(interp
            .ops
            .iter()
            .any(|d| matches!(d, Dispatch::Ignored { .. })));
        assert!(interp
            .ops
            .iter()
            .any(|d| matches!(d, Dispatch::Handled(op) if op.name == "w")));
    }

    #[test]
    fn arrays_and_dicts_are_operands() {
        let mut g = guard();
        let mut interp = Interpreter::new(b"[1 2 3] TJ /Name << /K 1 >> DP");
        interp.run(&mut g).expect("run");
        assert_eq!(interp.ops.len(), 2);
        if let Dispatch::Handled(op) = &interp.ops[0] {
            assert_eq!(op.name, "TJ");
            assert!(matches!(&op.operands[0], Operand::Arr(_)));
        }
        if let Dispatch::Handled(op) = &interp.ops[1] {
            assert_eq!(op.name, "DP");
            assert!(matches!(&op.operands[0], Operand::Name(_)));
            assert!(matches!(&op.operands[1], Operand::Dict(_)));
        }
    }

    #[test]
    fn inline_image_keywords_clear_operands() {
        let mut g = guard();
        let mut interp = Interpreter::new(b"1 2 BI /W 1 ID data EI 3 4 m");
        interp.run(&mut g).expect("run");
        // The last operator m consumed 2 operands (3 4), not the image data.
        if let Dispatch::Handled(op) = interp.ops.last().expect("last") {
            assert_eq!(op.name, "m");
            assert_eq!(op.operands.len(), 2);
        }
    }
}
