//! The engine Session: open a PDF, resolve pages, and render (SL-2.RAST.11).
//!
//! The Session is the single entry point every shell needs: parse the COS
//! document, resolve the page tree, and render a page to a backend with fonts
//! and images resolved from the document's resources.

use selis_bytes::Bytes;
use selis_color::Rgba;
use selis_error::{err, Code, Result};
use selis_geom::{Matrix, Point};
use selis_pdf_content::dispatch::Operand;
use selis_pdf_cos::{Doc, Obj};
use selis_pdf_doc::Resolver;
use selis_raster::{
    FillRule, Paint as RasterPaint, Path as RasterPath, PathCmd, TinySkiaBackend,
};
use selis_raster::image::{decode_image, Decode, DecodedImage};
use selis_raster::soft_mask::{build_mask, Mask, MaskGroup, SoftMask, SoftMaskType};
use selis_sandbox::{Budget, BudgetGuard, CancelToken, FixedClock};

use crate::render::render_display_list;

/// The engine session: a parsed, resolved document ready to render.
pub struct Session {
    /// The revision view (for the Resolver).
    doc: Doc,
    /// The source bytes.
    src: Vec<u8>,
    /// The resolved document model (pages, resources).
    document: selis_pdf_doc::Document,
}

impl Session {
    /// Open a PDF document from its source bytes.
    pub fn open(src: Vec<u8>, budget: &Budget) -> Result<Self> {
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let startxref = selis_pdf_cos::xref::find_startxref(&src, 2048).ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "session-open",
                detail = "no startxref"
            )
        })?;
        let doc = selis_pdf_cos::parse_revisions(&src, startxref, budget, &mut g)?;
        let document = selis_pdf_doc::Document::resolve(&doc, &src, budget, &mut g)?;
        let _ = g;
        Ok(Self { doc, src, document })
    }

    /// The number of pages.
    pub fn len(&self) -> usize {
        self.document.len()
    }

    /// Whether the document has no pages.
    pub fn is_empty(&self) -> bool {
        self.document.is_empty()
    }

    /// The media-box size of a page in points.
    #[must_use]
    pub fn page_size(&self, page_num: usize) -> Option<(f64, f64)> {
        self.document
            .pages
            .get(page_num)
            .and_then(|p| p.media_box)
            .map(|r| (r.width(), r.height()))
    }

    /// Render a page onto a backend.
    pub fn render_page(
        &self,
        page_num: usize,
        backend: &mut TinySkiaBackend,
        budget: &Budget,
        g: &mut BudgetGuard<'_>,
    ) -> Result<()> {
        let dl = self.page_display_list(page_num, budget, g)?;
        let budget_copy = *budget;
        let page = self.document.pages.get(page_num).ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "session-render",
                detail = "page index"
            )
        })?;
        let font_data = move |font_name: &Bytes| -> Option<Vec<u8>> {
            let mut bg = budget_copy.guard_with(&FixedClock(0), CancelToken::new());
            let mut res = Resolver::new(&self.doc, &self.src, &budget_copy);
            font_data_inner(&mut res, page, font_name, &mut bg)
        };
        let resolve_smask = move |key: &Bytes| -> Option<Mask> {
            let mut bg = budget_copy.guard_with(&FixedClock(0), CancelToken::new());
            let mut res = Resolver::new(&self.doc, &self.src, &budget_copy);
            resolve_smask_inner(&mut res, key, &mut bg)
        };
        let resolve_inline_image = move |dict: &[(Bytes, Bytes)], data: &[u8]| {
            let mut bg = budget_copy.guard_with(&FixedClock(0), CancelToken::new());
            resolve_inline_image_inner(dict, data, &mut bg)
        };
        let resolve_shading = move |name: &Bytes, state: &selis_pdf_content::display_list::ResolvedState| {
            let mut bg = budget_copy.guard_with(&FixedClock(0), CancelToken::new());
            let mut res = Resolver::new(&self.doc, &self.src, &budget_copy);
            resolve_shading_inner(&mut res, page, name, state, &mut bg)
        };
        // The page's initial backdrop is white (PDF 32000-2 §11.3.1), not
        // transparent black — fill the canvas before painting content.
        fill_page_backdrop(backend);
        render_display_list(
            &dl,
            backend,
            &font_data,
            &resolve_smask,
            &resolve_inline_image,
            &resolve_shading,
            g,
        );
        Ok(())
    }

    /// The display list of a page (no rasterisation).
    pub fn page_display_list(
        &self,
        page_num: usize,
        budget: &Budget,
        g: &mut BudgetGuard<'_>,
    ) -> Result<selis_pdf_content::display_list::DisplayList> {
        let Some(page) = self.document.pages.get(page_num) else {
            return Err(err!(
                Code::ObjUnexpected,
                during = "session-page",
                detail = "page index"
            ));
        };
        let mut resolver = Resolver::new(&self.doc, &self.src, budget);
        let content = resolve_page_content(&mut resolver, page, g)?;
        if content.is_empty() {
            return Ok(selis_pdf_content::display_list::DisplayList::default());
        }
        let budget_copy = *budget;
        let font_width = move |font_name: &Bytes, code: u16| -> f64 {
            let mut bg = budget_copy.guard_with(&FixedClock(0), CancelToken::new());
            let mut res = Resolver::new(&self.doc, &self.src, &budget_copy);
            font_width_inner(&mut res, page, font_name, code, &mut bg).unwrap_or(0.0)
        };
        let resolve_do = move |name: &Bytes| -> Option<selis_pdf_content::exec::DoTarget> {
            let mut bg = budget_copy.guard_with(&FixedClock(0), CancelToken::new());
            let mut res = Resolver::new(&self.doc, &self.src, &budget_copy);
            resolve_xobject_inner(&mut res, page, name, &mut bg)
                .ok()
                .flatten()
        };
        let resolve_ext_gstate =
            move |name: &Bytes| -> Option<Vec<(Bytes, selis_pdf_content::dispatch::Operand)>> {
                let mut bg = budget_copy.guard_with(&FixedClock(0), CancelToken::new());
                let mut res = Resolver::new(&self.doc, &self.src, &budget_copy);
                resolve_ext_gstate_inner(&mut res, page, name, &mut bg)
                    .ok()
                    .flatten()
            };
        selis_pdf_content::exec::execute(&content, &font_width, &resolve_do, &resolve_ext_gstate, g)
    }
}

