//! Full-document writer (SL-1A.WRITE.01).
//!
//! Assembles a complete, valid PDF: header, catalog, page tree, page objects,
//! content streams, a classic xref table, and a trailer — readable by `selis`
//! itself and by any conforming reader. Text uses the standard-14 base fonts
//! (rendered by the Liberation fallback); images can be embedded as XObjects.

use selis_error::{err, Code, Result};
use selis_sandbox::{Budget, BudgetGuard};

use crate::obj::{Obj, Ref};
use crate::writer::Writer;

/// A PDF document under construction.
#[derive(Debug, Default)]
pub struct DocumentBuilder {
    /// (object number, object), in write order.
    objects: Vec<(u32, Obj)>,
    /// The next object number to allocate.
    next_num: u32,
    /// The leaf page references, in order.
    page_refs: Vec<Ref>,
    /// `/Info` fields (e.g. `Title`, `Author`).
    info: Vec<(Vec<u8>, Obj)>,
    /// Extra catalog entries appended at write time (SL-1A.WRITE.04:
    /// `/Outlines`, `/Names`, `/PageLabels`, …).
    catalog_extra: Vec<(Vec<u8>, Obj)>,
    /// The trailer `/ID` pair, when set.
    id: Option<(Vec<u8>, Vec<u8>)>,
}

impl DocumentBuilder {
    /// A new document: object 1 = catalog, object 2 = pages tree root.
    #[must_use]
    pub fn new() -> Self {
        let mut b = Self {
            objects: Vec::new(),
            next_num: 1,
            page_refs: Vec::new(),
            info: Vec::new(),
            catalog_extra: Vec::new(),
            id: None,
        };
        b.objects.push((
            1,
            dict(&[
                (b"Type".to_vec(), Obj::Name(bytes(b"Catalog"))),
                (b"Pages".to_vec(), Obj::Ref(Ref::new(2, 0))),
                (b"Version".to_vec(), Obj::Name(bytes(b"1.4"))),
            ]),
        ));
        b.objects.push((
            2,
            dict(&[
                (b"Type".to_vec(), Obj::Name(bytes(b"Pages"))),
                (b"Kids".to_vec(), Obj::Array(Vec::new())),
                (b"Count".to_vec(), Obj::Int(0)),
            ]),
        ));
        b.next_num = 3;
        b
    }

    /// Mutable access to the collected objects. Used by the reconciliation
    /// pass (SL-1A.WRITE.04) to rewrite copied structures in place — e.g.
    /// re-pointing outline `/Parent` entries at the merged outline root.
    ///
    /// Callers must not remove objects or introduce duplicate numbers; doing so
    /// corrupts the xref written by [`write`](DocumentBuilder::write).
    pub fn objects_mut(&mut self) -> &mut Vec<(u32, Obj)> {
        &mut self.objects
    }

    /// Add (or replace) a catalog entry appended at write time.
    ///
    /// # Budget
    ///
    /// No charge: the value is buffered and charged by
    /// [`write`](DocumentBuilder::write).
    ///
    /// # Malformed Input
    ///
    /// None: `key` and `value` are caller-provided, written verbatim.
    pub fn add_catalog_entry(&mut self, key: &[u8], value: Obj) {
        self.catalog_extra.retain(|(k, _)| k.as_slice() != key);
        self.catalog_extra.push((key.to_vec(), value));
    }

    /// Set the trailer `/ID` pair (two byte strings; conventionally the file
    /// identifier at creation time and its value at the last modification).
    pub fn set_id(&mut self, first: Vec<u8>, second: Vec<u8>) {
        self.id = Some((first, second));
    }

    /// Set an `/Info` field (e.g. `Title`, `Author`).
    ///
    /// # Budget
    ///
    /// No charge: the value is buffered and charged by
    /// [`write`](DocumentBuilder::write).
    ///
    /// # Malformed Input
    ///
    /// None: `key` and `value` are caller-provided metadata, written verbatim.
    pub fn set_info(&mut self, key: &[u8], value: &str) {
        self.info
            .push((key.to_vec(), Obj::String(bytes(value.as_bytes()))));
    }

    /// Add a page with a content stream. Returns the page's object reference.
    ///
    /// # Budget
    ///
    /// No charge: the content bytes are buffered and charged by
    /// [`write`](DocumentBuilder::write).
    ///
    /// # Malformed Input
    ///
    /// None: `content` is a caller-generated content stream, embedded as-is.
    pub fn add_page(&mut self, width: f64, height: f64, content: &[u8]) -> Ref {
        let content_num = self.allocate();
        self.objects.push((
            content_num,
            Obj::Stream {
                dict: vec![(
                    bytes(b"Length"),
                    Obj::Int(i64::try_from(content.len()).unwrap_or(i64::MAX)),
                )],
                data: bytes(content),
            },
        ));
        let page_num = self.allocate();
        self.objects.push((
            page_num,
            dict(&[
                (b"Type".to_vec(), Obj::Name(bytes(b"Page"))),
                (b"Parent".to_vec(), Obj::Ref(Ref::new(2, 0))),
                (
                    b"MediaBox".to_vec(),
                    Obj::Array(vec![
                        Obj::Real {
                            scaled: 0,
                            scale: 0,
                        },
                        Obj::Real {
                            scaled: 0,
                            scale: 0,
                        },
                        real(width),
                        real(height),
                    ]),
                ),
                (b"Contents".to_vec(), Obj::Ref(Ref::new(content_num, 0))),
                (b"Resources".to_vec(), Obj::Dict(Vec::new())),
            ]),
        ));
        let page_ref = Ref::new(page_num, 0);
        self.page_refs.push(page_ref);
        self.update_pages_tree();
        page_ref
    }

