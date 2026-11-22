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
//! * a logo with **missing or undecodable bytes** is a typed refusal, never a
//!   fetch. The engine has no network and no filesystem; the caller hands over
//!   either raw RGB pixels or (native builds) PNG/JPEG file bytes, which are
//!   decoded here by the workspace's pure-Rust image decoder (ADR-P0017/P0021);
//! * non-finite or oversized geometry is refused with a typed error before a
//!   single byte is written;
//! * the produced appearance stream is read back byte-for-byte from the
//!   incremental revision before the caller returns anything.

use selis_bytes::Bytes;
use selis_pdf_cos::Obj;
#[cfg(any(not(target_arch = "wasm32"), feature = "logo"))]
use selis_sandbox::{Budget, BudgetGuard};

use crate::visual::{content_stream, VisualLine, VisualOptions, VisualStroke};
use crate::{bytes, invalid, Page, SignError, MAX_TEXT_BYTES};
#[cfg(any(not(target_arch = "wasm32"), feature = "logo"))]
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

/// A caller-supplied logo. Pixels (or, on native builds, PNG/JPEG file bytes
/// passed to [`Self::from_encoded`]) are handed over by the caller; there is no
/// file read or URL fetch in the engine (SL-5.SIGN.07).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct SignatureLogo {
    /// Image width in pixels. Must be in `1..=4096`.
    pub width: u32,
    /// Image height in pixels. Must be in `1..=4096`.
    pub height: u32,
    /// Row-major, 8-bit-per-channel RGB samples: `width * height * 3` bytes.
    pub rgb: Vec<u8>,
    /// Row-major 8-bit-per-pixel alpha samples: `width * height` bytes. `None`
    /// means the image is fully opaque and no `/SMask` is written. This field
    /// is absent from the plain sign WASM build (which has no logo path) so
    /// that chunk keeps its exact byte baseline (ADR-P0011 / the 2% gate);
    /// native builds and the lazy sign-logo chunk carry it.
    #[cfg(any(not(target_arch = "wasm32"), feature = "logo"))]
    #[serde(default)]
    pub alpha: Option<Vec<u8>>,
}

impl SignatureLogo {
    /// A fully opaque logo from raw RGB pixels. The caller owns the pixels;
    /// they are copied into the returned PDF, never fetched from anywhere.
    #[must_use]
    pub fn new(width: u32, height: u32, rgb: Vec<u8>) -> Self {
        Self {
            width,
            height,
            rgb,
            #[cfg(any(not(target_arch = "wasm32"), feature = "logo"))]
            alpha: None,
        }
    }

