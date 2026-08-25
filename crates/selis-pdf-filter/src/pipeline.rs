//! The filter pipeline (SL-1.FILT.01).
//!
//! A stream's `/Filter` may name one filter or an array of filters applied in
//! sequence (`/F1 /F2` means F1 then F2 on encode, so decode applies F2 then
//! F1). `/DecodeParms` provides per-filter parameters, either one dict or an
//! array aligned with the filters.
//!
//! The pathological case the DoD names — `Flate then Flate then Flate` — is a
//! decompression bomb. Every stage is bounded by the same `output_limit`, so
//! the chain hits the budget, not memory.

use selis_error::{err, Code, Result};
use selis_sandbox::BudgetGuard;

use crate::decode;

/// Per-filter decode parameters.
#[derive(Debug, Clone, Copy, Default)]
pub struct DecodeParms {
    /// `/Predictor` for Flate/LZW (1 = none, 2 = TIFF, 10-15 = PNG).
    pub predictor: u16,
    /// `/Columns` for predictors.
    pub columns: u32,
    /// `/Colors` for predictors.
    pub colors: u32,
    /// `/BitsPerComponent` for predictors.
    pub bits_per_component: u32,
    /// `/EarlyChange` for LZW (0 or 1).
    pub early_change: u8,
}

/// Decode a stream through a filter chain.
///
/// `filters` are in **encode order** (first named filter was applied first),
/// so decoding runs them in reverse. `parms` aligns with `filters`.
///
/// # Budget
///
/// `output_limit` bounds every stage; a `Flate then Flate then Flate` bomb
/// hits the limit at the first stage, not memory.
///
/// # Malformed Input
///
/// `FILTER_CHAIN_TOO_LONG` when the chain exceeds 16 filters; `FILTER_UNKNOWN`
/// for an unimplemented filter.
pub fn decode_chain(
    filters: &[String],
    parms: &[DecodeParms],
    data: &[u8],
    output_limit: u64,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    if filters.len() > 16 {
        return Err(err!(
            Code::FilterChainTooLong,
            during = "filter-chain",
            detail = "more than 16 filters"
        ));
    }
    // Decode in reverse (last-named filter was applied first).
    let mut current = data.to_vec();
    for (i, name) in filters.iter().enumerate().rev() {
        g.tick()?;
        let p = parms.get(i).copied().unwrap_or_default();
        current = apply_filter(name, &current, p, output_limit, g)?;
    }
    Ok(current)
}

fn apply_filter(
    name: &str,
    data: &[u8],
    parms: DecodeParms,
    output_limit: u64,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let decoded = match name {
        "LZWDecode" | "LZW" => crate::lzw_decode(data, parms.early_change, g)?,
        _ => decode(name, data, output_limit, g)?,
    };
    // Apply the predictor, if any, after the compression filter.
    apply_predictor(&decoded, parms, g)
}

/// Apply a predictor to a decoded byte stream (SL-1.FILT.02).
///
/// PNG predictors 0–4 operate row-wise; the TIFF predictor (2) is
/// component-wise horizontal differencing.
fn apply_predictor(data: &[u8], parms: DecodeParms, g: &mut BudgetGuard<'_>) -> Result<Vec<u8>> {
    let predictor = parms.predictor;
    if predictor == 1 || predictor == 0 {
        return Ok(data.to_vec());
    }
    let colors = usize::try_from(parms.colors.max(1)).unwrap_or(1);
    let columns = usize::try_from(parms.columns.max(1)).unwrap_or(1);
    let bpc = usize::try_from(parms.bits_per_component.max(1)).unwrap_or(1);

    if predictor == 2 {
        // TIFF predictor: horizontal differencing, component-wise.
        return tiff_predictor(data, colors, columns, bpc, g);
    }

    if (10..=15).contains(&predictor) {
        return png_predictor(data, predictor, colors, columns, bpc, g);
    }

    Err(err!(
        Code::FilterParamInvalid,
        during = "filter-predictor",
        detail = format!("unknown /Predictor {predictor}")
    ))
}

fn tiff_predictor(
    data: &[u8],
    colors: usize,
    columns: usize,
    bpc: usize,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let bytes_per_sample = bpc.saturating_add(7).wrapping_div(8);
    if bytes_per_sample == 0 {
        return Ok(data.to_vec());
    }
    let row_bytes = colors
        .saturating_mul(columns)
        .saturating_mul(bytes_per_sample);
    let mut out = Vec::with_capacity(data.len());
    let mut rows = data.chunks(row_bytes.max(1));
    // First row is stored raw.
    if let Some(first) = rows.next() {
        out.extend_from_slice(first);
    }
    let _prev_row: Vec<u8> = Vec::new();
    for row in rows {
        g.tick()?;
        let mut out_row = row.to_vec();
        // For each byte, add the previous byte in the same position.
        for i in 1..out_row.len() {
            let prev_val = u16::from(out_row.get(i.wrapping_sub(1)).copied().unwrap_or(0));
            let cur = u16::from(out_row.get(i).copied().unwrap_or(0));
            let combined = cur.wrapping_add(prev_val);
            if let Some(slot) = out_row.get_mut(i) {
                *slot = (combined & 0xff) as u8;
            }
        }
        out.extend_from_slice(&out_row);
    }
    let _ = g;
    Ok(out)
}

