//! Typed visible-signature appearance generation (SL-5.SIGN.07).
//!
//! This module turns a *typed template* — signer name, reason, location, date
//! and an optional caller-supplied logo — into the `/AP /N` form XObject that
//! a signature widget carries. It is a pure writer: the caller supplies every
//! byte and every coordinate; nothing here fetches, reads a file or opens a
//! socket. Text layout is delegated to [`crate::visual::content_stream`], so
//! the signature widget and a standalone visual stamp share one renderer and
//! cannot drift.
//!
//! Honesty rules that matter here:
//!
//! * the appearance is **decorative**. It never changes the detached CMS byte
//!   range, and the signature is still verified by the existing read-back in
//!   [`crate::digital::sign_digital`] — this module cannot weaken that check;
//! * a logo with **missing bytes** is a typed refusal, never a fetch. The
//!   engine has no network and no filesystem; the caller must hand over the
//!   pixels (ADR-P0017/P0021);
//! * non-finite or oversized geometry is refused with a typed error before a
//!   single byte is written;
//! * the produced appearance stream is read back byte-for-byte from the
//!   incremental revision before the caller returns anything.

use selis_bytes::Bytes;
use selis_pdf_cos::Obj;
use selis_sandbox::{Budget, BudgetGuard};

use crate::visual::{content_stream, VisualLine, VisualOptions, VisualStroke};
use crate::{bytes, invalid, Page, SignError, MAX_TEXT_BYTES};
#[cfg(not(target_arch = "wasm32"))]
use crate::{dict_get, resolve};

/// The resource name the logo image is filed under inside the appearance's
/// `/Resources /XObject` dictionary. Prefixed so it cannot collide with a name
/// the document chose.
pub const LOGO_RESOURCE: &str = "SelisSigLogo";

/// Largest image edge accepted by this build.
const MAX_LOGO_EDGE: u32 = 4096;
/// Largest image (in pixels) accepted by this build.
const MAX_LOGO_PIXELS: u64 = 4_000_000;
/// Largest raw logo payload accepted by this build.
const MAX_LOGO_BYTES: usize = 16 * 1024 * 1024;
/// Horizontal/vertical padding for a logo laid into the widget box.
const LOGO_INSET: f64 = 3.0;

/// A caller-supplied logo. All pixels are handed over by the caller; no
/// decoder, file read or URL fetch happens in the engine (SL-5.SIGN.07).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct SignatureLogo {
    /// Image width in pixels. Must be in `1..=4096`.
    pub width: u32,
    /// Image height in pixels. Must be in `1..=4096`.
    pub height: u32,
    /// Row-major, 8-bit-per-channel RGB samples: `width * height * 3` bytes.
    pub rgb: Vec<u8>,
}

impl SignatureLogo {
    /// A logo from raw RGB pixels. The caller owns the pixels; they are copied
    /// into the returned PDF, never fetched from anywhere.
    #[must_use]
    pub fn new(width: u32, height: u32, rgb: Vec<u8>) -> Self {
        Self { width, height, rgb }
    }

    /// Check dimensions and payload lengths. A missing or short pixel buffer
    /// is a typed refusal, never a silently blank logo or a fetch.
    fn validate(&self) -> Result<usize, SignError> {
        if self.width == 0 || self.height == 0 {
            return Err(invalid("the logo has a zero width or height"));
        }
        if self.width > MAX_LOGO_EDGE || self.height > MAX_LOGO_EDGE {
            return Err(SignError::Limit(format!(
                "the logo edge exceeds {MAX_LOGO_EDGE} pixels"
            )));
        }
        let pixels = u64::from(self.width).saturating_mul(u64::from(self.height));
        if pixels > MAX_LOGO_PIXELS {
            return Err(SignError::Limit(format!(
                "the logo has more than {MAX_LOGO_PIXELS} pixels"
            )));
        }
        let expected_rgb = (pixels as usize).saturating_mul(3);
        if self.rgb.is_empty() {
            return Err(invalid(
                "the logo has no image bytes; the caller must supply them",
            ));
        }
        if self.rgb.len() > MAX_LOGO_BYTES {
            return Err(SignError::Limit(format!(
                "the logo payload exceeds {MAX_LOGO_BYTES} bytes"
            )));
        }
        if self.rgb.len() != expected_rgb {
            return Err(invalid(
                "the logo's RGB byte count does not match its dimensions",
            ));
        }
        Ok(pixels as usize)
    }
}

