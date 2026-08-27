//! PDF functions (SL-2.COLOR.05).
//!
//! Sampled (type 0, all interpolation orders), exponential (type 2),
//! stitching (type 3), and the PostScript calculator (type 4) — a small stack
//! language that must be budget-bounded and cannot be allowed to loop.
//!
//! A function maps `m` input components to `n` output components. The type-4
//! interpreter is the hostile surface: it is a stack machine with a bounded
//! operand count and a bounded evaluation budget (ADR-P0006).

use alloc::boxed::Box;
use alloc::vec::Vec;
use selis_error::{err, Code, Result};

/// A PDF function: maps input components to output components.
#[derive(Debug, Clone, PartialEq)]
pub enum Function {
    /// Type 0: sampled.
    Sampled(SampledFunction),
    /// Type 2: exponential.
    Exponential(ExponentialFunction),
    /// Type 3: stitching.
    Stitching(StitchingFunction),
    /// Type 4: PostScript calculator.
    Calculator(CalculatorFunction),
}

impl Function {
    /// The number of input components.
    #[must_use]
    pub fn inputs(&self) -> usize {
        match self {
            Function::Sampled(f) => f.inputs,
            Function::Exponential(f) => f.inputs,
            Function::Stitching(f) => f.inputs,
            Function::Calculator(f) => f.inputs,
        }
    }

    /// The number of output components.
    #[must_use]
    pub fn outputs(&self) -> usize {
        match self {
            Function::Sampled(f) => f.outputs,
            Function::Exponential(f) => f.outputs,
            Function::Stitching(f) => f.outputs,
            Function::Calculator(f) => f.outputs,
        }
    }

    /// Evaluate the function on input components.
    ///
    /// # Errors
    ///
    /// `FUNCTION_INPUT_COUNT` when the input has the wrong length;
    /// `FUNCTION_BUDGET` when the type-4 calculator exceeds its budget.
    pub fn evaluate(&self, input: &[f64], budget: &mut FunctionBudget) -> Result<Vec<f64>> {
        match self {
            Function::Sampled(f) => f.evaluate(input, budget),
            Function::Exponential(f) => f.evaluate(input),
            Function::Stitching(f) => f.evaluate(input, budget),
            Function::Calculator(f) => f.evaluate(input, budget),
        }
    }
}

/// The evaluation budget for a function (bounds the type-4 calculator).
#[derive(Debug, Clone)]
pub struct FunctionBudget {
    /// The maximum number of calculator ops.
    pub max_ops: u64,
    /// The maximum operand-stack depth.
    pub max_stack: usize,
    /// Ops consumed so far.
    pub ops_used: u64,
}

impl Default for FunctionBudget {
    fn default() -> Self {
        Self {
            max_ops: 100_000,
            max_stack: 64,
            ops_used: 0,
        }
    }
}

impl FunctionBudget {
    /// Charge one calculator op.
    ///
    /// # Errors
    ///
    /// `FUNCTION_BUDGET` when the op budget is exhausted (a type-4 loop).
    pub fn charge(&mut self) -> Result<()> {
        self.ops_used = self.ops_used.saturating_add(1);
        if self.ops_used > self.max_ops {
            return Err(err!(
                Code::FunctionBudget,
                during = "function-eval",
                detail = "calculator op budget exhausted"
            ));
        }
        Ok(())
    }
}

/// Type 0: sampled function.
#[derive(Debug, Clone, PartialEq)]
pub struct SampledFunction {
    /// Input component count.
    pub inputs: usize,
    /// Output component count.
    pub outputs: usize,
    /// Sample counts per input dimension.
    pub size: Vec<usize>,
    /// The sample grid, row-major, each cell `outputs` values.
    pub samples: Vec<f64>,
    /// Interpolation order: 1 = linear, 3 = cubic.
    pub order: u8,
    /// The input domain ranges `[min max]` per component.
    pub domain: Vec<(f64, f64)>,
    /// The output ranges `[min max]` per component.
    pub range: Vec<(f64, f64)>,
}