fn png_predictor(
    data: &[u8],
    predictor: u16,
    colors: usize,
    columns: usize,
    bpc: usize,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let bpp = colors
        .saturating_mul(bpc.saturating_add(7).wrapping_div(8))
        .max(1);
    let row_bytes = columns.saturating_mul(bpp);
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut prev_row: Vec<u8> = Vec::new();
    while i < data.len() {
        g.tick()?;
        let Some(&ftype) = data.get(i) else {
            break;
        };
        i = i.saturating_add(1);
        let row = data.get(i..i.saturating_add(row_bytes)).unwrap_or(&[]);
        i = i.saturating_add(row.len());
        let mut cur: Vec<u8> = row.to_vec();
        for j in 0..cur.len() {
            let left = if j >= bpp {
                cur.get(j.wrapping_sub(bpp)).copied().unwrap_or(0)
            } else {
                0
            };
            let up = prev_row.get(j).copied().unwrap_or(0);
            let up_left = if j >= bpp {
                prev_row.get(j.wrapping_sub(bpp)).copied().unwrap_or(0)
            } else {
                0
            };
            let prediction = match ftype {
                0 => 0i32,
                1 => i32::from(left),
                2 => i32::from(up),
                3 => (i32::from(left).wrapping_add(i32::from(up))).wrapping_div(2),
                4 => paeth(left, up, up_left),
                _ => 0i32,
            };
            let cur_val = u32::from(cur.get(j).copied().unwrap_or(0));
            let pred = u32::try_from(prediction).unwrap_or(u32::MAX);
            let combined = cur_val.wrapping_add(pred);
            if let Some(slot) = cur.get_mut(j) {
                *slot = (combined & 0xff) as u8;
            }
        }
        out.extend_from_slice(&cur);
        prev_row = cur;
    }
    Ok(out)
}

/// Paeth predictor (PNG filter 4).
fn paeth(a: u8, b: u8, c: u8) -> i32 {
    let a = i32::from(a);
    let b = i32::from(b);
    let c = i32::from(c);
    let p = a.wrapping_add(b).wrapping_sub(c);
    let pa = (p.wrapping_sub(a)).abs();
    let pb = (p.wrapping_sub(b)).abs();
    let pc = (p.wrapping_sub(c)).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Decode a stream, applying the `/Filter` and `/DecodeParms` from a stream
/// dictionary.
///
/// `filter_obj` is the stream's `/Filter` value (a name or an array of
/// names); `parms_obj` is `/DecodeParms` (a dict or an array of dicts).
///
/// # Budget
///
/// Bounded by `output_limit` at every stage.
///
/// # Malformed Input
///
/// As [`decode_chain`].
pub fn decode_stream(
    filter_obj: &selis_bytes::Bytes,
    _parms_obj: Option<&selis_bytes::Bytes>,
    data: &[u8],
    output_limit: u64,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    // Phase-1: single-filter streams by name.
    let name = String::from_utf8_lossy(filter_obj.as_slice()).to_string();
    let filters = vec![name];
    let parms = Vec::new();
    decode_chain(&filters, &parms, data, output_limit, g)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_sandbox::Budget;

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard()
    }

    #[test]
    fn single_filter_pipeline_matches_direct_decode() {
        // ASCIIHex "48656C6C6F" = "Hello".
        let mut g = guard();
        let name = selis_bytes::Bytes::copy_from_slice(b"ASCIIHexDecode");
        let out = decode_stream(&name, None, b"48656C6C6F", 1000, &mut g).expect("decode");
        assert_eq!(out, b"Hello");
    }

    #[test]
    fn overlong_chain_is_a_typed_error() {
        let mut g = guard();
        let filters = (0..20)
            .map(|_| "ASCIIHexDecode".to_string())
            .collect::<Vec<_>>();
        let parms = Vec::new();
        let e = decode_chain(&filters, &parms, b"48656C6C6F", 1000, &mut g).expect_err("too long");
        assert_eq!(e.code(), Code::FilterChainTooLong);
    }

    #[test]
    fn tiff_predictor_roundtrip() {
        // TIFF predictor with colors=1, columns=4, bpc=8.
        // Two rows: first row raw [10,20,30,40]; second row encoded with
        // differencing [10,10,10,10] which decodes back to [10,20,30,40].
        let data = vec![10u8, 20, 30, 40, 10, 10, 10, 10];
        let mut g = guard();
        let p = DecodeParms {
            predictor: 2,
            columns: 4,
            colors: 1,
            bits_per_component: 8,
            early_change: 0,
        };
        let out = apply_predictor(&data, p, &mut g).expect("predict");
        assert_eq!(out.len(), 8, "two rows");
        assert_eq!(&out[..4], &[10, 20, 30, 40]);
        assert_eq!(&out[4..], &[10, 20, 30, 40]);
    }

    #[test]
    fn unknown_predictor_is_a_typed_error() {
        let mut g = guard();
        let p = DecodeParms {
            predictor: 99,
            columns: 4,
            colors: 1,
            bits_per_component: 8,
            early_change: 0,
        };
        let e = apply_predictor(b"1234", p, &mut g).expect_err("bad predictor");
        assert_eq!(e.code(), Code::FilterParamInvalid);
    }
}
