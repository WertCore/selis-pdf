//! SVG to PDF: the documented subset.
//!
//! # Supported
//!
//! * One `<svg>` canvas. `width`/`height` may carry px, pt, pc, mm, cm, in or
//!   no unit (treated as px, 1 px = 0.75 pt); `viewBox` sets the user-space
//!   window and the two together scale it. With no `width`/`height` the
//!   viewBox sizes the page 1 user unit = 0.75 pt; with neither, the page is
//!   300 x 150 px. The page carries no margin and matches the canvas exactly.
//! * Containers: `<g>`, `<a>`, `<svg>` (nested, scaling into the same canvas is
//!   not supported - the outermost canvas sets the page). `<defs>` is skipped.
//! * Shapes: `<rect>` (sharp corners; `rx`/`ry` are ignored), `<circle>`,
//!   `<ellipse>`, `<line>`, `<polyline>`, `<polygon>`, `<path>` with the
//!   commands M m L l H h V v Z z C c S s Q q T t. Quadratic curves are
//!   promoted to cubics. **`A` (arcs) is refused with a message** - it is the
//!   one command this converter does not pretend to understand.
//! * `<text>`: a single laid-out line in a standard-14 face, positioned by
//!   `x`/`y`, sized by the inherited or own `font-size` (default 16), anchored
//!   by `text-anchor` (`start`, `middle`, `end`). `font-family` is ignored -
//!   Helvetica (or its bold/oblique face from `font-weight`/`font-style`) is
//!   used. `<tspan>` contributes its text to the surrounding `<text>` and its
//!   own positioning is dropped.
//! * Paint: `fill` and `stroke` with a solid colour - `#rgb`, `#rrggbb`,
//!   `rgb(...)` (integers or percents), a small set of CSS named colours,
//!   `none` or `transparent`. `url(...)` paints (gradients, patterns) are read
//!   as `none`. `fill-rule` is honoured (`nonzero`, `evenodd`). `stroke-width`,
//!   `stroke-linecap` (butt/round/square), `stroke-linejoin` (miter/round/
//!   bevel) and `stroke-dasharray` are honoured; the dash phase is 0.
//! * `transform` on any container or shape: `translate`, `scale`, `rotate`
//!   (with optional centre), `matrix`, `skewX`, `skewY`.
//! * The canvas is flipped once for PDF's y-up axis; every nested transform
//!   composes on top of that flip with `cm`.
//!
//! # Not supported (dropped silently)
//!
//! `opacity`, `fill-opacity`, `stroke-opacity`, `stroke-dashoffset`,
//! `stroke-miterlimit`, `color`, percentages of currentColor, `use`, `image`,
//! `foreignObject`, `clipPath`, `mask`, `marker`, `<style>` sheets, CSS in
//! `style="..."` attributes, and any element outside the supported set. Text is
//! encoded to WinAnsi like every other converter in this crate; characters it
//! cannot represent become `?`.
//!
//! # Errors
//!
//! Malformed XML or an `<path>` `A` command names the line or the command. An
//! SVG that contains none of the supported shapes still produces a one-page
//! PDF with the canvas on it.

#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation
)]

use selis_bytes::Bytes;
use selis_error::{Code, Ctx, Error, Result};
use selis_pdf_cos::doc_writer::{ContentBuilder, DocumentBuilder};
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::{Budget, BudgetGuard, Resource};

/// An input the converter cannot use, with the detail sanitised so a
/// document-derived name can appear in a diagnostic safely (ADR-P0017).
fn malformed(during: &'static str, message: String) -> Error {
    Error::with(
        Code::ConvertMalformed,
        Ctx::new()
            .during(during)
            .detail(Ctx::sanitise(message.as_bytes())),
    )
}

/// A construct that is valid but not implemented.
fn unsupported(during: &'static str, message: String) -> Error {
    Error::with(
        Code::ConvertUnsupported,
        Ctx::new()
            .during(during)
            .detail(Ctx::sanitise(message.as_bytes())),
    )
}

/// Route the walker's `String` refusals to the right code: arcs and unknown
/// commands are unimplemented features; everything else is malformed.
fn from_walk(message: String) -> Error {
    if message.contains("not supported") {
        unsupported("svg", message)
    } else {
        malformed("svg", message)
    }
}

use crate::blocks::{encode_text, measure};
use crate::emit::{bytes, take_num};
use crate::xmlmini::{self, Element};
use crate::Options;