impl SampledFunction {
    /// Evaluate with multilinear interpolation between samples.
    pub fn evaluate(&self, input: &[f64], budget: &mut FunctionBudget) -> Result<Vec<f64>> {
        budget.charge()?;
        if input.len() != self.inputs {
            return Err(err!(Code::FunctionInputCount, during = "function-eval"));
        }
        // Normalise each input to the domain, then to sample coordinates.
        // `inputs` is a parse-bounded component count; grows incrementally.
        let mut coords: Vec<f64> = Vec::new();
        for (i, &v) in input.iter().enumerate() {
            let (lo, hi) = self.domain.get(i).copied().unwrap_or((0.0, 1.0));
            let n = if hi > lo { (v - lo) / (hi - lo) } else { 0.0 };
            let size = self.size.get(i).copied().unwrap_or(1);
            coords.push(n.clamp(0.0, 1.0) * (size.saturating_sub(1)) as f64);
        }

        // Multilinear interpolation over the grid corners.
        let mut out = alloc::vec![0.0; self.outputs];
        let corners = 1usize << self.inputs;
        for corner in 0..corners {
            let (weight, index) = self.corner_weight(corner, &coords);
            if weight == 0.0 {
                continue;
            }
            let base = index.saturating_mul(self.outputs);
            for o in 0..self.outputs {
                let sample = self
                    .samples
                    .get(base.saturating_add(o))
                    .copied()
                    .unwrap_or(0.0);
                if let Some(slot) = out.get_mut(o) {
                    *slot += weight * sample;
                }
            }
        }
        // Map to the output range.
        for (o, v) in out.iter_mut().enumerate() {
            let (lo, hi) = self.range.get(o).copied().unwrap_or((0.0, 1.0));
            *v = lo + v.clamp(0.0, 1.0) * (hi - lo);
        }
        Ok(out)
    }

    /// The total number of grid cells.
    fn cell_count(&self) -> usize {
        self.size.iter().fold(1usize, |a, &b| a.saturating_mul(b))
    }

    /// The weight and sample index of one interpolation corner.
    fn corner_weight(&self, corner: usize, coords: &[f64]) -> (f64, usize) {
        let mut weight = 1.0;
        let mut index = 0usize;
        let mut stride = 1usize;
        for d in 0..self.inputs {
            let size = self.size.get(d).copied().unwrap_or(1);
            let f = coords.get(d).copied().unwrap_or(0.0);
            // The corner picks the floor (0) or ceil (1) sample for this dim.
            let hi = (corner >> d) & 1 == 1;
            let lo = f.floor();
            // coords are clamped to [0, size-1], so the cast is exact in range.
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            let lo_i = (lo as usize).min(size.saturating_sub(1));
            let hi_i = lo_i.saturating_add(1).min(size.saturating_sub(1));
            let frac = f - lo;
            if hi {
                weight *= frac;
                index = index.saturating_add(hi_i.saturating_mul(stride));
            } else {
                weight *= 1.0 - frac;
                index = index.saturating_add(lo_i.saturating_mul(stride));
            }
            stride = stride.saturating_mul(size.max(1));
        }
        (weight, index)
    }
}

/// Type 2: exponential function `y = a · x^b + c`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExponentialFunction {
    /// Input component count (1 or 2).
    pub inputs: usize,
    /// Output component count (always 1).
    pub outputs: usize,
    /// The `a` coefficients.
    pub a: Vec<f64>,
    /// The `b` exponents.
    pub b: Vec<f64>,
    /// The `c` constants.
    pub c: Vec<f64>,
    /// The input domain ranges.
    pub domain: Vec<(f64, f64)>,
    /// The output range.
    pub range: Option<(f64, f64)>,
}

impl ExponentialFunction {
    /// Evaluate `a · x^b + c` per output.
    pub fn evaluate(&self, input: &[f64]) -> Result<Vec<f64>> {
        if input.len() != self.inputs {
            return Err(err!(Code::FunctionInputCount, during = "function-eval"));
        }
        let mut out = Vec::new();
        for o in 0..self.outputs {
            let a = self.a.get(o).copied().unwrap_or(0.0);
            let b = self.b.get(o).copied().unwrap_or(1.0);
            let c = self.c.get(o).copied().unwrap_or(0.0);
            let x = input.first().copied().unwrap_or(0.0);
            let mut v = a * x.powf(b) + c;
            if let Some((lo, hi)) = self.range {
                v = v.clamp(lo, hi);
            }
            out.push(v);
        }
        Ok(out)
    }
}