/// Fill the canvas with the page's initial backdrop: opaque white.
fn fill_page_backdrop(backend: &mut TinySkiaBackend) {
    let (w, h) = backend.dimensions();
    if w == 0 || h == 0 {
        return;
    }
    let path = RasterPath {
        commands: vec![
            PathCmd::Move(Point::new(0.0, 0.0)),
            PathCmd::Line(Point::new(f64::from(w), 0.0)),
            PathCmd::Line(Point::new(f64::from(w), f64::from(h))),
            PathCmd::Line(Point::new(0.0, f64::from(h))),
            PathCmd::Close,
        ],
    };
    let paint = RasterPaint {
        colour: Rgba::new(1.0, 1.0, 1.0, 1.0),
    };
    let _ = selis_raster::render::fill(backend, &path, FillRule::NonZero, &paint);
}

/// The embedded font program bytes for a font resource name.
fn font_data_inner(
    resolver: &mut Resolver<'_>,
    page: &selis_pdf_doc::Page,
    font_name: &Bytes,
    g: &mut BudgetGuard<'_>,
) -> Option<Vec<u8>> {
    let font_dict = resolve_font_dict(resolver, page, font_name, g)?;
    let font_file = font_dict.font_file?;
    Some(font_file.data().as_slice().to_vec())
}

