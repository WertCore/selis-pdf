//! The engine Session: open a PDF, resolve pages, and render (SL-2.RAST.11).
//!
//! The Session is the single entry point every shell needs: parse the COS
//! document, resolve the page tree, and render a page to a backend with fonts
//! and images resolved from the document's resources.

use selis_bytes::Bytes;
use selis_error::{err, Code, Result};
use selis_pdf_cos::{Doc, Obj};
use selis_pdf_doc::Resolver;
use selis_raster::TinySkiaBackend;
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
        let Some(page) = self.document.pages.get(page_num) else {
            return Err(err!(
                Code::ObjUnexpected,
                during = "session-render",
                detail = "page index"
            ));
        };
        let mut resolver = Resolver::new(&self.doc, &self.src, budget);
        let content = resolve_page_content(&mut resolver, page, g)?;
        if content.is_empty() {
            return Ok(());
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
        let dl = selis_pdf_content::exec::execute(&content, &font_width, &resolve_do, g)?;
        render_display_list(&dl, backend, &|_| None, g);
        Ok(())
    }
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
    // Only image XObjects are handled (form XObjects land with the worklist).
    let is_image = matches!(
        dict_get_obj(&dict, b"Subtype"),
        Some(Obj::Name(n)) if n.as_slice() == b"Image"
    );
    if !is_image {
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
}
