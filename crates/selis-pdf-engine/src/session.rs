//! The engine Session: open a PDF, resolve pages, and render (SL-2.RAST.11).
//!
//! The Session is the single entry point every shell needs: parse the COS
//! document, resolve the page tree, and render a page to a backend with fonts
//! and images resolved from the document's resources.

use std::cell::RefCell;
use std::collections::HashMap;

use selis_bytes::Bytes;
use selis_color::Rgba;
use selis_error::{err, Code, Result};
use selis_geom::{Matrix, Point};
use selis_pdf_content::dispatch::Operand;
use selis_pdf_cos::{Doc, Obj};
use selis_pdf_doc::Resolver;
use selis_raster::image::{decode_image, Decode, DecodedImage};
use selis_raster::soft_mask::{build_mask, Mask, MaskGroup, SoftMask, SoftMaskType};
use selis_raster::{FillRule, Paint as RasterPaint, Path as RasterPath, PathCmd, TinySkiaBackend};
use selis_sandbox::{Budget, BudgetGuard, CancelToken, Clock};

use crate::page::{page_view as compute_page_view, PageView};
use crate::render::render_display_list;
/// The engine session: a parsed, resolved document ready to render.
pub struct Session {
    /// The revision view (for the Resolver).
    doc: Doc,
    /// The source bytes.
    src: Vec<u8>,
    /// The resolved document model (pages, resources).
    document: selis_pdf_doc::Document,
    /// The encryption key (bytes, revision, AES flag), if the document is
    /// password-protected and the (user) password authenticated.
    key: Option<selis_pdf_cos::encrypt::DecryptPolicy>,
}

impl Session {
    /// Open a PDF document from its source bytes.
    ///
    /// # Budget
    ///
    /// Charged against `budget` for the whole open. The clock is injected by
    /// the caller (SL-0.SBX.07): an L4/L5 shell passes a real monotonic clock
    /// ([`selis_sandbox::InstantClock`], or a `performance.now` wrapper on the
    /// web) so the `Budget::wall` deadline actually fires on this path; tests
    /// pass `FixedClock`/`ManualClock` for determinism. Every sub-guard the
    /// session builds later measures against the same clock.
    ///
    /// # Malformed Input
    ///
    /// When there is no `startxref` at all — truncated and fuzzed files
    /// commonly omit it — or the `startxref` is present but its target is
    /// damaged beyond the xref parser's recovery window, fall back to
    /// scanning for object headers and a `/Root` (SL-1.ROB.01) instead of
    /// refusing the document. Budget, cancellation, and pending errors
    /// propagate as-is; only malformed-input parse failures trigger the
    /// scan-based recovery.
    pub fn open(src: Vec<u8>, budget: &Budget, clock: &dyn Clock) -> Result<Self> {
        let mut g = budget.guard_with(clock, CancelToken::new());
        let parsed = match selis_pdf_cos::xref::find_startxref(&src, 2048) {
            Some(startxref) => {
                match selis_pdf_cos::parse_revisions(&src, startxref, budget, &mut g) {
                    Ok(doc) => Some(doc),
                    Err(e) if e.is_budget() || e.is_cancelled() || e.is_pending() => {
                        return Err(e);
                    }
                    Err(_) => None,
                }
            }
            None => None,
        };

        // Detect encryption; authenticate with the (empty) user password.
        //
        // PDF encryption only encrypts strings and stream bodies — the page
        // tree's structural tokens stay plaintext — so a document whose
        // password we cannot supply still *opens* (catalog + page tree resolve)
        // even without a key; content just won't decode. We therefore treat a
        // failed authentication as "open unencrypted", matching tolerant
        // viewers, rather than refusing the document (SL-1.ROB.01).
        fn open_doc(
            src: &[u8],
            doc: &Doc,
            budget: &Budget,
            g: &mut BudgetGuard<'_>,
        ) -> Result<(
            Doc,
            selis_pdf_doc::Document,
            Option<selis_pdf_cos::encrypt::DecryptPolicy>,
        )> {
            let key: Option<selis_pdf_cos::encrypt::DecryptPolicy> = {
                let encrypt_ref = doc.revisions().last().and_then(|v| v.encrypt);
                match encrypt_ref {
                    None => None,
                    Some(r) => {
                        match selis_pdf_cos::encrypt::parse_encrypt(src, Some(r), budget, g) {
                            Ok(Some(info)) => {
                                let id = doc
                                    .revisions()
                                    .last()
                                    .map(|v| v.trailer.clone())
                                    .map(|t| selis_pdf_cos::encrypt::document_id(&t))
                                    .unwrap_or_default();
                                selis_pdf_cos::encrypt::authenticate(&info, &id, b"").map(|k| {
                                    selis_pdf_cos::encrypt::DecryptPolicy::from_encrypt(&info, k)
                                })
                            }
                            // Unreadable or non-standard handler: open unencrypted.
                            Ok(None) => None,
                            Err(_) => None,
                        }
                    }
                }
            };
            let document = selis_pdf_doc::Document::resolve(doc, src, budget, g, key.as_ref())?;
            Ok((doc.clone(), document, key))
        }

        // A parsed xref that resolves is preferred; when its document model
        // cannot be built (the catalog's object is marked free or absent in a
        // damaged table), retry against the scan-based reconstruction index,
        // which recovers live object bodies regardless of the xref state
        // (SL-1.ROB.01).
        if let Some(doc) = &parsed {
            match open_doc(&src, doc, budget, &mut g) {
                Ok((doc, document, key)) => {
                    return Ok(Self {
                        doc,
                        src,
                        document,
                        key,
                    });
                }
                Err(e) if e.is_budget() || e.is_cancelled() || e.is_pending() => {
                    return Err(e);
                }
                Err(_) => {}
            }
        }
        let rec = selis_pdf_cos::reconstruct(&src, budget, &mut g)?.0;
        let (doc, document, key) = open_doc(&src, &rec, budget, &mut g)?;
        let _ = g;
        Ok(Self {
            doc,
            src,
            document,
            key,
        })
    }

    /// A resolver for this document, with the encryption key applied.
    fn new_resolver<'a>(&'a self, budget: &'a Budget) -> Resolver<'a> {
        let mut res = Resolver::new(&self.doc, &self.src, budget);
        if let Some(policy) = &self.key {
            res.set_key(policy.clone());
        }
        res
    }

    /// The number of pages.
    pub fn len(&self) -> usize {
        self.document.len()
    }

    /// Whether the document has no pages.
    pub fn is_empty(&self) -> bool {
        self.document.is_empty()
    }

    /// The effective render size of a page in points.
    ///
    /// # Malformed Input
    ///
    /// A page whose `/MediaBox` is absent from its inheritance chain, or that
    /// declares a non-positive area, renders at the ISO 32000-2 §7.10.1
    /// default (`612 0 0 792 0 0`) — the fallback mainstream viewers apply
    /// (SL-2.CONF.05: matches MuPDF and Acrobat on the affected corpus files;
    /// a missing or degenerate `/MediaBox` is incomplete authoring, not a
    /// reason to refuse the document). Returns `None` only when `page_num` is
    /// out of range.
    #[must_use]
    pub fn page_size(&self, page_num: usize) -> Option<(f64, f64)> {
        const DEFAULT_MEDIA_BOX: (f64, f64) = (612.0, 792.0);
        self.document.pages.get(page_num).map(|p| {
            p.media_box
                .map(|r| (r.width(), r.height()))
                .filter(|(w, h)| *w > 0.0 && *h > 0.0)
                .unwrap_or(DEFAULT_MEDIA_BOX)
        })
    }

    /// The rendered view of a page at `dpi` (SL-2.RAST.12): the output canvas
    /// size and the user-space → device transform, honouring page `/Rotate`
    /// (ISO 32000-2 §14.11.2 — rotation is part of the rendered page view, and
    /// the canvas dimensions swap for 90/270).
    ///
    /// `None` when the page does not exist or has no `/MediaBox`.
    #[must_use]
    pub fn page_view(&self, page_num: usize, dpi: f64) -> Option<PageView> {
        let page = self.document.pages.get(page_num)?;
        let box_ = page.media_box?;
        let rotate = selis_geom::Rotation::from_degrees(i64::from(page.rotate.unwrap_or(0)));
        Some(compute_page_view(box_, rotate, dpi))
    }

    /// The embedded-file inventory (metadata only — extraction is policy
    /// gated).
    pub fn attachments(
        &self,
        budget: &Budget,
        g: &mut BudgetGuard<'_>,
    ) -> Result<Vec<selis_pdf_doc::Attachment>> {
        let mut resolver = self.new_resolver(budget);
        selis_pdf_doc::embedded_files(&mut resolver, &self.document.catalog, budget, g)
    }

    /// An embedded file's decoded bytes by name-tree key.
    pub fn embedded_file_data(
        &self,
        key: &str,
        budget: &Budget,
        g: &mut BudgetGuard<'_>,
    ) -> Result<Option<Vec<u8>>> {
        let mut resolver = self.new_resolver(budget);
        selis_pdf_doc::embedded_file_data(&mut resolver, &self.document.catalog, key, budget, g)
    }

    /// The structure tree's marked-content order (reading order), or an empty
    /// list when the document is untagged.
    pub fn mcid_order(&self, budget: &Budget, g: &mut BudgetGuard<'_>) -> Result<Vec<u32>> {
        let mut resolver = self.new_resolver(budget);
        let tree =
            selis_pdf_doc::StructTree::resolve(&mut resolver, &self.document.catalog, budget, g)?;
        Ok(tree.mcid_order())
    }

    /// Evaluate the conformance rules for a profile.
    pub fn conformance(
        &self,
        profile: selis_pdf_doc::Profile,
        budget: &Budget,
        g: &mut BudgetGuard<'_>,
    ) -> Result<Vec<selis_pdf_doc::RuleResult>> {
        let mut resolver = self.new_resolver(budget);
        let tree =
            selis_pdf_doc::StructTree::resolve(&mut resolver, &self.document.catalog, budget, g)?;
        let meta =
            selis_pdf_doc::Metadata::resolve(&mut resolver, &self.document.catalog, budget, g)?;
        Ok(selis_pdf_doc::evaluate(
            &self.document,
            &self.document.catalog,
            &tree,
            &meta,
            profile,
            budget,
            g,
        ))
    }