/// One affine matrix in SVG order: maps (x, y) to (a*x + c*y + e, b*x + d*y + f).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Matrix {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Matrix {
    const IDENTITY: Matrix = Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    fn then(self, t: Matrix) -> Matrix {
        // `t` is applied first, then `self` (left-to-right SVG composition).
        Matrix {
            a: self.a * t.a + self.c * t.b,
            b: self.b * t.a + self.d * t.b,
            c: self.a * t.c + self.c * t.d,
            d: self.b * t.c + self.d * t.d,
            e: self.a * t.e + self.c * t.f + self.e,
            f: self.b * t.e + self.d * t.f + self.f,
        }
    }

    fn translate(x: f64, y: f64) -> Matrix {
        Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: x,
            f: y,
        }
    }

    fn scale(x: f64, y: f64) -> Matrix {
        Matrix {
            a: x,
            b: 0.0,
            c: 0.0,
            d: y,
            e: 0.0,
            f: 0.0,
        }
    }

    fn rotate(deg: f64, cx: f64, cy: f64) -> Matrix {
        let radians = deg.to_radians();
        let cos = radians.cos();
        let sin = radians.sin();
        Matrix::translate(cx, cy)
            .then(Matrix {
                a: cos,
                b: sin,
                c: -sin,
                d: cos,
                e: 0.0,
                f: 0.0,
            })
            .then(Matrix::translate(-cx, -cy))
    }

    fn skew_x(deg: f64) -> Matrix {
        Matrix {
            a: 1.0,
            b: 0.0,
            c: deg.to_radians().tan(),
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    fn skew_y(deg: f64) -> Matrix {
        Matrix {
            a: 1.0,
            b: deg.to_radians().tan(),
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }
}

/// Parse one SVG number (the grammar allows signs, decimals and exponents).
fn parse_number(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<f64>().ok()
}

/// Parse a comma/space separated list of SVG numbers.
fn parse_number_list(text: &str) -> Vec<f64> {
    let normalized = text.replace(',', " ");
    normalized
        .split_ascii_whitespace()
        .filter_map(parse_number)
        .collect()
}

/// Split an SVG transform list into its function calls.
fn transform_items(text: &str) -> Vec<(String, Vec<f64>)> {
    let mut out = Vec::new();
    let mut rest = text.trim();
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().to_ascii_lowercase();
        let Some(close) = rest[open..].find(')') else {
            break;
        };
        let args = parse_number_list(&rest[open + 1..open + close]);
        rest = rest[open + close + 1..].trim_start_matches([',', ' ', '\t', '\n', '\r']);
        if !name.is_empty() {
            out.push((name, args));
        }
    }
    out
}

/// Compose one `transform="..."` attribute into a single matrix.
fn compose_transform(text: &str) -> std::result::Result<Matrix, String> {
    let mut matrix = Matrix::IDENTITY;
    for (name, args) in transform_items(text) {
        let next = match name.as_str() {
            "translate" => {
                let x = args.first().copied().unwrap_or(0.0);
                let y = args.get(1).copied().unwrap_or(0.0);
                Matrix::translate(x, y)
            }
            "scale" => {
                let x = args.first().copied().unwrap_or(1.0);
                let y = args.get(1).copied().unwrap_or(x);
                Matrix::scale(x, y)
            }
            "rotate" => {
                let angle = args.first().copied().unwrap_or(0.0);
                let cx = args.get(1).copied().unwrap_or(0.0);
                let cy = args.get(2).copied().unwrap_or(0.0);
                Matrix::rotate(angle, cx, cy)
            }
            "matrix" => {
                if args.len() != 6 {
                    return Err(format!(
                        "the transform `matrix` needs 6 arguments, got {}",
                        args.len()
                    ));
                }
                Matrix {
                    a: args[0],
                    b: args[1],
                    c: args[2],
                    d: args[3],
                    e: args[4],
                    f: args[5],
                }
            }
            "skewx" => Matrix::skew_x(args.first().copied().unwrap_or(0.0)),
            "skewy" => Matrix::skew_y(args.first().copied().unwrap_or(0.0)),
            other => {
                return Err(format!(
                    "the transform `{other}` is not supported (translate, scale, rotate, matrix, skewX, skewY)"
                ))
            }
        };
        matrix = matrix.then(next);
    }
    Ok(matrix)
}

/// A CSS length converted to user units (px for unitless/px, pt kept apart).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Length {
    value: f64,
    unit: LengthUnit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LengthUnit {
    /// Points (1 pt = 1 pt in the page).
    Point,
    /// Pixels; SVG user units scale with px. 1 px = 0.75 pt.
    Pixel,
    /// Millimetres, centimetres, inches, picas - converted to points.
    Millimetre,
    Centimetre,
    Inch,
    Pica,
}

fn parse_length(text: &str) -> Option<Length> {
    let trimmed = text.trim();
    let number: String = trimmed
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == '-' || *ch == '+')
        .collect();
    let value = number.parse::<f64>().ok()?;
    let unit = trimmed[number.len()..].trim().to_ascii_lowercase();
    let unit = match unit.as_str() {
        "" | "px" => LengthUnit::Pixel,
        "pt" => LengthUnit::Point,
        "mm" => LengthUnit::Millimetre,
        "cm" => LengthUnit::Centimetre,
        "in" => LengthUnit::Inch,
        "pc" => LengthUnit::Pica,
        // Percentages and unknown units are refused rather than guessed.
        _ => return None,
    };
    Some(Length { value, unit })
}

fn length_to_px(length: Length) -> f64 {
    match length.unit {
        LengthUnit::Point => length.value / 0.75,
        LengthUnit::Pixel => length.value,
        LengthUnit::Millimetre => length.value * 72.0 / 25.4,
        LengthUnit::Centimetre => length.value * 72.0 / 2.54,
        LengthUnit::Inch => length.value * 72.0,
        LengthUnit::Pica => length.value * 12.0,
    }
}

fn px_to_pt(px: f64) -> f64 {
    px * 0.75
}

/// A resolved paint colour, or nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RgbColor(f64, f64, f64);

