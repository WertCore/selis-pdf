//! Budget-aware allocation wrappers (SL-0.SBX.02).
//!
//! # Why wrappers and not a global allocator hook
//!
//! `wasm32-unknown-unknown` has no usable global hook, and a global hook cannot
//! attribute an allocation to an operation anyway — the *same* engine may be
//! parsing two documents in one process, and the 40 GB length field in document
//! A must not be paid for by document B's budget. So allocation goes through
//! these functions, which charge [`Resource::Bytes`] **before** allocating.
//!
//! The ordering is the whole trick: a document-claimed 40 GB length yields
//! `BUDGET_BYTES` in constant time and constant memory, because the charge fails
//! and no allocation is ever attempted.
//!
//! `check-alloc` (SL-0.WS.05) mechanically enforces that document-derived
//! lengths reach the allocator only through this module.

use core::mem;
use std::vec::Vec;

use selis_error::Result;

use crate::budget::BudgetGuard;
use crate::Resource;

/// The byte charge for `len` elements of `T`, with overflow as an error.
///
/// A wrapped `len * size_of::<T>()` would under-charge and let a hostile length
/// allocate more than its budget — the exact bug these wrappers exist to make
/// impossible.
fn charge_len<T>(g: &mut BudgetGuard<'_>, len: usize) -> Result<u64> {
    let bytes = len
        .checked_mul(mem::size_of::<T>())
        .ok_or_else(|| exceeded(len))?;
    g.charge(Resource::Bytes, bytes as u64)?;
    Ok(bytes as u64)
}

fn exceeded(len: usize) -> selis_error::Error {
    selis_error::err!(
        selis_error::Code::BudgetBytes,
        during = "budgeted-alloc",
        detail = std::format!("length {len} cannot be budgeted")
    )
}

/// Allocate a `Vec<T>` with `capacity` slots, charging the budget first.
///
/// This is the `Vec::with_capacity` replacement for document-derived lengths.
/// On success the vector is fully charged for its peak capacity; a later `grow`
/// charges the difference.
///
/// # Errors
///
/// `BUDGET_BYTES` when the capacity's byte size exceeds the remaining budget.
/// On error **nothing is allocated**.
///
/// # Example
///
/// ```
/// use selis_sandbox::{alloc, Budget, Surface};
///
/// let mut g = Budget::profile(Surface::Viewer).guard();
/// // A 40 GB length field: rejected before any allocation, in constant time.
/// assert!(alloc::vec_with_capacity::<u8>(&mut g, 40_000_000_000).is_err());
///
/// // The failure poisoned that guard, so a fresh one shows the success path.
/// let mut g = Budget::profile(Surface::Viewer).guard();
/// let v: Vec<u8> = alloc::vec_with_capacity(&mut g, 16).expect("small cap fits");
/// assert_eq!(v.len(), 0);
/// assert_eq!(v.capacity(), 16);
/// ```
pub fn vec_with_capacity<T>(g: &mut BudgetGuard<'_>, capacity: usize) -> Result<Vec<T>> {
    charge_len::<T>(g, capacity)?;
    let mut v = Vec::new();
    v.try_reserve(capacity).map_err(|_| exceeded(capacity))?;
    Ok(v)
}

/// Grow a vector's reserve by `additional` slots, charging the *difference* in
/// bytes so the running charge tracks the vector's actual peak.
///
/// # Errors
///
/// `BUDGET_BYTES` when the additional bytes exceed the remaining budget.
pub fn grow<T>(g: &mut BudgetGuard<'_>, v: &mut Vec<T>, additional: usize) -> Result<()> {
    let current = v.capacity();
    let target = current
        .checked_add(additional)
        .ok_or_else(|| exceeded(additional))?;
    if target > current {
        charge_len::<T>(g, additional)?;
        v.try_reserve(additional)
            .map_err(|_| exceeded(additional))?;
    }
    Ok(())
}

/// Allocate a `Box<[T]>` of `len` zero-initialised elements, charging first.
///
/// The `vec![0; n]` replacement for document-derived `n`.
///
/// # Errors
///
/// `BUDGET_BYTES` when `len * size_of::<T>()` exceeds the remaining budget.
pub fn boxed_slice<T: Clone>(g: &mut BudgetGuard<'_>, len: usize, value: T) -> Result<Box<[T]>> {
    charge_len::<T>(g, len)?;
    let mut v = Vec::new();
    v.try_reserve(len).map_err(|_| exceeded(len))?;
    v.resize(len, value);
    Ok(v.into_boxed_slice())
}

/// Allocate a `Vec<T>` of `len` elements initialised to `value`, charging
/// first.
///
/// The `vec![value; n]` replacement for document-derived `n` where the caller
/// needs a growable `Vec` rather than a boxed slice.
///
/// # Errors
///
/// `BUDGET_BYTES` when `len * size_of::<T>()` exceeds the remaining budget.
/// On error **nothing is allocated**.
pub fn vec_filled<T: Clone>(g: &mut BudgetGuard<'_>, len: usize, value: T) -> Result<Vec<T>> {
    charge_len::<T>(g, len)?;
    let mut v = Vec::new();
    v.try_reserve(len).map_err(|_| exceeded(len))?;
    v.resize(len, value);
    Ok(v)
}

