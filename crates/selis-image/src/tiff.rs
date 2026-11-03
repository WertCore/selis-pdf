//! Baseline multi-page TIFF, written and read back by this crate.
//!
//! TIFF is the one raster output that is a *container*: several pages live in
//! one file as a chain of image file directories, so `PDF → TIFF` hands the
//! user one document rather than a ZIP of single-page files. That only makes
//! sense if the bytes can be re-opened, so this module carries both halves:
//!
//! * [`tiff_strip`] deflates one rendered page, [`encode_tiff`] lays the
//!   strips out as an IFD chain, and [`encode_tiff_one`] is the one-page case.
//! * [`tiff_page_infos`] walks the chain without touching pixels,
//!   [`tiff_strip_rgb`] inflates one page, and [`decode_tiff`] does both.
//!
//! The writer emits the profile it can also read — 8-bit contiguous RGB, one
//! strip per page, `Compression` 8 (zlib/Deflate, the same owned `flate_encode`
//! the PDF writer and PNG use) or 1 (none), little-endian — and refuses
//! anything outside what a baseline reader is promised: dimensions or a page
//! count that will not fit a `SHORT`, zero-sized pages, a zero DPI.
//!
//! The reader is deliberately narrow, and says so by name when it meets
//! something outside the profile: a `PlanarConfiguration` of 2, a
//! `BitsPerSample` that is not three 8-bit samples, `Compression` values we do
//! not write, and a multi-strip page are `IMAGE_UNSUPPORTED`, while a broken
//! chain, a truncated file or a strip that will not inflate to its declared
//! size is `IMAGE_MALFORMED`. Unknown tags are ignored, as TIFF requires.

use selis_error::{err, Code, Result};
use selis_sandbox::{alloc, BudgetGuard, Resource};

use crate::{flatten_over_white, Image};

/// The magic number that follows the byte-order mark (TIFF 6.0 §7).
const MAGIC: u16 = 42;
/// `Compression` 1: the strip holds its samples unchanged.
const COMPRESSION_NONE: u16 = 1;
/// `Compression` 8: Deflate, zlib-wrapped (TIFF 6.0 Technical Note 2).
const COMPRESSION_DEFLATE: u16 = 8;
/// `PhotometricInterpretation` 2: RGB.
const PHOTOMETRIC_RGB: u16 = 2;
/// `PlanarConfiguration` 1: channels interleaved per pixel.
const PLANAR_CONTIGUOUS: u16 = 1;
/// `ResolutionUnit` 2: inches, the unit a rendered page reports.
const RESOLUTION_INCH: u16 = 2;

/// Field type codes used by the entries this module writes.
const FIELD_SHORT: u16 = 3;
const FIELD_LONG: u16 = 4;
const FIELD_RATIONAL: u16 = 5;

/// The largest value a `SHORT` tag can hold. `ImageWidth`, `ImageLength`,
/// `RowsPerStrip` and the `PageNumber` pair are all `SHORT` in a baseline
/// file, so a page beyond this is refused instead of written with a truncated
/// dimension the reader would believe.
const SHORT_MAX: u32 = 65535;
/// The ceiling on IFD entries a reader will walk. A file claiming more is
/// either broken or hostile; a page this module writes carries 15.
const MAX_ENTRIES: u32 = 256;

/// The bytes one written page spends on structure: the 2-byte entry count, the
/// 15 entries at 12 bytes each, and the 4-byte `NextIFD` pointer.
const IFD_LEN: usize = 186;
/// The bytes one written page spends on out-of-line tag values: three 8-bit
/// `BitsPerSample` entries and two `RATIONAL` resolutions.
const VALUES_LEN: usize = 22;

/// One rendered page, deflated and ready to be laid into a TIFF container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TiffStrip {
    /// Pixel width of the page.
    pub width: u32,
    /// Pixel height of the page.
    pub height: u32,
    /// The page's RGB rows, Deflate-compressed, in `Compression` 8 form.
    pub data: Vec<u8>,
}

/// One page as its image file directory describes it, before any pixel data
/// is touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TiffPageInfo {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// `Compression`: 1 or 8, the two this module understands.
    pub compression: u16,
    /// Where the page's single strip begins in the file.
    pub strip_offset: usize,
    /// How many compressed bytes the strip declares.
    pub strip_len: usize,
}

/// Validate a page and deflate its samples into one strip.
///
/// The samples are composited over opaque white by
/// [`crate::flatten_over_white`] — TIFF carries no alpha in this profile, and
/// a rendered page is opaque wherever it has ink anyway — then compressed with
/// the engine's own zlib writer at `level`, the same scale `flate_encode`
/// accepts (0 stores, 9 compresses hardest).
///
/// # Budget
///
/// Charges the flattened RGB buffer and the compressed strip against `Bytes`.
/// [`encode_tiff`] charges the file it assembles separately, so a caller that
/// holds both really pays for both.
///
/// # Malformed Input
///
/// `IMAGE_MALFORMED` when either dimension is zero, when a dimension will not
/// fit a baseline `SHORT`, or when `rgba` is not exactly `width * height * 4`
/// bytes.
pub fn tiff_strip(image: &Image, level: u8, g: &mut BudgetGuard<'_>) -> Result<TiffStrip> {
    let (width, height) = checked_dimensions(image)?;
    let rgb = flatten_over_white(&image.rgba, g)?;
    let data = selis_pdf_filter::flate_encode(&rgb, level);
    g.charge(
        Resource::Bytes,
        u64::try_from(data.len()).unwrap_or(u64::MAX),
    )?;
    Ok(TiffStrip {
        width,
        height,
        data,
    })
}

