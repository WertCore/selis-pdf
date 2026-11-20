//! The annotation commands: appearance regeneration and ink creation
//! (SL-5.ANNOT.02 / SL-5.ANNOT.03).
//!
//! The engine work belongs to `selis-pdf-annot`; this module is the native
//! binding boundary only. It supplies the file bytes and the real clock, runs
//! the shared WRITE.05 gate before committing anything, and prints the same
//! verification object every other writing command prints. No widget
//! appearance synthesis is claimed here: `/NeedAppearances` is reported by the
//! engine and left for the FORM layer.

use selis_sandbox::{Budget, BudgetGuard};

use crate::{
    read_file, runtime, shell_clock,
    verify_report::Verification,
    write_gate::{self, PRESERVE_ON_APPEND},
    CliError, CliResult,
};

/// `selis annot-appearances <path> --output <file>`
///
/// Rebuild missing or stale markup `/AP` streams in one incremental revision.
pub(crate) fn regenerate_appearances(path: &str, output: &str) -> CliResult<Verification> {
    let src = read_file(path)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let budget = Budget::unlimited();
    let clock = shell_clock();
    let mut g = runtime::cli_guard(&budget, &clock);
    let (bytes, report) = selis_pdf_tools::regenerate_appearances(&src, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let verification = commit(&src, &bytes, path, output, in_bytes, &budget, &mut g)?;
    eprintln!(
        "appearances: {} page(s), {} annotation(s), {} rebuilt, /NeedAppearances={} \
         (widget appearances remain the FORM layer's job) -> {output}",
        report.pages, report.annotations, report.regenerated, report.need_appearances
    );
    Ok(verification)
}

/// `selis annot-ink <path> --output <file> --page N --stroke "x,y;..."`
///
/// Every `--stroke` becomes one `/InkList` array. The rectangle is derived
/// from the points by the engine (grown by half the stroke width), so a caller
/// cannot clip its own mark with a stale `--rect`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ink(
    path: &str,
    output: &str,
    page: u32,
    rect: Option<&str>,
    strokes: &[String],
    width: f64,
    colour: [f64; 3],
    contents: &str,
    author: &str,
) -> CliResult<Verification> {
    if strokes.is_empty() {
        return Err(CliError(
            "--stroke needs at least one stroke of two or more points".to_string(),
        ));
    }
    let mut spec = selis_pdf_tools::annot::AnnotSpec::new(
        selis_pdf_tools::annot::AnnotKind::Ink,
        page,
        parse_rect(rect)?,
        colour,
    );
    spec.paths = strokes
        .iter()
        .map(|stroke| parse_stroke(stroke))
        .collect::<CliResult<Vec<_>>>()?;
    spec.width = width;
    spec.contents = contents.to_string();
    spec.author = author.to_string();

    let src = read_file(path)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let budget = Budget::unlimited();
    let clock = shell_clock();
    let mut g = runtime::cli_guard(&budget, &clock);
    let (bytes, report) = selis_pdf_tools::annotate_spec(&src, &spec, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    let verification = commit(&src, &bytes, path, output, in_bytes, &budget, &mut g)?;
    eprintln!(
        "ink: page {} got {} strokes, {} annotation(s) on the page, original bytes preserved -> {output}",
        report.page,
        strokes.len(),
        report.annots
    );
    Ok(verification)
}

/// Run the shared write gate and build the verification line.
fn commit(
    src: &[u8],
    bytes: &[u8],
    path: &str,
    output: &str,
    in_bytes: u64,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> CliResult<Verification> {
    let observed = selis_pdf_cos::verify::survey(src, budget, g)
        .map_err(|e| CliError(format!("{path}: input survey failed: {e}")))?;
    let expected = write_gate::expectations_from(&observed, &PRESERVE_ON_APPEND);
    let verdict = write_gate::write_verified(bytes, output, &expected, budget, g)?;
    let verification = Verification::from_gate(
        Some(&observed),
        &verdict,
        &expected,
        in_bytes,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    );
    verification.emit_line();
    Ok(verification)
}

/// Parse `x,y,w,h`. An omitted rectangle is `0,0,0,0`: for ink the engine
/// derives the real `/Rect` from the strokes, so zeros are an honest placeholder.
fn parse_rect(rect: Option<&str>) -> CliResult<[f64; 4]> {
    let Some(rect) = rect else {
        return Ok([0.0; 4]);
    };
    let parts: Vec<&str> = rect.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(CliError(format!(
            "--rect must be x,y,w,h (four comma-separated numbers), got {rect:?}"
        )));
    }
    let mut values = [0.0; 4];
    for (value, part) in values.iter_mut().zip(parts) {
        *value = part
            .parse::<f64>()
            .map_err(|_| CliError(format!("--rect has a non-numeric coordinate in {rect:?}")))?;
        if !value.is_finite() {
            return Err(CliError(format!(
                "--rect has a non-finite coordinate in {rect:?}"
            )));
        }
    }
    Ok(values)
}

/// Parse `--colour r,g,b`. Each channel must be finite and in `0..=1`.
pub(crate) fn parse_colour(value: &str) -> CliResult<[f64; 3]> {
    let parts: Vec<&str> = value.split(',').map(str::trim).collect();
    if parts.len() != 3 {
        return Err(CliError(format!(
            "--colour must be r,g,b (three numbers in 0..=1), got {value:?}"
        )));
    }
    let mut colour = [0.0; 3];
    for (channel, part) in colour.iter_mut().zip(parts) {
        *channel = part
            .parse::<f64>()
            .map_err(|_| CliError(format!("--colour has a non-numeric channel in {value:?}")))?;
        if !channel.is_finite() || !(*channel >= 0.0 && *channel <= 1.0) {
            return Err(CliError(format!(
                "--colour channels must be finite numbers in 0..=1, got {value:?}"
            )));
        }
    }
    Ok(colour)
}

/// Parse one `--stroke`: `x,y;x,y;...`, at least two finite points.
fn parse_stroke(stroke: &str) -> CliResult<Vec<(f64, f64)>> {
    let mut points = Vec::new();
    for pair in stroke
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let mut coords = pair.split(',').map(str::trim);
        let x = coords
            .next()
            .and_then(|value| value.parse::<f64>().ok())
            .ok_or_else(|| CliError(format!("--stroke has a malformed point in {stroke:?}")))?;
        let y = coords
            .next()
            .and_then(|value| value.parse::<f64>().ok())
            .ok_or_else(|| CliError(format!("--stroke has a malformed point in {stroke:?}")))?;
        if coords.next().is_some() || !x.is_finite() || !y.is_finite() {
            return Err(CliError(format!(
                "--stroke has a non-finite or malformed point in {stroke:?}"
            )));
        }
        points.push((x, y));
    }
    if points.len() < 2 {
        return Err(CliError(format!(
            "--stroke needs two or more points, got {stroke:?}"
        )));
    }
    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strokes_need_two_finite_points() {
        assert!(parse_stroke("1,2;3,4").is_ok());
        assert!(parse_stroke("1,2").is_err());
        assert!(parse_stroke("1,2;NaN,4").is_err());
        assert!(parse_stroke("1,2,3;4,5").is_err());
        assert!(parse_stroke("").is_err());
    }

    #[test]
    fn rects_need_four_finite_numbers() {
        assert_eq!(parse_rect(None).unwrap(), [0.0; 4]);
        assert_eq!(parse_rect(Some("1, 2,3,4")).unwrap(), [1.0, 2.0, 3.0, 4.0]);
        assert!(parse_rect(Some("1,2,3")).is_err());
        assert!(parse_rect(Some("1,2,3,inf")).is_err());
    }
}