    /// Add a pre-renumbered object to the document (used by Merge/Split when
    /// copying object graphs).
    pub fn add_object(&mut self, num: u32, obj: Obj) {
        self.next_num = self.next_num.max(num.saturating_add(1));
        self.objects.push((num, obj));
    }

    /// The next-object-number counter (kept in sync by
    /// [`add_object`](DocumentBuilder::add_object) and
    /// [`allocate`](DocumentBuilder::allocate)); passed to the object-graph
    /// copier so renumbering never collides.
    pub fn next_num_mut(&mut self) -> &mut u32 {
        &mut self.next_num
    }

    /// Add a page referencing existing content-stream objects (used by
    /// Merge/Split after copying the content graphs).
    pub fn add_page_with(
        &mut self,
        width: f64,
        height: f64,
        content_refs: &[Ref],
        resources: Option<Ref>,
    ) -> Ref {
        self.add_page_with_extra(width, height, content_refs, resources, Vec::new())
    }

    /// Add a page referencing existing content-stream objects, with extra
    /// page-dictionary entries appended after the standard keys (used by the
    /// page operations to carry `/Rotate`).
    pub fn add_page_with_extra(
        &mut self,
        width: f64,
        height: f64,
        content_refs: &[Ref],
        resources: Option<Ref>,
        extra: Vec<(Vec<u8>, Obj)>,
    ) -> Ref {
        let contents = if content_refs.len() == 1 {
            match content_refs.first() {
                Some(r) => Obj::Ref(*r),
                None => Obj::Null,
            }
        } else {
            Obj::Array(content_refs.iter().map(|r| Obj::Ref(*r)).collect())
        };
        let mut pairs: Vec<(Vec<u8>, Obj)> = vec![
            (b"Type".to_vec(), Obj::Name(bytes(b"Page"))),
            (b"Parent".to_vec(), Obj::Ref(Ref::new(2, 0))),
            (
                b"MediaBox".to_vec(),
                Obj::Array(vec![
                    Obj::Real {
                        scaled: 0,
                        scale: 0,
                    },
                    Obj::Real {
                        scaled: 0,
                        scale: 0,
                    },
                    real(width),
                    real(height),
                ]),
            ),
            (b"Contents".to_vec(), contents),
        ];
        if let Some(r) = resources {
            pairs.push((b"Resources".to_vec(), Obj::Ref(r)));
        }
        pairs.extend(extra);
        let page_num = self.allocate();
        self.objects.push((page_num, dict(&pairs)));
        let page_ref = Ref::new(page_num, 0);
        self.page_refs.push(page_ref);
        self.update_pages_tree();
        page_ref
    }

    /// Deduplicate byte-identical objects already collected in the builder
    /// (WRITE.03: identical resources across merged inputs collapse to one).
    /// Page refs are remapped at the survivors and the pages tree is rebuilt,
    /// so subsequent [`write`](DocumentBuilder::write) output is consistent.
    /// Returns the number of objects removed.
    ///
    /// # Budget
    ///
    /// Charges each serialisation pass; see
    /// [`dedup_objects`](crate::copy::dedup_objects).
    ///
    /// # Malformed Input
    ///
    /// None: operates on builder-owned objects only.
    pub fn dedup(&mut self, budget: &Budget, g: &mut BudgetGuard<'_>) -> usize {
        let objects = std::mem::take(&mut self.objects);
        let (survivors, remap, removed) = crate::copy::dedup_objects(objects, budget, g);
        self.objects = survivors;
        if removed > 0 {
            for r in &mut self.page_refs {
                r.num = remap.get(&r.num).copied().unwrap_or(r.num);
            }
            self.update_pages_tree();
        }
        removed
    }