/// Named colours this subset recognises; alles else falls back to `none`.
const NAMED_COLORS: &[(&str, (f64, f64, f64))] = &[
    ("black", (0.0, 0.0, 0.0)),
    ("white", (1.0, 1.0, 1.0)),
    ("red", (1.0, 0.0, 0.0)),
    ("green", (0.0, 0.50196078, 0.0)),
    ("lime", (0.0, 1.0, 0.0)),
    ("blue", (0.0, 0.0, 1.0)),
    ("yellow", (1.0, 1.0, 0.0)),
    ("cyan", (0.0, 1.0, 1.0)),
    ("aqua", (0.0, 1.0, 1.0)),
    ("magenta", (1.0, 0.0, 1.0)),
    ("fuchsia", (1.0, 0.0, 1.0)),
    ("gray", (0.50196078, 0.50196078, 0.50196078)),
    ("grey", (0.50196078, 0.50196078, 0.50196078)),
    ("silver", (0.7529412, 0.7529412, 0.7529412)),
    ("maroon", (0.50196078, 0.0, 0.0)),
    ("olive", (0.50196078, 0.50196078, 0.0)),
    ("navy", (0.0, 0.0, 0.50196078)),
    ("purple", (0.50196078, 0.0, 0.50196078)),
    ("teal", (0.0, 0.50196078, 0.50196078)),
    ("orange", (1.0, 0.6470588, 0.0)),
    ("transparent", (0.0, 0.0, 0.0)),
];

/// Parse an SVG colour; `None` means "no paint" (`none`, `transparent`, a
/// `url()` paint, or an empty reference).
fn parse_color(text: &str) -> Option<RgbColor> {
    let trimmed = text.trim().to_ascii_lowercase();
    if trimmed.is_empty()
        || trimmed == "none"
        || trimmed == "transparent"
        || trimmed.starts_with("url(")
    {
        return None;
    }
    if let Some(hex) = trimmed.strip_prefix('#') {
        let bytes = hex.as_bytes();
        let channel = |at: usize, width: usize| -> Option<f64> {
            let mut value = 0u32;
            for offset in 0..width {
                value = value * 16 + (*bytes.get(at + offset)? as char).to_digit(16)?;
            }
            let scale = if width == 1 { 17.0 } else { 255.0 };
            Some(f64::from(value) / scale)
        };
        return match hex.len() {
            3 => Some(RgbColor(channel(0, 1)?, channel(1, 1)?, channel(2, 1)?)),
            6 => Some(RgbColor(channel(0, 2)?, channel(2, 2)?, channel(4, 2)?)),
            _ => None,
        };
    }
    if let Some(inner) = trimmed
        .strip_prefix("rgb(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let any_percent = inner.contains('%');
        let normalized = inner.replace('%', "");
        let parts = parse_number_list(&normalized);
        if parts.len() == 3 {
            let channel = |value: f64| -> f64 {
                if any_percent {
                    value / 100.0
                } else {
                    (value / 255.0).clamp(0.0, 1.0)
                }
            };
            return Some(RgbColor(
                channel(parts[0]).clamp(0.0, 1.0),
                channel(parts[1]).clamp(0.0, 1.0),
                channel(parts[2]).clamp(0.0, 1.0),
            ));
        }
        return None;
    }
    for (name, (r, g, b)) in NAMED_COLORS {
        if *name == trimmed {
            return Some(RgbColor(*r, *g, *b));
        }
    }
    None
}

/// The paint state inherited down the tree.
#[derive(Debug, Clone, PartialEq)]
struct State {
    fill: Option<RgbColor>,
    stroke: Option<RgbColor>,
    stroke_width: f64,
    fill_even_odd: bool,
    line_cap: u32,
    line_join: u32,
    dash: Vec<f64>,
    font_size: f64,
    font_weight_bold: bool,
    font_style_italic: bool,
    text_anchor: Anchor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    Start,
    Middle,
    End,
}

impl Default for State {
    fn default() -> Self {
        Self {
            fill: Some(RgbColor(0.0, 0.0, 0.0)),
            stroke: None,
            stroke_width: 1.0,
            fill_even_odd: false,
            line_cap: 0,
            line_join: 0,
            dash: Vec::new(),
            font_size: 16.0,
            font_weight_bold: false,
            font_style_italic: false,
            text_anchor: Anchor::Start,
        }
    }
}

impl State {
    /// The `font-family`-substituted face for this state.
    fn font(&self) -> &'static str {
        match (self.font_weight_bold, self.font_style_italic) {
            (true, true) => "Helvetica-BoldOblique",
            (true, false) => "Helvetica-Bold",
            (false, true) => "Helvetica-Oblique",
            (false, false) => "Helvetica",
        }
    }

    fn font_key(&self) -> &'static str {
        match (self.font_weight_bold, self.font_style_italic) {
            (true, true) => "F4",
            (true, false) => "F2",
            (false, true) => "F3",
            (false, false) => "F1",
        }
    }
}

/// Resolve one presentation attribute into the state.
fn apply_state(mut state: State, attrs: &Element) -> State {
    if let Some(fill) = attrs.attr("fill") {
        state.fill = parse_color(fill);
    }
    if let Some(stroke) = attrs.attr("stroke") {
        state.stroke = parse_color(stroke);
    }
    if let Some(width) = attrs
        .attr("stroke-width")
        .and_then(parse_number)
        .filter(|value| *value >= 0.0)
    {
        state.stroke_width = width;
    }
    if let Some(rule) = attrs.attr("fill-rule") {
        state.fill_even_odd = rule.trim().eq_ignore_ascii_case("evenodd");
    }
    if let Some(cap) = attrs.attr("stroke-linecap") {
        state.line_cap = match cap.trim().to_ascii_lowercase().as_str() {
            "round" => 1,
            "square" => 2,
            _ => 0,
        };
    }
    if let Some(join) = attrs.attr("stroke-linejoin") {
        state.line_join = match join.trim().to_ascii_lowercase().as_str() {
            "round" => 1,
            "bevel" => 2,
            _ => 0,
        };
    }
    if let Some(dash) = attrs.attr("stroke-dasharray") {
        state.dash = if dash.trim().eq_ignore_ascii_case("none") {
            Vec::new()
        } else {
            parse_number_list(dash)
                .into_iter()
                .filter(|value| *value > 0.0)
                .collect()
        };
    }
    if let Some(size) = attrs.attr("font-size").and_then(parse_length) {
        state.font_size = length_to_px(size);
    } else if let Some(size) = attrs.attr("font-size").and_then(parse_number) {
        state.font_size = size;
    }
    if let Some(weight) = attrs.attr("font-weight") {
        state.font_weight_bold = matches!(
            weight.trim().to_ascii_lowercase().as_str(),
            "bold" | "bolder" | "600" | "700" | "800" | "900"
        );
    }
    if let Some(style) = attrs.attr("font-style") {
        state.font_style_italic = matches!(
            style.trim().to_ascii_lowercase().as_str(),
            "italic" | "oblique"
        );
    }
    if let Some(anchor) = attrs.attr("text-anchor") {
        state.text_anchor = match anchor.trim().to_ascii_lowercase().as_str() {
            "middle" => Anchor::Middle,
            "end" => Anchor::End,
            _ => Anchor::Start,
        };
    }
    state
}