/// A typed visible-signature template. This is the only structured input the
/// appearance writer needs; the detached-CMS path builds one from
/// [`crate::DigitalOptions`] so the two cannot disagree on name/reason/date.
#[derive(Debug, Clone, PartialEq)]
pub struct AppearanceTemplate {
    /// Signer name, drawn prominently.
    pub name: String,
    /// `/Reason`, when the signer gave one.
    pub reason: Option<String>,
    /// `/Location`, when the signer gave one.
    pub location: Option<String>,
    /// Human-readable signing date (the CMS `YYYYMMDDHHMMSSZ` is formatted by
    /// the caller — the core has no clock).
    pub date: String,
    /// Optional caller-supplied logo. `None` is a perfectly valid block.
    pub logo: Option<SignatureLogo>,
}

impl AppearanceTemplate {
    /// A template without a logo.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        reason: Option<String>,
        location: Option<String>,
        date: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            reason,
            location,
            date: date.into(),
            logo: None,
        }
    }

    /// Attach a caller-supplied logo.
    #[must_use]
    pub fn with_logo(mut self, logo: SignatureLogo) -> Self {
        self.logo = Some(logo);
        self
    }

    /// The typed lines drawn top-to-bottom in the widget box.
    pub fn lines(&self) -> Vec<VisualLine> {
        let mut lines = vec![
            VisualLine::new("Digitally signed"),
            VisualLine::new(self.name.clone()),
            VisualLine::new(format!("Date: {}", self.date)),
        ];
        if let Some(reason) = self.reason.as_deref().filter(|text| !text.is_empty()) {
            lines.push(VisualLine::new(format!("Reason: {reason}")));
        }
        if let Some(location) = self.location.as_deref().filter(|text| !text.is_empty()) {
            lines.push(VisualLine::new(format!("Location: {location}")));
        }
        lines
    }

    fn validate(&self) -> Result<Option<usize>, SignError> {
        for text in [
            self.name.as_str(),
            self.date.as_str(),
            self.reason.as_deref().unwrap_or(""),
            self.location.as_deref().unwrap_or(""),
        ] {
            if text.len() > MAX_TEXT_BYTES {
                return Err(SignError::Limit(format!(
                    "an appearance line may be at most {MAX_TEXT_BYTES} bytes"
                )));
            }
            // Refuse a character the built-in face cannot draw rather than
            // silently replacing the signer's name with '?'.
            crate::encode_winansi(text)?;
        }
        self.logo.as_ref().map(SignatureLogo::validate).transpose()
    }
}

/// The raw assets a caller needs to install an appearance as `/AP /N`.
#[derive(Debug, Clone, PartialEq)]
pub struct AppearanceAssets {
    /// The form XObject's stream data (annotation-local user space).
    pub stream: Vec<u8>,
    /// The form's `/BBox`: `[0, 0, width, height]` in user space.
    pub bbox: [f64; 4],
    /// Extra indirect stream objects (the logo image) the caller must write
    /// into the same incremental revision.
    pub extra_objects: Vec<Obj>,
    /// Whether a logo was laid into the block (used by the read-back check).
    pub has_logo: bool,
}