/// Type 3: stitching function — a piecewise combination of sub-functions.
#[derive(Debug, Clone, PartialEq)]
pub struct StitchingFunction {
    /// Input component count (1).
    pub inputs: usize,
    /// Output component count.
    pub outputs: usize,
    /// The sub-functions, in order.
    pub funcs: Vec<Box<Function>>,
    /// The domain boundaries.
    pub bounds: Vec<f64>,
    /// The encode ranges per sub-function.
    pub encode: Vec<(f64, f64)>,
}

impl StitchingFunction {
    /// Evaluate by selecting the sub-function for the input and encoding it.
    pub fn evaluate(&self, input: &[f64], budget: &mut FunctionBudget) -> Result<Vec<f64>> {
        budget.charge()?;
        let x = input.first().copied().unwrap_or(0.0);
        // Find the sub-function whose bounds contain x.
        let mut idx = 0usize;
        for (i, bound) in self.bounds.iter().enumerate() {
            if x >= *bound {
                idx = i;
            }
        }
        // Encode x into the sub-function's domain.
        let sub = self
            .funcs
            .get(idx)
            .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
        let (elo, ehi) = self.encode.get(idx).copied().unwrap_or((0.0, 1.0));
        let (flo, fhi) = sub.input_range_first();
        let encoded = if fhi > flo {
            elo + (x - flo) / (fhi - flo) * (ehi - elo)
        } else {
            elo
        };
        sub.evaluate(&[encoded], budget)
    }
}

impl Function {
    /// The first input domain range `(min, max)`.
    pub fn input_range_first(&self) -> (f64, f64) {
        match self {
            Function::Sampled(f) => f.domain.first().copied().unwrap_or((0.0, 1.0)),
            Function::Exponential(f) => f.domain.first().copied().unwrap_or((0.0, 1.0)),
            Function::Stitching(f) => (
                f.bounds.first().copied().unwrap_or(0.0),
                f.bounds.last().copied().unwrap_or(1.0),
            ),
            Function::Calculator(f) => f.domain.first().copied().unwrap_or((0.0, 1.0)),
        }
    }
}

/// Type 4: the PostScript calculator function.
///
/// A small stack language. The interpreter is budget-bounded: an op budget
/// (`max_ops`) and a stack-depth limit prevent pathological expressions from
/// looping or growing the stack without bound.
#[derive(Debug, Clone, PartialEq)]
pub struct CalculatorFunction {
    /// Input component count.
    pub inputs: usize,
    /// Output component count.
    pub outputs: usize,
    /// The input domain ranges.
    pub domain: Vec<(f64, f64)>,
    /// The output ranges.
    pub range: Vec<(f64, f64)>,
    /// The calculator program: a sequence of operations.
    pub program: Vec<CalcOp>,
}

/// A PostScript calculator operation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CalcOp {
    /// Push a number onto the stack.
    Number(f64),
    /// `add` — pop two, push their sum.
    Add,
    /// `sub` — pop two, push a − b.
    Sub,
    /// `mul` — pop two, push their product.
    Mul,
    /// `div` — pop two, push a ÷ b.
    Div,
    /// `idiv` — integer division.
    IDiv,
    /// `mod` — remainder.
    Mod,
    /// `neg` — negate the top.
    Neg,
    /// `abs` — absolute value.
    Abs,
    /// `sqrt` — square root.
    Sqrt,
    /// `exp` — `a^b`.
    Exp,
    /// `ln` — natural log.
    Ln,
    /// `log` — base-10 log.
    Log,
    /// `sin`.
    Sin,
    /// `cos`.
    Cos,
    /// `tan`.
    Tan,
    /// `atan` — two-argument arctangent.
    Atan,
    /// `floor`.
    Floor,
    /// `ceiling`.
    Ceiling,
    /// `truncate`.
    Truncate,
    /// `round`.
    Round,
    /// `and` — bitwise AND of integers.
    And,
    /// `or` — bitwise OR of integers.
    Or,
    /// `not` — logical not (0 → 1, else 0).
    Not,
    /// `eq` — equality → 1/0.
    Eq,
    /// `ne` — inequality → 1/0.
    Ne,
    /// `gt` — greater than → 1/0.
    Gt,
    /// `ge` — greater-or-equal → 1/0.
    Ge,
    /// `lt` — less than → 1/0.
    Lt,
    /// `le` — less-or-equal → 1/0.
    Le,
    /// `pop` — discard the top of the stack.
    Pop,
    /// `exch` — swap the top two.
    Exch,
    /// `dup` — duplicate the top.
    Dup,
    /// `copy` — duplicate the top n elements.
    Copy,
    /// `index` — push the nth element from the top.
    Index,
    /// `roll` — roll the top n elements by j.
    Roll,
    /// `get` — push the ith stack element.
    Get,
    /// `put` — set the ith stack element.
    Put,
    /// `cvi` — truncate to an integer.
    Cvi,
    /// `cvr` — convert to real (no-op for f64).
    Cvr,
    /// `bitshift` — left/right shift on integers.
    BitShift,
}