    /// Serialise the whole document to bytes.
    ///
    /// # Budget
    ///
    /// Charges the output bytes and per-object writes.
    ///
    /// # Malformed Input
    ///
    /// `BUDGET_BYTES` on exhaustion.
    pub fn write(&mut self, budget: &Budget, g: &mut BudgetGuard<'_>) -> Result<Vec<u8>> {
        // The trailer needs the info object number (if any) and the final
        // object count.
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n");
        g.charge(
            selis_sandbox::Resource::Bytes,
            u64::try_from(out.len()).unwrap_or(u64::MAX),
        )?;

        // Info dictionary (if set) is appended before the xref table and the
        // catalog references it.
        if !self.info.is_empty() {
            let info_num = self.allocate();
            let info = Obj::Dict(
                self.info
                    .iter()
                    .map(|(k, v)| (bytes(k), v.clone()))
                    .collect(),
            );
            self.objects.push((info_num, info));
            // Point the catalog's /Info at it.
            for (num, obj) in &mut self.objects {
                if *num == 1 {
                    if let Obj::Dict(pairs) = obj {
                        pairs.push((bytes(b"Info"), Obj::Ref(Ref::new(info_num, 0))));
                    }
                }
            }
        }

        // Extra catalog entries collected by reconciliation (WRITE.04).
        if !self.catalog_extra.is_empty() {
            let extras = self
                .catalog_extra
                .iter()
                .map(|(k, v)| (bytes(k), v.clone()))
                .collect::<Vec<_>>();
            for (num, obj) in &mut self.objects {
                if *num == 1 {
                    if let Obj::Dict(pairs) = obj {
                        pairs.extend(extras.clone());
                    }
                }
            }
        }

        // Write objects sequentially into `out`, recording absolute offsets.
        let mut offsets: Vec<Option<u64>> = Vec::new();
        for (num, obj) in &self.objects {
            let num_us = usize::try_from(*num).unwrap_or(usize::MAX);
            while offsets.len() <= num_us {
                offsets.push(None);
            }
            if let Some(slot) = offsets.get_mut(*num as usize) {
                *slot = Some(u64::try_from(out.len()).unwrap_or(u64::MAX));
            }
            out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            let mut w = Writer::new(budget);
            out.extend_from_slice(&w.to_bytes(obj, g)?);
            out.push(b'\n');
            out.extend_from_slice(b"endobj\n");
        }

        // Classic xref table. Object numbers with no written object (e.g.
        // removed by dedup) become free entries — an in-use entry pointing at
        // offset 0 would be malformed.
        let size = u64::try_from(offsets.len()).unwrap_or(u64::MAX);
        let startxref = u64::try_from(out.len()).unwrap_or(u64::MAX);
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        for (i, off) in offsets.iter().enumerate() {
            match off {
                Some(off) if i > 0 => {
                    out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
                }
                _ => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(b"trailer\n");
        let mut trailer_pairs: Vec<(Vec<u8>, Obj)> = vec![
            (
                b"Size".to_vec(),
                Obj::Int(i64::try_from(size).unwrap_or(i64::MAX)),
            ),
            (b"Root".to_vec(), Obj::Ref(Ref::new(1, 0))),
        ];
        if let Some((first, second)) = &self.id {
            trailer_pairs.push((
                b"ID".to_vec(),
                Obj::Array(vec![Obj::String(bytes(first)), Obj::String(bytes(second))]),
            ));
        }
        let trailer = Obj::Dict(
            trailer_pairs
                .into_iter()
                .map(|(k, v)| (bytes(&k), v))
                .collect(),
        );
        let mut tw = Writer::new(budget);
        out.extend_from_slice(&tw.to_bytes(&trailer, g)?);
        out.push(b'\n');
        out.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());

        g.charge(
            selis_sandbox::Resource::Bytes,
            u64::try_from(out.len()).unwrap_or(u64::MAX),
        )?;
        Ok(out)
    }

    /// Allocate the next object number, advancing the counter.
    pub fn allocate(&mut self) -> u32 {
        let n = self.next_num;
        self.next_num = self.next_num.saturating_add(1);
        n
    }

    fn update_pages_tree(&mut self) {
        let kids: Vec<Obj> = self.page_refs.iter().map(|r| Obj::Ref(*r)).collect();
        let count = Obj::Int(i64::try_from(self.page_refs.len()).unwrap_or(i64::MAX));
        // Replace object 2's Kids/Count.
        for (num, obj) in &mut self.objects {
            if *num == 2 {
                *obj = dict(&[
                    (b"Type".to_vec(), Obj::Name(bytes(b"Pages"))),
                    (b"Kids".to_vec(), Obj::Array(kids.clone())),
                    (b"Count".to_vec(), count.clone()),
                ]);
            }
        }
    }
}

/// A content-stream builder producing PDF graphics operators.
#[derive(Debug, Default, Clone)]
pub struct ContentBuilder {
    out: Vec<u8>,
}

impl ContentBuilder {
    /// A new empty content stream.
    #[must_use]
    pub fn new() -> Self {
        Self { out: Vec::new() }
    }

    /// Set the non-stroking fill colour (device RGB, 0..1).
    pub fn set_fill(&mut self, r: f64, g: f64, b: f64) -> &mut Self {
        self.push_fmt(format!("{} {} {} rg\n", trim(r), trim(g), trim(b)));
        self
    }

    /// Fill a rectangle (PDF `re f`).
    pub fn fill_rect(&mut self, x: f64, y: f64, w: f64, h: f64) -> &mut Self {
        self.push_fmt(format!(
            "{} {} {} {} re f\n",
            trim(x),
            trim(y),
            trim(w),
            trim(h)
        ));
        self
    }

    /// Begin a text object.
    pub fn begin_text(&mut self) -> &mut Self {
        self.push(b"BT\n");
        self
    }

    /// End a text object.
    pub fn end_text(&mut self) -> &mut Self {
        self.push(b"ET\n");
        self
    }

    /// Select the font (a standard-14 name) and size.
    pub fn set_font(&mut self, name: &str, size: f64) -> &mut Self {
        self.push_fmt(format!("/{name} {} Tf\n", trim(size)));
        self
    }

    /// Move the text origin (user space), relative to the current line start
    /// (PDF `Td`). Repeated calls accumulate; for absolute positioning use
    /// [`set_text_matrix`](Self::set_text_matrix).
    pub fn text_at(&mut self, x: f64, y: f64) -> &mut Self {
        self.push_fmt(format!("{} {} Td\n", trim(x), trim(y)));
        self
    }

    /// Set the text matrix to an absolute position (PDF `Tm` with an identity
    /// rotation/scale). Each call places the text origin at exactly `(x, y)`
    /// in user space, independent of any earlier text positioning.
    pub fn set_text_matrix(&mut self, x: f64, y: f64) -> &mut Self {
        self.push_fmt(format!("1 0 0 1 {} {} Tm\n", trim(x), trim(y)));
        self
    }