/// Paint one path with the current state.
fn paint_path(cb: &mut ContentBuilder, state: &State, close: bool) {
    if state.stroke.is_some() {
        cb.set_line_width(state.stroke_width);
        cb.set_line_cap(state.line_cap);
        cb.set_line_join(state.line_join);
        cb.set_dash(&state.dash, 0.0);
        let [r, g, b] = {
            let RgbColor(r, g, b) = state.stroke.unwrap_or(RgbColor(0.0, 0.0, 0.0));
            [r, g, b]
        };
        cb.set_stroke(r, g, b);
    }
    if let Some(RgbColor(r, g, b)) = state.fill {
        cb.set_fill(r, g, b);
    }
    match (state.fill.is_some(), state.stroke.is_some()) {
        (true, true) => {
            if state.fill_even_odd {
                if close {
                    cb.close_fill_stroke_even_odd();
                } else {
                    cb.fill_stroke_even_odd();
                }
            } else if close {
                cb.close_fill_stroke();
            } else {
                cb.fill_stroke();
            }
        }
        (true, false) => {
            if state.fill_even_odd {
                cb.fill_even_odd();
            } else {
                cb.fill();
            }
        }
        (false, true) => {
            if close {
                cb.close_stroke();
            } else {
                cb.stroke();
            }
        }
        (false, false) => {
            // Nothing paints: discard the path.
            cb.discard_path();
        }
    }
}

/// Emit an SVG arc as a cubic - refused by name instead.
fn arc_refused() -> String {
    "path command `A` (arc) is not supported in this converter; use cubic curves (C) instead"
        .to_string()
}

/// The current point while walking a path's `d` attribute.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PathCursor {
    x: f64,
    y: f64,
    /// The last cubic second control point, for `S`.
    last_cubic_control: Option<(f64, f64)>,
    /// The last quadratic control point, for `T`.
    last_quad_control: Option<(f64, f64)>,
    start_x: f64,
    start_y: f64,
    subpath_closed: bool,
}

impl PathCursor {
    const ORIGIN: PathCursor = PathCursor {
        x: 0.0,
        y: 0.0,
        last_cubic_control: None,
        last_quad_control: None,
        start_x: 0.0,
        start_y: 0.0,
        subpath_closed: false,
    };
}

/// Lowercase the command letters of a path `d` attribute while leaving the
/// numbers alone, so relative and absolute forms share one walker.
fn path_commands(text: &str) -> Vec<(char, Vec<f64>)> {
    let mut out = Vec::new();
    let mut current: Option<char> = None;
    let mut numbers = String::new();
    let mut flush_numbers = |numbers: &mut String, current: &mut Option<char>| {
        if let Some(command) = *current {
            let args = parse_number_list(numbers);
            out.push((command, args));
        }
        numbers.clear();
        *current = None;
    };
    for ch in text.chars() {
        if ch.is_ascii_alphabetic() {
            if ch == 'e' || ch == 'E' {
                // Part of a number's exponent.
                numbers.push(ch);
                continue;
            }
            flush_numbers(&mut numbers, &mut current);
            current = Some(ch);
            continue;
        }
        numbers.push(ch);
    }
    flush_numbers(&mut numbers, &mut current);
    out
}

