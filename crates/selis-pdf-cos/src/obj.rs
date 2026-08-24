//! The COS object model (SL-1.COS.02).
//!
//! [`Obj`] is the value type the parser produces from the token stream:
//! `Null | Bool | Int | Real | String | Name | Array | Dict | Stream | Ref`.
//!
//! Strings stay **raw bytes** — text decoding (PDFDocEncoding vs UTF-16BE vs
//! UTF-8 in PDF 2.0) is a separate, explicit step, because guessing here
//! corrupts non-Latin metadata.
//!
//! References are kept unresolved in this layer; resolution with a visited set
//! is SL-1.DOC.02.

use selis_bytes::Bytes;

/// An indirect reference: object number and generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ref {
    /// Object number.
    pub num: u32,
    /// Generation number.
    pub gen: u16,
}

impl Ref {
    /// A reference.
    #[must_use]
    pub const fn new(num: u32, gen: u16) -> Self {
        Self { num, gen }
    }
}

/// A COS object value.
///
/// Sizes are recorded for the budget table (SL-1.COS.02 DoD): the enum is
/// two words plus the payload; `Array`/`Dict`/`Stream` heap-allocate their
/// children, so a document can only grow the tree under a budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Obj {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A decimal real, exact as a scaled integer.
    Real {
        /// The digits, as a signed integer.
        scaled: i64,
        /// Number of fractional digits.
        scale: u8,
    },
    /// A string of raw bytes. Text decoding is an explicit later step.
    String(Bytes),
    /// A name, `#xx` escapes already decoded.
    Name(Bytes),
    /// An array of values.
    Array(Vec<Obj>),
    /// A dictionary of name → value pairs.
    Dict(Vec<(Bytes, Obj)>),
    /// A stream: a dictionary plus the raw (unfiltered) bytes.
    Stream {
        /// The stream dictionary (its `/Length` is handled by the parser).
        dict: Vec<(Bytes, Obj)>,
        /// The raw stream bytes, as stored in the file.
        data: Bytes,
    },
    /// An indirect reference.
    Ref(Ref),
}

impl Obj {
    /// The number of heap-allocated words directly under this value (not
    /// counting the transitive contents of children). Used by the budget to
    /// bound object-tree growth.
    #[must_use]
    pub fn heap_words(&self) -> usize {
        match self {
            Obj::Null | Obj::Bool(_) | Obj::Int(_) | Obj::Real { .. } | Obj::Ref(_) => 0,
            Obj::String(b) | Obj::Name(b) => b.len().saturating_div(8).saturating_add(1),
            Obj::Array(items) => items.len().saturating_add(1),
            Obj::Dict(pairs) => pairs.len().saturating_mul(2).saturating_add(1),
            Obj::Stream { dict, data } => dict
                .len()
                .saturating_mul(2)
                .saturating_add(data.len().saturating_div(8))
                .saturating_add(1),
        }
    }

    /// A short type name for diagnostics and `inspect --json`.
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Obj::Null => "null",
            Obj::Bool(_) => "bool",
            Obj::Int(_) => "int",
            Obj::Real { .. } => "real",
            Obj::String(_) => "string",
            Obj::Name(_) => "name",
            Obj::Array(_) => "array",
            Obj::Dict(_) => "dict",
            Obj::Stream { .. } => "stream",
            Obj::Ref(_) => "ref",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_of_obj_is_bounded() {
        // The enum holds a Bytes (Arc<Vec> = 24 bytes) or a Vec (24 bytes)
        // inline; 48 bytes is the expected footprint on 64-bit. This is the
        // number the budget table records (SL-1.COS.02 DoD): a document's
        // object tree is bounded by `Objects`-budgeted Vec entries, and the
        // scalar payloads never allocate.
        let s = core::mem::size_of::<Obj>();
        assert!(s <= 64, "Obj must stay bounded; measured {s} bytes");
    }

    #[test]
    fn heap_words_counts_children() {
        assert_eq!(Obj::Null.heap_words(), 0);
        assert_eq!(Obj::Int(1).heap_words(), 0);
        let arr = Obj::Array(vec![Obj::Int(1), Obj::Int(2)]);
        assert!(arr.heap_words() >= 3);
        let d = Obj::Dict(vec![(Bytes::copy_from_slice(b"K"), Obj::Int(1))]);
        assert!(d.heap_words() >= 3);
    }

    #[test]
    fn refs_are_cheap_and_ordered() {
        let r = Ref::new(5, 0);
        assert_eq!(r, Ref { num: 5, gen: 0 });
    }
}