    /// Show a text string (parentheses-escaped).
    pub fn show_text(&mut self, text: &str) -> &mut Self {
        self.show_raw(text.as_bytes())
    }

    /// Show raw bytes as a parentheses-escaped PDF string literal.
    ///
    /// Used for pre-encoded text (e.g. WinAnsiEncoding bytes produced by the
    /// conversion layout engine), where the bytes are not necessarily valid
    /// UTF-8.
    ///
    /// # Budget
    ///
    /// No charge: the bytes are appended to the content stream and charged by
    /// [`write`](DocumentBuilder::write).
    ///
    /// # Malformed Input
    ///
    /// None: the bytes are escaped verbatim, never parsed.
    pub fn show_raw(&mut self, text: &[u8]) -> &mut Self {
        self.push(b"(");
        for &b in text {
            match b {
                b'(' => self.push(b"\\("),
                b')' => self.push(b"\\)"),
                b'\\' => self.push(b"\\\\"),
                _ => self.out.push(b),
            }
        }
        self.push(b") Tj\n");
        self
    }

    /// Draw an image XObject (scaled to the unit square under the current CTM).
    pub fn draw_image(&mut self, name: &str) -> &mut Self {
        self.push_fmt(format!("/{name} Do\n"));
        self
    }

    /// The content bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.out.clone()
    }

    fn push(&mut self, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
    }

    fn push_fmt(&mut self, s: String) {
        self.out.extend_from_slice(s.as_bytes());
    }
}

/// Embed an RGBA image as an XObject and draw it. Returns the image object
/// reference (call before writing the document).
pub fn embed_image_rgba(
    builder: &mut DocumentBuilder,
    width: u32,
    height: u32,
    rgba8: &[u8],
) -> Ref {
    // Convert RGBA → RGB (PDF DeviceRGB). Grows incrementally over the
    // already-resident source buffer; the writer carries no budget guard.
    let mut rgb = Vec::new();
    for px in rgba8.chunks(4) {
        rgb.extend_from_slice(px.get(..3).unwrap_or(&[]));
    }
    let num = builder.allocate();
    let img = Obj::Stream {
        dict: vec![
            (bytes(b"Type"), Obj::Name(bytes(b"XObject"))),
            (bytes(b"Subtype"), Obj::Name(bytes(b"Image"))),
            (bytes(b"Width"), Obj::Int(i64::from(width))),
            (bytes(b"Height"), Obj::Int(i64::from(height))),
            (bytes(b"ColorSpace"), Obj::Name(bytes(b"DeviceRGB"))),
            (bytes(b"BitsPerComponent"), Obj::Int(8)),
            (
                bytes(b"Length"),
                Obj::Int(i64::try_from(rgb.len()).unwrap_or(i64::MAX)),
            ),
        ],
        data: bytes(&rgb),
    };
    builder.objects.push((num, img));
    Ref::new(num, 0)
}