    /// Render a page onto a backend.
    ///
    /// `page_ctm` is the page-to-device transform from
    /// [`Session::page_view`] — the caller sizes the backend from the same
    /// view, so the transform and the canvas agree (RAST.12).
    pub fn render_page(
        &self,
        page_num: usize,
        backend: &mut TinySkiaBackend,
        page_ctm: selis_geom::Matrix,
        budget: &Budget,
        g: &mut BudgetGuard<'_>,
    ) -> Result<()> {
        let mut stats = crate::render::RenderStats::default();
        self.render_page_with_stats(page_num, backend, page_ctm, budget, g, &mut stats)
    }

    /// Render a page onto a backend, filling `stats` with the raster walk's
    /// workload counters (see [`crate::render::RenderStats`]).
    pub fn render_page_with_stats(
        &self,
        page_num: usize,
        backend: &mut TinySkiaBackend,

        page_ctm: selis_geom::Matrix,
        budget: &Budget,
        g: &mut BudgetGuard<'_>,
        stats: &mut crate::render::RenderStats,
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
        // Sub-guards measure against the same injected clock the open path
        // used (SL-0.SBX.07): the caller's deadline advances for resource
        // resolution too, instead of resetting at every closure.
        let clock = g.clock();
        let font_data = move |font_name: &Bytes| -> Option<Vec<u8>> {
            let mut bg = budget_copy.guard_with(clock, CancelToken::new());
            let mut res = self.new_resolver(&budget_copy);
            font_data_inner(&mut res, page.resources.as_ref(), font_name, &mut bg)
        };
        let resolve_smask = move |key: &Bytes| -> Option<Mask> {
            let mut bg = budget_copy.guard_with(clock, CancelToken::new());
            let mut res = self.new_resolver(&budget_copy);
            resolve_smask_inner(&mut res, key, &mut bg)
        };
        let resolve_inline_image = move |dict: &[(Bytes, Bytes)], data: &[u8]| {
            let mut bg = budget_copy.guard_with(clock, CancelToken::new());
            resolve_inline_image_inner(dict, data, &mut bg)
        };
        let resolve_shading =
            move |name: &Bytes, state: &selis_pdf_content::display_list::ResolvedState| {
                let mut bg = budget_copy.guard_with(clock, CancelToken::new());
                let mut res = self.new_resolver(&budget_copy);
                resolve_shading_inner(&mut res, page.resources.as_ref(), name, state, &mut bg)
            };
        let resolve_pattern = move |name: &Bytes| -> Option<selis_raster::pattern::TilingPattern> {
            let mut bg = budget_copy.guard_with(clock, CancelToken::new());
            resolve_pattern_inner(self, page.resources.as_ref(), name, &budget_copy, &mut bg)
        };
        // The page's initial backdrop is white (PDF 32000-2 §11.3.1), not
        // transparent black — fill the canvas before painting content.
        fill_page_backdrop(backend);
        crate::render::render_display_list_with_stats(
            &dl,
            backend,
            page_ctm,
            &font_data,
            &resolve_smask,
            &resolve_inline_image,
            &resolve_shading,
            &resolve_pattern,
            g,
            stats,
        );
        // Annotation appearance streams (SL-2.RAST.13, ISO 32000-2 §12.5.5):
        // a page whose visible content is annotation-borne must not paint
        // blank. Each /AP /N appearance renders as a form mapped onto its
        // /Rect, after the page content.
        let annot_dl = annotation_appearance_display_list(self, page, budget, g)?;
        if !annot_dl.ops.is_empty() {
            render_display_list(
                &annot_dl,
                backend,
                page_ctm,
                &font_data,
                &resolve_smask,
                &resolve_inline_image,
                &resolve_shading,
                &resolve_pattern,
                g,
            );
        }
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
        let mut resolver = self.new_resolver(budget);
        let content = resolve_page_content(&mut resolver, page, g)?;
        if content.is_empty() {
            return Ok(selis_pdf_content::display_list::DisplayList::default());
        }
        build_display_list(self, content, page.resources.as_ref(), budget, g)
    }
}