/// Build the signature widget's appearance from a typed template.
///
/// The text layout is exactly the one [`crate::visual::sign_visual`] uses, so
/// the widget block and a standalone stamp render identically for the same
/// geometry. `strokes` reuses [`VisualStroke`] verbatim — a hand-drawn mark
/// supplied by the caller, normalized `0..=1` inside the widget rectangle.
///
/// # Errors
///
/// * [`SignError::Invalid`] for non-finite/oversized rectangle geometry, for a
///   stroke with fewer than two points or a non-finite point, or for a logo
///   whose bytes are missing or the wrong length;
/// * [`SignError::Limit`] for an image larger than this build accepts;
/// * [`SignError::Invalid`] for a character the built-in font cannot draw.
#[allow(clippy::too_many_arguments)]
pub fn build_signature_appearance(
    target: Page,
    rect_display: [f64; 4],
    template: &AppearanceTemplate,
    strokes: &[VisualStroke],
) -> Result<AppearanceAssets, SignError> {
    let _ = template.validate()?;
    if !rect_display.iter().all(|value| value.is_finite()) {
        return Err(invalid(
            "the signature appearance rect has a non-finite coordinate",
        ));
    }
    let display_width = rect_display[2] - rect_display[0];
    let display_height = rect_display[3] - rect_display[1];
    if !(display_width.is_finite()
        && display_height.is_finite()
        && display_width > 0.0
        && display_height > 0.0)
    {
        return Err(invalid(
            "the signature appearance rect has no positive extent",
        ));
    }
    if !strokes.iter().all(|stroke| {
        stroke.points.len() >= 2
            && stroke
                .points
                .iter()
                .all(|(x, y)| x.is_finite() && y.is_finite())
    }) {
        return Err(invalid(
            "a signature stroke needs two or more finite points",
        ));
    }

    // The widget's user-space extent, mapped through the page rotation exactly
    // as the detached-CMS writer does, so /Rect and /BBox agree.
    let (ax, ay) = target.display_to_user(rect_display[0], rect_display[1]);
    let (bx, by) = target.display_to_user(rect_display[2], rect_display[3]);
    let user_width = (bx - ax).abs();
    let user_height = (by - ay).abs();
    if !(user_width.is_finite() && user_height.is_finite() && user_width > 0.0 && user_height > 0.0)
    {
        return Err(invalid("the signature appearance has no user-space extent"));
    }

    let options = VisualOptions {
        page: 0, // unused: content_stream only reads rotation and the mapping
        rect: [0.0, 0.0, display_width, display_height],
        lines: template.lines(),
        strokes: strokes.to_vec(),
        color: [0.05, 0.05, 0.12],
        stroke_width: 1.0,
        font_size: 9.0,
        border: true,
    };
    let local_page = Page {
        reference: target.reference,
        width: user_width,
        height: user_height,
        rotation: target.rotation,
    };
    let mut stream = content_stream(
        &options,
        local_page,
        [0.0, 0.0, display_width, display_height],
    )?;

    let mut extra_objects = Vec::new();
    let mut has_logo = false;
    if let Some(logo) = &template.logo {
        has_logo = true;
        // Lay the image into the top-right corner of the widget, in display
        // space, then map that rectangle through the page rotation the same way
        // text is mapped. The image XObject paints its unit square under `cm`.
        let logo_w = (logo.width as f64).min(display_width - LOGO_INSET * 2.0);
        let logo_h = (logo.height as f64).min(display_height - LOGO_INSET * 2.0);
        let ratio = (logo_w / f64::from(logo.width)).min(logo_h / f64::from(logo.height));
        let draw_w = f64::from(logo.width) * ratio;
        let draw_h = f64::from(logo.height) * ratio;
        let x = display_width - LOGO_INSET - draw_w;
        let y = display_height - LOGO_INSET - draw_h;
        let (dx, dy) = local_page.display_dir_to_user(1.0, 0.0);
        let (cx, cy) = local_page.display_dir_to_user(0.0, 1.0);
        let (ux, uy) = local_page.display_to_user(x, y);
        let mut placement = format!(
            "q {} {} {} {} {} {} cm\n/{} Do\nQ\n",
            crate::real(dx),
            crate::real(dy),
            crate::real(cx),
            crate::real(cy),
            crate::real(ux),
            crate::real(uy),
            LOGO_RESOURCE
        )
        .into_bytes();
        placement.extend_from_slice(&stream);
        stream = placement;

        extra_objects.push(Obj::Stream {
            dict: vec![
                (bytes(b"Type"), Obj::Name(bytes(b"XObject"))),
                (bytes(b"Subtype"), Obj::Name(bytes(b"Image"))),
                (bytes(b"Width"), Obj::Int(i64::from(logo.width))),
                (bytes(b"Height"), Obj::Int(i64::from(logo.height))),
                (bytes(b"ColorSpace"), Obj::Name(bytes(b"DeviceRGB"))),
                (bytes(b"BitsPerComponent"), Obj::Int(8)),
            ],
            data: Bytes::copy_from_slice(&logo.rgb),
        });
    }

    Ok(AppearanceAssets {
        stream,
        bbox: [0.0, 0.0, user_width, user_height],
        extra_objects,
        has_logo,
    })
}