/// Lay strips out as one multi-page baseline TIFF file.
///
/// The layout is one IFD per page, each followed by its out-of-line tag values
/// and its strip, with the page's `NextIFD` pointer naming the next directory
/// and `PageNumber` carrying the zero-based index against the last page's
/// index. Every block starts on an even byte, so a word-aligned reader never
/// straddles a boundary.
///
/// # Budget
///
/// Charges the file it returns against `Bytes`.
///
/// # Malformed Input
///
/// `IMAGE_MALFORMED` when there is no page, when there are too many pages for
/// a `SHORT` `PageNumber`, when a page dimension will not fit a `SHORT`, when
/// a strip is too large for a 32-bit `StripByteCounts`, when the chain would
/// need an offset a 32-bit pointer cannot carry, or when `dpi` is zero.
pub fn encode_tiff(strips: &[TiffStrip], dpi: u32, g: &mut BudgetGuard<'_>) -> Result<Vec<u8>> {
    if strips.is_empty() {
        return Err(err!(
            Code::ImageMalformed,
            during = "tiff-encode",
            detail = "a TIFF needs at least one page"
        ));
    }
    if strips.len() > usize::try_from(SHORT_MAX).unwrap_or(usize::MAX) {
        return Err(err!(
            Code::ImageMalformed,
            during = "tiff-encode",
            detail = "a baseline PageNumber cannot count this many pages"
        ));
    }
    if dpi == 0 {
        return Err(err!(
            Code::ImageMalformed,
            during = "tiff-encode",
            detail = "DPI must be at least 1 to be written as a resolution"
        ));
    }
    let mut total = 8usize;
    for strip in strips {
        if strip.width == 0
            || strip.height == 0
            || strip.width > SHORT_MAX
            || strip.height > SHORT_MAX
        {
            return Err(err!(
                Code::ImageMalformed,
                during = "tiff-encode",
                detail = "a page size does not fit a baseline SHORT dimension"
            ));
        }
        if u32::try_from(strip.data.len()).is_err() {
            return Err(err!(
                Code::ImageMalformed,
                during = "tiff-encode",
                detail = "a strip is too large for a 32-bit StripByteCounts"
            ));
        }
        total = total
            .checked_add(page_len(strip))
            .ok_or_else(offset_overflow)?;
    }

    // The one charge for the file: every byte of it, strips included.
    let mut out = alloc::vec_with_capacity::<u8>(g, total)?;
    out.extend_from_slice(b"II");
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&8u32.to_le_bytes());

    let last_index = u16::try_from(strips.len())
        .map_err(|_| {
            err!(
                Code::ImageMalformed,
                during = "tiff-encode",
                detail = "a baseline PageNumber cannot count this many pages"
            )
        })?
        .saturating_sub(1);
    let mut ifd_at: usize = 8;
    for (index, strip) in strips.iter().enumerate() {
        let values_at = offset(ifd_at.saturating_add(IFD_LEN))?;
        let strip_at = offset((values_at as usize).saturating_add(VALUES_LEN))?;
        let next_at = offset(ifd_at.saturating_add(page_len(strip)))?;
        let is_last = index.saturating_add(1) == strips.len();
        let strip_len = u32::try_from(strip.data.len()).unwrap_or(u32::MAX);
        let page_index = u32::try_from(index).unwrap_or(u32::MAX);

        push_u16(&mut out, 15);
        // Tags must be written in ascending order.
        push_entry_long(&mut out, 254, if index == 0 { 0 } else { 1 });
        push_entry_short(&mut out, 256, strip.width);
        push_entry_short(&mut out, 257, strip.height);
        push_entry_array(&mut out, 258, FIELD_SHORT, 3, values_at);
        push_entry_short(&mut out, 259, u32::from(COMPRESSION_DEFLATE));
        push_entry_short(&mut out, 262, u32::from(PHOTOMETRIC_RGB));
        push_entry_long(&mut out, 273, strip_at);
        push_entry_short(&mut out, 277, 3);
        push_entry_short(&mut out, 278, strip.height);
        push_entry_long(&mut out, 279, strip_len);
        push_entry_array(
            &mut out,
            282,
            FIELD_RATIONAL,
            1,
            values_at.saturating_add(6),
        );
        push_entry_array(
            &mut out,
            283,
            FIELD_RATIONAL,
            1,
            values_at.saturating_add(14),
        );
        push_entry_short(&mut out, 284, u32::from(PLANAR_CONTIGUOUS));
        push_entry_short(&mut out, 296, u32::from(RESOLUTION_INCH));
        push_entry_two_shorts(&mut out, 297, page_index, u32::from(last_index));
        push_u32(&mut out, if is_last { 0 } else { next_at });

        // Out-of-line values: three 8-bit samples, then the two resolutions.
        for _ in 0..3 {
            push_u16(&mut out, 8);
        }
        push_u32(&mut out, dpi);
        push_u32(&mut out, 1);
        push_u32(&mut out, dpi);
        push_u32(&mut out, 1);

        out.extend_from_slice(&strip.data);
        if strip.data.len() & 1 != 0 {
            out.push(0);
        }
        ifd_at = next_at as usize;
    }
    Ok(out)
}

/// Compress and lay one page out as a single-page TIFF.
///
/// # Budget
///
/// As [`tiff_strip`] and [`encode_tiff`].
pub fn encode_tiff_one(
    image: &Image,
    level: u8,
    dpi: u32,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let strip = tiff_strip(image, level, g)?;
    encode_tiff(std::slice::from_ref(&strip), dpi, g)
}