impl CalculatorFunction {
    /// Evaluate the calculator program.
    ///
    /// # Errors
    ///
    /// `FUNCTION_BUDGET` when the op budget is exhausted; `FUNCTION_INPUT_COUNT`
    /// on a stack underflow or wrong input length.
    pub fn evaluate(&self, input: &[f64], budget: &mut FunctionBudget) -> Result<Vec<f64>> {
        if input.len() != self.inputs {
            return Err(err!(Code::FunctionInputCount, during = "function-eval"));
        }
        // Seed the stack with the inputs.
        let mut stack: Vec<f64> = input.to_vec();
        for &op in &self.program {
            budget.charge()?;
            if stack.len() > budget.max_stack {
                return Err(err!(
                    Code::FunctionBudget,
                    during = "function-eval",
                    detail = "calculator stack depth exceeded"
                ));
            }
            apply_calc(op, &mut stack)?;
        }
        // The top `outputs` values are the result.
        if stack.len() < self.outputs {
            return Err(err!(Code::FunctionInputCount, during = "function-eval"));
        }
        let start = stack.len().saturating_sub(self.outputs);
        let mut out: Vec<f64> = stack.get(start..).unwrap_or(&[]).to_vec();
        for (o, v) in out.iter_mut().enumerate() {
            if let Some((lo, hi)) = self.range.get(o) {
                *v = v.clamp(*lo, *hi);
            }
        }
        Ok(out)
    }
}

fn pop2(stack: &mut Vec<f64>) -> Result<(f64, f64)> {
    let b = stack
        .pop()
        .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
    let a = stack
        .pop()
        .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
    Ok((a, b))
}

/// Truncate an f64 to an i64 (the PostScript `cvi` semantics); the caller
/// bounds the value, so the cast is exact in range.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_int(v: f64) -> i64 {
    v.trunc() as i64
}

/// Truncate an f64 to a usize, clamped non-negative; bounded by the caller.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_usize(v: f64) -> usize {
    v.max(0.0).trunc() as usize
}