/// Walk a path `d` attribute, emitting the path ops.
fn emit_path(cb: &mut ContentBuilder, d: &str) -> std::result::Result<(), String> {
    let mut cursor = PathCursor::ORIGIN;
    for (command, args) in path_commands(d) {
        match command {
            'M' | 'm' => {
                let relative = command == 'm';
                let mut index = 0usize;
                let mut first = true;
                while index < args.len() {
                    let (Some(&px), Some(&py)) = (args.get(index), args.get(index + 1)) else {
                        break;
                    };
                    let (x, y) = if relative {
                        (cursor.x + px, cursor.y + py)
                    } else {
                        (px, py)
                    };
                    if first {
                        cb.move_to(x, y);
                        cursor.start_x = x;
                        cursor.start_y = y;
                        first = false;
                        cursor.subpath_closed = false;
                    } else {
                        // Implicit lineto after the first moveto pair.
                        cb.line_to(x, y);
                    }
                    cursor.x = x;
                    cursor.y = y;
                    cursor.last_cubic_control = None;
                    cursor.last_quad_control = None;
                    index += 2;
                }
            }
            'L' | 'l' => {
                let relative = command == 'l';
                for pair in args.chunks(2) {
                    let [px, py] = pair else { break };
                    let (x, y) = if relative {
                        (cursor.x + px, cursor.y + py)
                    } else {
                        (*px, *py)
                    };
                    cb.line_to(x, y);
                    cursor.x = x;
                    cursor.y = y;
                    cursor.last_cubic_control = None;
                    cursor.last_quad_control = None;
                }
            }
            'H' | 'h' => {
                let relative = command == 'h';
                for arg in args {
                    let x = if relative { cursor.x + arg } else { arg };
                    cb.line_to(x, cursor.y);
                    cursor.x = x;
                    cursor.last_cubic_control = None;
                    cursor.last_quad_control = None;
                }
            }
            'V' | 'v' => {
                let relative = command == 'v';
                for arg in args {
                    let y = if relative { cursor.y + arg } else { arg };
                    cb.line_to(cursor.x, y);
                    cursor.y = y;
                    cursor.last_cubic_control = None;
                    cursor.last_quad_control = None;
                }
            }
            'Z' | 'z' => {
                cb.close_path();
                cursor.x = cursor.start_x;
                cursor.y = cursor.start_y;
                cursor.last_cubic_control = None;
                cursor.last_quad_control = None;
                cursor.subpath_closed = true;
            }
            'C' | 'c' => {
                let relative = command == 'c';
                for chunk in args.chunks(6) {
                    let [x1, y1, x2, y2, x, y] = chunk else { break };
                    let (x1, y1, x2, y2, x, y) = if relative {
                        (
                            cursor.x + x1,
                            cursor.y + y1,
                            cursor.x + x2,
                            cursor.y + y2,
                            cursor.x + x,
                            cursor.y + y,
                        )
                    } else {
                        (*x1, *y1, *x2, *y2, *x, *y)
                    };
                    cb.cubic_to(x1, y1, x2, y2, x, y);
                    cursor.last_cubic_control = Some((x2, y2));
                    cursor.last_quad_control = None;
                    cursor.x = x;
                    cursor.y = y;
                }
            }
            'S' | 's' => {
                let relative = command == 's';
                for chunk in args.chunks(4) {
                    let [x2, y2, x, y] = chunk else { break };
                    let (x2, y2, x, y) = if relative {
                        (cursor.x + x2, cursor.y + y2, cursor.x + x, cursor.y + y)
                    } else {
                        (*x2, *y2, *x, *y)
                    };
                    let (x1, y1) = cursor
                        .last_cubic_control
                        .map(|(cx, cy)| (2.0 * cursor.x - cx, 2.0 * cursor.y - cy))
                        .unwrap_or((cursor.x, cursor.y));
                    cb.cubic_to(x1, y1, x2, y2, x, y);
                    cursor.last_cubic_control = Some((x2, y2));
                    cursor.last_quad_control = None;
                    cursor.x = x;
                    cursor.y = y;
                }
            }
            'Q' | 'q' => {
                let relative = command == 'q';
                for chunk in args.chunks(4) {
                    let [qx, qy, x, y] = chunk else { break };
                    let (qx, qy, x, y) = if relative {
                        (cursor.x + qx, cursor.y + qy, cursor.x + x, cursor.y + y)
                    } else {
                        (*qx, *qy, *x, *y)
                    };
                    // Promote the quadratic to a cubic.
                    let x1 = cursor.x + 2.0 * (qx - cursor.x) / 3.0;
                    let y1 = cursor.y + 2.0 * (qy - cursor.y) / 3.0;
                    let x2 = x + 2.0 * (qx - x) / 3.0;
                    let y2 = y + 2.0 * (qy - y) / 3.0;
                    cb.cubic_to(x1, y1, x2, y2, x, y);
                    cursor.last_quad_control = Some((qx, qy));
                    cursor.last_cubic_control = None;
                    cursor.x = x;
                    cursor.y = y;
                }
            }
            'T' | 't' => {
                let relative = command == 't';
                for pair in args.chunks(2) {
                    let [x, y] = pair else { break };
                    let (x, y) = if relative {
                        (cursor.x + x, cursor.y + y)
                    } else {
                        (*x, *y)
                    };
                    let (qx, qy) = cursor
                        .last_quad_control
                        .map(|(cx, cy)| (2.0 * cursor.x - cx, 2.0 * cursor.y - cy))
                        .unwrap_or((cursor.x, cursor.y));
                    let x1 = cursor.x + 2.0 * (qx - cursor.x) / 3.0;
                    let y1 = cursor.y + 2.0 * (qy - cursor.y) / 3.0;
                    let x2 = x + 2.0 * (qx - x) / 3.0;
                    let y2 = y + 2.0 * (qy - y) / 3.0;
                    cb.cubic_to(x1, y1, x2, y2, x, y);
                    cursor.last_quad_control = Some((qx, qy));
                    cursor.last_cubic_control = None;
                    cursor.x = x;
                    cursor.y = y;
                }
            }
            'A' | 'a' => {
                return Err(arc_refused());
            }
            other => {
                return Err(format!(
                    "path command `{other}` is not supported (M L H V C S Q T Z)"
                ));
            }
        }
    }
    Ok(())
}

/// Emit the path ops for a `<path>` element.
fn emit_path_element(
    cb: &mut ContentBuilder,
    d: &str,
    state: &State,
) -> std::result::Result<(), String> {
    emit_path(cb, d)?;
    paint_path(cb, state, false);
    Ok(())
}