/// Walk a TIFF's IFD chain and report every page, without decoding pixels.
///
/// Reading the chain separately from the samples is what lets a caller verify
/// a document page by page — check one strip, drop it, check the next — instead
/// of holding the whole raster set at once.
///
/// # Budget
///
/// Charges one [`TiffPageInfo`] per page reported against `Bytes`.
///
/// # Malformed Input
///
/// `IMAGE_UNSUPPORTED` for a file that is not TIFF at all. `IMAGE_MALFORMED`
/// for a missing or out-of-range header, a chain that does not move forward, an
/// IFD the file cannot hold, a page missing a tag this profile needs, or a tag
/// value that does not fit the file.
pub fn tiff_page_infos(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<Vec<TiffPageInfo>> {
    let le = byte_order(data)?;
    let magic = read_u16(data, 2, le).ok_or_else(short_file)?;
    if magic != MAGIC {
        return Err(err!(
            Code::ImageMalformed,
            during = "tiff-decode",
            detail = format!("the TIFF magic is {magic}, expected {MAGIC}")
        ));
    }
    let first = read_u32(data, 4, le).ok_or_else(short_file)?;
    let mut ifd_at = usize::try_from(first).unwrap_or(usize::MAX);
    let mut pages: Vec<TiffPageInfo> = Vec::new();
    let mut previous: usize = 0;

    while ifd_at != 0 {
        // A chain that does not advance is a cycle, and a cycle is a hang.
        if ifd_at <= previous || ifd_at < 8 || ifd_at >= data.len() {
            return Err(err!(
                Code::ImageMalformed,
                during = "tiff-decode",
                detail = format!("the IFD chain points at {ifd_at}, which it cannot read from")
            ));
        }
        let entries = u32::from(read_u16(data, ifd_at, le).ok_or_else(short_file)?);
        if entries > MAX_ENTRIES {
            return Err(err!(
                Code::ImageMalformed,
                during = "tiff-decode",
                detail = format!(
                    "an IFD claims {entries} entries, over the {MAX_ENTRIES} a reader will walk"
                )
            ));
        }
        let body = usize::try_from(entries)
            .unwrap_or(usize::MAX)
            .saturating_mul(12)
            .saturating_add(6);
        let ifd_end = ifd_at
            .checked_add(body)
            .ok_or_else(|| malformed("an IFD runs off the end of the file"))?;
        if ifd_end > data.len() {
            return Err(malformed("an IFD runs off the end of the file"));
        }
        let next_at = ifd_end.saturating_sub(4);
        let next = usize::try_from(read_u32(data, next_at, le).ok_or_else(short_file)?)
            .unwrap_or(usize::MAX);

        let mut page = PageTags::default();
        for entry in 0..usize::try_from(entries).unwrap_or(usize::MAX) {
            let base = ifd_at
                .saturating_add(2)
                .saturating_add(entry.saturating_mul(12));
            let tag = read_u16(data, base, le).ok_or_else(short_file)?;
            let field = read_u16(data, base.saturating_add(2), le).ok_or_else(short_file)?;
            let count = read_u32(data, base.saturating_add(4), le).ok_or_else(short_file)?;
            let raw: [u8; 4] = data
                .get(base.saturating_add(8)..base.saturating_add(12))
                .and_then(|slice| slice.try_into().ok())
                .ok_or_else(short_file)?;
            page.note(tag, field, count, &raw, data, le)?;
        }
        let info = page.into_page(data)?;
        g.charge(
            Resource::Bytes,
            u64::try_from(std::mem::size_of::<TiffPageInfo>()).unwrap_or(u64::MAX),
        )?;
        pages.push(info);

        previous = ifd_at;
        ifd_at = next;
    }
    if pages.is_empty() {
        return Err(malformed("the file declares no pages"));
    }
    Ok(pages)
}

/// Read the samples of one page back into RGB.
///
/// # Budget
///
/// Charges the inflated buffer against `Bytes`, bounded by the page's declared
/// size; a strip claiming more than its dimensions hold is refused before it is
/// allocated.
///
/// # Malformed Input
///
/// `IMAGE_MALFORMED` when the strip lies outside the file, when Deflate data
/// will not inflate, or when it does not inflate to exactly
/// `width * height * 3` bytes. `IMAGE_UNSUPPORTED` for a compression this
/// profile does not write.
pub fn tiff_strip_rgb(
    data: &[u8],
    info: &TiffPageInfo,
    g: &mut BudgetGuard<'_>,
) -> Result<Vec<u8>> {
    let expected = usize::try_from(
        u64::from(info.width)
            .saturating_mul(u64::from(info.height))
            .saturating_mul(3),
    )
    .unwrap_or(usize::MAX);
    let end = info
        .strip_offset
        .checked_add(info.strip_len)
        .ok_or_else(|| malformed("a strip length runs past the addressable file"))?;
    let bytes = data
        .get(info.strip_offset..end)
        .ok_or_else(|| malformed("a strip points outside the file"))?;
    match info.compression {
        COMPRESSION_NONE => {
            if bytes.len() != expected {
                return Err(err!(
                    Code::ImageMalformed,
                    during = "tiff-decode",
                    detail = format!(
                        "an uncompressed strip holds {} bytes, expected {expected}",
                        bytes.len()
                    )
                ));
            }
            let mut rgb = alloc::vec_with_capacity::<u8>(g, expected)?;
            rgb.extend_from_slice(bytes);
            Ok(rgb)
        }
        COMPRESSION_DEFLATE => selis_pdf_filter::flate_decode_bounded(
            bytes,
            u64::try_from(expected).unwrap_or(u64::MAX),
            g,
        )
        .and_then(|rgb| {
            if rgb.len() != expected {
                return Err(err!(
                    Code::ImageMalformed,
                    during = "tiff-decode",
                    detail = format!(
                        "a strip inflates to {} bytes, expected {expected}",
                        rgb.len()
                    )
                ));
            }
            Ok(rgb)
        }),
        other => Err(err!(
            Code::ImageUnsupported,
            during = "tiff-decode",
            detail = format!("TIFF compression {other} is not a profile this engine writes")
        )),
    }
}

/// Decode a TIFF into its pages, top-down, with every alpha channel set to
/// opaque — the samples this profile stores carry no transparency to restore.
///
/// # Budget
///
/// As [`tiff_page_infos`] and [`tiff_strip_rgb`], for every page held at once.
pub fn decode_tiff(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<Vec<Image>> {
    let infos = tiff_page_infos(data, g)?;
    let mut images = Vec::with_capacity(infos.len());
    for info in &infos {
        let rgb = tiff_strip_rgb(data, info, g)?;
        let count = usize::try_from(u64::from(info.width).saturating_mul(u64::from(info.height)))
            .unwrap_or(usize::MAX);
        if rgb.len() != count.saturating_mul(3) {
            return Err(malformed("a page's samples do not fill its declared size"));
        }
        let mut rgba = alloc::vec_with_capacity::<u8>(g, count.saturating_mul(4))?;
        for px in rgb.chunks_exact(3) {
            rgba.extend_from_slice(&[
                px.first().copied().unwrap_or(0),
                px.get(1).copied().unwrap_or(0),
                px.get(2).copied().unwrap_or(0),
                255,
            ]);
        }
        images.push(Image {
            width: info.width,
            height: info.height,
            rgba,
        });
    }
    Ok(images)
}

/// The tags one page needs before its strip can be read, while the IFD is
/// still being walked.
#[derive(Debug, Default)]
struct PageTags {
    width: Option<u32>,
    height: Option<u32>,
    compression: Option<u32>,
    photometric: Option<u32>,
    planar: Option<u32>,
    samples: Option<u32>,
    bits: Option<u32>,
    rows: Option<u32>,
    strip_offset: Option<u32>,
    strip_len: Option<u32>,
    offset_count: Option<u32>,
    count_count: Option<u32>,
}

impl PageTags {
    /// Record one entry. Tags this profile does not carry are ignored, which is
    /// what TIFF requires of a conforming reader.
    fn note(
        &mut self,
        tag: u16,
        field: u16,
        count: u32,
        raw: &[u8; 4],
        data: &[u8],
        le: bool,
    ) -> Result<()> {
        match tag {
            256 => self.width = inline_number(field, count, raw, le)?,
            257 => self.height = inline_number(field, count, raw, le)?,
            258 => {
                self.bits = if count == 1 {
                    inline_number(field, count, raw, le)?
                } else {
                    let at = offset_of(raw, le)?;
                    let first = read_u16(data, at, le).ok_or_else(short_file)?;
                    let second = read_u16(data, at.saturating_add(2), le).ok_or_else(short_file)?;
                    let third = read_u16(data, at.saturating_add(4), le).ok_or_else(short_file)?;
                    if first == second && second == third {
                        Some(u32::from(first))
                    } else {
                        // A per-channel depth this profile cannot hold.
                        Some(u32::MAX)
                    }
                }
            }
            259 => self.compression = inline_number(field, count, raw, le)?,
            262 => self.photometric = inline_number(field, count, raw, le)?,
            273 => {
                self.strip_offset = inline_number(field, count, raw, le)?;
                self.offset_count = Some(count);
            }
            277 => self.samples = inline_number(field, count, raw, le)?,
            278 => self.rows = inline_number(field, count, raw, le)?,
            279 => {
                self.strip_len = inline_number(field, count, raw, le)?;
                self.count_count = Some(count);
            }
            284 => self.planar = inline_number(field, count, raw, le)?,
            _ => {}
        }
        Ok(())
    }

    /// Turn the recorded tags into a page, refusing anything outside the
    /// profile this crate writes.
    fn into_page(self, data: &[u8]) -> Result<TiffPageInfo> {
        let missing = |tag: &str| {
            err!(
                Code::ImageMalformed,
                during = "tiff-decode",
                detail = format!("a page IFD carries no {tag}")
            )
        };
        let width = self.width.ok_or_else(|| missing("ImageWidth"))?;
        let height = self.height.ok_or_else(|| missing("ImageLength"))?;
        if width == 0 || height == 0 || width > SHORT_MAX || height > SHORT_MAX {
            return Err(malformed("a page declares a size outside a baseline TIFF"));
        }
        let unsupported = |what: &str, value: u32| {
            err!(
                Code::ImageUnsupported,
                during = "tiff-decode",
                detail = format!("{what} is {value}, outside the RGB profile this engine writes")
            )
        };
        if self.photometric != Some(u32::from(PHOTOMETRIC_RGB)) {
            return Err(unsupported(
                "PhotometricInterpretation",
                self.photometric.unwrap_or(0),
            ));
        }
        if self.planar != Some(u32::from(PLANAR_CONTIGUOUS)) {
            return Err(unsupported("PlanarConfiguration", self.planar.unwrap_or(0)));
        }
        if self.samples != Some(3) {
            return Err(unsupported("SamplesPerPixel", self.samples.unwrap_or(0)));
        }
        if self.bits != Some(8) {
            return Err(unsupported("BitsPerSample", self.bits.unwrap_or(0)));
        }
        let compression = self.compression.ok_or_else(|| missing("Compression"))?;
        if compression != u32::from(COMPRESSION_NONE)
            && compression != u32::from(COMPRESSION_DEFLATE)
        {
            return Err(unsupported("Compression", compression));
        }
        // One strip per page is what this writer emits; a multi-strip page is a
        // file it never produces, and guessing at the order is worse than a
        // named refusal.
        if self.offset_count != Some(1) || self.count_count != Some(1) {
            return Err(err!(
                Code::ImageUnsupported,
                during = "tiff-decode",
                detail = "a multi-strip page is outside the profile this engine writes"
            ));
        }
        let strip_offset = self.strip_offset.ok_or_else(|| missing("StripOffsets"))?;
        let strip_len = self.strip_len.ok_or_else(|| missing("StripByteCounts"))?;
        let rows = self.rows.unwrap_or(height);
        if rows != 0 && rows != height {
            return Err(err!(
                Code::ImageUnsupported,
                during = "tiff-decode",
                detail = format!("RowsPerStrip is {rows}, not the one strip this engine writes")
            ));
        }
        let offset = usize::try_from(strip_offset).unwrap_or(usize::MAX);
        let len = usize::try_from(strip_len).unwrap_or(usize::MAX);
        if offset < 8 || offset >= data.len() {
            return Err(malformed("StripOffsets points outside the file"));
        }
        Ok(TiffPageInfo {
            width,
            height,
            compression: u16::try_from(compression).unwrap_or(0),
            strip_offset: offset,
            strip_len: len,
        })
    }
}

/// The byte order the file declares, or a named refusal.
fn byte_order(data: &[u8]) -> Result<bool> {
    match data.get(..2) {
        Some([b'I', b'I']) => Ok(true),
        Some([b'M', b'M']) => Ok(false),
        _ => Err(err!(
            Code::ImageUnsupported,
            during = "tiff-decode",
            detail = "the file does not begin with a TIFF byte-order mark"
        )),
    }
}

/// A file that ends before a fixed field it should hold.
fn short_file() -> selis_error::Error {
    err!(
        Code::ImageMalformed,
        during = "tiff-decode",
        detail = "the file ends inside a TIFF structure"
    )
}

/// A TIFF structure that cannot be read as this profile.
fn malformed(detail: &str) -> selis_error::Error {
    err!(
        Code::ImageMalformed,
        during = "tiff-decode",
        detail = detail.to_string()
    )
}

/// An offset the writer wants to put in a 32-bit field.
fn offset(value: usize) -> Result<u32> {
    u32::try_from(value).map_err(|_| offset_overflow())
}

/// The file has outgrown what a TIFF pointer can address.
fn offset_overflow() -> selis_error::Error {
    err!(
        Code::ImageMalformed,
        during = "tiff-encode",
        detail = "the TIFF would exceed what a 32-bit offset can address"
    )
}

/// How many bytes one page occupies: its IFD, its out-of-line values, its
/// strip, and the pad byte that keeps the next block on an even address.
fn page_len(strip: &TiffStrip) -> usize {
    IFD_LEN
        .saturating_add(VALUES_LEN)
        .saturating_add(strip.data.len())
        .saturating_add(strip.data.len() & 1)
}

fn read_u16(data: &[u8], at: usize, le: bool) -> Option<u16> {
    let bytes: [u8; 2] = data.get(at..at.saturating_add(2))?.try_into().ok()?;
    Some(if le {
        u16::from_le_bytes(bytes)
    } else {
        u16::from_be_bytes(bytes)
    })
}

fn read_u32(data: &[u8], at: usize, le: bool) -> Option<u32> {
    let bytes: [u8; 4] = data.get(at..at.saturating_add(4))?.try_into().ok()?;
    Some(if le {
        u32::from_le_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes)
    })
}

/// An entry's value field, read as the `SHORT` or `LONG` the field type says.
/// A count other than one is not a single number, and is refused.
fn inline_number(field: u16, count: u32, raw: &[u8; 4], le: bool) -> Result<Option<u32>> {
    if count != 1 {
        return Ok(None);
    }
    let value = match field {
        FIELD_SHORT => u32::from(read_u16(raw, 0, le).ok_or_else(short_file)?),
        FIELD_LONG => read_u32(raw, 0, le).ok_or_else(short_file)?,
        _ => return Ok(None),
    };
    Ok(Some(value))
}

/// An entry whose value does not fit the 4-byte field: the field holds the
/// offset of the real value.
fn offset_of(raw: &[u8; 4], le: bool) -> Result<usize> {
    let value = read_u32(raw, 0, le).ok_or_else(short_file)?;
    usize::try_from(value).map_err(|_| short_file())
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Write one IFD entry's head: tag, field type, count.
///
/// Each helper below finishes the entry with its 4-byte value field, so an
/// entry is always exactly 12 bytes. A `SHORT` inline value sits in the field's
/// first two bytes with the pad pair after it — a reader ignores the pad, but a
/// writer that leaves it out would shift every entry after it.
fn push_entry_head(out: &mut Vec<u8>, tag: u16, field: u16, count: u32) {
    push_u16(out, tag);
    push_u16(out, field);
    push_u32(out, count);
}

fn push_entry_short(out: &mut Vec<u8>, tag: u16, value: u32) {
    let narrow = u16::try_from(value).unwrap_or(0);
    push_entry_head(out, tag, FIELD_SHORT, 1);
    push_u16(out, narrow);
    push_u16(out, 0);
}

fn push_entry_long(out: &mut Vec<u8>, tag: u16, value: u32) {
    push_entry_head(out, tag, FIELD_LONG, 1);
    push_u32(out, value);
}

fn push_entry_array(out: &mut Vec<u8>, tag: u16, field: u16, count: u32, offset: u32) {
    push_entry_head(out, tag, field, count);
    push_u32(out, offset);
}

/// Two `SHORT`s in the value field: TIFF lays them in the field's own bytes,
/// so the pair is written low half first.
fn push_entry_two_shorts(out: &mut Vec<u8>, tag: u16, first: u32, second: u32) {
    let a = u16::try_from(first).unwrap_or(0);
    let b = u16::try_from(second).unwrap_or(0);
    push_entry_head(out, tag, FIELD_SHORT, 2);
    push_u16(out, a);
    push_u16(out, b);
}

/// The dimensions of an incoming page, bounded by what a baseline file can say.
fn checked_dimensions(image: &Image) -> Result<(u32, u32)> {
    let width = image.width;
    let height = image.height;
    if width == 0 || height == 0 || width > SHORT_MAX || height > SHORT_MAX {
        return Err(err!(
            Code::ImageMalformed,
            during = "tiff-encode",
            detail = "a page size is zero or too large for a baseline TIFF"
        ));
    }
    let expected = usize::try_from(
        u64::from(width)
            .saturating_mul(u64::from(height))
            .saturating_mul(4),
    )
    .unwrap_or(usize::MAX);
    if image.rgba.len() != expected {
        return Err(err!(
            Code::ImageMalformed,
            during = "tiff-encode",
            detail = "pixel buffer length does not match the dimensions"
        ));
    }
    Ok((width, height))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation
    )]

    use super::*;
    use selis_sandbox::{Budget, CancelToken, FixedClock};

    fn guard() -> BudgetGuard<'static> {
        Budget::unlimited().guard_with(&FixedClock(0), CancelToken::new())
    }

    fn solid(width: u32, height: u32, px: [u8; 4]) -> Image {
        let count = usize::try_from(width.saturating_mul(height)).unwrap_or(0);
        Image {
            width,
            height,
            rgba: px.to_vec().repeat(count),
        }
    }

    /// A page whose samples vary, so a lost or shuffled row cannot pass.
    fn gradient(width: u32, height: u32) -> Image {
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                rgba.push(u8::try_from(x & 0xFF).unwrap_or(0));
                rgba.push(u8::try_from(y & 0xFF).unwrap_or(0));
                rgba.push(u8::try_from((x ^ y) & 0xFF).unwrap_or(0));
                rgba.push(255);
            }
        }
        Image {
            width,
            height,
            rgba,
        }
    }

    #[test]
    fn a_page_round_trips_to_its_own_samples() {
        let image = gradient(9, 5);
        let bytes = encode_tiff_one(&image, 6, 150, &mut guard()).expect("encode");
        assert_eq!(&bytes[..2], b"II");
        let pages = decode_tiff(&bytes, &mut guard()).expect("decode");
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0], image, "the file must hold the page it was given");
    }

    #[test]
    fn several_pages_stay_in_order_in_one_file() {
        let first = gradient(7, 3);
        let second = solid(4, 4, [10, 20, 30, 255]);
        let third = gradient(2, 9);
        let strips = [
            tiff_strip(&first, 6, &mut guard()).expect("strip 1"),
            tiff_strip(&second, 6, &mut guard()).expect("strip 2"),
            tiff_strip(&third, 6, &mut guard()).expect("strip 3"),
        ];
        let bytes = encode_tiff(&strips, 96, &mut guard()).expect("encode");
        let pages = decode_tiff(&bytes, &mut guard()).expect("decode");
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0], first);
        assert_eq!(pages[1], second);
        assert_eq!(pages[2], third);
    }

    #[test]
    fn the_same_pages_encode_to_the_same_bytes() {
        let image = gradient(11, 6);
        let first = encode_tiff_one(&image, 6, 150, &mut guard()).expect("first");
        let second = encode_tiff_one(&image, 6, 150, &mut guard()).expect("second");
        assert_eq!(first, second, "a writer that drifts cannot be verified");
    }

    #[test]
    fn the_chain_reports_every_page_without_decoding_them() {
        let strips = [
            tiff_strip(&gradient(5, 2), 6, &mut guard()).expect("a"),
            tiff_strip(&gradient(5, 2), 6, &mut guard()).expect("b"),
        ];
        let bytes = encode_tiff(&strips, 300, &mut guard()).expect("encode");
        let infos = tiff_page_infos(&bytes, &mut guard()).expect("walk");
        assert_eq!(infos.len(), 2);
        assert!(infos.iter().all(|info| info.width == 5 && info.height == 2));
        assert!(infos
            .iter()
            .all(|info| info.compression == COMPRESSION_DEFLATE));
        for info in &infos {
            assert!(tiff_strip_rgb(&bytes, info, &mut guard()).is_ok());
        }
    }

    #[test]
    fn a_transparent_sample_lands_on_white() {
        let image = solid(3, 2, [0, 0, 0, 0]);
        let bytes = encode_tiff_one(&image, 6, 72, &mut guard()).expect("encode");
        let pages = decode_tiff(&bytes, &mut guard()).expect("decode");
        assert_eq!(
            pages[0].rgba[..12],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255]
        );
    }

    #[test]
    fn an_odd_length_strip_leaves_the_next_ifd_aligned() {
        // Two tiny pages: whichever strip lands odd, the chain must still walk
        // and both pages must still read back.
        let file = encode_tiff(
            &[
                tiff_strip(&solid(1, 2, [1, 2, 3, 255]), 0, &mut guard()).expect("a"),
                tiff_strip(&solid(1, 2, [4, 5, 6, 255]), 0, &mut guard()).expect("b"),
            ],
            72,
            &mut guard(),
        )
        .expect("encode");
        let pages = decode_tiff(&file, &mut guard()).expect("decode");
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].rgba[..4], [1, 2, 3, 255]);
        assert_eq!(pages[1].rgba[..4], [4, 5, 6, 255]);
        for info in tiff_page_infos(&file, &mut guard()).expect("walk") {
            assert_eq!(
                info.strip_offset % 2,
                0,
                "every block starts on an even byte"
            );
        }
    }

    #[test]
    fn encode_refuses_pages_that_cannot_be_described() {
        let mut g = guard();
        let strip = tiff_strip(&gradient(3, 3), 6, &mut g).expect("strip");
        assert!(encode_tiff(&[], 72, &mut g).is_err(), "an empty document");
        assert!(encode_tiff(&[strip], 0, &mut g).is_err(), "a zero DPI");

        let oversized = Image {
            width: 70_000,
            height: 1,
            rgba: vec![0; 70_000_usize.saturating_mul(4)],
        };
        assert!(
            tiff_strip(&oversized, 6, &mut g).is_err(),
            "a width a SHORT cannot hold"
        );

        let lying = Image {
            width: 4,
            height: 4,
            rgba: vec![0; 40],
        };
        assert!(
            tiff_strip(&lying, 6, &mut g).is_err(),
            "a buffer that is not the page"
        );
    }

    #[test]
    fn encode_refuses_a_strip_laid_out_by_another_caller() {
        let mut g = guard();
        // `encode_tiff` is public, so a caller can hand it a strip that
        // `tiff_strip` would never produce. Both halves check for themselves.
        let oversized = TiffStrip {
            width: SHORT_MAX.saturating_add(1),
            height: 2,
            data: vec![0; 4],
        };
        assert!(
            encode_tiff(std::slice::from_ref(&oversized), 72, &mut g).is_err(),
            "a dimension a SHORT cannot hold"
        );
        let empty = TiffStrip {
            width: 0,
            height: 0,
            data: Vec::new(),
        };
        assert!(
            encode_tiff(&[empty], 72, &mut g).is_err(),
            "a page with no pixels"
        );
    }

    #[test]
    fn decode_refuses_files_that_are_not_tiff() {
        let mut g = guard();
        assert!(decode_tiff(b"", &mut g).is_err());
        assert!(matches!(
            byte_order(b"PK\x03\x04"),
            Err(error) if error.code() == Code::ImageUnsupported
        ));
        let mut bad_magic = [b'I', b'I', 0, 0, 0, 0, 0, 0];
        bad_magic[2..4].copy_from_slice(&43u16.to_le_bytes());
        assert!(
            decode_tiff(&bad_magic, &mut g).is_err(),
            "magic 43 is not TIFF"
        );
    }

    #[test]
    fn decode_refuses_a_chain_that_loops_or_runs_off_the_file() {
        let bytes = encode_tiff_one(&gradient(6, 4), 6, 150, &mut guard()).expect("encode");
        let mut g = guard();

        // Point the only IFD's NextIFD back at itself: a cycle a naive reader
        // would follow forever. The pointer is the last four bytes of the
        // directory, not of the file — the strip comes after it.
        let next_at = 8 + IFD_LEN - 4;
        let mut looping = bytes.clone();
        looping[next_at..next_at + 4].copy_from_slice(&8u32.to_le_bytes());
        assert!(
            tiff_page_infos(&looping, &mut g).is_err(),
            "a chain must move forward"
        );

        let mut off_end = bytes.clone();
        off_end[next_at..next_at + 4].copy_from_slice(&90_000u32.to_le_bytes());
        assert!(
            tiff_page_infos(&off_end, &mut g).is_err(),
            "an IFD outside the file"
        );

        let truncated = bytes[..bytes.len().saturating_div(2).max(8)].to_vec();
        assert!(
            tiff_page_infos(&truncated, &mut g).is_err(),
            "a file cut inside a strip"
        );
    }

    #[test]
    fn decode_refuses_a_strip_that_lies_about_its_size() {
        let bytes = encode_tiff_one(&gradient(6, 4), 6, 150, &mut guard()).expect("encode");
        let mut infos = tiff_page_infos(&bytes, &mut guard()).expect("walk");
        let info = infos.first_mut().expect("one page");
        info.strip_len = info.strip_len.saturating_add(4);
        let mut g = guard();
        assert!(
            tiff_strip_rgb(&bytes, info, &mut g).is_err(),
            "a strip that claims more than it holds"
        );
    }

    #[test]
    fn decode_names_the_profile_it_does_not_read() {
        let base = hand_rgb(3, 2, vec![0; 18]);
        let mut g = guard();

        // Compression 5 (LZW) is a real TIFF method and outside this profile.
        let lzw = hand_file(&Hand {
            compression: 5,
            ..base.clone()
        });
        let error = tiff_page_infos(&lzw, &mut g).expect_err("LZW is refused");
        assert_eq!(error.code(), Code::ImageUnsupported, "{error:?}");

        let cases = [
            (
                "a 16-bit sample depth",
                Hand {
                    bits: 16,
                    ..base.clone()
                },
            ),
            (
                "a paletted page",
                Hand {
                    photometric: 3,
                    ..base.clone()
                },
            ),
            (
                "planar channels",
                Hand {
                    planar: 2,
                    ..base.clone()
                },
            ),
            (
                "two samples",
                Hand {
                    samples: 2,
                    ..base.clone()
                },
            ),
            (
                "a tiled strip",
                Hand {
                    rows: 1,
                    ..base.clone()
                },
            ),
        ];
        for (what, hand) in cases {
            let file = hand_file(&hand);
            let error = tiff_page_infos(&file, &mut g).expect_err(what);
            assert_eq!(error.code(), Code::ImageUnsupported, "{what}: {error:?}");
        }
    }

    #[test]
    fn an_uncompressed_page_reads_back_exactly() {
        // A file the writer would not produce, to prove the reader is a reader:
        // Compression 1 with the raw samples in the strip.
        let file = hand_file(&hand_rgb(
            2,
            2,
            vec![7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18],
        ));
        let pages = decode_tiff(&file, &mut guard()).expect("decode");
        assert_eq!(pages[0].width, 2);
        assert_eq!(
            pages[0].rgba,
            vec![7, 8, 9, 255, 10, 11, 12, 255, 13, 14, 15, 255, 16, 17, 18, 255]
        );
    }

    #[test]
    fn a_big_endian_file_reads_as_well_as_a_little_endian_one() {
        let raw = vec![7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18];
        let little = hand_file(&hand_rgb(2, 2, raw.clone()));
        let big = hand_file(&Hand {
            big_endian: true,
            ..hand_rgb(2, 2, raw)
        });
        assert_ne!(little, big, "the two files are not the same bytes");
        let from_little = decode_tiff(&little, &mut guard()).expect("decode LE");
        let from_big = decode_tiff(&big, &mut guard()).expect("decode BE");
        assert_eq!(from_little, from_big, "byte order is not a page");
        assert_eq!(from_big[0].rgba[3], 255, "the samples survived the walk");
    }

    #[test]
    fn decode_refuses_a_multi_strip_page() {
        // StripByteCounts with a count of two: the writer never emits it, and
        // guessing at the order of the strips is worse than refusing.
        let mut file = hand_file(&hand_rgb(
            2,
            2,
            vec![7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18],
        ));
        let at = 8 + 2 + 8 * 12;
        let mut entry = [0u8; 12];
        entry[..2].copy_from_slice(&279u16.to_le_bytes());
        entry[2..4].copy_from_slice(&FIELD_LONG.to_le_bytes());
        entry[4..8].copy_from_slice(&2u32.to_le_bytes());
        entry[8..12].copy_from_slice(&9u32.to_le_bytes());
        file[at..at + 12].copy_from_slice(&entry);
        let error = tiff_page_infos(&file, &mut guard()).expect_err("two strips is not one");
        assert_eq!(error.code(), Code::ImageUnsupported, "{error:?}");
    }

    /// A one-page baseline file, laid out by hand.
    ///
    /// The writer refuses to produce most of these, which is the point: the
    /// reader is tested against files it never sees from its own encoder.
    #[derive(Debug, Clone)]
    struct Hand {
        width: u32,
        height: u32,
        compression: u32,
        photometric: u32,
        planar: u32,
        samples: u32,
        bits: u32,
        rows: u32,
        strip: Vec<u8>,
        big_endian: bool,
    }

    /// An RGB page that this profile would accept, carrying `strip`.
    fn hand_rgb(width: u32, height: u32, strip: Vec<u8>) -> Hand {
        Hand {
            width,
            height,
            compression: 1,
            photometric: 2,
            planar: 1,
            samples: 3,
            bits: 8,
            rows: height,
            strip,
            big_endian: false,
        }
    }

    fn hand_file(hand: &Hand) -> Vec<u8> {
        let be = hand.big_endian;
        let values_at: u32 = (8 + 2 + 12 * 10 + 4) as u32;
        let strip_at = values_at + 6;
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(if be { b"MM" } else { b"II" });
        put16(&mut out, MAGIC, be);
        put32(&mut out, 8, be);
        put16(&mut out, 10, be);
        put(&mut out, 256, FIELD_SHORT, 1, hand.width, be);
        put(&mut out, 257, FIELD_SHORT, 1, hand.height, be);
        put(&mut out, 258, FIELD_SHORT, 3, values_at, be);
        put(&mut out, 259, FIELD_SHORT, 1, hand.compression, be);
        put(&mut out, 262, FIELD_SHORT, 1, hand.photometric, be);
        put(&mut out, 273, FIELD_LONG, 1, strip_at, be);
        put(&mut out, 277, FIELD_SHORT, 1, hand.samples, be);
        put(&mut out, 278, FIELD_SHORT, 1, hand.rows, be);
        put(&mut out, 279, FIELD_LONG, 1, hand.strip.len() as u32, be);
        put(&mut out, 284, FIELD_SHORT, 1, hand.planar, be);
        put32(&mut out, 0, be);
        for _ in 0..3 {
            put16(&mut out, hand.bits as u16, be);
        }
        out.extend_from_slice(&hand.strip);
        out
    }

    fn put16(out: &mut Vec<u8>, value: u16, be: bool) {
        let bytes = if be {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        };
        out.extend_from_slice(&bytes);
    }

    fn put32(out: &mut Vec<u8>, value: u32, be: bool) {
        let bytes = if be {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        };
        out.extend_from_slice(&bytes);
    }

    fn put(out: &mut Vec<u8>, tag: u16, field: u16, count: u32, value: u32, be: bool) {
        put16(out, tag, be);
        put16(out, field, be);
        put32(out, count, be);
        // Only a value that fits the 4-byte field lives in it. A `SHORT` pair
        // lives left-justified — in big-endian that means the high byte first,
        // so a 2 is [0x00, 0x02], not the low half of a long. Anything wider
        // (three samples, a rational, a strip) is an offset, and an offset is
        // always a full `LONG`, even when the field type is `SHORT`.
        let width = if field == FIELD_SHORT { 2 } else { 4 };
        if u64::from(count).saturating_mul(width) <= 4 && field == FIELD_SHORT {
            put16(out, value as u16, be);
            put16(out, 0, be);
        } else {
            put32(out, value, be);
        }
    }
}