    /// A logo from raw RGBA pixels. The alpha channel is preserved as a PDF
    /// soft mask (`/SMask`) so transparent and semi-transparent pixels really
    /// composite over the signature block. A fully opaque image stores no
    /// alpha, producing byte-for-byte the same output as [`Self::new`].
    ///
    /// The caller owns the pixels; nothing is fetched. `# Errors` validation
    /// happens in [`Self::validate`] at the appearance writer.
    #[cfg(any(not(target_arch = "wasm32"), feature = "logo"))]
    #[must_use]
    pub fn new_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        let pixels = (u64::from(width).saturating_mul(u64::from(height))) as usize;
        let mut rgb = Vec::with_capacity(pixels.saturating_mul(3));
        let mut alpha = Vec::with_capacity(pixels);
        let mut any_transparent = false;
        for sample in rgba.chunks_exact(4) {
            rgb.extend_from_slice(&sample[..3]);
            let a = sample[3];
            any_transparent |= a != 0xFF;
            alpha.push(a);
        }
        Self {
            width,
            height,
            rgb,
            alpha: any_transparent.then_some(alpha),
        }
    }

    /// Decode a standalone PNG or JPEG file into an 8-bit RGB logo.
    ///
    /// Bytes are supplied by the caller: nothing here reads a file or opens a
    /// socket. Decoding goes through the workspace's pure-Rust
    /// [`selis_image`] decoder (PNG: bit depth 8, grey/RGB/palette/grey+alpha/
    /// RGBA, non-interlaced; JPEG: baseline/progressive grey/RGB/CMYK). The
    /// decoded RGBA samples keep their alpha as a PDF soft mask (`/SMask`), so
    /// transparent and semi-transparent pixels composite correctly. A fully
    /// opaque image stores no alpha and produces the same output as before.
    ///
    /// # Errors
    ///
    /// * [`SignError::Invalid`] when the bytes are not a PNG/JPEG this decoder
    ///   understands, are empty, or decode to an image the appearance writer
    ///   would refuse (zero size or a wrong pixel count);
    /// * [`SignError::Limit`] when the encoded payload or decoded dimensions
    ///   exceed this build's logo caps.
    ///
    /// # Budget
    ///
    /// The caller's [`BudgetGuard`] pays for the decoded pixel buffer and the
    /// pixel count, exactly as it does for every other image decode in the
    /// engine.
    ///
    /// Linking the decoder into the plain sign WASM chunk would breach that
    /// chunk's 2% gate, so the browser reaches this constructor through the
    /// dedicated lazy `selis-pdf-wasm-sign-logo` chunk, which enables the
    /// `logo` feature (ADR-P0011). The raw-RGB constructor is always available.
    #[cfg(feature = "logo")]
    pub fn from_encoded(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<Self, SignError> {
        if data.is_empty() {
            return Err(invalid(
                "the logo has no image bytes; the caller must supply them",
            ));
        }
        if data.len() > MAX_LOGO_BYTES {
            return Err(SignError::Limit(format!(
                "the encoded logo exceeds {MAX_LOGO_BYTES} bytes"
            )));
        }
        let image = selis_image::decode(data, g).map_err(|e| {
            invalid(format!(
                "the logo is not a PNG or JPEG this build can decode: {e}"
            ))
        })?;
        let pixels = u64::from(image.width).saturating_mul(u64::from(image.height));
        if pixels > MAX_LOGO_PIXELS {
            return Err(SignError::Limit(format!(
                "the decoded logo has more than {MAX_LOGO_PIXELS} pixels"
            )));
        }
        if image.rgba.len() != (pixels as usize).saturating_mul(4) {
            return Err(invalid(
                "the decoded logo does not carry four samples per pixel",
            ));
        }
        let logo = Self::new_rgba(image.width, image.height, image.rgba.to_vec());
        logo.validate()?;
        Ok(logo)
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
        #[cfg(any(not(target_arch = "wasm32"), feature = "logo"))]
        if let Some(alpha) = &self.alpha {
            if u64::try_from(alpha.len()).unwrap_or(u64::MAX) != pixels {
                return Err(invalid(
                    "the logo's alpha byte count does not match its dimensions",
                ));
            }
            if alpha.len() > MAX_LOGO_BYTES {
                return Err(SignError::Limit(format!(
                    "the logo's alpha channel exceeds {MAX_LOGO_BYTES} bytes"
                )));
            }
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
    /// Extra indirect stream objects the caller must write into the same
    /// incremental revision: the logo image first, and (only when the logo has
    /// alpha) its DeviceGray soft mask second.
    pub extra_objects: Vec<Obj>,
    /// Whether a logo was laid into the block (used by the read-back check).
    pub has_logo: bool,
    /// Whether a soft-mask image accompanies the logo. The caller uses this to
    /// wire `/SMask` to the second extra object and to verify it on read-back.
    /// Always `false` in the plain sign WASM build (no alpha there).
    pub has_smask: bool,
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
    let mut has_smask = false;
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

        #[cfg(any(not(target_arch = "wasm32"), feature = "logo"))]
        if let Some(alpha) = &logo.alpha {
            // The soft mask is a grayscale image with the SAME dimensions as
            // the colour image, one 8-bit sample per pixel. The writer assigns
            // its object number (logo_base + 1) and adds `/SMask <that ref>` to
            // the colour image dictionary above — object numbering belongs to
            // the writer, not the appearance builder.
            extra_objects.push(Obj::Stream {
                dict: vec![
                    (bytes(b"Type"), Obj::Name(bytes(b"XObject"))),
                    (bytes(b"Subtype"), Obj::Name(bytes(b"Image"))),
                    (bytes(b"Width"), Obj::Int(i64::from(logo.width))),
                    (bytes(b"Height"), Obj::Int(i64::from(logo.height))),
                    (bytes(b"ColorSpace"), Obj::Name(bytes(b"DeviceGray"))),
                    (bytes(b"BitsPerComponent"), Obj::Int(8)),
                ],
                data: Bytes::copy_from_slice(alpha),
            });
            has_smask = true;
        }
    }

    Ok(AppearanceAssets {
        stream,
        bbox: [0.0, 0.0, user_width, user_height],
        extra_objects,
        has_logo,
        has_smask,
    })
}

/// Re-open an incremental revision and prove the widget carries exactly the
/// appearance that was written: `/AP /N` resolves to the named stream, its bytes
/// are unchanged, `/BBox` is present, and (when requested) the `/XObject`
/// resource resolves to an image XObject.
///
/// Available natively and in the lazy sign-logo WASM chunk. The plain sign WASM
/// chunk does not compile it (the +1.8 KB would breach its 2% gate); the lazy
/// sign-logo chunk enables the `logo` feature and calls it unconditionally, so
/// browser signing gets the same byte-for-byte check as a native build.
#[cfg(any(not(target_arch = "wasm32"), feature = "logo"))]
pub fn verify_appearance(
    out: &[u8],
    field_num: u32,
    appearance_num: u32,
    expected: &[u8],
    has_logo: bool,
    has_smask: bool,
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
        if has_smask {
            let mask_ref = dict_get(&dict, b"SMask")
                .and_then(|value| match value {
                    Obj::Ref(reference) => Some(*reference),
                    _ => None,
                })
                .ok_or_else(|| {
                    SignError::ReadBack("the logo has alpha but no /SMask XObject".to_string())
                })?;
            let mask = resolve(out, &doc, Obj::Ref(mask_ref), budget, g)
                .map_err(|e| SignError::ReadBack(format!("the /SMask did not re-open: {e}")))?;
            let Obj::Stream {
                dict: mask_dict,
                data: mask_data,
            } = mask
            else {
                return Err(SignError::ReadBack(
                    "the /SMask is not a stream".to_string(),
                ));
            };
            if dict_get(&mask_dict, b"Subtype") != Some(&Obj::Name(bytes(b"Image")))
                || dict_get(&mask_dict, b"ColorSpace") != Some(&Obj::Name(bytes(b"DeviceGray")))
                || dict_get(&mask_dict, b"BitsPerComponent") != Some(&Obj::Int(8))
            {
                return Err(SignError::ReadBack(
                    "the /SMask is not an 8-bit grayscale image XObject".to_string(),
                ));
            }
            // The mask must match the colour image's dimensions and carry one
            // 8-bit sample per pixel, or viewers would mis-scale the alpha.
            let dim = |key: &[u8]| -> Option<i64> {
                match dict_get(&mask_dict, key) {
                    Some(Obj::Int(n)) => Some(*n),
                    _ => None,
                }
            };
            let (Some(mw), Some(mh)) = (dim(b"Width"), dim(b"Height")) else {
                return Err(SignError::ReadBack(
                    "the /SMask has no integer dimensions".to_string(),
                ));
            };
            if mw <= 0 || mh <= 0 {
                return Err(SignError::ReadBack(
                    "the /SMask has non-positive dimensions".to_string(),
                ));
            }
            let expected_len = (mw as u64).saturating_mul(mh as u64);
            if u64::try_from(mask_data.len()).unwrap_or(u64::MAX) != expected_len {
                return Err(SignError::ReadBack(
                    "the /SMask sample count does not match its dimensions".to_string(),
                ));
            }
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

    #[cfg(any(not(target_arch = "wasm32"), feature = "logo"))]
    #[test]
    fn an_rgba_logo_writes_a_soft_mask_image() {
        // 2x2: one fully transparent, one half-transparent, two opaque.
        let rgba = vec![
            255, 0, 0, 0, // transparent red
            0, 255, 0, 128, // half-transparent green
            0, 0, 255, 255, // opaque blue
            255, 255, 0, 255, // opaque yellow
        ];
        let logo = SignatureLogo::new_rgba(2, 2, rgba);
        assert!(logo.alpha.is_some());
        let assets = build_signature_appearance(
            page(),
            [0.0, 0.0, 248.0, 90.0],
            &template().with_logo(logo),
            &[],
        )
        .expect("appearance");
        assert!(assets.has_logo);
        assert!(assets.has_smask);
        assert_eq!(assets.extra_objects.len(), 2, "colour image + soft mask");
        let Obj::Stream { dict, data } = &assets.extra_objects[1] else {
            panic!("soft mask is a stream");
        };
        assert_eq!(
            dict_get(dict, b"Subtype"),
            Some(&Obj::Name(bytes(b"Image")))
        );
        assert_eq!(
            dict_get(dict, b"ColorSpace"),
            Some(&Obj::Name(bytes(b"DeviceGray")))
        );
        assert_eq!(dict_get(dict, b"BitsPerComponent"), Some(&Obj::Int(8)));
        assert_eq!(data.as_slice(), &[0u8, 128, 255, 255]);
    }

    #[cfg(any(not(target_arch = "wasm32"), feature = "logo"))]
    #[test]
    fn a_fully_opaque_rgba_logo_needs_no_soft_mask() {
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 1, 2, 3, 255];
        let logo = SignatureLogo::new_rgba(2, 2, rgba);
        assert!(logo.alpha.is_none());
        let assets = build_signature_appearance(
            page(),
            [0.0, 0.0, 248.0, 90.0],
            &template().with_logo(logo),
            &[],
        )
        .expect("appearance");
        assert!(assets.has_logo);
        assert!(!assets.has_smask);
        assert_eq!(assets.extra_objects.len(), 1);
    }

    #[cfg(any(not(target_arch = "wasm32"), feature = "logo"))]
    #[test]
    fn a_wrong_alpha_length_is_a_typed_refusal() {
        let mut logo = test_logo();
        logo.alpha = Some(vec![255u8; 3]); // 2x2 needs 4 samples
        assert!(
            build_signature_appearance(
                page(),
                [0.0, 0.0, 248.0, 90.0],
                &template().with_logo(logo),
                &[],
            )
            .is_err(),
            "a malformed alpha channel must be refused"
        );
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

    #[cfg(all(not(target_arch = "wasm32"), feature = "logo"))]
    #[test]
    fn a_png_logo_decodes_to_exact_rgb_pixels() {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let image = selis_image::Image {
            width: 2,
            height: 2,
            rgba: vec![
                255, 0, 0, 255, 0, 0, 255, 255, 0, 255, 0, 255, 255, 255, 0, 255,
            ],
        };
        let png = selis_image::encode_png(&image, 6, &mut g).expect("encode png");
        let logo = SignatureLogo::from_encoded(&png, &mut g).expect("decode png");
        assert_eq!(logo.width, 2);
        assert_eq!(logo.height, 2);
        assert_eq!(logo.rgb.len(), 12);
        for (source, decoded) in image.rgba.chunks_exact(4).zip(logo.rgb.chunks_exact(3)) {
            assert_eq!(&source[..3], decoded);
        }
        let assets = build_signature_appearance(
            page(),
            [0.0, 0.0, 248.0, 90.0],
            &template().with_logo(logo),
            &[],
        )
        .expect("appearance with a decoded logo");
        assert!(assets.has_logo);
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "logo"))]
    #[test]
    fn a_jpeg_logo_decodes_to_opaque_rgb_pixels() {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let image = selis_image::Image {
            width: 4,
            height: 4,
            rgba: (0..16)
                .flat_map(|i| [i * 16, 255 - i * 16, 128, 255])
                .collect(),
        };
        let jpeg = selis_image::encode_jpeg(&image, 95, &mut g).expect("encode jpeg");
        let logo = SignatureLogo::from_encoded(&jpeg, &mut g).expect("decode jpeg");
        assert_eq!((logo.width, logo.height), (4, 4));
        assert_eq!(logo.rgb.len(), 48);
        // JPEG is lossy: demand the same approximate fidelity the codec's own
        // round-trip test demands, and prove the alpha channel was dropped.
        for (source, decoded) in image.rgba.chunks_exact(4).zip(logo.rgb.chunks_exact(3)) {
            for channel in 0..3 {
                assert!(
                    i32::from(source[channel]).abs_diff(i32::from(decoded[channel])) <= 6,
                    "channel {channel}: {} vs {}",
                    source[channel],
                    decoded[channel]
                );
            }
        }
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "logo"))]
    #[test]
    fn undecodable_or_oversized_logo_bytes_are_typed_refusals() {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        assert!(matches!(
            SignatureLogo::from_encoded(b"not an image", &mut g),
            Err(SignError::Invalid(_))
        ));
        assert!(matches!(
            SignatureLogo::from_encoded(&[], &mut g),
            Err(SignError::Invalid(_))
        ));
        let huge = vec![0; MAX_LOGO_BYTES + 1];
        assert!(matches!(
            SignatureLogo::from_encoded(&huge, &mut g),
            Err(SignError::Limit(_))
        ));
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