/// Emit the ops for one shape element.
fn emit_shape(
    cb: &mut ContentBuilder,
    element: &Element,
    state: &State,
) -> std::result::Result<(), String> {
    match element.tag.as_str() {
        "rect" => {
            let x = element.attr("x").and_then(parse_number).unwrap_or(0.0);
            let y = element.attr("y").and_then(parse_number).unwrap_or(0.0);
            let width = element.attr("width").and_then(parse_number).unwrap_or(0.0);
            let height = element.attr("height").and_then(parse_number).unwrap_or(0.0);
            if width > 0.0 && height > 0.0 {
                cb.move_to(x, y);
                cb.line_to(x + width, y);
                cb.line_to(x + width, y + height);
                cb.line_to(x, y + height);
                cb.close_path();
                paint_path(cb, state, false);
            }
        }
        "circle" => {
            let cx = element.attr("cx").and_then(parse_number).unwrap_or(0.0);
            let cy = element.attr("cy").and_then(parse_number).unwrap_or(0.0);
            let r = element.attr("r").and_then(parse_number).unwrap_or(0.0);
            if r > 0.0 {
                ellipse(cb, cx, cy, r, r);
                paint_path(cb, state, false);
            }
        }
        "ellipse" => {
            let cx = element.attr("cx").and_then(parse_number).unwrap_or(0.0);
            let cy = element.attr("cy").and_then(parse_number).unwrap_or(0.0);
            let rx = element.attr("rx").and_then(parse_number).unwrap_or(0.0);
            let ry = element.attr("ry").and_then(parse_number).unwrap_or(0.0);
            if rx > 0.0 && ry > 0.0 {
                ellipse(cb, cx, cy, rx, ry);
                paint_path(cb, state, false);
            }
        }
        "line" => {
            let x1 = element.attr("x1").and_then(parse_number).unwrap_or(0.0);
            let y1 = element.attr("y1").and_then(parse_number).unwrap_or(0.0);
            let x2 = element.attr("x2").and_then(parse_number).unwrap_or(0.0);
            let y2 = element.attr("y2").and_then(parse_number).unwrap_or(0.0);
            cb.move_to(x1, y1);
            cb.line_to(x2, y2);
            // `line` never fills, whatever the fill state says.
            let line_state = State {
                fill: None,
                ..state.clone()
            };
            paint_path(cb, &line_state, false);
        }
        "polyline" | "polygon" => {
            let points: Vec<(f64, f64)> = parse_number_list(element.attr("points").unwrap_or(""))
                .chunks(2)
                .filter_map(|pair| match pair {
                    [x, y] => Some((*x, *y)),
                    _ => None,
                })
                .collect();
            let Some(first) = points.first() else {
                return Ok(());
            };
            cb.move_to(first.0, first.1);
            for (x, y) in points.iter().skip(1) {
                cb.line_to(*x, *y);
            }
            let closed = element.tag == "polygon";
            if closed {
                cb.close_path();
            }
            paint_path(cb, state, false);
        }
        "path" => {
            let d = element.attr("d").unwrap_or("");
            emit_path_element(cb, d, state)?;
        }
        "text" => {
            let x = element.attr("x").and_then(parse_number).unwrap_or(0.0);
            let y = element.attr("y").and_then(parse_number).unwrap_or(0.0);
            let content = collapse_text(&element.text);
            if content.is_empty() {
                return Ok(());
            }
            let width = measure(state.font(), &content, state.font_size);
            let anchored = match state.text_anchor {
                Anchor::Start => x,
                Anchor::Middle => x - width / 2.0,
                Anchor::End => x - width,
            };
            if let Some(RgbColor(r, g, b)) = state.fill {
                cb.set_fill(r, g, b);
            }
            cb.begin_text();
            cb.set_font(state.font_key(), state.font_size);
            // SVG y is the baseline; PDF text matrix is y-up after the flip.
            cb.set_text_matrix(anchored, y);
            cb.show_raw(&encode_text(&content));
            cb.end_text();
        }
        _ => {}
    }
    Ok(())
}

/// Collapse runs of whitespace the way SVG does by default.
fn collapse_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Emit a circle/ellipse as four cubics.
fn ellipse(cb: &mut ContentBuilder, cx: f64, cy: f64, rx: f64, ry: f64) {
    const KAPPA: f64 = 0.5522847498307936;
    let ox = KAPPA * rx;
    let oy = KAPPA * ry;
    cb.move_to(cx + rx, cy);
    cb.cubic_to(cx + rx, cy + oy, cx + ox, cy + ry, cx, cy + ry);
    cb.cubic_to(cx - ox, cy + ry, cx - rx, cy + oy, cx - rx, cy);
    cb.cubic_to(cx - rx, cy - oy, cx - ox, cy - ry, cx, cy - ry);
    cb.cubic_to(cx + ox, cy - ry, cx + rx, cy - oy, cx + rx, cy);
    cb.close_path();
}

/// Walk the tree, emitting every supported element.
fn emit_element(
    cb: &mut ContentBuilder,
    element: &Element,
    state: State,
) -> std::result::Result<(), String> {
    let tag = element.tag.as_str();
    if tag == "defs" || tag == "style" {
        return Ok(());
    }
    let state = apply_state(state, element);
    let own_transform = element
        .attr("transform")
        .map(compose_transform)
        .transpose()?;
    let nested = matches!(tag, "g" | "a" | "svg");
    let had_transform = own_transform.is_some();
    if let Some(matrix) = own_transform {
        cb.save_state();
        cb.concat(matrix.a, matrix.b, matrix.c, matrix.d, matrix.e, matrix.f);
    } else if nested {
        cb.save_state();
    }
    match tag {
        "g" | "a" | "svg" => {
            for child in &element.children {
                emit_element(cb, child, state.clone())?;
            }
        }
        "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" | "path" | "text" => {
            emit_shape(cb, element, &state)?;
        }
        _ => {
            // Unknown elements are skipped with their subtrees (see the docs).
        }
    }
    if had_transform || nested {
        cb.restore_state();
    }
    Ok(())
}

