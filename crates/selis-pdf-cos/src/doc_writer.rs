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

    /// Set an `/Info` field (e.g. `Title`, `Author`).
    pub fn set_info(&mut self, key: &[u8], value: &str) {
        self.info
            .push((key.to_vec(), Obj::String(bytes(value.as_bytes()))));
    }

    /// Add a page with a content stream. Returns the page's object reference.
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
                        Obj::Real { scaled: 0, scale: 0 },
                        Obj::Real { scaled: 0, scale: 0 },
                        real(width),
                        real(height),
                    ]),
                ),
                (
                    b"Contents".to_vec(),
                    Obj::Ref(Ref::new(content_num, 0)),
                ),
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
                    Obj::Real { scaled: 0, scale: 0 },
                    Obj::Real { scaled: 0, scale: 0 },
                    real(width),
                    real(height),
                ]),
            ),
            (b"Contents".to_vec(), contents),
        ];
        if let Some(r) = resources {
            pairs.push((b"Resources".to_vec(), Obj::Ref(r)));
        }
        let page_num = self.allocate();
        self.objects.push((page_num, dict(&pairs)));
        let page_ref = Ref::new(page_num, 0);
        self.page_refs.push(page_ref);
        self.update_pages_tree();
        page_ref
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
        g.charge(selis_sandbox::Resource::Bytes, u64::try_from(out.len()).unwrap_or(u64::MAX))?;

        // Info dictionary (if set) is appended before the xref table and the
        // catalog references it.
        if !self.info.is_empty() {
            let info_num = self.allocate();
            let info = Obj::Dict(self.info.iter().map(|(k, v)| (bytes(k), v.clone())).collect());
            self.objects.push((info_num, info));
            // Point the catalog's /Info at it.
            for (num, obj) in &mut self.objects {
                if *num == 1 {
                    if let Obj::Dict(pairs) = obj {
                        pairs.push((
                            bytes(b"Info"),
                            Obj::Ref(Ref::new(info_num, 0)),
                        ));
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

        // Classic xref table.
        let size = u64::try_from(offsets.len()).unwrap_or(u64::MAX);
        let startxref = u64::try_from(out.len()).unwrap_or(u64::MAX);
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        for (i, off) in offsets.iter().enumerate() {
            if i == 0 {
                out.extend_from_slice(b"0000000000 65535 f \n");
            } else {
                let off = off.unwrap_or(0);
                out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            }
        }
        out.extend_from_slice(b"trailer\n");
        let trailer = dict(&[
            (b"Size".to_vec(), Obj::Int(i64::try_from(size).unwrap_or(i64::MAX))),
            (b"Root".to_vec(), Obj::Ref(Ref::new(1, 0))),
        ]);
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

    fn allocate(&mut self) -> u32 {
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

    /// Move the text origin (user space).
    pub fn text_at(&mut self, x: f64, y: f64) -> &mut Self {
        self.push_fmt(format!("{} {} Td\n", trim(x), trim(y)));
        self
    }

    /// Show a text string (parentheses-escaped).
    pub fn show_text(&mut self, text: &str) -> &mut Self {
        self.push(b"(");
        for b in text.bytes() {
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
    // Convert RGBA → RGB (PDF DeviceRGB).
    let mut rgb = Vec::with_capacity((width as usize).saturating_mul(height as usize).saturating_mul(3));
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
}