/// Copy `src` into a freshly budgeted `Vec<T>`.
///
/// Charges the source's byte size, then copies. The caller keeps `src`.
///
/// # Errors
///
/// `BUDGET_BYTES` when the copy's byte size exceeds the remaining budget.
pub fn copy_slice<T: Copy>(g: &mut BudgetGuard<'_>, src: &[T]) -> Result<Vec<T>> {
    charge_len::<T>(g, src.len())?;
    let mut v = Vec::new();
    v.try_reserve(src.len()).map_err(|_| exceeded(src.len()))?;
    v.extend_from_slice(src);
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Budget, Resource, Surface};
    use selis_error::Code;

    /// SL-0.SBX.02 DoD: a 40 GB length field yields `BUDGET_BYTES` in constant
    /// memory and constant time — nothing was allocated and the guard is
    /// poisoned, so a swallowed error cannot silently continue.
    #[test]
    fn a_forty_gigabyte_length_field_yields_budget_exceeded_without_allocating() {
        let mut g = Budget::profile(Surface::Viewer).guard();
        let e = vec_with_capacity::<u8>(&mut g, 40 * 1024 * 1024 * 1024)
            .expect_err("40 GB cannot fit a viewer budget");
        assert_eq!(e.code(), Code::BudgetBytes);
        assert_eq!(
            g.usage().bytes,
            0,
            "charge-before-allocate means zero charged"
        );
        assert_eq!(g.poisoned_by(), Some(Resource::Bytes));
        // The poison covers every later charge too.
        assert_eq!(
            vec_with_capacity::<u8>(&mut g, 1)
                .expect_err("poisoned")
                .code(),
            Code::BudgetPoisoned
        );
    }

    #[test]
    fn vec_with_capacity_charges_peak_not_len() {
        let mut g = Budget {
            bytes: 100,
            wall: u64::MAX,
            depth: 100,
            objects: u32::MAX,
            pixels: u64::MAX,
        }
        .guard();
        let v: Vec<u64> = vec_with_capacity(&mut g, 10).expect("80 bytes fits");
        assert_eq!(v.capacity(), 10);
        assert_eq!(g.usage().bytes, 80);
    }

    #[test]
    fn grow_charges_only_the_difference() {
        let mut g = Budget {
            bytes: 100,
            wall: u64::MAX,
            depth: 100,
            objects: u32::MAX,
            pixels: u64::MAX,
        }
        .guard();
        let mut v: Vec<u64> = vec_with_capacity(&mut g, 4).expect("32 bytes fits");
        grow(&mut g, &mut v, 4).expect("another 32 bytes fits");
        assert!(g.usage().bytes <= 64);
        grow(&mut g, &mut v, 0).expect("no-op grow does not charge");
        assert!(g.usage().bytes <= 64);
    }

    #[test]
    fn boxed_slice_charges_and_initialises() {
        let mut g = Budget {
            bytes: 1000,
            wall: u64::MAX,
            depth: 100,
            objects: u32::MAX,
            pixels: u64::MAX,
        }
        .guard();
        let s = boxed_slice(&mut g, 10, 7u8).expect("10 bytes fits");
        assert_eq!(s.len(), 10);
        assert!(s.iter().all(|&b| b == 7));
        assert_eq!(g.usage().bytes, 10);
    }

    #[test]
    fn vec_filled_charges_and_initialises() {
        let mut g = Budget {
            bytes: 1000,
            wall: u64::MAX,
            depth: 100,
            objects: u32::MAX,
            pixels: u64::MAX,
        }
        .guard();
        let v: Vec<u8> = vec_filled(&mut g, 10, 7).expect("10 bytes fits");
        assert_eq!(v.len(), 10);
        assert!(v.iter().all(|&b| b == 7));
        assert_eq!(g.usage().bytes, 10);
        assert!(vec_filled::<u8>(&mut g, 2000, 0).is_err(), "over budget");
    }

    #[test]
    fn copy_slice_charges_and_copies() {
        let mut g = Budget::unlimited().guard();
        let src = [1u32, 2, 3, 4, 5];
        let v = copy_slice(&mut g, &src).expect("unlimited fits");
        assert_eq!(v, src);
        assert_eq!(g.usage().bytes, 5 * 4);
    }

    #[test]
    fn zero_capacity_is_free() {
        let mut g = Budget {
            bytes: 0,
            wall: u64::MAX,
            depth: 100,
            objects: u32::MAX,
            pixels: u64::MAX,
        }
        .guard();
        let v: Vec<u8> = vec_with_capacity(&mut g, 0).expect("0 bytes never fails");
        assert_eq!(v.capacity(), 0);
    }
}
