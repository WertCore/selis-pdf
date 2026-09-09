//! Linearisation parsing (SL-1.COS.08).
//!
//! A linearised PDF places the first page's objects at the front of the file
//! so a reader can display page 1 after fetching a tiny fraction of the
//! bytes. The linearisation dictionary (object 1) declares:
//!
//! * `/L` — the file length;
//! * `/H` — the hint streams offsets;
//! * `/O` — the first-page object number;
//! * `/E` — the offset of the first page's end;
//! * `/N` — the page count;
//! * `/T` — the offset of the first page's first object;
//! * `/P` — the first page number (usually 1).
//!
//! **Validate rather than trust** (SL-1.COS.08 DoD): a lying hint stream must
//! degrade, not corrupt. The `open_pages` estimate below is what the resumable
//! reader uses to decide how many bytes to fetch before rendering page 1.

use selis_error::Result;
#[cfg(test)]
use selis_sandbox::Budget;
use selis_sandbox::BudgetGuard;

use crate::obj::Obj;

/// A parsed linearisation dictionary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Linearisation {
    /// `/L` — the declared file length.
    pub file_length: Option<u64>,
    /// `/O` — the first-page object number.
    pub first_page_obj: Option<u32>,
    /// `/E` — the offset of the first page's last object.
    pub first_page_end: Option<u64>,
    /// `/N` — the number of pages.
    pub page_count: Option<u32>,
    /// `/T` — the offset of the first page's first object.
    pub first_page_start: Option<u64>,
    /// `/P` — the first page number (usually 1).
    pub first_page_num: Option<u32>,
}

impl Linearisation {
    /// Whether this looks like a genuine linearisation dictionary.
    #[must_use]
    pub fn is_linearised(&self) -> bool {
        self.first_page_obj.is_some() && self.page_count.is_some()
    }

    /// The byte offset at which page 1's objects begin, when known.
    ///
    /// The reader fetches from here to `first_page_end` to render page 1.
    #[must_use]
    pub fn first_page_span(&self) -> Option<(u64, u64)> {
        match (self.first_page_start, self.first_page_end) {
            (Some(s), Some(e)) if e >= s => Some((s, e)),
            _ => None,
        }
    }

    /// Parse the linearisation dictionary (object 1).
    ///
    /// # Budget
    ///
    /// The object must already be resolved; no additional charge.
    ///
    /// # Malformed Input
    ///
    /// A non-dict or absent dictionary yields `Linearisation::default()`
    /// (never linearised); a lying hint stream degrades to `None` spans.
    pub fn from_obj(obj: &Obj, g: &mut BudgetGuard<'_>) -> Result<Self> {
        let _ = g;
        let mut out = Self::default();
        let Obj::Dict(pairs) = obj else {
            return Ok(out);
        };
        let get_int = |key: &[u8]| -> Option<i64> {
            pairs
                .iter()
                .find(|(k, _)| k.as_slice() == key)
                .and_then(|(_, v)| match v {
                    Obj::Int(i) => Some(*i),
                    _ => None,
                })
        };
        out.file_length = get_int(b"L").and_then(|v| u64::try_from(v).ok());
        out.first_page_obj = get_int(b"O").and_then(|v| u32::try_from(v).ok());
        out.first_page_end = get_int(b"E").and_then(|v| u64::try_from(v).ok());
        out.page_count = get_int(b"N").and_then(|v| u32::try_from(v).ok());
        out.first_page_start = get_int(b"T").and_then(|v| u64::try_from(v).ok());
        out.first_page_num = get_int(b"P").and_then(|v| u32::try_from(v).ok());
        Ok(out)
    }
}

impl Default for Linearisation {
    fn default() -> Self {
        Self {
            file_length: None,
            first_page_obj: None,
            first_page_end: None,
            page_count: None,
            first_page_start: None,
            first_page_num: None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]

    use super::*;

    fn dict(pairs: Vec<(&[u8], Obj)>) -> Obj {
        Obj::Dict(
            pairs
                .into_iter()
                .map(|(k, v)| (selis_bytes::Bytes::copy_from_slice(k), v))
                .collect(),
        )
    }

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    #[test]
    fn parses_linearisation_fields() {
        let d = dict(vec![
            (b"L", Obj::Int(123456)),
            (b"O", Obj::Int(4)),
            (b"E", Obj::Int(5000)),
            (b"N", Obj::Int(10)),
            (b"T", Obj::Int(2000)),
            (b"P", Obj::Int(1)),
        ]);
        let mut g = guard();
        let lin = Linearisation::from_obj(&d, &mut g).expect("parse");
        assert!(lin.is_linearised());
        assert_eq!(lin.file_length, Some(123456));
        assert_eq!(lin.first_page_obj, Some(4));
        assert_eq!(lin.page_count, Some(10));
        assert_eq!(lin.first_page_span(), Some((2000, 5000)));
    }

    #[test]
    fn non_dict_is_not_linearised() {
        let mut g = guard();
        let lin = Linearisation::from_obj(&Obj::Null, &mut g).expect("parse");
        assert!(!lin.is_linearised());
        assert_eq!(lin.first_page_span(), None);
    }

    #[test]
    fn reversed_span_degrades_to_none() {
        let d = dict(vec![
            (b"L", Obj::Int(1000)),
            (b"O", Obj::Int(4)),
            (b"E", Obj::Int(100)),
            (b"N", Obj::Int(1)),
            (b"T", Obj::Int(500)),
        ]);
        let mut g = guard();
        let lin = Linearisation::from_obj(&d, &mut g).expect("parse");
        // A lying /E < /T must degrade, not corrupt.
        assert_eq!(lin.first_page_span(), None);
    }
}