/// Count emitted elements to bound the walk; an SVG bomb is a real class of
/// hostile input. Iterative so a deep tree cannot overflow the stack.
const MAX_ELEMENTS: usize = 20_000;

fn count_elements(element: &Element) -> usize {
    let mut count = 0usize;
    let mut stack = vec![element];
    while let Some(node) = stack.pop() {
        count = count.saturating_add(1);
        for child in &node.children {
            stack.push(child);
        }
    }
    count
}

/// Convert SVG text into a one-page PDF.
///
/// # Malformed Input
///
/// Malformed XML, an unsupported `A` arc command, an unsupported transform or
/// a too-large document are refusals with a message; they never produce a
/// partial file.
///
/// # Output Guarantees
///
/// A complete single-revision PDF 1.4 document with one page sized to the
/// canvas and font resources for the faces referenced.
///
/// # Errors
///
/// `BUDGET_*` on budget exhaustion, plus the refusals above.
pub(crate) fn svg_to_pdf(
    input: &str,
    opts: &Options,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let root = xmlmini::parse(input).map_err(|message| malformed("svg", message))?;
    if !root.tag.eq_ignore_ascii_case("svg") {
        return Err(malformed(
            "svg",
            format!("expected an <svg> root, got <{}>", root.tag),
        ));
    }

    // Canvas: px-based lengths.
    let view_box: Option<[f64; 4]> =
        root.attr("viewBox")
            .map(parse_number_list)
            .and_then(|values| {
                let [a, b, c, d] = values.as_slice() else {
                    return None;
                };
                Some([*a, *b, *c, *d])
            });
    let width = root.attr("width").and_then(parse_length).map(length_to_px);
    let height = root.attr("height").and_then(parse_length).map(length_to_px);
    let (user_w, user_h) = match view_box {
        Some([_, _, w, h]) if w > 0.0 && h > 0.0 => (w, h),
        _ => (width.unwrap_or(300.0), height.unwrap_or(150.0)),
    };
    let (canvas_w, canvas_h) = match (width, height) {
        (Some(w), Some(h)) if w > 0.0 && h > 0.0 => (w, h),
        _ => (user_w, user_h),
    };
    let page_w = px_to_pt(canvas_w);
    let page_h = px_to_pt(canvas_h);
    if !(page_w.is_finite() && page_h.is_finite()) || page_w <= 0.0 || page_h <= 0.0 {
        return Err(malformed(
            "svg",
            "the SVG canvas has no usable size".to_string(),
        ));
    }
    if count_elements(&root) > MAX_ELEMENTS {
        return Err(malformed(
            "svg",
            format!("the SVG has more than {MAX_ELEMENTS} elements"),
        ));
    }

    let scale_x = canvas_w / user_w;
    let scale_y = canvas_h / user_h;

    let mut cb = ContentBuilder::new();
    // Flip y once: SVG is y-down, PDF is y-up.
    cb.save_state();
    cb.concat(1.0, 0.0, 0.0, -1.0, 0.0, page_h);
    if (scale_x - 1.0).abs() > f64::EPSILON || (scale_y - 1.0).abs() > f64::EPSILON {
        cb.concat(scale_x, 0.0, 0.0, scale_y, 0.0, 0.0);
    }
    if let Some([vx, vy, _, _]) = view_box {
        if vx != 0.0 || vy != 0.0 {
            cb.concat(1.0, 0.0, 0.0, 1.0, -vx, -vy);
        }
    }

    emit_element(&mut cb, &root, State::default()).map_err(from_walk)?;
    cb.restore_state();

    const FONTS: [&str; 5] = [
        "Helvetica",
        "Helvetica-Bold",
        "Helvetica-Oblique",
        "Helvetica-BoldOblique",
        "Courier",
    ];

    let mut b = DocumentBuilder::new();
    if let Some(title) = &opts.title {
        if !title.is_empty() {
            b.set_info(b"Title", title);
        }
    }
    let mut font_pairs: Vec<(Bytes, Obj)> = Vec::new();
    for (index, name) in FONTS.iter().enumerate() {
        let num = take_num(&mut b);
        b.add_object(
            num,
            Obj::Dict(vec![
                (bytes(b"Type"), Obj::Name(bytes(b"Font"))),
                (bytes(b"Subtype"), Obj::Name(bytes(b"Type1"))),
                (bytes(b"BaseFont"), Obj::Name(bytes(name.as_bytes()))),
                (bytes(b"Encoding"), Obj::Name(bytes(b"WinAnsiEncoding"))),
            ]),
        );
        let key = format!("F{}", index.saturating_add(1));
        font_pairs.push((bytes(key.as_bytes()), Obj::Ref(Ref::new(num, 0))));
    }
    let res_num = take_num(&mut b);
    b.add_object(
        res_num,
        Obj::Dict(vec![(bytes(b"Font"), Obj::Dict(font_pairs))]),
    );
    let res_ref = Ref::new(res_num, 0);

    let data = cb.to_bytes();
    let content_num = take_num(&mut b);
    b.add_object(
        content_num,
        Obj::Stream {
            dict: vec![(
                bytes(b"Length"),
                Obj::Int(i64::try_from(data.len()).unwrap_or(i64::MAX)),
            )],
            data: Bytes::from(data),
        },
    );
    b.add_page_with(page_w, page_h, &[Ref::new(content_num, 0)], Some(res_ref));

    g.charge(Resource::Objects, 8)?;
    b.write(budget, g)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    fn render(svg: &str) -> Vec<u8> {
        svg_to_pdf(svg, &Options::default(), &Budget::unlimited(), &mut guard())
            .expect("svg should convert")
    }

    #[test]
    fn a_document_with_shapes_converts() {
        let bytes = render(
            r##"<svg width="100" height="50" viewBox="0 0 100 50" xmlns="http://www.w3.org/2000/svg">
                 <rect x="5" y="5" width="40" height="30" fill="#ff0000" stroke="#000" stroke-width="2"/>
                 <circle cx="70" cy="20" r="12" fill="blue"/>
                 <text x="10" y="45" font-size="12">Hi</text>
               </svg>"##,
        );
        assert!(bytes.starts_with(b"%PDF-1.4"));
        let text = String::from_utf8_lossy(&bytes);
        // The flip, the shapes and the text are in the content stream.
        assert!(text.contains(" cm\n"), "{text}");
        assert!(text.contains("1 0 0 -1 0 37.5 cm"), "{text}");
        assert!(text.contains("Hi"), "{text}");
        opens_with_page_count(&bytes, 1);
    }

    #[test]
    fn arcs_are_refused_by_name() {
        let Err(message) = svg_to_pdf(
            r##"<svg width="10" height="10"><path d="M 0 0 A 5 5 0 0 1 9 9" fill="none" stroke="#000"/></svg>"##,
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        ) else {
            panic!("an arc must be refused");
        };
        let detail = message.ctx().detail.clone().unwrap_or_default();
        assert!(detail.contains("arc"), "{message}");
        assert_eq!(message.code(), selis_error::Code::ConvertUnsupported);
    }

    #[test]
    fn malformed_xml_is_malformed_not_a_crash() {
        let Err(message) = svg_to_pdf(
            "<svg><g></svg></g>",
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        ) else {
            panic!("mismatched tags must be refused");
        };
        assert_eq!(message.code(), selis_error::Code::ConvertMalformed);
    }

    #[test]
    fn a_non_svg_root_is_malformed() {
        let Err(message) = svg_to_pdf(
            "<html/>",
            &Options::default(),
            &Budget::unlimited(),
            &mut guard(),
        ) else {
            panic!("a non-SVG root must be refused");
        };
        assert_eq!(message.code(), selis_error::Code::ConvertMalformed);
    }

    #[test]
    fn transforms_compose_left_to_right() {
        // translate(10, 0) then scale(2): a point at 1,0 lands at x = 10 + 2*1 = 12.
        let bytes = render(
            r##"<svg width="50" height="10" xmlns="http://www.w3.org/2000/svg">
                 <g transform="translate(10 0) scale(2)">
                   <rect width="1" height="1" fill="#000000"/>
                 </g>
               </svg>"##,
        );
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("2 0 0 2 10 0 cm"), "{text}");
    }

    #[test]
    fn paths_support_relative_and_quadratic_commands() {
        let bytes = render(
            r##"<svg width="50" height="10" xmlns="http://www.w3.org/2000/svg">
                 <path d="m 1 1 q 4 6 8 0 t 8 0" stroke="#000000" fill="none"/>
               </svg>"##,
        );
        // Two quadratics were promoted to two cubics; the output opens fine.
        opens_with_page_count(&bytes, 1);
    }

    #[test]
    fn text_anchor_middle_and_end_shift_the_start() {
        let start = String::from_utf8_lossy(&render(
            r##"<svg width="100" height="20" xmlns="http://www.w3.org/2000/svg"><text x="50" y="10" text-anchor="middle">abc</text></svg>"##,
        ))
        .into_owned();
        let plain = String::from_utf8_lossy(&render(
            r##"<svg width="100" height="20" xmlns="http://www.w3.org/2000/svg"><text x="50" y="10">abc</text></svg>"##,
        ))
        .into_owned();
        assert_ne!(start, plain);
    }

    #[test]
    fn a_page_with_no_viewbox_and_no_size_uses_the_default_canvas() {
        let bytes = render(r##"<svg/>"##);
        opens_with_page_count(&bytes, 1);
    }

    /// The output must open with selis's own parser.
    fn opens_with_page_count(bytes: &[u8], expected_pages: usize) {
        let budget = Budget::unlimited();
        let mut g = Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new());
        let startxref = selis_pdf_cos::xref::find_startxref(bytes, 4096).expect("startxref");
        let doc = selis_pdf_cos::parse_revisions(bytes, startxref, &budget, &mut g).expect("open");
        let rev = doc.revisions().last().expect("revision");
        let mut pages = 0usize;
        for &num in rev.entries.keys() {
            if let Ok(selis_pdf_cos::Obj::Dict(pairs)) = selis_pdf_cos::resolve_object(
                bytes,
                rev.entries
                    .get(&num)
                    .map(|e| match e {
                        selis_pdf_cos::XrefEntry::InUse { offset, .. } => *offset,
                        _ => 0,
                    })
                    .unwrap_or(0),
                &budget,
                &mut g,
            ) {
                if pairs.iter().any(|(k, v)| {
                    k.as_slice() == b"Type"
                        && matches!(v, selis_pdf_cos::Obj::Name(n) if n.as_slice() == b"Page")
                }) {
                    pages += 1;
                }
            }
        }
        assert_eq!(pages, expected_pages, "page count mismatch");
    }
}