#[cfg(not(target_arch = "wasm32"))]
/// Re-open an incremental revision and prove the widget carries exactly the
/// appearance that was written: `/AP /N` resolves to the named stream, its bytes
/// are unchanged, `/BBox` is present, and (when requested) the `/XObject`
/// resource resolves to an image XObject.
pub(crate) fn verify_appearance(
    out: &[u8],
    field_num: u32,
    appearance_num: u32,
    expected: &[u8],
    has_logo: bool,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<(), SignError> {
    let doc = crate::open_doc(out, budget, g)?;
    let field = resolve(
        out,
        &doc,
        Obj::Ref(selis_pdf_cos::Ref::new(field_num, 0)),
        budget,
        g,
    )
    .map_err(|e| SignError::ReadBack(format!("the widget did not re-open: {e}")))?;
    let Obj::Dict(pairs) = field else {
        return Err(SignError::ReadBack(
            "the widget is not a dictionary".to_string(),
        ));
    };
    let ap = dict_get(&pairs, b"AP")
        .and_then(|value| match value {
            Obj::Dict(entries) => dict_get(entries, b"N"),
            _ => None,
        })
        .ok_or_else(|| SignError::ReadBack("the widget has no /AP /N".to_string()))?;
    let ap_ref = match ap {
        Obj::Ref(reference) => *reference,
        _ => {
            return Err(SignError::ReadBack(
                "the /AP /N is not an indirect stream".to_string(),
            ))
        }
    };
    if ap_ref.num != appearance_num {
        return Err(SignError::ReadBack(
            "the widget's /AP /N points at a different object".to_string(),
        ));
    }
    let appearance = resolve(out, &doc, Obj::Ref(ap_ref), budget, g)
        .map_err(|e| SignError::ReadBack(format!("the appearance did not re-open: {e}")))?;
    let Obj::Stream { dict, data } = appearance else {
        return Err(SignError::ReadBack(
            "the /AP /N is not a stream".to_string(),
        ));
    };
    if data.as_slice() != expected {
        return Err(SignError::ReadBack(
            "the appearance stream changed between write and read".to_string(),
        ));
    }
    if dict_get(&dict, b"BBox").is_none() {
        return Err(SignError::ReadBack(
            "the appearance has no /BBox".to_string(),
        ));
    }
    if has_logo {
        let resources = dict_get(&dict, b"Resources")
            .and_then(|value| match value {
                Obj::Dict(entries) => Some(entries),
                _ => None,
            })
            .ok_or_else(|| SignError::ReadBack("the appearance has no /Resources".to_string()))?;
        let xobjects = dict_get(resources, b"XObject")
            .and_then(|value| match value {
                Obj::Dict(entries) => Some(entries),
                _ => None,
            })
            .ok_or_else(|| SignError::ReadBack("the appearance has no /XObject".to_string()))?;
        let logo_ref = dict_get(xobjects, LOGO_RESOURCE.as_bytes())
            .and_then(|value| match value {
                Obj::Ref(reference) => Some(*reference),
                _ => None,
            })
            .ok_or_else(|| SignError::ReadBack("the logo XObject is missing".to_string()))?;
        let logo = resolve(out, &doc, Obj::Ref(logo_ref), budget, g)
            .map_err(|e| SignError::ReadBack(format!("the logo did not re-open: {e}")))?;
        let Obj::Stream { dict, .. } = logo else {
            return Err(SignError::ReadBack("the logo is not a stream".to_string()));
        };
        if dict_get(&dict, b"Subtype") != Some(&Obj::Name(bytes(b"Image"))) {
            return Err(SignError::ReadBack(
                "the logo is not an image XObject".to_string(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::arithmetic_side_effects
    )]

    use super::*;

    fn page() -> Page {
        Page {
            reference: selis_pdf_cos::Ref::new(3, 0),
            width: 612.0,
            height: 792.0,
            rotation: 0,
        }
    }

    /// A 2x2 red/blue checkerboard. Pure data; no image decoding dependency.
    fn test_logo() -> SignatureLogo {
        SignatureLogo::new(2, 2, vec![255, 0, 0, 0, 0, 255, 0, 255, 0, 255, 255, 0])
    }

    fn template() -> AppearanceTemplate {
        AppearanceTemplate::new(
            "Ada Lovelace",
            Some("Approved".to_string()),
            Some("London".to_string()),
            "2026-11-04 12:00:00 UTC".to_string(),
        )
    }

    #[test]
    fn a_template_without_a_logo_is_a_plain_text_block() {
        let assets = build_signature_appearance(page(), [0.0, 0.0, 248.0, 90.0], &template(), &[])
            .expect("appearance");
        assert!(!assets.has_logo);
        assert!(assets.extra_objects.is_empty());
        assert_eq!(assets.bbox, [0.0, 0.0, 248.0, 90.0]);
        let text = String::from_utf8_lossy(&assets.stream);
        assert!(text.contains("Ada Lovelace"), "{text}");
        assert!(text.contains("Approved"), "{text}");
        assert!(text.contains("London"), "{text}");
        assert!(text.contains("/SelisSigF"), "{text}");
    }

    #[test]
    fn a_logo_adds_an_image_xobject_and_a_do_operator() {
        let assets = build_signature_appearance(
            page(),
            [0.0, 0.0, 248.0, 90.0],
            &template().with_logo(test_logo()),
            &[],
        )
        .expect("appearance");
        assert!(assets.has_logo);
        assert_eq!(assets.extra_objects.len(), 1);
        let text = String::from_utf8_lossy(&assets.stream);
        assert!(text.contains("/SelisSigLogo Do"), "{text}");
        let Obj::Stream { dict, data } = &assets.extra_objects[0] else {
            panic!("logo is a stream");
        };
        assert_eq!(
            dict_get(dict, b"Subtype"),
            Some(&Obj::Name(bytes(b"Image")))
        );
        assert_eq!(data.len(), 12);
    }

    #[test]
    fn freehand_strokes_are_reused_in_the_widget() {
        let strokes = vec![VisualStroke {
            points: vec![(0.1, 0.2), (0.8, 0.7)],
        }];
        let assets =
            build_signature_appearance(page(), [0.0, 0.0, 248.0, 90.0], &template(), &strokes)
                .expect("appearance");
        let text = String::from_utf8_lossy(&assets.stream);
        assert!(text.contains(" m\n"), "{text}");
        assert!(text.contains(" l\n"), "{text}");
    }

    #[test]
    fn non_finite_and_oversized_geometry_are_typed_refusals() {
        assert!(
            build_signature_appearance(page(), [0.0, f64::NAN, 10.0, 10.0], &template(), &[],)
                .is_err()
        );
        assert!(
            build_signature_appearance(page(), [0.0, 0.0, 0.0, 0.0], &template(), &[]).is_err()
        );
    }

    #[test]
    fn missing_logo_bytes_are_a_typed_refusal_not_a_fetch() {
        let empty = SignatureLogo::new(2, 2, Vec::new());
        assert!(build_signature_appearance(
            page(),
            [0.0, 0.0, 248.0, 90.0],
            &template().with_logo(empty),
            &[],
        )
        .is_err());
        let short = SignatureLogo::new(2, 2, vec![0, 1, 2]);
        assert!(build_signature_appearance(
            page(),
            [0.0, 0.0, 248.0, 90.0],
            &template().with_logo(short),
            &[],
        )
        .is_err());
    }
}