/// Append an incremental update to an existing PDF (SL-1A.WRITE.02).
///
/// The original bytes are preserved verbatim as a prefix; the replacement
/// objects and a new classic xref table + trailer are appended, with `/Prev`
/// pointing back at the previous revision's `startxref` (ISO 32000-1 §7.5.6).
/// This is the pattern Adobe, Aspose, and Foxit emit for small edits: only the
/// changed objects are rewritten, everything else stays byte-identical, so a
/// small change to a large file costs bytes proportional to the edit, and
/// digital signatures over prior revisions stay valid (the bytes they sign are
/// untouched).
///
/// `new_objects` is the set of objects to (re)define in this revision, keyed
/// by object number (numbers must be unique; gaps are allowed and emitted as
/// separate xref subsections). `trailer` carries the new revision's trailer
/// entries — typically `/Root` and `/ID` carried over from the previous
/// revision; `/Size` and `/Prev` are filled in here.
///
/// # Budget
///
/// Charges the original bytes, the appended output bytes, and per-object
/// writes.
///
/// # Malformed Input
///
/// `OBJ_UNEXPECTED` on a duplicate object number or an original with no
/// `startxref`.
pub fn write_incremental_update(
    original: &[u8],
    new_objects: &[(u32, Obj)],
    trailer: &[(Vec<u8>, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    // The previous revision's xref offset — the value on its `startxref` line.
    let startxref_kw = crate::xref::find_startxref(original, 4096).ok_or_else(|| {
        err!(
            Code::ObjUnexpected,
            during = "incr-write",
            detail = "original has no startxref"
        )
    })?;
    let kw = usize::try_from(startxref_kw).unwrap_or(usize::MAX);
    let (prev, _) = crate::xref::read_startxref_value(
        original,
        kw.saturating_add("startxref".len()),
    )?;

    // `/Size` is the highest object number across all revisions + 1: the
    // previous revision's declared size, raised by any newly-allocated object.
    let original_size = {
        let doc = crate::parse_revisions(original, startxref_kw, budget, g)?;
        doc.revisions()
            .last()
            .and_then(|r| {
                r.trailer
                    .iter()
                    .find(|(k, _)| k.as_slice() == b"Size")
                    .and_then(|(_, v)| match v {
                        Obj::Int(n) => Some(u64::try_from(*n).unwrap_or(u64::MAX)),
                        _ => None,
                    })
            })
            .unwrap_or(0)
    };
    let mut max_new = 0u64;
    let mut sorted: Vec<(u32, &Obj)> = new_objects.iter().map(|(n, o)| (*n, o)).collect();
    sorted.sort_by_key(|(n, _)| *n);
    for (i, (num, _)) in sorted.iter().enumerate() {
        if i > 0 && sorted.get(i.saturating_sub(1)).map(|(n, _)| *n) == Some(*num) {
            return Err(err!(
                Code::ObjUnexpected,
                during = "incr-write",
                object = *num,
                detail = "duplicate object number"
            ));
        }
        max_new = max_new.max(u64::from(*num));
    }
    let size = original_size.max(max_new.saturating_add(1));

    let mut out = Vec::new();
    out.extend_from_slice(original);
    g.charge(
        selis_sandbox::Resource::Bytes,
        u64::try_from(out.len()).unwrap_or(u64::MAX),
    )?;

    // Append each replacement object, recording its absolute offset.
    let mut offsets: Vec<(u32, u64)> = Vec::with_capacity(sorted.len());
    for (num, obj) in &sorted {
        let off = u64::try_from(out.len()).unwrap_or(u64::MAX);
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        let mut w = Writer::new(budget);
        out.extend_from_slice(&w.to_bytes(obj, g)?);
        out.push(b'\n');
        out.extend_from_slice(b"endobj\n");
        offsets.push((*num, off));
    }

    // Classic xref table: one subsection per contiguous run of replacement
    // object numbers. Only the changed objects are declared; everything else
    // is inherited from the previous revision (ISO 32000-1 §7.5.8.3) — listing
    // them as free here would incorrectly free them.
    let startxref = u64::try_from(out.len()).unwrap_or(u64::MAX);
    out.extend_from_slice(b"xref\n");
    let mut i = 0usize;
    while i < offsets.len() {
        let run_start = i;
        let mut num = offsets.get(i).map(|(n, _)| *n).unwrap_or(0);
        while i < offsets.len() && offsets.get(i).map(|(n, _)| *n) == Some(num) {
            i = i.saturating_add(1);
            num = num.saturating_add(1);
        }
        let run = offsets
            .get(run_start..i)
            .unwrap_or(offsets.get(run_start..run_start).unwrap_or(&[]));
        let first = run.first().map(|(n, _)| *n).unwrap_or(0);
        let count = u64::try_from(run.len()).unwrap_or(u64::MAX);
        out.extend_from_slice(format!("{first} {count}\n").as_bytes());
        for (_, off) in run {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
    }

    // Trailer: the caller's entries (Root, ID, …) plus /Size and /Prev.
    out.extend_from_slice(b"trailer\n");
    let mut pairs: Vec<(selis_bytes::Bytes, Obj)> = trailer
        .iter()
        .map(|(k, v)| (bytes(k), v.clone()))
        .collect();
    pairs.push((
        bytes(b"Size"),
        Obj::Int(i64::try_from(size).unwrap_or(i64::MAX)),
    ));
    pairs.push((
        bytes(b"Prev"),
        Obj::Int(i64::try_from(prev).unwrap_or(i64::MAX)),
    ));
    let trailer_obj = Obj::Dict(pairs);
    let mut tw = Writer::new(budget);
    out.extend_from_slice(&tw.to_bytes(&trailer_obj, g)?);
    out.push(b'\n');
    out.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());

    g.charge(
        selis_sandbox::Resource::Bytes,
        u64::try_from(out.len()).unwrap_or(u64::MAX),
    )?;
    Ok(out)
}

/// Write a complete single-revision PDF from pre-built objects (the output
/// path for optimisation/rewriting tools such as SL-1A.TOOL.07, where the
/// object graph already exists and only needs serialising with a valid xref).
///
/// `objects` may be sparse (gaps become free xref entries) but must not
/// contain duplicate object numbers; `root` is the trailer's `/Root`.
///
/// # Budget
///
/// Charges the serialised output bytes and one object unit per object.
///
/// # Malformed Input
///
/// `BUDGET_BYTES` on exhaustion; `OBJ_UNEXPECTED` on duplicate object numbers.
pub fn write_objects_as_document(
    objects: &[(u32, Obj)],
    root: Ref,
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    write_objects_as_document_with_trailer(objects, root, &[], budget, g)
}

/// As [`write_objects_as_document`], with extra trailer entries appended after
/// `/Size` and `/Root` (used by SL-1A.TOOL.04 to carry `/Info` and `/ID` into
/// a rewritten document).
///
/// # Budget
///
/// As [`write_objects_as_document`].
///
/// # Malformed Input
///
/// As [`write_objects_as_document`].
pub fn write_objects_as_document_with_trailer(
    objects: &[(u32, Obj)],
    root: Ref,
    extra_trailer: &[(Vec<u8>, Obj)],
    budget: &Budget,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let mut max_num = 0u32;
    for (num, _) in objects {
        max_num = max_num.max(*num);
    }
    let mut offsets: Vec<Option<u64>> =
        vec![None; usize::try_from(max_num).map_or(usize::MAX, |n| n.saturating_add(1))];
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n");
    g.charge(
        selis_sandbox::Resource::Bytes,
        u64::try_from(out.len()).unwrap_or(u64::MAX),
    )?;

    for (num, obj) in objects {
        let num_us = usize::try_from(*num).unwrap_or(usize::MAX);
        let slot = offsets.get_mut(num_us).ok_or_else(|| {
            err!(
                Code::ObjUnexpected,
                during = "doc-write",
                detail = "duplicate object number"
            )
        })?;
        if slot.is_some() {
            return Err(err!(
                Code::ObjUnexpected,
                during = "doc-write",
                object = *num,
                detail = "duplicate object number"
            ));
        }
        *slot = Some(u64::try_from(out.len()).unwrap_or(u64::MAX));
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        let mut w = Writer::new(budget);
        out.extend_from_slice(&w.to_bytes(obj, g)?);
        out.push(b'\n');
        out.extend_from_slice(b"endobj\n");
    }

    let size = u64::try_from(offsets.len()).unwrap_or(u64::MAX);
    let startxref = u64::try_from(out.len()).unwrap_or(u64::MAX);
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for (i, off) in offsets.iter().enumerate() {
        match off {
            Some(off) if i > 0 => {
                out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            }
            _ => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(b"trailer\n");
    let mut trailer_pairs: Vec<(Vec<u8>, Obj)> = vec![
        (
            b"Size".to_vec(),
            Obj::Int(i64::try_from(size).unwrap_or(i64::MAX)),
        ),
        (b"Root".to_vec(), Obj::Ref(root)),
    ];
    trailer_pairs.extend_from_slice(extra_trailer);
    let trailer = dict(&trailer_pairs);
    let mut tw = Writer::new(budget);
    out.extend_from_slice(&tw.to_bytes(&trailer, g)?);
    out.push(b'\n');
    out.extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());

    g.charge(
        selis_sandbox::Resource::Bytes,
        u64::try_from(out.len()).unwrap_or(u64::MAX),
    )?;
    Ok(out)
}

/// A real number object.
fn real(v: f64) -> Obj {
    let max = 9_223_372_036_854_775_807i64;
    let scaled = (v * 1000.0).round();
    let scaled = if scaled >= max as f64 {
        max
    } else {
        #[allow(clippy::cast_possible_truncation)]
        {
            scaled as i64
        }
    };
    Obj::Real { scaled, scale: 3 }
}

/// Format a number without exponent notation.
fn trim(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s.is_empty() || s == "-" {
        "0".to_string()
    } else {
        s
    }
}

fn bytes(v: &[u8]) -> selis_bytes::Bytes {
    selis_bytes::Bytes::copy_from_slice(v)
}

fn dict(pairs: &[(Vec<u8>, Obj)]) -> Obj {
    Obj::Dict(pairs.iter().map(|(k, v)| (bytes(k), v.clone())).collect())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;
    use selis_sandbox::{CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        selis_sandbox::Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    #[test]
    fn a_written_document_opens_and_has_one_page() {
        let mut b = DocumentBuilder::new();
        let mut c = ContentBuilder::new();
        c.set_fill(1.0, 0.0, 0.0).fill_rect(0.0, 0.0, 100.0, 100.0);
        c.begin_text()
            .set_font("Helvetica", 12.0)
            .text_at(10.0, 50.0)
            .show_text("Hello writer")
            .end_text();
        b.add_page(100.0, 100.0, c.to_bytes().as_slice());
        let mut g = guard();
        let budget = selis_sandbox::Budget::unlimited();
        let bytes = b.write(&budget, &mut g).expect("write");
        // The bytes parse as one revision with a classic xref table.
        let startxref = crate::xref::find_startxref(&bytes, 2048).expect("startxref");
        let doc = crate::parse_revisions(&bytes, startxref, &budget, &mut g).expect("open");
        assert_eq!(doc.revisions().len(), 1);
        let rev = &doc.revisions()[0];
        // Objects: 1 catalog, 2 pages, 3 content, 4 page.
        assert!(rev.entries.get(&1).is_some());
        assert!(rev.entries.get(&2).is_some());
        assert!(rev.entries.get(&3).is_some());
        assert!(rev.entries.get(&4).is_some());
    }

    #[test]
    fn embedded_image_is_in_the_document() {
        let mut b = DocumentBuilder::new();
        let img = embed_image_rgba(&mut b, 1, 1, &[255, 0, 0, 255]);
        let mut c = ContentBuilder::new();
        c.set_fill(1.0, 1.0, 1.0).fill_rect(0.0, 0.0, 1.0, 1.0);
        c.draw_image(&format!("Im{}", img.num));
        b.add_page(100.0, 100.0, c.to_bytes().as_slice());
        let mut g = guard();
        let budget = selis_sandbox::Budget::unlimited();
        let bytes = b.write(&budget, &mut g).expect("write");
        assert!(String::from_utf8_lossy(&bytes).contains("/Subtype /Image"));
    }

    #[test]
    fn write_objects_as_document_opens_with_sparse_numbers() {
        // Objects 1 (catalog), 5 (pages), 7 (page) — gaps must become free
        // xref entries and the trailer must point at the given root.
        let objects = vec![
            (
                1,
                dict(&[
                    (b"Type".to_vec(), Obj::Name(bytes(b"Catalog"))),
                    (b"Pages".to_vec(), Obj::Ref(Ref::new(5, 0))),
                ]),
            ),
            (
                5,
                dict(&[
                    (b"Type".to_vec(), Obj::Name(bytes(b"Pages"))),
                    (b"Kids".to_vec(), Obj::Array(vec![Obj::Ref(Ref::new(7, 0))])),
                    (b"Count".to_vec(), Obj::Int(1)),
                ]),
            ),
            (
                7,
                dict(&[
                    (b"Type".to_vec(), Obj::Name(bytes(b"Page"))),
                    (b"Parent".to_vec(), Obj::Ref(Ref::new(5, 0))),
                    (
                        b"MediaBox".to_vec(),
                        Obj::Array(vec![Obj::Int(0), Obj::Int(0), Obj::Int(100), Obj::Int(100)]),
                    ),
                ]),
            ),
        ];
        let budget = selis_sandbox::Budget::unlimited();
        let mut g = guard();
        let out =
            write_objects_as_document(&objects, Ref::new(1, 0), &budget, &mut g).expect("write");
        let startxref = crate::xref::find_startxref(&out, 2048).expect("startxref");
        let doc = crate::parse_revisions(&out, startxref, &budget, &mut g).expect("open");
        assert_eq!(doc.revisions().len(), 1);
        let rev = &doc.revisions()[0];
        assert_eq!(rev.root, Some(Ref::new(1, 0)));
        assert!(rev.entries.get(&1).is_some());
        assert!(rev.entries.get(&5).is_some());
        assert!(rev.entries.get(&7).is_some());
        // A gap (object 2) is a free entry, not in use.
        assert!(matches!(
            rev.entries.get(&2),
            None | Some(crate::XrefEntry::Free { .. })
        ));
    }

    #[test]
    fn write_objects_as_document_rejects_duplicate_numbers() {
        let objects = vec![(1u32, Obj::Int(1)), (1u32, Obj::Int(2))];
        let budget = selis_sandbox::Budget::unlimited();
        let mut g = guard();
        let err = write_objects_as_document(&objects, Ref::new(1, 0), &budget, &mut g)
            .expect_err("duplicate numbers rejected");
        assert_eq!(err.code(), Code::ObjUnexpected);
    }

    #[test]
    fn builder_dedup_merges_identical_pages_and_keeps_count() {
        // Two identical pages share an identical content stream, which dedup
        // merges — but the page dicts themselves are page-tree nodes and must
        // stay distinct (merging them would read as a cycle in the tree).
        let mut b = DocumentBuilder::new();
        let mut c = ContentBuilder::new();
        c.set_fill(0.0, 0.0, 1.0).fill_rect(0.0, 0.0, 50.0, 50.0);
        b.add_page(100.0, 100.0, c.to_bytes().as_slice());
        b.add_page(100.0, 100.0, c.to_bytes().as_slice());
        let budget = selis_sandbox::Budget::unlimited();
        let mut g = guard();
        let removed = b.dedup(&budget, &mut g);
        assert_eq!(
            removed, 1,
            "only the shared content stream merges: {removed}"
        );
        let bytes = b.write(&budget, &mut g).expect("write");
        // The output parses and still reports two pages.
        assert_eq!(
            pages_count(&bytes, &budget, &mut g),
            2,
            "both pages survive dedup"
        );
        // The two page dicts must be distinct objects.
        let startxref = crate::xref::find_startxref(&bytes, 2048).expect("startxref");
        let doc = crate::parse_revisions(&bytes, startxref, &budget, &mut g).expect("open");
        let mut page_nums: Vec<u32> = Vec::new();
        for rev in doc.revisions() {
            for (num, e) in &rev.entries {
                if let crate::XrefEntry::InUse { offset, .. } = e {
                    if let Ok(Obj::Dict(pairs)) =
                        crate::resolve_object(&bytes, *offset, &budget, &mut g)
                    {
                        if pairs.iter().any(|(k, v)| {
                            k.as_slice() == b"Type"
                                && matches!(v, Obj::Name(n) if n.as_slice() == b"Page")
                        }) {
                            page_nums.push(*num);
                        }
                    }
                }
            }
        }
        assert_eq!(
            page_nums.len(),
            2,
            "two distinct page objects: {page_nums:?}"
        );
    }

    /// Incremental update: original bytes are a prefix, two revisions parse,
    /// and the replacement objects resolve from the newest revision.
    #[test]
    fn incremental_update_appends_a_revision() {
        let mut b = DocumentBuilder::new();
        let mut c = ContentBuilder::new();
        c.set_fill(1.0, 0.0, 0.0).fill_rect(0.0, 0.0, 100.0, 100.0);
        b.add_page(100.0, 100.0, c.to_bytes().as_slice());
        let budget = Budget::unlimited();
        let mut g = guard();
        let original = b.write(&budget, &mut g).expect("write");

        // Redefine the catalog (1) with a /Producer string and the page (4)
        // with a /Rotate — these are separate xref subsections.
        let catalog = dict(&[
            (b"Type".to_vec(), Obj::Name(bytes(b"Catalog"))),
            (b"Pages".to_vec(), Obj::Ref(Ref::new(2, 0))),
            (b"Version".to_vec(), Obj::Name(bytes(b"1.4"))),
            (b"Producer".to_vec(), Obj::String(bytes(b"selis-incr"))),
        ]);
        let page = dict(&[
            (b"Type".to_vec(), Obj::Name(bytes(b"Page"))),
            (b"Parent".to_vec(), Obj::Ref(Ref::new(2, 0))),
            (
                b"MediaBox".to_vec(),
                Obj::Array(vec![
                    Obj::Int(0),
                    Obj::Int(0),
                    Obj::Int(100),
                    Obj::Int(100),
                ]),
            ),
            (b"Contents".to_vec(), Obj::Ref(Ref::new(3, 0))),
            (b"Resources".to_vec(), Obj::Dict(Vec::new())),
            (b"Rotate".to_vec(), Obj::Int(90)),
        ]);
        let trailer = vec![
            (b"Root".to_vec(), Obj::Ref(Ref::new(1, 0))),
            (
                b"ID".to_vec(),
                Obj::Array(vec![
                    Obj::String(bytes(b"first")),
                    Obj::String(bytes(b"second")),
                ]),
            ),
        ];
        let updated = write_incremental_update(
            &original,
            &[(1, catalog), (4, page)],
            &trailer,
            &budget,
            &mut g,
        )
        .expect("incr");

        // Original bytes are a byte-identical prefix.
        assert!(updated.starts_with(&original), "original must be a prefix");

        // Two revisions, and the new objects resolve from the newest.
        let startxref = crate::xref::find_startxref(&updated, 4096).expect("startxref");
        let doc = crate::parse_revisions(&updated, startxref, &budget, &mut g).expect("open");
        assert_eq!(doc.revisions().len(), 2);
        let view = doc.at_revision(1).expect("newest view");

        // Catalog /Producer is present.
        let cat_off = match view.xref.get(&1) {
            Some(crate::XrefEntry::InUse { offset, .. }) => *offset,
            _ => panic!("catalog not in use"),
        };
        let Obj::Dict(cat_pairs) =
            crate::resolve_object_numbered(&updated, cat_off, 1, &budget, &mut g)
                .expect("resolve catalog")
        else {
            panic!("catalog is a dict");
        };
        assert!(
            cat_pairs
                .iter()
                .any(|(k, v)| k.as_slice() == b"Producer"
                    && matches!(v, Obj::String(s) if s.as_slice() == b"selis-incr")),
            "catalog has /Producer"
        );

        // Page /Rotate is present.
        let page_off = match view.xref.get(&4) {
            Some(crate::XrefEntry::InUse { offset, .. }) => *offset,
            _ => panic!("page not in use"),
        };
        let Obj::Dict(page_pairs) =
            crate::resolve_object_numbered(&updated, page_off, 4, &budget, &mut g)
                .expect("resolve page")
        else {
            panic!("page is a dict");
        };
        assert!(
            page_pairs
                .iter()
                .any(|(k, v)| k.as_slice() == b"Rotate" && matches!(v, Obj::Int(90))),
            "page has /Rotate"
        );
    }

    /// Duplicate object numbers in the update are rejected.
    #[test]
    fn incremental_update_rejects_duplicates() {
        let mut b = DocumentBuilder::new();
        let budget = Budget::unlimited();
        let mut g = guard();
        let original = b.write(&budget, &mut g).expect("write");
        let err = write_incremental_update(
            &original,
            &[(1, Obj::Int(1)), (1, Obj::Int(2))],
            &[(b"Root".to_vec(), Obj::Ref(Ref::new(1, 0)))],
            &budget,
            &mut g,
        )
        .expect_err("duplicates rejected");
        assert_eq!(err.code(), Code::ObjUnexpected);
    }

    /// `write_objects_as_document_with_trailer` carries extra trailer entries.
    #[test]
    fn write_objects_as_document_with_trailer_carries_extra_entries() {
        let objects = vec![(
            1u32,
            dict(&[
                (b"Type".to_vec(), Obj::Name(bytes(b"Catalog"))),
                (b"Pages".to_vec(), Obj::Ref(Ref::new(2, 0))),
            ]),
        )];
        let budget = Budget::unlimited();
        let mut g = guard();
        let extra = vec![
            (b"Info".to_vec(), Obj::Ref(Ref::new(99, 0))),
            (
                b"ID".to_vec(),
                Obj::Array(vec![
                    Obj::String(bytes(b"abc")),
                    Obj::String(bytes(b"def")),
                ]),
            ),
        ];
        let out = write_objects_as_document_with_trailer(
            &objects,
            Ref::new(1, 0),
            &extra,
            &budget,
            &mut g,
        )
        .expect("write");
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("/Info 99 0 R"), "trailer has /Info: {s}");
        assert!(
            s.contains("(abc)") && s.contains("(def)"),
            "trailer has /ID strings: {s}"
        );
    }

    /// Parse `src` and return the `/Count` of its `/Type /Pages` object.
    fn pages_count(src: &[u8], budget: &Budget, g: &mut BudgetGuard<'_>) -> i64 {
        let startxref = crate::xref::find_startxref(src, 2048).expect("startxref");
        let doc = crate::parse_revisions(src, startxref, budget, g).expect("open");
        let mut offsets: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
        for rev in doc.revisions() {
            for (num, e) in &rev.entries {
                if let crate::XrefEntry::InUse { offset, .. } = e {
                    offsets.insert(*num, *offset);
                }
            }
        }
        for off in offsets.values() {
            let Ok(obj) = crate::resolve_object(src, *off, budget, g) else {
                continue;
            };
            let Obj::Dict(pairs) = obj else { continue };
            let is_pages = pairs.iter().any(|(k, v)| {
                k.as_slice() == b"Type" && matches!(v, Obj::Name(n) if n.as_slice() == b"Pages")
            });
            if is_pages {
                if let Some((_, Obj::Int(n))) = pairs.iter().find(|(k, _)| k.as_slice() == b"Count")
                {
                    return *n;
                }
            }
        }
        0
    }
}