fn resolve_page_content(
    resolver: &mut Resolver<'_>,
    page: &selis_pdf_doc::Page,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let Some(refs) = &page.contents else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for r in refs {
        // `resolve_object` cannot parse streams; read the stream directly from
        // the source at the xref offset.
        match resolve_stream(resolver, *r, g) {
            Ok(Some((dict, data))) => {
                let filter = dict
                    .iter()
                    .find(|(k, _)| k.as_slice() == b"Filter")
                    .and_then(|(_, v)| match v {
                        Obj::Name(n) => {
                            Some(std::str::from_utf8(n.as_slice()).unwrap_or("").to_string())
                        }
                        _ => None,
                    });
                if let Some(filt) = filter {
                    if let Ok(decoded) = selis_pdf_filter::decode(&filt, &data, u64::MAX, g) {
                        out.extend_from_slice(&decoded);
                    }
                } else {
                    out.extend_from_slice(&data);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Read a stream object's dictionary and data directly from the source.
///
/// The `Resolver` parses the object's dictionary; the stream body is read by
/// finding the `stream` keyword after the dict and slicing `/Length` bytes.
fn resolve_stream(
    resolver: &mut Resolver<'_>,
    r: selis_pdf_cos::Ref,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<(Vec<(Bytes, Obj)>, Vec<u8>)>> {
    let obj = resolver.resolve(r, g)?;
    let dict = match &obj {
        Obj::Dict(d) => d.clone(),
        Obj::Stream { dict, .. } => dict.clone(),
        _ => return Ok(None),
    };
    // The object's byte offset in the source: from the xref.
    let offset = {
        let view = resolver.at_revision().ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "session-stream",
                detail = "no revision"
            )
        })?;
        match view.xref.get(&r.num) {
            Some(selis_pdf_cos::XrefEntry::InUse { offset, .. }) => *offset,
            _ => return Ok(None),
        }
    };
    // Find the "stream" keyword after the object header.
    let start = usize::try_from(offset).unwrap_or(0);
    let rest = resolver.src().get(start..).unwrap_or(&[]);
    let Some(stream_pos) = rest.windows(6).position(|w| w == b"stream") else {
        return Ok(None); // not a stream
    };
    let mut data_start = stream_pos.saturating_add(6); // after "stream"
                                                       // Skip the EOL.
    if rest.get(data_start) == Some(&b'\r') {
        data_start = data_start.saturating_add(1);
    }
    if rest.get(data_start) == Some(&b'\n') {
        data_start = data_start.saturating_add(1);
    }
    // Read /Length bytes.
    let length = dict
        .iter()
        .find(|(k, _)| k.as_slice() == b"Length")
        .and_then(|(_, v)| match v {
            Obj::Int(n) => u64::try_from(*n).ok(),
            _ => None,
        })
        .unwrap_or(0);
    let length = usize::try_from(length).unwrap_or(0);
    let end = data_start.saturating_add(length);
    let data = rest
        .get(data_start..end.min(rest.len()))
        .unwrap_or(&[])
        .to_vec();
    let _ = g;
    Ok(Some((dict, data)))
}

fn font_width_inner(
    resolver: &mut Resolver<'_>,
    page: &selis_pdf_doc::Page,
    font_name: &Bytes,
    code: u16,
    g: &mut BudgetGuard<'_>,
) -> Option<f64> {
    let font_dict = resolve_font_dict(resolver, page, font_name, g)?;
    let resolved = selis_font::resolve_widths(&font_dict, g).ok()?;
    Some(resolved.width(u32::from(code)))
}

fn resolve_font_dict(
    resolver: &mut Resolver<'_>,
    page: &selis_pdf_doc::Page,
    font_name: &Bytes,
    g: &mut BudgetGuard<'_>,
) -> Option<selis_font::FontDict> {
    let resources = page.resources.as_ref()?;
    let fonts = dict_get(resources, b"Font")?;
    let font_obj = dict_get(fonts, font_name.as_slice())?;
    let font_ref = match font_obj {
        Obj::Ref(r) => *r,
        Obj::Dict(_) => return Some(parse_font_dict(resolver, font_obj, g)),
        _ => return None,
    };
    let font_obj = resolver.resolve(font_ref, g).ok()?;
    Some(parse_font_dict(resolver, &font_obj, g))
}

fn parse_font_dict(
    resolver: &mut Resolver<'_>,
    obj: &Obj,
    g: &mut BudgetGuard<'_>,
) -> selis_font::FontDict {
    let mut fd = selis_font::FontDict::simple(selis_font::FontSubtype::TrueType);
    let dict = match obj {
        Obj::Dict(d) => d,
        _ => return fd,
    };
    if let Some(Obj::Name(n)) = dict_get_obj(dict, b"Subtype") {
        fd.subtype = match std::str::from_utf8(n.as_slice()).unwrap_or("") {
            "Type1" => selis_font::FontSubtype::Type1,
            "TrueType" => selis_font::FontSubtype::TrueType,
            "Type3" => selis_font::FontSubtype::Type3,
            "Type0" => selis_font::FontSubtype::Type0,
            "CIDFontType0" => selis_font::FontSubtype::CidFontType0,
            "CIDFontType2" => selis_font::FontSubtype::CidFontType2,
            _ => selis_font::FontSubtype::TrueType,
        };
    }
    if let Some(Obj::Name(n)) = dict_get_obj(dict, b"BaseFont") {
        fd.base_font = String::from_utf8_lossy(n.as_slice()).to_string();
    }
    if let Some(Obj::Int(v)) = dict_get_obj(dict, b"FirstChar") {
        fd.first_char = u32::try_from(*v).unwrap_or(0);
    }
    if let Some(Obj::Int(v)) = dict_get_obj(dict, b"LastChar") {
        fd.last_char = u32::try_from(*v).unwrap_or(255);
    }
    if let Some(Obj::Array(items)) = dict_get_obj(dict, b"Widths") {
        let widths: Vec<f64> = items
            .iter()
            .filter_map(|o| match o {
                Obj::Int(v) => Some(*v as f64),
                Obj::Real { scaled, scale } => Some(*scaled as f64 / 10f64.powi(*scale as i32)),
                _ => None,
            })
            .collect();
        if !widths.is_empty() {
            fd.widths = Some(widths);
        }
    }
    if let Some(Obj::Ref(r)) = dict_get_obj(dict, b"FontDescriptor") {
        if let Ok(Obj::Dict(desc)) = resolver.resolve(*r, g) {
            let mut descriptor = selis_font::FontDescriptor::default();
            if let Some(Obj::Int(v)) = dict_get_obj(&desc, b"Flags") {
                descriptor.flags = u32::try_from(*v).unwrap_or(0);
            }
            if let Some(Obj::Int(v)) = dict_get_obj(&desc, b"MissingWidth") {
                descriptor.missing_width = *v as f64;
            }
            if let Some(Obj::Real { scaled, scale }) = dict_get_obj(&desc, b"MissingWidth") {
                descriptor.missing_width = *scaled as f64 / 10f64.powi(*scale as i32);
            }
            for (tag, make_font_file) in &[
                (
                    &b"FontFile"[..],
                    selis_font::FontFile::Type1 as fn(Bytes) -> selis_font::FontFile,
                ),
                (
                    &b"FontFile2"[..],
                    selis_font::FontFile::TrueType as fn(Bytes) -> selis_font::FontFile,
                ),
                (
                    &b"FontFile3"[..],
                    selis_font::FontFile::OpenType as fn(Bytes) -> selis_font::FontFile,
                ),
            ] {
                if let Some(Obj::Ref(r)) = dict_get_obj(&desc, tag) {
                    if let Ok(Some((dict, data))) = resolve_stream(resolver, *r, g) {
                        let unfiltered = unfilter_stream_data(&dict, &data, g);
                        fd.font_file = Some(make_font_file(Bytes::copy_from_slice(&unfiltered)));
                        break;
                    }
                }
            }
            fd.descriptor = Some(descriptor);
        }
    }
    fd
}

/// Unfilter stream data given its dictionary.
fn unfilter_stream_data(dict: &[(Bytes, Obj)], data: &[u8], g: &mut BudgetGuard<'_>) -> Vec<u8> {
    if let Some(Obj::Name(n)) = dict_get_obj(dict, b"Filter") {
        let filt = std::str::from_utf8(n.as_slice()).unwrap_or("");
        if let Ok(decoded) = selis_pdf_filter::decode(filt, data, u64::MAX, g) {
            return decoded;
        }
    }
    data.to_vec()
}

/// Resolve an image XObject from the page's resources into decoded RGBA.
fn resolve_xobject_inner(
    resolver: &mut Resolver<'_>,
    page: &selis_pdf_doc::Page,
    name: &Bytes,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<selis_pdf_content::exec::DoTarget>> {
    let resources = match &page.resources {
        Some(r) => r,
        None => return Ok(None),
    };
    let xobjects = match dict_get(resources, b"XObject") {
        Some(o) => o,
        None => return Ok(None),
    };
    let xobj = match dict_get(xobjects, name.as_slice()) {
        Some(o) => o,
        None => return Ok(None),
    };
    let r = match xobj {
        Obj::Ref(r) => *r,
        _ => return Ok(None),
    };
    let Some((dict, data)) = resolve_stream(resolver, r, g)? else {
        return Ok(None);
    };
    let subtype = match dict_get_obj(&dict, b"Subtype") {
        Some(Obj::Name(n)) => n.as_slice(),
        _ => return Ok(None),
    };
    if subtype == b"Form" {
        // A form XObject: content + /Matrix. The form's own /Resources
        // scoping is a refinement; the page's resources are used here.
        let content = unfilter_stream_data(&dict, &data, g);
        let matrix = dict_get_obj(&dict, b"Matrix")
            .and_then(matrix_from_obj)
            .unwrap_or(Matrix::IDENTITY);
        return Ok(Some(selis_pdf_content::exec::DoTarget::Form {
            content,
            matrix,
        }));
    }
    if subtype != b"Image" {
        return Ok(None);
    }
    let width = dict_get_obj(&dict, b"Width")
        .and_then(|v| match v {
            Obj::Int(n) => u32::try_from(*n).ok(),
            _ => None,
        })
        .unwrap_or(0);
    let height = dict_get_obj(&dict, b"Height")
        .and_then(|v| match v {
            Obj::Int(n) => u32::try_from(*n).ok(),
            _ => None,
        })
        .unwrap_or(0);
    let bpc = dict_get_obj(&dict, b"BitsPerComponent")
        .and_then(|v| match v {
            Obj::Int(n) => u8::try_from(*n).ok(),
            _ => None,
        })
        .unwrap_or(8);
    // Component count from the colour space (DeviceRGB/Gray/CMYK; others fall
    // back to RGB).
    let components: u8 = match dict_get_obj(&dict, b"ColorSpace") {
        Some(Obj::Name(n)) => match n.as_slice() {
            b"DeviceGray" => 1,
            b"DeviceCMYK" => 4,
            _ => 3,
        },
        _ => 3,
    };
    let unfiltered = unfilter_stream_data(&dict, &data, g);
    let decode = selis_raster::image::Decode::identity(usize::from(components));
    let img =
        selis_raster::image::decode_image(width, height, components, bpc, &unfiltered, &decode, g)?;
    Ok(Some(selis_pdf_content::exec::DoTarget::Image {
        width: img.width,
        height: img.height,
        rgba8: Bytes::copy_from_slice(&img.rgba8),
    }))
}

/// Decode a stream (dict + raw data) as an image into straight RGBA8.
fn decode_image_rgba(
    dict: &[(selis_bytes::Bytes, Obj)],
    data: &[u8],
    g: &mut BudgetGuard<'_>,
) -> Result<DecodedImage> {
    let width = dict_get_obj(dict, b"Width")
        .and_then(|v| match v {
            Obj::Int(n) => u32::try_from(*n).ok(),
            _ => None,
        })
        .unwrap_or(0);
    let height = dict_get_obj(dict, b"Height")
        .and_then(|v| match v {
            Obj::Int(n) => u32::try_from(*n).ok(),
            _ => None,
        })
        .unwrap_or(0);
    let bpc = dict_get_obj(dict, b"BitsPerComponent")
        .and_then(|v| match v {
            Obj::Int(n) => u8::try_from(*n).ok(),
            _ => None,
        })
        .unwrap_or(8);
    let components: u8 = match dict_get_obj(dict, b"ColorSpace") {
        Some(Obj::Name(n)) => match n.as_slice() {
            b"DeviceGray" => 1,
            b"DeviceCMYK" => 4,
            _ => 3,
        },
        _ => 3,
    };
    let unfiltered = unfilter_stream_data(dict, data, g);
    let decode = Decode::identity(usize::from(components));
    decode_image(width, height, components, bpc, &unfiltered, &decode, g)
}

/// Resolve a soft-mask key (an object reference to an `/SMask` dict) into a
/// per-pixel alpha mask.
fn resolve_smask_inner(
    resolver: &mut Resolver<'_>,
    key: &Bytes,
    g: &mut BudgetGuard<'_>,
) -> Option<Mask> {
    let key_str = std::str::from_utf8(key.as_slice()).ok()?;
    let mut it = key_str.split_whitespace();
    let num: u32 = it.next()?.parse().ok()?;
    let gen: u16 = it.next()?.parse().ok()?;
    let obj = resolver.resolve(selis_pdf_cos::Ref::new(num, gen), g).ok()?;
    let Obj::Dict(pairs) = &obj else {
        return None;
    };
    let kind = match dict_get_obj(pairs, b"S") {
        Some(Obj::Name(n)) if n.as_slice() == b"Alpha" => SoftMaskType::Alpha,
        _ => SoftMaskType::Luminosity,
    };
    let backdrop = dict_backdrop(pairs);
    let group_ref = match dict_get_obj(pairs, b"G") {
        Some(Obj::Ref(r)) => *r,
        _ => return None,
    };
    let (dict, data) = resolve_stream(resolver, group_ref, g).ok()??;
    let img = decode_image_rgba(&dict, &data, g).ok()?;
    let group = MaskGroup {
        width: img.width,
        height: img.height,
        rgba8: img.rgba8,
    };
    let sm = SoftMask {
        kind,
        backdrop,
        transfer: None,
        group,
    };
    build_mask(&sm, g).ok()
}

/// The backdrop colour from an `/SMask` `/BC` array (default transparent
/// black).
fn dict_backdrop(pairs: &[(selis_bytes::Bytes, Obj)]) -> Rgba {
    let nums: Vec<f64> = match dict_get_obj(pairs, b"BC") {
        Some(Obj::Array(items)) => items
            .iter()
            .filter_map(|o| match o {
                Obj::Int(v) => Some(*v as f64),
                Obj::Real { scaled, scale } => {
                    Some(*scaled as f64 / 10f64.powi(*scale as i32))
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    Rgba::new(
        nums.first().copied().unwrap_or(0.0).clamp(0.0, 1.0),
        nums.get(1).copied().unwrap_or(0.0).clamp(0.0, 1.0),
        nums.get(2).copied().unwrap_or(0.0).clamp(0.0, 1.0),
        1.0,
    )
}

/// Decode an inline image (`BI`/…/`EI`) to straight RGBA8 samples.
fn resolve_inline_image_inner(
    dict: &[(selis_bytes::Bytes, selis_bytes::Bytes)],
    data: &[u8],
    g: &mut BudgetGuard<'_>,
) -> Option<(u32, u32, selis_bytes::Bytes)> {
    let get = |key: &[u8]| -> Option<&selis_bytes::Bytes> {
        dict.iter().find(|(k, _)| k.as_slice() == key).map(|(_, v)| v)
    };
    let parse_u32 = |v: &selis_bytes::Bytes| -> Option<u32> {
        let s = std::str::from_utf8(v.as_slice()).ok()?;
        s.trim().parse().ok()
    };
    let width = get(b"W").and_then(parse_u32)?;
    let height = get(b"H").and_then(parse_u32)?;
    let bpc = get(b"BPC").and_then(|v| parse_u32(v).map(|n| n.min(16) as u8)).unwrap_or(8);
    let components: u8 = match get(b"CS") {
        Some(v) if v.as_slice().strip_prefix(b"/").unwrap_or(v.as_slice()) == b"G" => 1,
        Some(v) if v.as_slice().strip_prefix(b"/").unwrap_or(v.as_slice()) == b"RGB" => 3,
        Some(v) if v.as_slice().strip_prefix(b"/").unwrap_or(v.as_slice()) == b"CMYK" => 4,
        _ => 3,
    };
    let unfiltered = match get(b"F").or_else(|| get(b"Filter")) {
        Some(filt) => {
            let name = std::str::from_utf8(
                filt.as_slice().strip_prefix(b"/").unwrap_or(filt.as_slice()),
            )
            .unwrap_or("");
            selis_pdf_filter::decode(name, data, u64::MAX, g).unwrap_or_else(|_| data.to_vec())
        }
        None => data.to_vec(),
    };
    let decode = Decode::identity(usize::from(components));
    let img = decode_image(width, height, components, bpc, &unfiltered, &decode, g).ok()?;
    Some((img.width, img.height, selis_bytes::Bytes::copy_from_slice(&img.rgba8)))
}

/// Resolve a shading resource to a rasterised RGBA image in device space.
fn resolve_shading_inner(
    resolver: &mut Resolver<'_>,
    page: &selis_pdf_doc::Page,
    name: &Bytes,
    state: &selis_pdf_content::display_list::ResolvedState,
    g: &mut BudgetGuard<'_>,
) -> Option<(u32, u32, selis_bytes::Bytes, selis_geom::Rect)> {
    use selis_geom::Rect;
    fn o<'a>(dict: &'a [(Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
        dict.iter().find(|(k, _)| k.as_slice() == key).map(|(_, v)| v)
    }
    fn on(dict: &[(Bytes, Obj)], key: &[u8]) -> Option<f64> {
        match o(dict, key)? {
            Obj::Int(n) => Some(*n as f64),
            Obj::Real { scaled, scale } => Some(*scaled as f64 / 10f64.powi(*scale as i32)),
            _ => None,
        }
    }
    fn arr_n(arr: &[Obj], i: usize) -> Option<f64> {
        match arr.get(i) {
            Some(Obj::Int(v)) => Some(*v as f64),
            Some(Obj::Real { scaled, scale }) => Some(*scaled as f64 / 10f64.powi(*scale as i32)),
            _ => None,
        }
    }
    fn arr_to_rect(arr: &[Obj]) -> Option<Rect> {
        Some(Rect::new(arr_n(arr, 0)?, arr_n(arr, 1)?, arr_n(arr, 2)?, arr_n(arr, 3)?))
    }
    fn arr_to_matrix(arr: &[Obj]) -> Option<selis_geom::Matrix> {
        Some(selis_geom::Matrix::new(arr_n(arr, 0)?, arr_n(arr, 1)?, arr_n(arr, 2)?, arr_n(arr, 3)?, arr_n(arr, 4)?, arr_n(arr, 5)?))
    }
    // Resolve the shading dict from /Shading resources.
    let resources = page.resources.as_ref()?;
    let shadings = dict_get(resources, b"Shading")?;
    let obj = match dict_get(shadings, name.as_slice())? {
        Obj::Ref(r) => resolver.resolve(*r, g).ok()?,
        o => o.clone(),
    };
    let Obj::Dict(pairs) = &obj else { return None };
    let shading_type = on(pairs, b"ShadingType")? as i64;
    let bbox = match o(pairs, b"BBox") {
        Some(Obj::Array(arr)) => arr_to_rect(arr)?,
        _ => Rect::new(0.0, 0.0, 1.0, 1.0),
    };
    // /Matrix maps user space → shading space.
    let shading_matrix = match o(pairs, b"Matrix") {
        Some(Obj::Array(arr)) => arr_to_matrix(arr)?,
        _ => selis_geom::Matrix::IDENTITY,
    };
    // Device → shading = ctm_inv ∘ shading_matrix
    let ctm_inv = state.ctm.invert()?;
    let to_shading = ctm_inv.then(shading_matrix);
    // Shading → device = shading_matrix_inv ∘ ctm
    let shading_matrix_inv = shading_matrix.invert()?;
    let to_device = shading_matrix_inv.then(state.ctm);
    // Compute the device rect from the BBox.
    let corners = [
        to_device.apply(selis_geom::Point::new(bbox.x0, bbox.y0)),
        to_device.apply(selis_geom::Point::new(bbox.x1, bbox.y0)),
        to_device.apply(selis_geom::Point::new(bbox.x0, bbox.y1)),
        to_device.apply(selis_geom::Point::new(bbox.x1, bbox.y1)),
    ];
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in &corners {
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    let dev_rect = Rect::new(min_x, min_y, max_x, max_y);
    let w = (dev_rect.width().ceil() as u32).max(1);
    let h = (dev_rect.height().ceil() as u32).max(1);
    // Parse /Function and /ColorSpace.
    let func = parse_shading_function(o(pairs, b"Function")?)?;
    let color_space = match o(pairs, b"ColorSpace") {
        Some(Obj::Name(n)) => n.as_slice(),
        _ => b"DeviceRGB",
    };
    // Parse /Domain and /Extend (axial/radial).
    let domain = match o(pairs, b"Domain") {
        Some(Obj::Array(arr)) => (arr_n(arr, 0).unwrap_or(0.0), arr_n(arr, 1).unwrap_or(1.0)),
        _ => (0.0, 1.0),
    };
    let extend = match o(pairs, b"Extend") {
        Some(Obj::Array(arr)) => (arr_n(arr, 0).unwrap_or(0.0) != 0.0, arr_n(arr, 1).unwrap_or(0.0) != 0.0),
        _ => (false, false),
    };
    // Parse /Coords.
    let coords = match o(pairs, b"Coords") {
        Some(Obj::Array(arr)) => arr,
        _ => return None,
    };
    let mut rgba = Vec::with_capacity(
        usize::try_from(w).unwrap_or(0).saturating_mul(usize::try_from(h).unwrap_or(0)).saturating_mul(4),
    );
    for py in 0..h {
        for px in 0..w {
            let dev_x = dev_rect.x0 + (f64::from(px) + 0.5) / f64::from(w) * dev_rect.width();
            let dev_y = dev_rect.y0 + (f64::from(py) + 0.5) / f64::from(h) * dev_rect.height();
            let in_shading = to_shading.apply(selis_geom::Point::new(dev_x, dev_y));
            let components = match shading_type {
                2 => selis_raster::shading::axial_colour(
                    &selis_raster::shading::AxialShading {
                        start: selis_geom::Point::new(arr_n(coords, 0)?, arr_n(coords, 1)?),
                        end: selis_geom::Point::new(arr_n(coords, 2)?, arr_n(coords, 3)?),
                        domain,
                        function: func.clone(),
                        extend_start: extend.0,
                        extend_end: extend.1,
                    },
                    in_shading,
                ),
                3 => match selis_raster::shading::radial_colour(
                    &selis_raster::shading::RadialShading {
                        start: selis_geom::Point::new(arr_n(coords, 0)?, arr_n(coords, 1)?),
                        start_radius: arr_n(coords, 2)?,
                        end: selis_geom::Point::new(arr_n(coords, 3)?, arr_n(coords, 4)?),
                        end_radius: arr_n(coords, 5)?,
                        domain,
                        function: func.clone(),
                        extend_start: extend.0,
                        extend_end: extend.1,
                    },
                    in_shading,
                ) {
                    Some(c) => c,
                    None => continue,
                },
                _ => continue,
            };
            let rgb = components_to_rgb(&components, color_space);
            let r = (rgb[0].clamp(0.0, 1.0) * 255.0) as u8;
            let g = (rgb[1].clamp(0.0, 1.0) * 255.0) as u8;
            let b = (rgb[2].clamp(0.0, 1.0) * 255.0) as u8;
            rgba.push(r);
            rgba.push(g);
            rgba.push(b);
            rgba.push(255);
        }
    }
    if rgba.is_empty() {
        return None;
    }
    Some((w, h, Bytes::from(rgba), dev_rect))
}

/// Parse a /Function obj into a `Function`, handling type 2 (exponential).
fn parse_shading_function(obj: &Obj) -> Option<selis_color::function::Function> {
    match obj {
        Obj::Dict(pairs) => {
            let ft = match pairs.iter().find(|(k, _)| k.as_slice() == b"FunctionType") {
                Some((_, Obj::Int(n))) => *n,
                _ => return None,
            };
            match ft {
                2 => {
                    let get = |key: &[u8]| -> Option<f64> {
                        pairs.iter().find(|(k, _)| k.as_slice() == key).and_then(|(_, v)| match v {
                            Obj::Int(n) => Some(*n as f64),
                            Obj::Real { scaled, scale } => Some(*scaled as f64 / 10f64.powi(*scale as i32)),
                            _ => None,
                        })
                    };
                    let get_arr = |key: &[u8]| -> Option<Vec<f64>> {
                        match pairs.iter().find(|(k, _)| k.as_slice() == key) {
                            Some((_, Obj::Array(arr))) => {
                                arr.iter().map(|v| match v {
                                    Obj::Int(n) => Some(*n as f64),
                                    Obj::Real { scaled, scale } => Some(*scaled as f64 / 10f64.powi(*scale as i32)),
                                    _ => None,
                                }).collect()
                            }
                            _ => None,
                        }
                    };
                    let n = get(b"N")?;
                    let c0 = get_arr(b"C0").unwrap_or_else(|| vec![0.0]);
                    let c1 = get_arr(b"C1").unwrap_or_else(|| vec![1.0]);
                    let outputs = c0.len().max(c1.len());
                    // The exponential function: a = c1 - c0, b = n, c = c0.
                    // f(x) = (c1 - c0) * x^n + c0 for each output.
                    // But the ExponentialFunction stores a, b, c per output.
                    let a: Vec<f64> = (0..outputs).map(|i| {
                        c1.get(i).copied().unwrap_or(1.0) - c0.get(i).copied().unwrap_or(0.0)
                    }).collect();
                    let b = vec![n; outputs];
                    let c: Vec<f64> = (0..outputs).map(|i| c0.get(i).copied().unwrap_or(0.0)).collect();
                    Some(selis_color::function::Function::Exponential(
                        selis_color::function::ExponentialFunction {
                            inputs: 1,
                            outputs,
                            a,
                            b,
                            c,
                            domain: vec![(0.0, 1.0)],
                            range: None,
                        }
                    ))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Convert colour components to sRGB [0, 1].
fn components_to_rgb(components: &[f64], color_space: &[u8]) -> [f64; 3] {
    match color_space {
        b"DeviceGray" | b"G" => {
            let g = components.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
            [g, g, g]
        }
        b"DeviceCMYK" | b"CMYK" => {
            let c = components.get(0).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let m = components.get(1).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let y = components.get(2).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let k = components.get(3).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let r = 1.0 - (c * (1.0 - k) + k);
            let g = 1.0 - (m * (1.0 - k) + k);
            let b = 1.0 - (y * (1.0 - k) + k);
            [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
        }
        _ => {
            // DeviceRGB or other: treat first 3 components as RGB.
            let r = components.get(0).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let g = components.get(1).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let b = components.get(2).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            [r, g, b]
        }
    }
}

/// Resolve a `/ExtGState` resource name to its dictionary as `Operand` pairs,
/// for the content interpreter's `gs` operator. Unknown values are skipped so
/// a hostile ExtGState never aborts the page.
fn resolve_ext_gstate_inner(
    resolver: &mut Resolver<'_>,
    page: &selis_pdf_doc::Page,
    name: &Bytes,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<Vec<(Bytes, Operand)>>> {
    let resources = match &page.resources {
        Some(r) => r,
        None => return Ok(None),
    };
    let ext = match dict_get(resources, b"ExtGState") {
        Some(o) => o,
        None => return Ok(None),
    };
    let gs = match dict_get(ext, name.as_slice()) {
        Some(o) => o,
        None => return Ok(None),
    };
    let obj = match gs {
        Obj::Ref(r) => resolver.resolve(*r, g)?,
        Obj::Dict(_) => gs.clone(),
        _ => return Ok(None),
    };
    let Obj::Dict(pairs) = &obj else {
        return Ok(None);
    };
    let mut out = Vec::new();
    for (k, v) in pairs {
        // A soft mask is a dictionary (or a reference to one); emit a stable
        // key so the renderer can resolve it to a per-pixel mask later.
        if k.as_slice() == b"SMask" {
            if let Some(key) = smask_ref_key(v) {
                out.push((k.clone(), Operand::Name(key)));
            }
            continue;
        }
        if let Some(op) = obj_to_operand(v) {
            out.push((k.clone(), op));
        }
    }
    Ok(Some(out))
}

/// A stable key for an `/SMask` value: the object reference of the SMask
/// dictionary. `None` when the value is not a direct reference (inline soft
/// masks are a refinement; most producers reference the dictionary).
fn smask_ref_key(obj: &Obj) -> Option<Bytes> {
    match obj {
        Obj::Ref(r) => Some(Bytes::copy_from_slice(format!("{} {}", r.num, r.gen).as_bytes())),
        _ => None,
    }
}

/// Convert a resolved object to a content `Operand` (for ExtGState merging).
fn obj_to_operand(obj: &Obj) -> Option<Operand> {
    match obj {
        Obj::Int(v) => Some(Operand::Num(*v as f64)),
        Obj::Real { scaled, scale } => Some(Operand::Num(*scaled as f64 / 10f64.powi(*scale as i32))),
        Obj::Name(n) => Some(Operand::Name(n.clone())),
        Obj::Bool(b) => Some(Operand::Bool(*b)),
        Obj::Array(items) => {
            let ops: Vec<Operand> = items.iter().filter_map(obj_to_operand).collect();
            Some(Operand::Arr(ops))
        }
        _ => None,
    }
}

fn dict_get<'a>(obj: &'a Obj, key: &[u8]) -> Option<&'a Obj> {
    match obj {
        Obj::Dict(pairs) => pairs
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v),
        _ => None,
    }
}

fn dict_get_obj<'a>(pairs: &'a [(selis_bytes::Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
    pairs
        .iter()
        .find(|(k, _)| k.as_slice() == key)
        .map(|(_, v)| v)
}

/// A 6-element `/Matrix` array as a `Matrix`.
fn matrix_from_obj(obj: &Obj) -> Option<selis_geom::Matrix> {
    let Obj::Array(items) = obj else {
        return None;
    };
    if items.len() != 6 {
        return None;
    }
    let get = |i: usize| -> Option<f64> {
        match items.get(i)? {
            Obj::Int(v) => Some(*v as f64),
            Obj::Real { scaled, scale } => Some(*scaled as f64 / 10f64.powi(*scale as i32)),
            _ => None,
        }
    };
    Some(selis_geom::Matrix::new(
        get(0)?,
        get(1)?,
        get(2)?,
        get(3)?,
        get(4)?,
        get(5)?,
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    #[test]
    fn session_opens_and_renders_a_minimal_pdf() {
        let src = include_bytes!("fixtures/minimal.pdf");
        let budget = Budget::profile(selis_sandbox::Surface::Viewer);
        let session = Session::open(src.to_vec(), &budget).expect("open");
        assert_eq!(session.len(), 1);
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        session
            .render_page(0, &mut backend, &budget, &mut g)
            .expect("render");
        let data = backend.pixmap().data();
        // The black-filled square covers the page: the centre is black.
        let idx = (50 * 100 + 50) * 4;
        assert_eq!(&data[idx..idx + 3], &[0, 0, 0]);
        assert_eq!(data[idx + 3], 255);
    }

    /// A PDF with a 2×2 DeviceGray image XObject renders it scaled to the
    /// page.
    #[test]
    fn session_renders_an_image_xobject() {
        let src = include_bytes!("fixtures/image.pdf");
        let budget = Budget::profile(selis_sandbox::Surface::Viewer);
        let session = Session::open(src.to_vec(), &budget).expect("open");
        assert_eq!(session.len(), 1);
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let mut backend = TinySkiaBackend::new(2, 2).expect("pixmap");
        session
            .render_page(0, &mut backend, &budget, &mut g)
            .expect("render");
        let data = backend.pixmap().data();
        // The top-left pixel is white (255 gray); the rest are black.
        assert_eq!(&data[0..3], &[255, 255, 255]);
        assert_eq!(&data[8..11], &[0, 0, 0]);
    }

    /// A PDF with an embedded TrueType font renders its text glyphs.
    #[test]
    fn session_renders_embedded_text() {
        let src = include_bytes!("fixtures/text.pdf");
        let budget = Budget::profile(selis_sandbox::Surface::Viewer);
        let session = Session::open(src.to_vec(), &budget).expect("open");
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let mut backend = TinySkiaBackend::new(2000, 200).expect("pixmap");
        session
            .render_page(0, &mut backend, &budget, &mut g)
            .expect("render");
        let data = backend.pixmap().data();
        // The canvas has no background fill; glyphs paint black with alpha 255.
        let mut painted = 0usize;
        for px in data.chunks(4) {
            if px[3] == 255 {
                painted = painted.saturating_add(1);
            }
        }
        // At least some pixels were painted by the 'AB' glyphs.
        assert!(
            painted > 100,
            "expected painted glyph pixels, got {painted}"
        );
    }

    /// A PDF with a form XObject renders its content (a red square).
    #[test]
    fn session_renders_a_form_xobject() {
        let src = include_bytes!("fixtures/form.pdf");
        let budget = Budget::profile(selis_sandbox::Surface::Viewer);
        let session = Session::open(src.to_vec(), &budget).expect("open");
        assert_eq!(session.len(), 1);
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        session
            .render_page(0, &mut backend, &budget, &mut g)
            .expect("render");
        let data = backend.pixmap().data();
        // The form's red square fills the page: the centre pixel is red.
        let idx = (50 * 100 + 50) * 4;
        assert_eq!(&data[idx..idx + 3], &[255, 0, 0]);
    }
}