/// The display list of every drawable annotation appearance on a page
/// (SL-2.RAST.13): each `/Annots` entry with a `/AP` `/N` normal appearance
/// renders as a form XObject mapped onto its `/Rect` (ISO 32000-2 §12.5.5).
/// Annotations without an appearance (or hidden ones) contribute nothing.
fn annotation_appearance_display_list(
    session: &Session,
    page: &selis_pdf_doc::Page,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<selis_pdf_content::display_list::DisplayList> {
    let mut dl = selis_pdf_content::display_list::DisplayList::new();
    let mut resolver = session.new_resolver(budget);
    // The page dict: /Annots lives there, not in the doc model's Page.
    let Some(page_obj) = resolver
        .resolve(selis_pdf_cos::Ref::new(page.num, 0), g)
        .ok()
    else {
        return Ok(dl);
    };
    let pairs: &[(Bytes, Obj)] = match &page_obj {
        Obj::Dict(pairs) => pairs,
        _ => return Ok(dl),
    };
    let Some(annots_obj) = dict_get_obj(pairs, b"Annots") else {
        return Ok(dl);
    };
    let annots: Vec<Obj> = match annots_obj {
        Obj::Array(items) => items
            .iter()
            .filter_map(|o| match o {
                Obj::Ref(r) => resolver.resolve(*r, g).ok(),
                o @ Obj::Dict(_) => Some(o.clone()),
                _ => None,
            })
            .collect(),
        _ => return Ok(dl),
    };
    for annot in &annots {
        let annot_pairs: &[(Bytes, Obj)] = match annot {
            Obj::Dict(pairs) => pairs,
            _ => continue,
        };
        // Hidden flag (/F bit 2): not displayed, not printed.
        if dict_get_obj(annot_pairs, b"F")
            .and_then(|v| match v {
                Obj::Int(n) => Some(*n),
                _ => None,
            })
            .is_some_and(|f| f & 2 != 0)
        {
            continue;
        }
        // /AP /N: the normal appearance. /N may be a stream ref, an array
        // (take the first), or a state-name dict (take the first entry).
        let Some(ap) = dict_get_obj(annot_pairs, b"AP") else {
            continue;
        };
        let ap_resolved: Obj = match ap {
            Obj::Dict(_) => ap.clone(),
            Obj::Ref(r) => match resolver.resolve(*r, g) {
                Ok(d) => d,
                Err(_) => continue,
            },
            _ => continue,
        };
        let ap_pairs: &[(Bytes, Obj)] = match &ap_resolved {
            Obj::Dict(ap_pairs) => ap_pairs,
            _ => continue,
        };
        let n_resolved: Option<Obj> = match dict_get_obj(ap_pairs, b"N") {
            Some(Obj::Ref(r)) => resolver.resolve(*r, g).ok(),
            Some(Obj::Array(items)) => items.first().and_then(|o| match o {
                Obj::Ref(r) => resolver.resolve(*r, g).ok(),
                _ => None,
            }),
            Some(Obj::Dict(state_pairs)) => match dict_first_value(state_pairs) {
                Some(Obj::Ref(r)) => resolver.resolve(*r, g).ok(),
                _ => None,
            },
            _ => None,
        };
        let Some(Obj::Stream { dict, data }) = n_resolved else {
            continue;
        };
        // The appearance form's own /Resources (annotations carry them on
        // the appearance stream, not the page).
        let appearance_resources = dict_get_obj(&dict, b"Resources").map(|o| match o {
            Obj::Ref(r) => resolver.resolve(*r, g).unwrap_or(Obj::Null),
            other => other.clone(),
        });
        let content = unfilter_stream_data(&dict, &data, g);
        // Form /Matrix (form space → appearance user space; default identity).
        let form_matrix = dict_get_obj(&dict, b"Matrix")
            .and_then(matrix_from_obj)
            .unwrap_or(Matrix::IDENTITY);
        // /Rect maps the appearance /BBox onto the page. Rect values may be
        // swapped (lower-left > upper-right is legal); normalise.
        let Some(Obj::Array(rect_arr)) = dict_get_obj(annot_pairs, b"Rect") else {
            continue;
        };
        let num = |v: Option<&Obj>| -> Option<f64> {
            match v? {
                Obj::Int(n) => Some(*n as f64),
                Obj::Real { scaled, scale } => Some(*scaled as f64 / 10f64.powi(*scale as i32)),
                _ => None,
            }
        };
        let (Some(rx0), Some(ry0), Some(rx1), Some(ry1)) = (
            num(rect_arr.first()),
            num(rect_arr.get(1)),
            num(rect_arr.get(2)),
            num(rect_arr.get(3)),
        ) else {
            continue;
        };
        let (rx0, rx1) = (rx0.min(rx1), rx1.max(rx0));
        let (ry0, ry1) = (ry0.min(ry1), ry1.max(ry0));
        let (rw, rh) = (rx1 - rx0, ry1 - ry0);
        if !rw.is_finite() || !rh.is_finite() || rw <= 0.0 || rh <= 0.0 {
            continue; // degenerate /Rect: nothing drawable
        }
        // BBox (form space); default the full unit square-ish box if absent.
        let bbox = dict_get_obj(&dict, b"BBox")
            .and_then(|o| match o {
                Obj::Array(arr) => Some((
                    num(arr.first()),
                    num(arr.get(1)),
                    num(arr.get(2)),
                    num(arr.get(3)),
                )),
                _ => None,
            })
            .and_then(|(a, b, c, d)| Some((a?, b?, c?, d?)))
            .unwrap_or((0.0, 0.0, rw, rh));
        let (bw, bh) = (bbox.2 - bbox.0, bbox.3 - bbox.1);
        if !bw.is_finite() || !bh.is_finite() || bw <= 0.0 || bh <= 0.0 {
            continue;
        }
        // Placement: scale the BBox onto the /Rect and translate.
        let sx = rw / bw;
        let sy = rh / bh;
        let place = Matrix::new(sx, 0.0, 0.0, sy, rx0 - bbox.0 * sx, ry0 - bbox.1 * sy);
        if !place.is_finite() {
            continue;
        }
        // Content transform: /Matrix first, then the placement. The content
        // interpreter starts from an identity CTM, so both are prepended as
        // `cm` operators in application order.
        let prefix = format!(
            "{} {} {} {} {} {} cm {} {} {} {} {} {} cm\n",
            form_matrix.a,
            form_matrix.b,
            form_matrix.c,
            form_matrix.d,
            form_matrix.e,
            form_matrix.f,
            place.a,
            place.b,
            place.c,
            place.d,
            place.e,
            place.f
        );
        let mut full = prefix.into_bytes();
        full.extend_from_slice(&content);
        let Ok(appearance_dl) =
            build_display_list(session, full, appearance_resources.as_ref(), budget, g)
        else {
            continue; // a broken appearance is a deviation, never fatal
        };
        dl.ops.extend(appearance_dl.ops);
    }
    Ok(dl)
}

/// The first value of a dictionary (for appearance-state /N dicts).
fn dict_first_value(pairs: &[(Bytes, Obj)]) -> Option<&Obj> {
    pairs.iter().map(|(_, v)| v).next()
}

/// Build a display list from a content stream against a resource dictionary.
/// The resource closures (fonts, XObjects, ExtGState) resolve against
/// `resources`, which the page and pattern tiles both provide.
fn build_display_list(
    session: &Session,
    content: Vec<u8>,
    resources: Option<&Obj>,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<selis_pdf_content::display_list::DisplayList> {
    let budget_copy = *budget;
    let clock = g.clock();
    // Glyph-width cache (SL-2.PERF.02): the interpreter asks per glyph, so
    // resolving the font dict and width table per call repeats identical
    // work. Keyed by (font, resource scope, code) — a different scope may
    // resolve the same name differently, so the key must not collapse it.
    // Walk-local and lookup-only, so determinism holds by construction.
    let width_cache: RefCell<HashMap<(Bytes, Option<Bytes>, u16), f64>> =
        RefCell::new(HashMap::new());
    let font_width = move |font_name: &Bytes, code: u16, key: Option<&Bytes>| -> f64 {
        let cache_key = (font_name.clone(), key.cloned(), code);
        if let Some(w) = width_cache.borrow().get(&cache_key) {
            return *w;
        }
        let mut bg = budget_copy.guard_with(clock, CancelToken::new());
        let mut res = session.new_resolver(&budget_copy);
        let r = resolve_resources_for_key(&mut res, key, resources, &mut bg);
        let w = font_width_inner(&mut res, r.as_ref(), font_name, code, &mut bg).unwrap_or(0.0);
        if width_cache.borrow().len() < 4096 {
            width_cache.borrow_mut().insert(cache_key, w);
        }
        w
    };
    let resolve_do =
        move |name: &Bytes, key: Option<&Bytes>| -> Option<selis_pdf_content::exec::DoTarget> {
            let mut bg = budget_copy.guard_with(clock, CancelToken::new());
            let mut res = session.new_resolver(&budget_copy);
            let r = resolve_resources_for_key(&mut res, key, resources, &mut bg);
            resolve_xobject_inner(&mut res, r.as_ref(), name, &mut bg)
                .ok()
                .flatten()
        };
    let resolve_ext_gstate =
        move |name: &Bytes,
              key: Option<&Bytes>|
              -> Option<Vec<(Bytes, selis_pdf_content::dispatch::Operand)>> {
            let mut bg = budget_copy.guard_with(clock, CancelToken::new());
            let mut res = session.new_resolver(&budget_copy);
            let r = resolve_resources_for_key(&mut res, key, resources, &mut bg);
            resolve_ext_gstate_inner(&mut res, r.as_ref(), name, &mut bg)
                .ok()
                .flatten()
        };
    selis_pdf_content::exec::execute(&content, &font_width, &resolve_do, &resolve_ext_gstate, g)
}

/// Resolve the resources dict for a resource key (a form XObject's object
/// reference): its `/Resources` when present, else the caller's fallback.
fn resolve_resources_for_key(
    resolver: &mut Resolver<'_>,
    key: Option<&Bytes>,
    fallback: Option<&Obj>,
    g: &mut BudgetGuard<'_>,
) -> Option<Obj> {
    let key = match key {
        Some(k) => k,
        None => return fallback.map(Obj::clone),
    };
    let key_str = std::str::from_utf8(key.as_slice()).ok()?;
    let mut it = key_str.split_whitespace();
    let num: u32 = it.next()?.parse().ok()?;
    let gen: u16 = it.next()?.parse().ok()?;
    let form = resolver
        .resolve(selis_pdf_cos::Ref::new(num, gen), g)
        .ok()?;
    match &form {
        Obj::Stream { dict, .. } => dict_get_obj(dict, b"Resources").map(Obj::clone),
        _ => fallback.map(Obj::clone),
    }
}

/// Resolve a tiling pattern (colour-space pattern, `/PatternType 1`) from the
/// resources: render the pattern's content into a tile, and return the
/// tiling-pattern (tile + steps + matrix) for placement.
fn resolve_pattern_inner(
    session: &Session,
    resources: Option<&Obj>,
    name: &Bytes,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Option<selis_raster::pattern::TilingPattern> {
    use selis_raster::pattern::{PatternTile, PatternType, TilingPattern};
    let caller_resources = resources;
    let resources = resources?;
    let patterns = dict_get(resources, b"Pattern")?;
    let pattern_obj = dict_get(patterns, name.as_slice())?;
    let pattern_obj = match pattern_obj {
        Obj::Ref(r) => {
            let mut res = session.new_resolver(budget);
            res.resolve(*r, g).ok()?
        }
        o => o.clone(),
    };
    let (pairs, data) = match &pattern_obj {
        Obj::Stream { dict, data } => (dict, data.as_slice()),
        _ => return None,
    };
    let int = |key: &[u8]| -> Option<i64> {
        dict_get_obj(pairs, key).and_then(|v| match v {
            Obj::Int(n) => Some(*n),
            _ => None,
        })
    };
    let num = |key: &[u8]| -> Option<f64> {
        match dict_get_obj(pairs, key) {
            Some(Obj::Int(n)) => Some(*n as f64),
            Some(Obj::Real { scaled, scale }) => Some(*scaled as f64 / 10f64.powi(*scale as i32)),
            _ => None,
        }
    };
    // Only tiling patterns (type 1); shading patterns (2) are not patterns in
    // this colour-space sense.
    if int(b"PatternType") != Some(1) {
        return None;
    }
    let paint_type = if int(b"PaintType") == Some(2) {
        PatternType::Uncoloured
    } else {
        PatternType::Coloured
    };
    let bbox = match dict_get_obj(pairs, b"BBox") {
        Some(Obj::Array(arr)) => {
            let n = |i: usize| match arr.get(i) {
                Some(Obj::Int(v)) => Some(*v as f64),
                Some(Obj::Real { scaled, scale }) => {
                    Some(*scaled as f64 / 10f64.powi(*scale as i32))
                }
                _ => None,
            };
            selis_geom::Rect::new(n(0)?, n(1)?, n(2)?, n(3)?)
        }
        _ => return None,
    };
    let x_step = num(b"XStep").unwrap_or(bbox.width());
    let y_step = num(b"YStep").unwrap_or(bbox.height());
    let matrix = match dict_get_obj(pairs, b"Matrix") {
        Some(Obj::Array(arr)) => {
            let n = |i: usize| match arr.get(i) {
                Some(Obj::Int(v)) => Some(*v as f64),
                Some(Obj::Real { scaled, scale }) => {
                    Some(*scaled as f64 / 10f64.powi(*scale as i32))
                }
                _ => None,
            };
            selis_geom::Matrix::new(n(0)?, n(1)?, n(2)?, n(3)?, n(4)?, n(5)?)
        }
        _ => selis_geom::Matrix::IDENTITY,
    };
    if !(x_step > 0.0) || !(y_step > 0.0) {
        return None;
    }
    // The pattern's content, executed against its own /Resources (falling
    // back to the caller's).
    let content = unfilter_stream_data(pairs, data, g);
    let pattern_resources = dict_get_obj(pairs, b"Resources")
        .map(|o| o.clone())
        .or_else(|| caller_resources.map(Obj::clone));
    let dl = build_display_list(session, content, pattern_resources.as_ref(), budget, g).ok()?;
    let w = dim_ceil(bbox.width()).max(1);
    let h = dim_ceil(bbox.height()).max(1);
    let mut tile = TinySkiaBackend::new(w, h)?;
    let no_font = |_name: &Bytes| -> Option<Vec<u8>> { None };
    let no_smask = |_key: &Bytes| -> Option<Mask> { None };
    let no_inline = |_d: &[(Bytes, Bytes)], _data: &[u8]| -> Option<(u32, u32, Bytes)> { None };
    let no_shading = |_n: &Bytes,
                      _s: &selis_pdf_content::display_list::ResolvedState|
     -> Option<(u32, u32, Bytes, selis_geom::Rect)> { None };
    let no_pattern = |_name: &Bytes| -> Option<selis_raster::pattern::TilingPattern> { None };
    crate::render::render_display_list(
        &dl,
        &mut tile,
        Matrix::IDENTITY,
        &no_font,
        &no_smask,
        &no_inline,
        &no_shading,
        &no_pattern,
        g,
    );
    Some(TilingPattern {
        paint_type,
        tile: PatternTile {
            width: w,
            height: h,
            rgba8: tile.pixmap().data().to_vec(),
        },
        x_step,
        y_step,
        matrix,
    })
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

/// A finite, non-negative f64 as a u32 dimension (ceil, saturate).
fn dim_ceil(v: f64) -> u32 {
    let v = v.ceil();
    if v <= 0.0 {
        0
    } else if v >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        // The value is bounded to [0, u32::MAX], so the narrowing is exact.
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        {
            v as u32
        }
    }
}

/// A value in [0, 1] as an 8-bit byte (clamped before the narrowing).
fn f64_to_u8(v: f64) -> u8 {
    let v = v.clamp(0.0, 1.0) * 255.0;
    // The value is bounded to [0, 255], so the narrowing is exact.
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    {
        v as u8
    }
}

/// A non-negative f64 as a u32 (saturating).
fn f64_to_u32(v: f64) -> u32 {
    let v = v.max(0.0);
    if v >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        // The value is bounded to [0, u32::MAX], so the narrowing is exact.
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        {
            v as u32
        }
    }
}

/// The embedded font program bytes for a font resource name.
fn font_data_inner(
    resolver: &mut Resolver<'_>,
    resources: Option<&Obj>,
    font_name: &Bytes,
    g: &mut BudgetGuard<'_>,
) -> Option<Vec<u8>> {
    // Prefer the embedded font program.
    if let Some(font_dict) = resolve_font_dict(resolver, resources, font_name, g) {
        if let Some(font_file) = font_dict.font_file {
            return Some(font_file.data().as_slice().to_vec());
        }
        // A non-embedded standard-14 font falls back to the bundled Liberation
        // font (SL-0.LEAD.07), keyed by the /BaseFont name.
        if let Some(bytes) = selis_font::fallback::fallback_bytes(&font_dict.base_font) {
            return Some(bytes.to_vec());
        }
    }
    // A `Tf` naming a standard-14 font that /Resources does not declare still
    // renders in every mainstream viewer (and MuPDF, the render oracle) — a
    // missing font resource is a deviation, not a silent no-draw.
    let name = std::str::from_utf8(font_name.as_slice()).ok()?;
    selis_font::fallback::fallback_bytes(name).map(|bytes| bytes.to_vec())
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
        // the source at the xref offset. Malformed-input failures on one
        // stream are tolerated (the page renders with the rest); budget,
        // cancellation, and pending errors propagate as-is — an injected wall
        // deadline must abort the page, not shrink its content
        // (SL-0.SBX.07).
        match resolve_stream(resolver, *r, g) {
            Ok(Some((dict, data))) => {
                // `unfilter_stream_data` handles filter chains and
                // /DecodeParms (a single-filter decode silently ignored
                // /EarlyChange, so real-world LZW content failed to decode
                // and the page painted blank).
                out.extend_from_slice(&unfilter_stream_data(&dict, &data, g));
            }
            Err(e) if e.is_budget() || e.is_cancelled() || e.is_pending() => return Err(e),
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
    match &obj {
        // The resolver decrypts stream bodies, so use its data directly.
        Obj::Stream { dict, data } => Ok(Some((dict.clone(), data.as_slice().to_vec()))),
        // A plain dict: read the stream body from the source at the xref offset.
        Obj::Dict(d) => {
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
            let start = usize::try_from(offset).unwrap_or(0);
            let rest = resolver.src().get(start..).unwrap_or(&[]);
            let Some(stream_pos) = rest.windows(6).position(|w| w == b"stream") else {
                return Ok(None); // not a stream
            };
            let mut data_start = stream_pos.saturating_add(6);
            if rest.get(data_start) == Some(&b'\r') {
                data_start = data_start.saturating_add(1);
            }
            if rest.get(data_start) == Some(&b'\n') {
                data_start = data_start.saturating_add(1);
            }
            let length = d
                .iter()
                .find(|(k, _)| k.as_slice() == b"Length")
                .and_then(|(_, v)| match v {
                    Obj::Int(n) => u64::try_from(*n).ok(),
                    _ => None,
                })
                .unwrap_or(0);
            let length = usize::try_from(length).unwrap_or(0);
            let data = rest
                .get(data_start..data_start.saturating_add(length).min(rest.len()))
                .unwrap_or(&[])
                .to_vec();
            let _ = g;
            Ok(Some((d.clone(), data)))
        }
        _ => Ok(None),
    }
}

fn font_width_inner(
    resolver: &mut Resolver<'_>,
    resources: Option<&Obj>,
    font_name: &Bytes,
    code: u16,
    g: &mut BudgetGuard<'_>,
) -> Option<f64> {
    let font_dict = match resolve_font_dict(resolver, resources, font_name, g) {
        Some(fd) => fd,
        None => {
            // Undeclared standard-14 font: metrics from the name itself
            // (the same tolerance as `font_data_inner`).
            let name = std::str::from_utf8(font_name.as_slice()).ok()?;
            let mut fd = selis_font::FontDict::simple(selis_font::FontSubtype::Type1);
            fd.base_font = name.to_string();
            fd
        }
    };
    let resolved = selis_font::resolve_widths(&font_dict, g).ok()?;
    Some(resolved.width(u32::from(code)))
}

fn resolve_font_dict(
    resolver: &mut Resolver<'_>,
    resources: Option<&Obj>,
    font_name: &Bytes,
    g: &mut BudgetGuard<'_>,
) -> Option<selis_font::FontDict> {
    let resources = resources?;
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

/// Unfilter a stream's data through its `/Filter` chain (Name or array) with
/// `/DecodeParms` (predictors). Falls back to the raw data on any decode
/// failure so a hostile stream never aborts the page.
fn unfilter_stream_data(dict: &[(Bytes, Obj)], data: &[u8], g: &mut BudgetGuard<'_>) -> Vec<u8> {
    let filters: Vec<String> = match dict_get_obj(dict, b"Filter") {
        Some(Obj::Name(n)) => vec![String::from_utf8_lossy(n.as_slice()).to_string()],
        Some(Obj::Array(arr)) => arr
            .iter()
            .filter_map(|o| match o {
                Obj::Name(n) => Some(String::from_utf8_lossy(n.as_slice()).to_string()),
                _ => None,
            })
            .collect(),
        _ => return data.to_vec(),
    };
    if filters.is_empty() {
        return data.to_vec();
    }
    let parms = decode_parms_from_obj(dict_get_obj(dict, b"DecodeParms"));
    selis_pdf_filter::decode_chain(&filters, &parms, data, u64::MAX, g)
        .unwrap_or_else(|_| data.to_vec())
}

/// Parse `/DecodeParms` (a dict, or an array aligned with the filter chain)
/// into per-filter parameters.
fn decode_parms_from_obj(obj: Option<&Obj>) -> Vec<selis_pdf_filter::DecodeParms> {
    let from_dict = |pairs: &[(Bytes, Obj)]| -> selis_pdf_filter::DecodeParms {
        let mut p = selis_pdf_filter::DecodeParms::default();
        let int = |key: &[u8]| -> Option<i64> {
            dict_get_obj(pairs, key).and_then(|v| match v {
                Obj::Int(n) => Some(*n),
                _ => None,
            })
        };
        p.predictor = int(b"Predictor")
            .and_then(|n| u16::try_from(n).ok())
            .unwrap_or(1);
        p.columns = int(b"Columns")
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(1);
        p.colors = int(b"Colors")
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(1);
        p.bits_per_component = int(b"BitsPerComponent")
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(8);
        p.early_change = int(b"EarlyChange")
            .and_then(|n| u8::try_from(n).ok())
            // The spec default is 1 (ISO 32000-2 §7.4.6.2), not 0.
            .unwrap_or(1);
        p
    };
    match obj {
        Some(Obj::Dict(pairs)) => vec![from_dict(pairs)],
        Some(Obj::Array(items)) => items
            .iter()
            .map(|o| match o {
                Obj::Dict(pairs) => from_dict(pairs),
                _ => selis_pdf_filter::DecodeParms::default(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Resolve an image XObject from the resources into decoded RGBA.
fn resolve_xobject_inner(
    resolver: &mut Resolver<'_>,
    resources: Option<&Obj>,
    name: &Bytes,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<selis_pdf_content::exec::DoTarget>> {
    let Some(resources) = resources else {
        return Ok(None);
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
        // A form XObject: content + /Matrix. The form's own /Resources are
        // resolved by the engine via the form's object ref.
        let content = unfilter_stream_data(&dict, &data, g);
        let matrix = dict_get_obj(&dict, b"Matrix")
            .and_then(matrix_from_obj)
            .unwrap_or(Matrix::IDENTITY);
        // The resources key is the form's object ref (e.g. "12 0"), which
        // the engine resolves to the form's /Resources dict.
        let resources = Some(Bytes::copy_from_slice(
            format!("{} {}", r.num, r.gen).as_bytes(),
        ));
        return Ok(Some(selis_pdf_content::exec::DoTarget::Form {
            content,
            matrix,
            resources,
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
    // Terminal image codecs first: DCT/JPX streams carry compressed image
    // data the raw-sample decoder cannot read — silently skipping them is
    // how a scanned page renders blank (SL-2.RAST.13).
    if let Some(img) = decode_terminal_image_rgba(&dict, &unfiltered, g) {
        return Ok(Some(selis_pdf_content::exec::DoTarget::Image {
            width: img.width,
            height: img.height,
            rgba8: Bytes::copy_from_slice(&img.rgba8),
        }));
    }
    let decode = selis_raster::image::Decode::identity(usize::from(components));
    let img =
        selis_raster::image::decode_image(width, height, components, bpc, &unfiltered, &decode, g)?;
    Ok(Some(selis_pdf_content::exec::DoTarget::Image {
        width: img.width,
        height: img.height,
        rgba8: Bytes::copy_from_slice(&img.rgba8),
    }))
}

/// Decode an image stream whose samples are still in a terminal image codec
/// (DCTDecode/JPXDecode, or a payload with the codecs' magic bytes) into
/// straight RGBA8. `None` when the stream is not a terminal-codec image and
/// the raw-sample decoder should take it.
///
/// # Malformed Input
///
/// A corrupt JPEG/JPX payload returns `None` here only after this function
/// already committed to the codec path — the caller surfaces the failure as a
/// typed decode error rather than drawing nothing.
fn decode_terminal_image_rgba(
    dict: &[(Bytes, Obj)],
    unfiltered: &[u8],
    g: &mut BudgetGuard<'_>,
) -> Option<DecodedImage> {
    let names = filter_names_from_dict(dict);
    let is_dct = names.iter().any(|f| f == "DCTDecode" || f == "DCT")
        || unfiltered.starts_with(&[0xFF, 0xD8]);
    let is_jpx = names.iter().any(|f| f == "JPXDecode")
        || unfiltered.starts_with(&[0x00, 0x00, 0x00, 0x0C, b'j', b'P', b' ', b' ']);
    if is_dct {
        let img = selis_image::decode_jpeg(unfiltered, g).ok()?;
        return Some(DecodedImage {
            width: img.width,
            height: img.height,
            rgba8: img.rgba,
        });
    }
    // JPXDecode runs OpenJPEG behind the Tier-2 WASM sandbox (SL-1.FILT.08);
    // the codec is only compiled in with the `wasm-host` feature, so a
    // default build surfaces it as a typed deviation instead (the record, not
    // a silent claim of support).
    #[cfg(feature = "wasm-host")]
    if is_jpx {
        let dct = selis_pdf_filter::jpx_decode(unfiltered, g).ok()?;
        let img = selis_image::dct_to_rgba(&dct, g).ok()?;
        return Some(DecodedImage {
            width: img.width,
            height: img.height,
            rgba8: img.rgba,
        });
    }
    let _ = is_jpx;
    None
}

/// The `/Filter` chain of a stream dict (Name or array of Names), lowercased
/// names as written.
fn filter_names_from_dict(dict: &[(Bytes, Obj)]) -> Vec<String> {
    match dict_get_obj(dict, b"Filter") {
        Some(Obj::Name(n)) => vec![String::from_utf8_lossy(n.as_slice()).to_string()],
        Some(Obj::Array(items)) => items
            .iter()
            .filter_map(|o| match o {
                Obj::Name(n) => Some(String::from_utf8_lossy(n.as_slice()).to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
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
    if let Some(img) = decode_terminal_image_rgba(dict, &unfiltered, g) {
        return Ok(img);
    }
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
    let obj = resolver
        .resolve(selis_pdf_cos::Ref::new(num, gen), g)
        .ok()?;
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
                Obj::Real { scaled, scale } => Some(*scaled as f64 / 10f64.powi(*scale as i32)),
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
        dict.iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v)
    };
    let parse_u32 = |v: &selis_bytes::Bytes| -> Option<u32> {
        let s = std::str::from_utf8(v.as_slice()).ok()?;
        s.trim().parse().ok()
    };
    let width = get(b"W").and_then(parse_u32)?;
    let height = get(b"H").and_then(parse_u32)?;
    let bpc = get(b"BPC")
        .and_then(|v| parse_u32(v).map(|n| n.min(16) as u8))
        .unwrap_or(8);
    let components: u8 = match get(b"CS") {
        Some(v) if v.as_slice().strip_prefix(b"/").unwrap_or(v.as_slice()) == b"G" => 1,
        Some(v) if v.as_slice().strip_prefix(b"/").unwrap_or(v.as_slice()) == b"RGB" => 3,
        Some(v) if v.as_slice().strip_prefix(b"/").unwrap_or(v.as_slice()) == b"CMYK" => 4,
        _ => 3,
    };
    let unfiltered = match get(b"F").or_else(|| get(b"Filter")) {
        Some(filt) => {
            let name = std::str::from_utf8(
                filt.as_slice()
                    .strip_prefix(b"/")
                    .unwrap_or(filt.as_slice()),
            )
            .unwrap_or("");
            selis_pdf_filter::decode(name, data, u64::MAX, g).unwrap_or_else(|_| data.to_vec())
        }
        None => data.to_vec(),
    };
    let decode = Decode::identity(usize::from(components));
    let img = decode_image(width, height, components, bpc, &unfiltered, &decode, g).ok()?;
    Some((
        img.width,
        img.height,
        selis_bytes::Bytes::copy_from_slice(&img.rgba8),
    ))
}

/// Resolve a shading resource to a rasterised RGBA image in device space.
fn resolve_shading_inner(
    resolver: &mut Resolver<'_>,
    resources: Option<&Obj>,
    name: &Bytes,
    state: &selis_pdf_content::display_list::ResolvedState,
    g: &mut BudgetGuard<'_>,
) -> Option<(u32, u32, selis_bytes::Bytes, selis_geom::Rect)> {
    use selis_geom::Rect;
    fn o<'a>(dict: &'a [(Bytes, Obj)], key: &[u8]) -> Option<&'a Obj> {
        dict.iter()
            .find(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v)
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
        Some(Rect::new(
            arr_n(arr, 0)?,
            arr_n(arr, 1)?,
            arr_n(arr, 2)?,
            arr_n(arr, 3)?,
        ))
    }
    fn arr_to_matrix(arr: &[Obj]) -> Option<selis_geom::Matrix> {
        Some(selis_geom::Matrix::new(
            arr_n(arr, 0)?,
            arr_n(arr, 1)?,
            arr_n(arr, 2)?,
            arr_n(arr, 3)?,
            arr_n(arr, 4)?,
            arr_n(arr, 5)?,
        ))
    }
    // Resolve the shading dict from /Shading resources.
    let shadings = dict_get(resources?, b"Shading")?;
    let obj = match dict_get(shadings, name.as_slice())? {
        Obj::Ref(r) => resolver.resolve(*r, g).ok()?,
        o => o.clone(),
    };
    // The shading is a dict, or (types 4–7) a stream carrying the mesh data.
    let (pairs, mesh_data) = match &obj {
        Obj::Dict(pairs) => (pairs, None),
        Obj::Stream { dict, data } => (dict, Some(data.as_slice())),
        _ => return None,
    };
    let shading_type = match dict_get_obj(pairs, b"ShadingType") {
        Some(Obj::Int(n)) => *n,
        _ => return None,
    };
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
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for p in &corners {
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    let dev_rect = Rect::new(min_x, min_y, max_x, max_y);
    let w = dim_ceil(dev_rect.width()).max(1);
    let h = dim_ceil(dev_rect.height()).max(1);
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
        Some(Obj::Array(arr)) => (
            arr_n(arr, 0).unwrap_or(0.0) != 0.0,
            arr_n(arr, 1).unwrap_or(0.0) != 0.0,
        ),
        _ => (false, false),
    };
    let coords: &[Obj] = match o(pairs, b"Coords") {
        Some(Obj::Array(arr)) => arr.as_slice(),
        _ => &[],
    };
    // Types 4/5 (Gouraud and lattice meshes): decode the packed vertex stream
    // into a common (vertices, triangles) mesh.
    let mesh: Option<(
        Vec<selis_raster::shading::ShadingPoint>,
        Vec<(u32, u32, u32)>,
    )> = match shading_type {
        4 | 5 => {
            let data = mesh_data?;
            let bpc = f64_to_u32(on(pairs, b"BitsPerCoordinate").unwrap_or(8.0));
            let bpc_color = f64_to_u32(on(pairs, b"BitsPerComponent").unwrap_or(8.0));
            let decode: Vec<f64> = match o(pairs, b"Decode") {
                Some(Obj::Array(arr)) => arr
                    .iter()
                    .filter_map(|v| match v {
                        Obj::Int(n) => Some(*n as f64),
                        Obj::Real { scaled, scale } => {
                            Some(*scaled as f64 / 10f64.powi(*scale as i32))
                        }
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let components = match color_space {
                b"DeviceGray" | b"G" => 1,
                b"DeviceCMYK" | b"CMYK" => 4,
                _ => 3,
            };
            if shading_type == 4 {
                let bpf = f64_to_u32(on(pairs, b"BitsPerFlag").unwrap_or(8.0));
                let m = parse_gouraud_shading(data, bpc, bpc_color, bpf, &decode, components)?;
                Some((m.vertices, m.triangles))
            } else {
                let cols = f64_to_u32(on(pairs, b"VerticesPerRow").unwrap_or(2.0));
                let m = parse_lattice_shading(
                    data,
                    bpc,
                    bpc_color,
                    &decode,
                    components,
                    cols as usize,
                )?;
                let tris = lattice_triangles(&m);
                Some((m.vertices, tris))
            }
        }
        6 | 7 => {
            let data = mesh_data?;
            let bpc = f64_to_u32(on(pairs, b"BitsPerCoordinate").unwrap_or(8.0));
            let bpc_color = f64_to_u32(on(pairs, b"BitsPerComponent").unwrap_or(8.0));
            let bpf = f64_to_u32(on(pairs, b"BitsPerFlag").unwrap_or(8.0));
            let decode: Vec<f64> = match o(pairs, b"Decode") {
                Some(Obj::Array(arr)) => arr
                    .iter()
                    .filter_map(|v| match v {
                        Obj::Int(n) => Some(*n as f64),
                        Obj::Real { scaled, scale } => {
                            Some(*scaled as f64 / 10f64.powi(*scale as i32))
                        }
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let components = match color_space {
                b"DeviceGray" | b"G" => 1,
                b"DeviceCMYK" | b"CMYK" => 4,
                _ => 3,
            };
            let is_tensor = shading_type == 7;
            let patches =
                parse_patch_shading(data, bpc, bpc_color, bpf, &decode, components, is_tensor)?;
            // Tessellate each patch into a (grid+1)² mesh and connect the
            // cells into triangle pairs.
            let grid = 8usize;
            let stride = grid.saturating_add(1);
            let mut vertices = Vec::new();
            let mut triangles = Vec::new();
            for patch in &patches {
                let base = u32::try_from(vertices.len()).unwrap_or(u32::MAX);
                vertices.extend(tessellate_patch(patch, is_tensor, grid));
                for j in 0..grid {
                    for i in 0..grid {
                        let v00 = base.saturating_add(
                            u32::try_from(j.saturating_mul(stride).saturating_add(i))
                                .unwrap_or(u32::MAX),
                        );
                        let v01 = v00.saturating_add(1);
                        let v10 = v00.saturating_add(u32::try_from(stride).unwrap_or(u32::MAX));
                        let v11 = v10.saturating_add(1);
                        triangles.push((v00, v01, v10));
                        triangles.push((v10, v01, v11));
                    }
                }
            }
            Some((vertices, triangles))
        }
        _ => None,
    };
    let mut rgba = selis_sandbox::alloc::vec_with_capacity::<u8>(
        g,
        usize::try_from(w)
            .unwrap_or(0)
            .saturating_mul(usize::try_from(h).unwrap_or(0))
            .saturating_mul(4),
    )
    .ok()?;
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
                4 | 5 | 6 | 7 => {
                    // Find the triangle containing the point and interpolate
                    // the vertex colours (barycentric).
                    let Some((vertices, triangles)) = &mesh else {
                        continue;
                    };
                    let mut found: Option<Vec<f64>> = None;
                    for tri in triangles {
                        let (Some(a), Some(b), Some(c)) = (
                            vertices.get(tri.0 as usize),
                            vertices.get(tri.1 as usize),
                            vertices.get(tri.2 as usize),
                        ) else {
                            continue;
                        };
                        if let Some((u, v, w)) =
                            barycentric_weights(a.point, b.point, c.point, in_shading)
                        {
                            found = Some(
                                a.components
                                    .iter()
                                    .zip(&b.components)
                                    .zip(&c.components)
                                    .map(|((&ca, &cb), &cc)| ca * u + cb * v + cc * w)
                                    .collect(),
                            );
                            break;
                        }
                    }
                    match found {
                        Some(c) => c,
                        None => continue,
                    }
                }
                _ => continue,
            };
            let rgb = components_to_rgb(&components, color_space);
            let r = f64_to_u8(rgb[0]);
            let g = f64_to_u8(rgb[1]);
            let b = f64_to_u8(rgb[2]);
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
                        pairs
                            .iter()
                            .find(|(k, _)| k.as_slice() == key)
                            .and_then(|(_, v)| match v {
                                Obj::Int(n) => Some(*n as f64),
                                Obj::Real { scaled, scale } => {
                                    Some(*scaled as f64 / 10f64.powi(*scale as i32))
                                }
                                _ => None,
                            })
                    };
                    let get_arr = |key: &[u8]| -> Option<Vec<f64>> {
                        match pairs.iter().find(|(k, _)| k.as_slice() == key) {
                            Some((_, Obj::Array(arr))) => arr
                                .iter()
                                .map(|v| match v {
                                    Obj::Int(n) => Some(*n as f64),
                                    Obj::Real { scaled, scale } => {
                                        Some(*scaled as f64 / 10f64.powi(*scale as i32))
                                    }
                                    _ => None,
                                })
                                .collect(),
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
                    let a: Vec<f64> = (0..outputs)
                        .map(|i| {
                            c1.get(i).copied().unwrap_or(1.0) - c0.get(i).copied().unwrap_or(0.0)
                        })
                        .collect();
                    let b = vec![n; outputs];
                    let c: Vec<f64> = (0..outputs)
                        .map(|i| c0.get(i).copied().unwrap_or(0.0))
                        .collect();
                    Some(selis_color::function::Function::Exponential(
                        selis_color::function::ExponentialFunction {
                            inputs: 1,
                            outputs,
                            a,
                            b,
                            c,
                            domain: vec![(0.0, 1.0)],
                            range: None,
                        },
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

/// A big-endian bit reader over a byte buffer.
struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: u64,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    fn read(&mut self, bits: u32) -> Option<u64> {
        if bits > 63 {
            return None;
        }
        let mut value = 0u64;
        for _ in 0..bits {
            let byte = *self
                .data
                .get(usize::try_from(self.bit_pos >> 3).unwrap_or(0))?;
            let bit = 7u32.wrapping_sub((self.bit_pos % 8) as u32);
            value = (value << 1) | u64::from((byte >> bit) & 1);
            self.bit_pos = self.bit_pos.saturating_add(1);
        }
        Some(value)
    }
}

/// Map a raw integer to its decoded range (PDF `/Decode` semantics).
fn decode_raw(raw: u64, bits: u32, lo: f64, hi: f64) -> f64 {
    if bits == 0 {
        return lo;
    }
    let max = (1u64 << bits).saturating_sub(1);
    if max == 0 {
        return lo;
    }
    let t = raw as f64 / max as f64;
    lo + (hi - lo) * t
}

/// Decode a type-4 free-form Gouraud triangle mesh from its packed bit stream.
fn parse_gouraud_shading(
    data: &[u8],
    bpc: u32,
    bpc_color: u32,
    bpf: u32,
    decode: &[f64],
    components: usize,
) -> Option<selis_raster::shading::GouraudShading> {
    use selis_geom::Point;
    use selis_raster::shading::{GouraudShading, ShadingPoint};
    let mut r = BitReader::new(data);
    let mut vertices: Vec<ShadingPoint> = Vec::new();
    let mut triangles: Vec<(u32, u32, u32)> = Vec::new();
    let mut pending: Vec<u32> = Vec::new();
    let coords = |decode: &[f64], x_raw: u64, y_raw: u64| -> (f64, f64) {
        (
            decode_raw(
                x_raw,
                bpc,
                decode.get(0).copied().unwrap_or(0.0),
                decode.get(1).copied().unwrap_or(0.0),
            ),
            decode_raw(
                y_raw,
                bpc,
                decode.get(2).copied().unwrap_or(0.0),
                decode.get(3).copied().unwrap_or(0.0),
            ),
        )
    };
    loop {
        let Some(flag) = r.read(bpf.max(1)) else {
            break;
        };
        let count = match flag {
            0 => 3,
            1 => 1,
            2 => 2,
            _ => return None,
        };
        let mut new_indices = Vec::new();
        for _ in 0..count {
            let Some(x_raw) = r.read(bpc.max(1)) else {
                break;
            };
            let Some(y_raw) = r.read(bpc.max(1)) else {
                break;
            };
            let (x, y) = coords(decode, x_raw, y_raw);
            let mut comps = Vec::new();
            let mut colour_ok = true;
            for c in 0..components {
                let Some(raw) = r.read(bpc_color.max(1)) else {
                    colour_ok = false;
                    break;
                };
                let lo = decode
                    .get(c.saturating_mul(2).saturating_add(4))
                    .copied()
                    .unwrap_or(0.0);
                let hi = decode
                    .get(c.saturating_mul(2).saturating_add(5))
                    .copied()
                    .unwrap_or(1.0);
                comps.push(decode_raw(raw, bpc_color.max(1), lo, hi));
            }
            if !colour_ok {
                break;
            }
            let idx = u32::try_from(vertices.len()).ok()?;
            vertices.push(ShadingPoint {
                point: Point::new(x, y),
                components: comps,
            });
            new_indices.push(idx);
        }
        if new_indices.len() != count {
            break;
        }
        let tri = match flag {
            0 => (
                *new_indices.get(0)?,
                *new_indices.get(1)?,
                *new_indices.get(2)?,
            ),
            1 => (*pending.get(0)?, *pending.get(1)?, *new_indices.get(0)?),
            2 => (*pending.get(0)?, *new_indices.get(0)?, *new_indices.get(1)?),
            _ => return None,
        };
        triangles.push(tri);
        pending = vec![tri.1, tri.2];
    }
    if vertices.is_empty() {
        None
    } else {
        Some(GouraudShading {
            vertices,
            triangles,
        })
    }
}

/// Decode a type-5 lattice-form Gouraud mesh from its packed bit stream. The
/// vertices form a grid of `/VerticesPerRow` columns; each 2×2 cell becomes
/// two triangles.
fn parse_lattice_shading(
    data: &[u8],
    bpc: u32,
    bpc_color: u32,
    decode: &[f64],
    components: usize,
    cols: usize,
) -> Option<selis_raster::shading::LatticeShading> {
    use selis_geom::Point;
    use selis_raster::shading::{LatticeShading, ShadingPoint};
    let mut r = BitReader::new(data);
    let mut vertices: Vec<ShadingPoint> = Vec::new();
    loop {
        let Some(x_raw) = r.read(bpc.max(1)) else {
            break;
        };
        let Some(y_raw) = r.read(bpc.max(1)) else {
            break;
        };
        let (x, y) = (
            decode_raw(
                x_raw,
                bpc,
                decode.get(0).copied().unwrap_or(0.0),
                decode.get(1).copied().unwrap_or(0.0),
            ),
            decode_raw(
                y_raw,
                bpc,
                decode.get(2).copied().unwrap_or(0.0),
                decode.get(3).copied().unwrap_or(0.0),
            ),
        );
        let mut comps = Vec::new();
        let mut ok = true;
        for c in 0..components {
            let Some(raw) = r.read(bpc_color.max(1)) else {
                ok = false;
                break;
            };
            let lo = decode
                .get(c.saturating_mul(2).saturating_add(4))
                .copied()
                .unwrap_or(0.0);
            let hi = decode
                .get(c.saturating_mul(2).saturating_add(5))
                .copied()
                .unwrap_or(1.0);
            comps.push(decode_raw(raw, bpc_color.max(1), lo, hi));
        }
        if !ok {
            break;
        }
        vertices.push(ShadingPoint {
            point: Point::new(x, y),
            components: comps,
        });
    }
    if vertices.len() < cols.saturating_mul(2) || vertices.len().rem_euclid(cols) != 0 {
        return None;
    }
    Some(LatticeShading {
        vertices,
        cols: u32::try_from(cols).ok()?,
    })
}

/// Triangulate a lattice-form mesh into its (vertex-index) triangles.
fn lattice_triangles(l: &selis_raster::shading::LatticeShading) -> Vec<(u32, u32, u32)> {
    let cols = usize::try_from(l.cols).unwrap_or(0);
    if cols < 2 {
        return Vec::new();
    }
    let rows = l.vertices.len().div_euclid(cols);
    let mut tris = Vec::new();
    for row in 0..rows.saturating_sub(1) {
        for col in 0..cols.saturating_sub(1) {
            let v00 =
                u32::try_from(row.saturating_mul(cols).saturating_add(col)).unwrap_or(u32::MAX);
            let v01 = u32::try_from(
                row.saturating_mul(cols)
                    .saturating_add(col)
                    .saturating_add(1),
            )
            .unwrap_or(u32::MAX);
            let v10 = u32::try_from(
                row.saturating_add(1)
                    .saturating_mul(cols)
                    .saturating_add(col),
            )
            .unwrap_or(u32::MAX);
            let v11 = u32::try_from(
                row.saturating_add(1)
                    .saturating_mul(cols)
                    .saturating_add(col)
                    .saturating_add(1),
            )
            .unwrap_or(u32::MAX);
            tris.push((v00, v01, v10));
            tris.push((v10, v01, v11));
        }
    }
    tris
}

/// The cubic Bernstein basis functions.
fn bernstein(i: usize, t: f64) -> f64 {
    match i {
        0 => (1.0 - t).powi(3),
        1 => 3.0 * (1.0 - t) * (1.0 - t) * t,
        2 => 3.0 * (1.0 - t) * t * t,
        3 => t * t * t,
        _ => 0.0,
    }
}

/// A cubic Bézier curve value at `t` for control vectors `a b c d`.
#[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
fn bezier3(a: &[f64], b: &[f64], c: &[f64], d: &[f64], t: f64) -> Vec<f64> {
    (0..a.len())
        .map(|k| {
            bernstein(0, t) * a[k]
                + bernstein(1, t) * b[k]
                + bernstein(2, t) * c[k]
                + bernstein(3, t) * d[k]
        })
        .collect()
}

/// A Coons patch surface value at `(u, v)` from its 12 control points
/// (each a position+colour vector). The control-point count is validated by
/// `parse_patch_shading` before this is called.
#[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
fn coons_patch(points: &[selis_raster::shading::ShadingPoint], u: f64, v: f64) -> Vec<f64> {
    let bottom = {
        let v0 = pts_vec(&points[0]);
        let v1 = pts_vec(&points[1]);
        let v2 = pts_vec(&points[2]);
        let v3 = pts_vec(&points[3]);
        bezier3(&v0, &v1, &v2, &v3, u)
    };
    let right = {
        let v3 = pts_vec(&points[3]);
        let v4 = pts_vec(&points[4]);
        let v5 = pts_vec(&points[5]);
        let v6 = pts_vec(&points[6]);
        bezier3(&v3, &v4, &v5, &v6, v)
    };
    let top = {
        let v6 = pts_vec(&points[6]);
        let v7 = pts_vec(&points[7]);
        let v8 = pts_vec(&points[8]);
        let v9 = pts_vec(&points[9]);
        bezier3(&v6, &v7, &v8, &v9, u)
    };
    let left = {
        let v9 = pts_vec(&points[9]);
        let v10 = pts_vec(&points[10]);
        let v11 = pts_vec(&points[11]);
        let v0 = pts_vec(&points[0]);
        bezier3(&v9, &v10, &v11, &v0, v)
    };
    let c0 = pts_vec(&points[0]);
    let c3 = pts_vec(&points[3]);
    let c6 = pts_vec(&points[6]);
    let c9 = pts_vec(&points[9]);
    let w = bottom.len();
    (0..w)
        .map(|k| {
            (1.0 - v) * bottom[k] + v * top[k] + (1.0 - u) * left[k] + u * right[k]
                - (1.0 - u) * (1.0 - v) * c0[k]
                - u * (1.0 - v) * c3[k]
                - u * v * c6[k]
                - (1.0 - u) * v * c9[k]
        })
        .collect()
}

/// A tensor-product (bicubic Bézier) patch surface value at `(u, v)` from its
/// 16 control points.
#[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
fn tensor_patch(points: &[selis_raster::shading::ShadingPoint], u: f64, v: f64) -> Vec<f64> {
    let dim = pts_vec(&points[0]).len();
    let mut out = vec![0.0; dim];
    for row in 0..4 {
        for col in 0..4 {
            let w = bernstein(row, v) * bernstein(col, u);
            let p = pts_vec(&points[row.saturating_mul(4).saturating_add(col)]);
            for k in 0..dim {
                out[k] += w * p[k];
            }
        }
    }
    out
}

/// A shading point as a (x, y, colours) vector.
fn pts_vec(sp: &selis_raster::shading::ShadingPoint) -> Vec<f64> {
    let mut v = Vec::new();
    v.push(sp.point.x);
    v.push(sp.point.y);
    v.extend_from_slice(&sp.components);
    v
}

/// Tessellate a patch's control points into a `(grid+1)²` mesh of shading
/// points (Coons or tensor).
#[allow(clippy::arithmetic_side_effects)]
fn tessellate_patch(
    patch: &[selis_raster::shading::ShadingPoint],
    is_tensor: bool,
    grid: usize,
) -> Vec<selis_raster::shading::ShadingPoint> {
    use selis_geom::Point;
    use selis_raster::shading::ShadingPoint;
    let g = grid.max(1);
    // Grid is a caller-bounded constant (8); grows incrementally.
    let mut out = Vec::new();
    for j in 0..=g {
        for i in 0..=g {
            let u = i as f64 / g as f64;
            let v = j as f64 / g as f64;
            let s = if is_tensor {
                tensor_patch(patch, u, v)
            } else {
                coons_patch(patch, u, v)
            };
            let point = Point::new(
                s.get(0).copied().unwrap_or(0.0),
                s.get(1).copied().unwrap_or(0.0),
            );
            let components = s.get(2..).map(|c| c.to_vec()).unwrap_or_default();
            out.push(ShadingPoint { point, components });
        }
    }
    out
}

/// Decode a type-6 (Coons) or type-7 (tensor-product) patch shading's control
/// points from its packed bit stream. Only standalone patches (flag 0) are
/// decoded; patch-reuse flags (1–2) are a refinement.
fn parse_patch_shading(
    data: &[u8],
    bpc: u32,
    bpc_color: u32,
    bpf: u32,
    decode: &[f64],
    components: usize,
    is_tensor: bool,
) -> Option<Vec<Vec<selis_raster::shading::ShadingPoint>>> {
    use selis_geom::Point;
    use selis_raster::shading::ShadingPoint;
    let per_patch = if is_tensor { 16 } else { 12 };
    let mut r = BitReader::new(data);
    let mut patches: Vec<Vec<ShadingPoint>> = Vec::new();
    loop {
        let Some(flag) = r.read(bpf.max(1)) else {
            break;
        };
        if flag != 0 {
            break; // patch-reuse flags are a refinement; skip the shading
        }
        // per_patch is a spec constant (≤ 16 for types 6/7).
        let mut points = Vec::new();
        for _ in 0..per_patch {
            let Some(x_raw) = r.read(bpc.max(1)) else {
                break;
            };
            let Some(y_raw) = r.read(bpc.max(1)) else {
                break;
            };
            let (x, y) = (
                decode_raw(
                    x_raw,
                    bpc,
                    decode.get(0).copied().unwrap_or(0.0),
                    decode.get(1).copied().unwrap_or(0.0),
                ),
                decode_raw(
                    y_raw,
                    bpc,
                    decode.get(2).copied().unwrap_or(0.0),
                    decode.get(3).copied().unwrap_or(0.0),
                ),
            );
            let mut comps = Vec::new();
            let mut ok = true;
            for c in 0..components {
                let Some(raw) = r.read(bpc_color.max(1)) else {
                    ok = false;
                    break;
                };
                let lo = decode
                    .get(c.saturating_mul(2).saturating_add(4))
                    .copied()
                    .unwrap_or(0.0);
                let hi = decode
                    .get(c.saturating_mul(2).saturating_add(5))
                    .copied()
                    .unwrap_or(1.0);
                comps.push(decode_raw(raw, bpc_color.max(1), lo, hi));
            }
            if !ok {
                break;
            }
            points.push(ShadingPoint {
                point: Point::new(x, y),
                components: comps,
            });
        }
        if points.len() != per_patch {
            break;
        }
        patches.push(points);
    }
    if patches.is_empty() {
        None
    } else {
        Some(patches)
    }
}

/// Barycentric weights of `p` inside triangle `a b c`, or `None` if outside.
fn barycentric_weights(
    a: selis_geom::Point,
    b: selis_geom::Point,
    c: selis_geom::Point,
    p: selis_geom::Point,
) -> Option<(f64, f64, f64)> {
    let den = (b.y - c.y) * (a.x - c.x) + (c.x - b.x) * (a.y - c.y);
    if den.abs() < 1e-12 {
        return None;
    }
    let u = ((b.y - c.y) * (p.x - c.x) + (c.x - b.x) * (p.y - c.y)) / den;
    let v = ((c.y - a.y) * (p.x - c.x) + (a.x - c.x) * (p.y - c.y)) / den;
    let w = 1.0 - u - v;
    if u >= -1e-9 && v >= -1e-9 && w >= -1e-9 {
        Some((u, v, w))
    } else {
        None
    }
}

/// Resolve a `/ExtGState` resource name to its dictionary as `Operand` pairs,
/// for the content interpreter's `gs` operator. Unknown values are skipped so
/// a hostile ExtGState never aborts the page.
fn resolve_ext_gstate_inner(
    resolver: &mut Resolver<'_>,
    resources: Option<&Obj>,
    name: &Bytes,
    g: &mut BudgetGuard<'_>,
) -> Result<Option<Vec<(Bytes, Operand)>>> {
    let Some(resources) = resources else {
        return Ok(None);
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
        Obj::Ref(r) => Some(Bytes::copy_from_slice(
            format!("{} {}", r.num, r.gen).as_bytes(),
        )),
        _ => None,
    }
}

/// Convert a resolved object to a content `Operand` (for ExtGState merging).
fn obj_to_operand(obj: &Obj) -> Option<Operand> {
    match obj {
        Obj::Int(v) => Some(Operand::Num(*v as f64)),
        Obj::Real { scaled, scale } => {
            Some(Operand::Num(*scaled as f64 / 10f64.powi(*scale as i32)))
        }
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
    use selis_pdf_cos::doc_writer::DocumentBuilder;
    use selis_pdf_cos::Obj;
    use selis_sandbox::FixedClock;

    /// SL-2.CONF.05: a page with no `/MediaBox`, or a zero-area one, renders
    /// at the ISO 32000-2 §7.10.1 default (612 × 792) instead of refusing —
    /// the mainstream-viewer fallback MuPDF applies to the affected corpus
    /// files.
    #[test]
    fn page_size_falls_back_to_the_iso_default_for_missing_or_degenerate_media_box() {
        let budget = Budget::profile(selis_sandbox::Surface::Viewer);
        let clock = FixedClock(0);
        for (case, zero_box) in [("missing", false), ("zero-area", true)] {
            let mut builder = DocumentBuilder::new();
            let page = builder.add_page(200.0, 300.0, &[]);
            if let Some((_, obj)) = builder
                .objects_mut()
                .iter_mut()
                .find(|(num, _)| *num == page.num)
            {
                *obj = if zero_box {
                    Obj::Dict(vec![(
                        b"MediaBox".to_vec().into(),
                        Obj::Array(
                            (0..4)
                                .map(|_| Obj::Real {
                                    scaled: 0,
                                    scale: 0,
                                })
                                .collect(),
                        ),
                    )])
                } else if let Obj::Dict(pairs) = obj {
                    Obj::Dict(
                        pairs
                            .iter()
                            .filter(|(k, _)| k.as_slice() != b"MediaBox")
                            .cloned()
                            .collect(),
                    )
                } else {
                    obj.clone()
                };
            }
            let mut g = budget.guard_with(&clock, CancelToken::new());
            let bytes = builder.write(&budget, &mut g).expect("write");
            let session = Session::open(bytes, &budget, &clock).expect("open");
            assert_eq!(
                session.page_size(0),
                Some((612.0, 792.0)),
                "{case}: must fall back to the ISO default"
            );
        }
    }

    #[test]
    fn session_opens_and_renders_a_minimal_pdf() {
        let src = include_bytes!("fixtures/minimal.pdf");
        let budget = Budget::profile(selis_sandbox::Surface::Viewer);
        let session = Session::open(src.to_vec(), &budget, &FixedClock(0)).expect("open");
        assert_eq!(session.len(), 1);
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        session
            .render_page(0, &mut backend, Matrix::IDENTITY, &budget, &mut g)
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
        let session = Session::open(src.to_vec(), &budget, &FixedClock(0)).expect("open");
        assert_eq!(session.len(), 1);
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let mut backend = TinySkiaBackend::new(2, 2).expect("pixmap");
        session
            .render_page(0, &mut backend, Matrix::IDENTITY, &budget, &mut g)
            .expect("render");
        let data = backend.pixmap().data();
        // The top-left pixel is white (255 gray); the rest are black.
        assert_eq!(&data[0..3], &[255, 255, 255]);
        assert_eq!(&data[8..11], &[0, 0, 0]);
    }

    /// The same image, but with `/Resources` as an indirect reference (as
    /// `selis`'s own writer emits): the dictionary must still resolve.
    #[test]
    fn session_renders_an_image_xobject_with_indirect_resources() {
        let src = include_bytes!("fixtures/image-indirect-resources.pdf");
        let budget = Budget::profile(selis_sandbox::Surface::Viewer);
        let session = Session::open(src.to_vec(), &budget, &FixedClock(0)).expect("open");
        assert_eq!(session.len(), 1);
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let mut backend = TinySkiaBackend::new(2, 2).expect("pixmap");
        session
            .render_page(0, &mut backend, Matrix::IDENTITY, &budget, &mut g)
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
        let session = Session::open(src.to_vec(), &budget, &FixedClock(0)).expect("open");
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let mut backend = TinySkiaBackend::new(2000, 200).expect("pixmap");
        session
            .render_page(0, &mut backend, Matrix::IDENTITY, &budget, &mut g)
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
        let session = Session::open(src.to_vec(), &budget, &FixedClock(0)).expect("open");
        assert_eq!(session.len(), 1);
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let mut backend = TinySkiaBackend::new(100, 100).expect("pixmap");
        session
            .render_page(0, &mut backend, Matrix::IDENTITY, &budget, &mut g)
            .expect("render");
        let data = backend.pixmap().data();
        // The form's red square fills the page: the centre pixel is red.
        let idx = (50 * 100 + 50) * 4;
        assert_eq!(&data[idx..idx + 3], &[255, 0, 0]);
    }

    /// A stream `/Filter` array decodes in reverse (the last-named filter was
    /// applied first on encode): `ASCIIHex(Flate(orig))` → `orig`.
    #[test]
    fn unfilter_handles_filter_arrays_in_reverse() {
        let budget = Budget::unlimited();
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let original = b"Hello filter chain!";
        // Encode: ASCIIHex, then Flate.
        let hex: Vec<u8> = original
            .iter()
            .flat_map(|b| format!("{b:02x}").into_bytes())
            .collect();
        let flated = miniz_oxide::deflate::compress_to_vec_zlib(&hex, 6);
        let dict = vec![(
            Bytes::copy_from_slice(b"Filter"),
            Obj::Array(vec![
                Obj::Name(Bytes::copy_from_slice(b"ASCIIHexDecode")),
                Obj::Name(Bytes::copy_from_slice(b"FlateDecode")),
            ]),
        )];
        let decoded = unfilter_stream_data(&dict, &flated, &mut g);
        assert_eq!(decoded, original);
    }

    /// `/DecodeParms` (a dict) parses the predictor settings.
    #[test]
    fn decode_parms_parse_predictor() {
        let pairs = vec![
            (Bytes::copy_from_slice(b"Predictor"), Obj::Int(12)),
            (Bytes::copy_from_slice(b"Columns"), Obj::Int(3)),
            (Bytes::copy_from_slice(b"Colors"), Obj::Int(3)),
            (Bytes::copy_from_slice(b"BitsPerComponent"), Obj::Int(8)),
        ];
        let obj = Obj::Dict(pairs);
        let parms = decode_parms_from_obj(Some(&obj));
        assert_eq!(parms.len(), 1);
        assert_eq!(parms[0].predictor, 12);
        assert_eq!(parms[0].columns, 3);
        assert_eq!(parms[0].colors, 3);
        assert_eq!(parms[0].bits_per_component, 8);
        // An array aligns with the filter chain.
        let arr = Obj::Array(vec![Obj::Dict(vec![(
            Bytes::copy_from_slice(b"Predictor"),
            Obj::Int(2),
        )])]);
        let parms = decode_parms_from_obj(Some(&arr));
        assert_eq!(parms.len(), 1);
        assert_eq!(parms[0].predictor, 2);
    }

    /// A type-4 Gouraud mesh with one triangle decodes from the packed bit
    /// stream: flag 0, then three (x, y, rgb) vertices.
    #[test]
    fn gouraud_mesh_decodes_a_single_triangle() {
        let data = [0x12, 0x49];
        let decode = [
            0.0, 1.0, 0.0, 1.0, // x, y ranges
            0.0, 1.0, 0.0, 1.0, 0.0, 1.0, // rgb ranges
        ];
        let mesh = parse_gouraud_shading(&data, 1, 1, 1, &decode, 3).expect("mesh");
        assert_eq!(mesh.triangles.len(), 1);
        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.vertices[0].point, selis_geom::Point::new(0.0, 0.0));
        assert_eq!(mesh.vertices[0].components, vec![1.0, 0.0, 0.0]);
        assert_eq!(mesh.vertices[1].point, selis_geom::Point::new(1.0, 0.0));
        assert_eq!(mesh.vertices[1].components, vec![0.0, 1.0, 0.0]);
        assert_eq!(mesh.vertices[2].point, selis_geom::Point::new(0.0, 1.0));
        assert_eq!(mesh.vertices[2].components, vec![0.0, 0.0, 1.0]);
    }

    /// Barycentric weights are positive inside a triangle and negative/`None`
    /// outside.
    #[test]
    fn barycentric_weights_identify_inside_points() {
        let a = selis_geom::Point::new(0.0, 0.0);
        let b = selis_geom::Point::new(1.0, 0.0);
        let c = selis_geom::Point::new(0.0, 1.0);
        assert!(barycentric_weights(a, b, c, selis_geom::Point::new(0.25, 0.25)).is_some());
        assert!(barycentric_weights(a, b, c, selis_geom::Point::new(1.0, 1.0)).is_none());
    }

    /// A type-5 lattice mesh (2 columns) decodes its grid and triangulates
    /// each 2×2 cell into two triangles.
    #[test]
    fn lattice_mesh_decodes_a_grid() {
        let data = [0x24, 0x93, 0xF0];
        let decode = [
            0.0, 1.0, 0.0, 1.0, // x, y ranges
            0.0, 1.0, 0.0, 1.0, 0.0, 1.0, // rgb ranges
        ];
        let lattice = parse_lattice_shading(&data, 1, 1, &decode, 3, 2).expect("lattice");
        assert_eq!(lattice.vertices.len(), 4);
        assert_eq!(lattice.cols, 2);
        assert_eq!(lattice.vertices[0].point, selis_geom::Point::new(0.0, 0.0));
        assert_eq!(lattice.vertices[3].point, selis_geom::Point::new(1.0, 1.0));
        let tris = lattice_triangles(&lattice);
        assert_eq!(tris.len(), 2);
        assert_eq!(tris[0], (0, 1, 2));
        assert_eq!(tris[1], (2, 1, 3));
    }

    /// A flat Coons patch (all 12 control points identical) tessellates to a
    /// uniform mesh: every vertex carries the same position and colour.
    #[test]
    fn coons_patch_tessellates_a_flat_surface() {
        use selis_raster::shading::ShadingPoint;
        let flat = ShadingPoint {
            point: selis_geom::Point::new(0.5, 0.5),
            components: vec![1.0, 0.0, 0.0],
        };
        let patch = vec![flat.clone(); 12];
        let mesh = tessellate_patch(&patch, false, 3);
        assert_eq!(mesh.len(), 16); // (3+1)²
        for sp in &mesh {
            assert!((sp.point.x - 0.5).abs() < 1e-9, "x preserved");
            assert!((sp.point.y - 0.5).abs() < 1e-9, "y preserved");
            assert!((sp.components[0] - 1.0).abs() < 1e-9, "red preserved");
        }
    }
}