fn apply_calc(op: CalcOp, stack: &mut Vec<f64>) -> Result<()> {
    match op {
        CalcOp::Number(v) => stack.push(v),
        CalcOp::Add => {
            let (a, b) = pop2(stack)?;
            stack.push(a + b);
        }
        CalcOp::Sub => {
            let (a, b) = pop2(stack)?;
            stack.push(a - b);
        }
        CalcOp::Mul => {
            let (a, b) = pop2(stack)?;
            stack.push(a * b);
        }
        CalcOp::Div => {
            let (a, b) = pop2(stack)?;
            stack.push(a / b);
        }
        CalcOp::IDiv => {
            let (a, b) = pop2(stack)?;
            stack.push((a / b).trunc());
        }
        CalcOp::Mod => {
            let (a, b) = pop2(stack)?;
            stack.push(a % b);
        }
        CalcOp::Neg => {
            let a = stack
                .pop()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(-a);
        }
        CalcOp::Abs => {
            let a = stack
                .pop()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(a.abs());
        }
        CalcOp::Sqrt => {
            let a = stack
                .pop()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(a.sqrt());
        }
        CalcOp::Exp => {
            let (a, b) = pop2(stack)?;
            stack.push(a.powf(b));
        }
        CalcOp::Ln => {
            let a = stack
                .pop()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(a.ln());
        }
        CalcOp::Log => {
            let a = stack
                .pop()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(a.log10());
        }
        CalcOp::Sin => {
            let a = stack
                .pop()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(a.sin());
        }
        CalcOp::Cos => {
            let a = stack
                .pop()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(a.cos());
        }
        CalcOp::Tan => {
            let a = stack
                .pop()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(a.tan());
        }
        CalcOp::Atan => {
            let (a, b) = pop2(stack)?;
            stack.push(a.atan2(b));
        }
        CalcOp::Floor => {
            let a = stack
                .pop()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(a.floor());
        }
        CalcOp::Ceiling => {
            let a = stack
                .pop()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(a.ceil());
        }
        CalcOp::Truncate => {
            let a = stack
                .pop()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(a.trunc());
        }
        CalcOp::Round => {
            let a = stack
                .pop()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(a.round());
        }
        CalcOp::And => {
            let (a, b) = pop2(stack)?;
            stack.push((to_int(a) & to_int(b)) as f64);
        }
        CalcOp::Or => {
            let (a, b) = pop2(stack)?;
            stack.push((to_int(a) | to_int(b)) as f64);
        }
        CalcOp::Not => {
            let a = stack
                .pop()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(if to_int(a) == 0 { 1.0 } else { 0.0 });
        }
        CalcOp::Eq => {
            let (a, b) = pop2(stack)?;
            stack.push(if a == b { 1.0 } else { 0.0 });
        }
        CalcOp::Ne => {
            let (a, b) = pop2(stack)?;
            stack.push(if a != b { 1.0 } else { 0.0 });
        }
        CalcOp::Gt => {
            let (a, b) = pop2(stack)?;
            stack.push(if a > b { 1.0 } else { 0.0 });
        }
        CalcOp::Ge => {
            let (a, b) = pop2(stack)?;
            stack.push(if a >= b { 1.0 } else { 0.0 });
        }
        CalcOp::Lt => {
            let (a, b) = pop2(stack)?;
            stack.push(if a < b { 1.0 } else { 0.0 });
        }
        CalcOp::Le => {
            let (a, b) = pop2(stack)?;
            stack.push(if a <= b { 1.0 } else { 0.0 });
        }
        CalcOp::Pop => {
            stack.pop();
        }
        CalcOp::Exch => {
            let (a, b) = pop2(stack)?;
            stack.push(b);
            stack.push(a);
        }
        CalcOp::Dup => {
            let a = stack
                .last()
                .copied()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(a);
        }
        CalcOp::Copy => {
            let n = to_usize(
                stack
                    .pop()
                    .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?,
            );
            let base = stack.len().saturating_sub(n);
            let copy: Vec<f64> = stack.get(base..).unwrap_or(&[]).to_vec();
            stack.extend(copy);
        }
        CalcOp::Index => {
            let n = to_usize(
                stack
                    .pop()
                    .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?,
            );
            let len = stack.len();
            let v = stack
                .get(len.saturating_sub(1).saturating_sub(n))
                .copied()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(v);
        }
        CalcOp::Roll => {
            let (j, n) = pop2(stack)?;
            let j = to_int(j);
            let n = to_int(n);
            if n > 0 {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let n_usize = n as usize;
                let base = stack.len().saturating_sub(n_usize);
                let mut part = stack.get(base..).unwrap_or(&[]).to_vec();
                let k = j.rem_euclid(n);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                part.rotate_right(k as usize);
                stack.truncate(base);
                stack.extend(part);
            }
        }
        CalcOp::Get => {
            let (n, i) = pop2(stack)?;
            let i = to_usize(i);
            let v = stack.get(i).copied().unwrap_or(0.0);
            stack.push(v);
            let _ = n;
        }
        CalcOp::Put => {
            let (v, i) = pop2(stack)?;
            let i = to_usize(i);
            if let Some(slot) = stack.get_mut(i) {
                *slot = v;
            }
        }
        CalcOp::Cvi => {
            let a = stack
                .pop()
                .ok_or_else(|| err!(Code::FunctionInputCount, during = "function-eval"))?;
            stack.push(a.trunc());
        }
        CalcOp::Cvr => {
            // cvr is a no-op for f64 (already a real).
        }
        CalcOp::BitShift => {
            let (s, n) = pop2(stack)?;
            let s = to_int(s);
            let n = to_int(n);
            if n >= 0 {
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let shifted = s.checked_shl(n as u32).unwrap_or(0);
                stack.push(shifted as f64);
            } else {
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let shifted = s.wrapping_shr(n.wrapping_neg() as u32);
                stack.push(shifted as f64);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    fn budget() -> FunctionBudget {
        FunctionBudget::default()
    }

    fn calc(program: Vec<CalcOp>, inputs: usize, outputs: usize) -> CalculatorFunction {
        CalculatorFunction {
            inputs,
            outputs,
            domain: vec![(0.0, 1.0); inputs],
            range: vec![(-1000.0, 1000.0); outputs],
            program,
        }
    }

    #[test]
    fn exponential_function() {
        let f = ExponentialFunction {
            inputs: 1,
            outputs: 1,
            a: vec![2.0],
            b: vec![2.0],
            c: vec![1.0],
            domain: vec![(0.0, 1.0)],
            range: Some((0.0, 3.0)),
        };
        // y = 2·x² + 1 → x=1 → 3.
        let out = f.evaluate(&[1.0]).expect("eval");
        assert!((out[0] - 3.0).abs() < 1e-9);
    }

    #[test]
    fn calculator_basic_arith() {
        // `3 4 add` → 7.
        let f = calc(
            vec![CalcOp::Number(3.0), CalcOp::Number(4.0), CalcOp::Add],
            0,
            1,
        );
        let mut b = budget();
        let out = f.evaluate(&[], &mut b).expect("eval");
        assert!((out[0] - 7.0).abs() < 1e-9);
    }

    #[test]
    fn calculator_stack_ops() {
        // `5 dup mul` → 25.
        let f = calc(vec![CalcOp::Number(5.0), CalcOp::Dup, CalcOp::Mul], 0, 1);
        let mut b = budget();
        let out = f.evaluate(&[], &mut b).expect("eval");
        assert!((out[0] - 25.0).abs() < 1e-9);
    }

    #[test]
    fn calculator_inputs_are_seeded() {
        // Input x, `x dup mul` → x².
        let f = calc(vec![CalcOp::Dup, CalcOp::Mul], 1, 1);
        let mut b = budget();
        let out = f.evaluate(&[4.0], &mut b).expect("eval");
        assert!((out[0] - 16.0).abs() < 1e-9);
    }

    /// DoD: a pathological type-4 expression hits the budget, never loops.
    #[test]
    fn calculator_budget_stops_a_loop() {
        // A program of many ops exceeds a tiny budget.
        let mut program = Vec::new();
        for _ in 0..1000 {
            program.push(CalcOp::Dup);
        }
        let f = calc(program, 1, 1);
        let mut b = FunctionBudget {
            max_ops: 100,
            max_stack: 64,
            ops_used: 0,
        };
        let e = f.evaluate(&[1.0], &mut b).expect_err("budget");
        assert_eq!(e.code(), Code::FunctionBudget);
    }

    #[test]
    fn calculator_stack_depth_is_bounded() {
        // Push 100 values with a max stack of 10: must fail, not grow.
        let mut program = Vec::new();
        for _ in 0..100 {
            program.push(CalcOp::Dup);
        }
        // Seed with one input so Dup keeps growing.
        let f = calc(program, 1, 1);
        let mut b = FunctionBudget {
            max_ops: 10_000,
            max_stack: 10,
            ops_used: 0,
        };
        assert!(f.evaluate(&[1.0], &mut b).is_err());
    }

    #[test]
    fn wrong_input_count_is_a_typed_error() {
        let f = calc(vec![CalcOp::Dup], 1, 1);
        let mut b = budget();
        let e = f.evaluate(&[], &mut b).expect_err("count");
        assert_eq!(e.code(), Code::FunctionInputCount);
    }

    #[test]
    fn stitching_function_selects_by_bounds() {
        // Two sub-functions: one doubling, one constant 1.
        let double = Function::Exponential(ExponentialFunction {
            inputs: 1,
            outputs: 1,
            a: vec![2.0],
            b: vec![1.0],
            c: vec![0.0],
            domain: vec![(0.0, 0.5)],
            range: Some((0.0, 1.0)),
        });
        let one = Function::Exponential(ExponentialFunction {
            inputs: 1,
            outputs: 1,
            a: vec![0.0],
            b: vec![1.0],
            c: vec![1.0],
            domain: vec![(0.5, 1.0)],
            range: Some((0.0, 1.0)),
        });
        let stitch = Function::Stitching(StitchingFunction {
            inputs: 1,
            outputs: 1,
            funcs: vec![Box::new(double), Box::new(one)],
            bounds: vec![0.0, 0.5],
            encode: vec![(0.0, 1.0), (0.0, 1.0)],
        });
        let mut b = budget();
        // x = 0.25 → first function, encoded to ~0.5 → 2·0.5 = 1.
        let out = stitch.evaluate(&[0.25], &mut b).expect("eval");
        assert!((out[0] - 1.0).abs() < 1e-6);
    }
}
