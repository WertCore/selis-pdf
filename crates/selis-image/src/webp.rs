//! WebP lossless (VP8L), written and read back by this crate.
//!
//! `PDF → WebP` is the fourth raster output, and the one whose bitstream had
//! no owner: PNG borrows the engine's Deflate writer, JPEG borrows the owned
//! DCT, TIFF wraps either — VP8L is its own entropy code, its own transforms
//! and its own container, so this module carries the whole of it.
//!
//! * [`encode_webp`] turns one rendered page into a `RIFF`/`VP8L` file.
//! * [`decode_webp`] reads one back to the exact samples it was given, which
//!   is what makes the claim in the raster chunk a verified one: a lossless
//!   format either reproduces every sample or it is not lossless.
//!
//! The profile the writer emits is the profile a reader must survive, and it
//! is a real one: a [`PrefixCode`] per channel, LZ77 backward references, a
//! subtract-green transform to decorrelate the achromatic ink of a document,
//! and a predictor transform whose mode is chosen per block. That is enough to
//! beat a filtered PNG on a text page; a card that produced larger files was
//! never going to be a feature.
//!
//! The reader is deliberately narrow, and says so by name when it meets
//! something outside the profile: a color cache, a meta prefix code, a color
//! transform, a color-indexing transform, one of the 120 close-range distance
//! codes, or a chunk that is not `VP8L`, is `IMAGE_UNSUPPORTED` — each is a
//! construct this crate does not write and therefore has no test for. A broken
//! container, an incomplete prefix code, a backward reference that reaches past
//! the image, or a stream that stops early is `IMAGE_MALFORMED`.

use selis_error::{err, Code, Result};
use selis_sandbox::{alloc, BudgetGuard};

use crate::Image;

/// Four bytes after the `RIFF` size, and the four that follow `WEBP`.
const RIFF: [u8; 4] = *b"RIFF";
const WEBP: [u8; 4] = *b"WEBP";
const VP8L: [u8; 4] = *b"VP8L";
/// The byte that must open a `VP8L` payload (RFC 9649 §3.1).
const SIGNATURE: u8 = 0x2F;
/// Both dimensions are 14-bit minus-one fields, so 16384 is the largest side.
const MAX_SIDE: u32 = 16_384;

/// Transform type codes (RFC 9649 §3.5, table 1). The fourth type, color
/// indexing, is value 3; it only means something with a color cache, which this
/// writer never emits, so it has no constant here.
const TX_PREDICTOR: u32 = 0;
const TX_COLOR: u32 = 1;
const TX_SUBTRACT_GREEN: u32 = 2;

/// The green channel's alphabet is 256 literals plus 24 length codes; the color
/// cache would add more, and this crate does not use one.
const GREEN_ALPHABET: usize = 256 + 24;
/// Red, blue and alpha each carry a plain 256-symbol alphabet.
const LITERAL_ALPHABET: usize = 256;
/// Distance codes carry the 40 LZ77 prefix codes.
const DIST_ALPHABET: usize = 40;
/// The alphabet of a prefix code's own code lengths.
const LENGTH_ALPHABET: usize = 19;
/// The order the 19 code lengths are stored in (RFC 9649 §3.7.2.1.2).
const LENGTH_ORDER: [usize; 19] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];

/// The longest code a prefix code may carry.
const MAX_CODE_LEN: usize = 15;
/// Kraft sum of a complete tree, expressed at [`MAX_CODE_LEN`] bits.
const KRAFT_FULL: u64 = 1 << MAX_CODE_LEN;

/// The first 120 distance codes name a close neighbourhood instead of a
/// scan-line offset; this writer only ever uses the offset form.
const CODE_TO_PLANE: u32 = 120;
/// A backward reference may reach this many pixels back at most (RFC 9649
/// §3.6.2.2.1 gives 2^20, and the far codes must also carry the `+120` offset).
const MAX_DISTANCE: usize = 1_048_456;
/// A copy is at most 4096 pixels long, which is why only 24 length codes exist.
const MAX_MATCH: usize = 4096;
/// Shorter than this and two literals are cheaper than one copy.
const MIN_MATCH: usize = 3;
/// Bits of the 3-pixel hash used to bucket candidate positions.
const HASH_BITS: u32 = 16;
/// How many earlier occurrences of a context one position may consult.
const CHAIN_LIMIT: usize = 16;
/// How much capacity [`alloc::grow`] buys a growing vector at a time.
const GROW_STEP: usize = 4096;

/// The ARGB packing a VP8L pixel is: alpha in the top byte, blue in the low one.
type Argb = u32;

// ---------------------------------------------------------------------------
// Bit I/O
// ---------------------------------------------------------------------------

/// A VP8L bit writer.
///
/// Every field the format fixes in width goes out least-significant bit first
/// into bytes of increasing address, which is what [`BitWriter::put_bits`]
/// does. Prefix codes are the exception: they are canonical codes and go out
/// most-significant bit first, which is [`BitWriter::put_code`].
#[derive(Debug)]
struct BitWriter {
    out: Vec<u8>,
    /// Bits already placed in `out` plus the ones waiting in `acc`.
    acc: u64,
    used: u32,
    /// Bytes reserved against the budget so far.
    reserved: usize,
}

impl BitWriter {
    fn new(g: &mut BudgetGuard<'_>, expected: usize) -> Result<Self> {
        let out = alloc::vec_with_capacity::<u8>(g, expected)?;
        Ok(Self {
            out,
            acc: 0,
            used: 0,
            reserved: expected,
        })
    }

    /// Append `n` bits of `value`, low bit first. `n` is at most 32.
    #[allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]
    fn put_bits(&mut self, g: &mut BudgetGuard<'_>, value: u32, n: u32) -> Result<()> {
        if n == 0 {
            return Ok(());
        }
        debug_assert!(n <= 32);
        self.acc |= u64::from(value & mask(n)) << self.used;
        self.used += n;
        while self.used >= 8 {
            self.push_byte((self.acc & 0xFF) as u8, g)?;
            self.acc >>= 8;
            self.used -= 8;
        }
        Ok(())
    }

    /// Append a canonical prefix code: `n` bits of `code`, high bit first.
    #[allow(clippy::arithmetic_side_effects)]
    fn put_code(&mut self, g: &mut BudgetGuard<'_>, code: u32, n: u32) -> Result<()> {
        self.put_bits(g, reverse_bits(n, code), n)
    }

    #[allow(clippy::arithmetic_side_effects)]
    fn push_byte(&mut self, byte: u8, g: &mut BudgetGuard<'_>) -> Result<()> {
        if self.out.len() >= self.reserved {
            alloc::grow(g, &mut self.out, GROW_STEP)?;
            self.reserved = self.reserved.saturating_add(GROW_STEP);
        }
        self.out.push(byte);
        Ok(())
    }

    /// Flush the partial last byte as zero bits and hand over the bytes.
    ///
    /// [`BitWriter::put_bits`] drains whole bytes as it goes, so at most seven
    /// bits are still waiting here.
    #[allow(clippy::arithmetic_side_effects)]
    fn finish(mut self, g: &mut BudgetGuard<'_>) -> Result<Vec<u8>> {
        if self.used > 0 {
            self.push_byte((self.acc & 0xFF) as u8, g)?;
        }
        Ok(self.out)
    }
}

/// A VP8L bit reader over a finished payload. See [`BitWriter`] for the order.
#[derive(Debug)]
struct BitReader<'a> {
    data: &'a [u8],
    /// Next byte not yet folded into `acc`.
    at: usize,
    acc: u64,
    used: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            at: 0,
            acc: 0,
            used: 0,
        }
    }

    /// Bring more whole bytes into the window, up to 56 bits.
    #[allow(clippy::arithmetic_side_effects)]
    fn refill(&mut self) {
        while self.used <= 56 {
            let Some(&byte) = self.data.get(self.at) else {
                break;
            };
            self.acc |= u64::from(byte) << self.used;
            self.at += 1;
            self.used += 8;
        }
    }

    /// Read `n` bits, low bit first. `n` is at most 32.
    #[allow(clippy::arithmetic_side_effects)]
    fn read_bits(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        debug_assert!(n <= 32);
        self.refill();
        if self.used < n {
            return Err(malformed("the bitstream ended inside a fixed-width field"));
        }
        let value = u32::try_from(self.acc & u64::from(mask(n))).unwrap_or(0);
        self.acc >>= n;
        self.used -= n;
        Ok(value)
    }

    #[allow(clippy::arithmetic_side_effects)]
    fn read_bit(&mut self) -> Result<u32> {
        self.read_bits(1)
    }
}

/// A mask of the low `n` bits, for `n` up to 32.
#[allow(clippy::arithmetic_side_effects)]
fn mask(n: u32) -> u32 {
    if n >= 32 {
        u32::MAX
    } else {
        (1u32 << n) - 1
    }
}

/// Reverse the low `n` bits of `value`.
#[allow(clippy::arithmetic_side_effects)]
fn reverse_bits(n: u32, value: u32) -> u32 {
    let mut out = 0u32;
    let mut shift = 0;
    while shift < n {
        if value & (1 << shift) != 0 {
            out |= 1 << (n - 1 - shift);
        }
        shift += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Prefix codes
// ---------------------------------------------------------------------------

/// One canonical prefix code, ready to write or to read with.
///
/// The format carries only the code *lengths*; the codes themselves are the
/// canonical assignment ordered by (length, symbol). A tree with exactly one
/// symbol is legal and is read without consuming any bit, which is what
/// [`PrefixCode::single`] records — an alphabet that never gets used, like the
/// distance code of an image with no backward reference, is coded as a single
/// symbol.
#[derive(Debug, Clone)]
struct PrefixCode {
    /// Code length per symbol; zero means the symbol is not in the tree.
    lengths: Vec<u8>,
    /// Canonical code per symbol, only the low `lengths[i]` bits significant.
    codes: Vec<u32>,
    /// Symbols ordered by (length, symbol).
    sorted: Vec<u32>,
    /// How many symbols carry each length.
    count_by_len: [u32; MAX_CODE_LEN + 1],
    /// First canonical code of each length.
    first_by_len: [u32; MAX_CODE_LEN + 1],
    /// `Some(symbol)` when the tree is a single leaf.
    single: Option<u32>,
}

impl PrefixCode {
    /// Build the tree a histogram of symbol counts implies, capped at
    /// [`MAX_CODE_LEN`] bits per code.
    ///
    /// The lengths come from a two-queue Huffman build over the nonzero counts.
    /// A skewed histogram can push a code past 15 bits, which VP8L does not
    /// allow, so the build repeats with every count raised to a floor that
    /// doubles each time — the same trick as holding a minimum population —
    /// until the tree is shallow enough. The floor only ever *adds* symbols to
    /// the tree, so the result stays a complete tree and decodes correctly; it
    /// just spends a few bits on symbols that never appear.
    fn from_counts(g: &mut BudgetGuard<'_>, counts: &[u32]) -> Result<Self> {
        let alive: Vec<usize> = counts
            .iter()
            .enumerate()
            .filter_map(|(symbol, &count)| if count > 0 { Some(symbol) } else { None })
            .collect();
        match alive.len() {
            0 => {
                // An empty code is coded as a single symbol 0 (RFC 9649
                // §3.7.2.1.1). Nothing is ever read from it.
                let mut lengths = vec![0u8; counts.len()];
                if let Some(slot) = lengths.first_mut() {
                    *slot = 1;
                }
                return Self::from_lengths(g, lengths);
            }
            1 => {
                let mut lengths = vec![0u8; counts.len()];
                if let Some(&symbol) = alive.first() {
                    if let Some(slot) = lengths.get_mut(symbol) {
                        *slot = 1;
                    }
                }
                return Self::from_lengths(g, lengths);
            }
            _ => {}
        }
        let mut floor: u64 = 1;
        loop {
            let lengths = huffman_lengths(g, counts, floor)?;
            let too_deep = lengths
                .iter()
                .any(|&length| u64::from(length) > MAX_CODE_LEN as u64);
            if !too_deep {
                return Self::from_lengths(g, lengths);
            }
            floor = floor.saturating_mul(2);
            if floor > u64::from(u32::MAX) {
                return Err(malformed(
                    "no prefix code of 15 bits or less fits the histogram",
                ));
            }
        }
    }

    /// Adopt explicit lengths, as the reader does after decoding them.
    #[allow(clippy::indexing_slicing)]
    fn from_lengths(g: &mut BudgetGuard<'_>, lengths: Vec<u8>) -> Result<Self> {
        let alphabet = lengths.len();
        let mut count_by_len = [0u32; MAX_CODE_LEN + 1];
        let mut nonzero = 0usize;
        for length in &lengths {
            let slot = usize::from(*length);
            if slot > MAX_CODE_LEN {
                return Err(malformed("a prefix code longer than 15 bits"));
            }
            if slot > 0 {
                count_by_len[slot] = count_by_len[slot].saturating_add(1);
                nonzero = nonzero.saturating_add(1);
            }
        }
        if nonzero == 0 {
            return Err(malformed("an empty prefix code table"));
        }
        let single = if nonzero == 1 {
            lengths
                .iter()
                .position(|&length| length > 0)
                .and_then(|symbol| u32::try_from(symbol).ok())
        } else {
            None
        };
        if single.is_some() {
            // A single leaf is read without bits; the format insists it carries
            // length 1, and anything else is a stream that cannot be decoded.
            if !matches!(lengths.iter().find(|&&length| length > 0), Some(1)) {
                return Err(malformed("a single-leaf prefix code longer than one bit"));
            }
            let codes = vec![0u32; alphabet];
            let sorted: Vec<u32> = single.into_iter().collect();
            return Ok(Self {
                lengths,
                codes,
                sorted,
                count_by_len,
                first_by_len: [0; MAX_CODE_LEN + 1],
                single,
            });
        }
        // A complete tree spends its whole key space: sum of 2^(15-len) == 2^15.
        let mut kraft: u64 = 0;
        for length in &lengths {
            if *length > 0 {
                kraft =
                    kraft.saturating_add(1u64 << MAX_CODE_LEN.saturating_sub(usize::from(*length)));
            }
        }
        if kraft != KRAFT_FULL {
            return Err(malformed("an incomplete prefix code"));
        }
        let mut first_by_len = [0u32; MAX_CODE_LEN + 1];
        let mut next = 0u32;
        for length in 1..=MAX_CODE_LEN {
            first_by_len[length] = next;
            next = next
                .saturating_add(count_by_len[length])
                .checked_shl(1)
                .unwrap_or(next);
        }
        let mut cursor = first_by_len;
        let mut codes = alloc::vec_with_capacity::<u32>(g, alphabet)?;
        for &length in &lengths {
            if length == 0 {
                codes.push(0);
                continue;
            }
            let slot = usize::from(length);
            let code = cursor.get(slot).copied().unwrap_or(0);
            if let Some(cell) = cursor.get_mut(slot) {
                *cell = code.saturating_add(1);
            }
            codes.push(code);
        }
        let mut sorted: Vec<u32> = Vec::new();
        for length in 1..=MAX_CODE_LEN {
            for (symbol, &cell) in lengths.iter().enumerate() {
                if usize::from(cell) == length {
                    sorted.push(u32::try_from(symbol).unwrap_or(u32::MAX));
                }
            }
        }
        Ok(Self {
            lengths,
            codes,
            sorted,
            count_by_len,
            first_by_len,
            single: None,
        })
    }

    /// Read the next symbol. A single-leaf tree consumes nothing.
    #[allow(clippy::arithmetic_side_effects, clippy::integer_division)]
    fn read(&self, br: &mut BitReader<'_>) -> Result<u32> {
        if let Some(symbol) = self.single {
            return Ok(symbol);
        }
        let mut code = 0u32;
        let mut index = 0usize;
        for length in 1..=MAX_CODE_LEN {
            code = code
                .checked_shl(1)
                .unwrap_or(0)
                .saturating_add(br.read_bit()?);
            let count = self.count_by_len.get(length).copied().unwrap_or(0);
            // A symbol of this length class is the offset of the read value
            // inside the class, measured from the class's first canonical code.
            let first = self.first_by_len.get(length).copied().unwrap_or(0);
            if code.wrapping_sub(first) < count {
                let offset = index.saturating_add(
                    usize::try_from(code.wrapping_sub(first)).unwrap_or(usize::MAX),
                );
                return self
                    .sorted
                    .get(offset)
                    .copied()
                    .ok_or_else(|| malformed("a prefix code out of range"));
            }
            index = index.saturating_add(usize::try_from(count).unwrap_or(usize::MAX));
        }
        Err(malformed("a prefix code longer than 15 bits"))
    }

    /// Write the code itself: the simple form for one or two symbols, the
    /// normal form otherwise (RFC 9649 §3.7.2.1).
    fn write(&self, g: &mut BudgetGuard<'_>, bw: &mut BitWriter) -> Result<()> {
        let used: Vec<usize> = self
            .lengths
            .iter()
            .enumerate()
            .filter_map(|(symbol, &length)| if length > 0 { Some(symbol) } else { None })
            .collect();
        // The simple form has 8 bits per symbol, so it can only describe
        // literals; a green symbol that means a length or a code that means a
        // plane has to go through the normal form (§3.7.2.1.1).
        if used.iter().all(|&symbol| symbol <= 255) {
            if let Some(&symbol) = used.first() {
                if used.len() == 1 {
                    return self.write_simple(g, bw, symbol, None);
                }
                if used.len() == 2 {
                    let second = used.get(1).copied().unwrap_or(symbol);
                    return self.write_simple(g, bw, symbol, Some(second));
                }
            }
        }
        bw.put_bits(g, 0, 1)?;
        self.write_normal(g, bw)
    }

    /// `1` then the one or two symbols that carry a length of 1 bit each.
    fn write_simple(
        &self,
        g: &mut BudgetGuard<'_>,
        bw: &mut BitWriter,
        first: usize,
        second: Option<usize>,
    ) -> Result<()> {
        bw.put_bits(g, 1, 1)?;
        bw.put_bits(g, u32::try_from(second.map_or(0, |_| 1)).unwrap_or(0), 1)?;
        let narrow = first <= 1;
        bw.put_bits(g, u32::from(!narrow), 1)?;
        if narrow {
            bw.put_bits(g, u32::try_from(first).unwrap_or(0), 1)?;
        } else {
            bw.put_bits(g, u32::try_from(first).unwrap_or(0), 8)?;
        }
        if let Some(symbol) = second {
            bw.put_bits(g, u32::try_from(symbol).unwrap_or(0), 8)?;
        }
        Ok(())
    }

    /// The code lengths of the code, coded with a 19-symbol prefix code of
    /// their own, run-length compressed (RFC 9649 §3.7.2.1.2).
    fn write_normal(&self, g: &mut BudgetGuard<'_>, bw: &mut BitWriter) -> Result<()> {
        let alphabet = self.lengths.len();
        let coded = self
            .lengths
            .iter()
            .rposition(|&length| length > 0)
            .map_or(0, |last| last.saturating_add(1));
        // Truncating the array at its last used entry is only worth it when the
        // token count then says where to stop; a code that uses its whole
        // alphabet, or compacts to fewer than two tokens, says nothing.
        let truncated_alphabet = if coded == 0 || coded >= alphabet {
            alphabet
        } else if length_tokens(&self.lengths, coded).len() >= 2 {
            coded
        } else {
            alphabet
        };
        let truncated = length_tokens(&self.lengths, truncated_alphabet);
        let mut counts = vec![0u32; LENGTH_ALPHABET];
        for token in &truncated {
            if let Some(slot) = counts.get_mut(usize::from(token.symbol)) {
                *slot = slot.saturating_add(1);
            }
        }
        let length_code = PrefixCode::from_counts(g, &counts)?;
        // The 3-bit lengths are stored in [`LENGTH_ORDER`] slots, so the count
        // of them is a slot index, not a symbol: symbol 15 sits in the last
        // slot, and stopping at "the highest symbol with a length" would leave
        // the reader with an incomplete tree.
        let needed = LENGTH_ORDER
            .iter()
            .rposition(|&symbol| length_code.lengths.get(symbol).copied().unwrap_or(0) > 0)
            .map_or(0, |slot| slot.saturating_add(1));
        let num_code_lengths = needed.clamp(4, LENGTH_ALPHABET);
        // The field order the reader walks: the code lengths first, then the
        // marker that says how many entries they cover, then the tokens.
        bw.put_bits(
            g,
            u32::try_from(num_code_lengths.saturating_sub(4)).unwrap_or(0),
            4,
        )?;
        for slot in 0..num_code_lengths {
            let symbol = LENGTH_ORDER.get(slot).copied().unwrap_or(0);
            let length = length_code.lengths.get(symbol).copied().unwrap_or(0);
            bw.put_bits(g, u32::from(length), 3)?;
        }
        if truncated_alphabet == alphabet {
            bw.put_bits(g, 0, 1)?;
        } else {
            let tokens = u32::try_from(truncated.len()).unwrap_or(2);
            let (index, nbits) = max_symbol_field(tokens);
            bw.put_bits(g, 1, 1)?;
            bw.put_bits(g, index, 3)?;
            bw.put_bits(g, tokens.saturating_sub(2), nbits)?;
        }
        for token in &truncated {
            let symbol = usize::from(token.symbol);
            let code = length_code.codes.get(symbol).copied().unwrap_or_default();
            let len = u32::from(length_code.lengths.get(symbol).copied().unwrap_or(0));
            bw.put_code(g, code, len)?;
            bw.put_bits(g, token.extra, token.extra_bits)?;
        }
        Ok(())
    }

    /// Decode a code of `alphabet` symbols from the stream.
    fn read_code(g: &mut BudgetGuard<'_>, br: &mut BitReader<'_>, alphabet: usize) -> Result<Self> {
        if br.read_bit()? == 1 {
            let num_symbols = br.read_bits(1)?.saturating_add(1);
            let is_first_8bits = br.read_bits(1)?;
            let mut lengths = vec![0u8; alphabet];
            let first = br.read_bits(if is_first_8bits == 0 { 1 } else { 8 })?;
            if let Some(slot) = lengths.get_mut(usize::try_from(first).unwrap_or(usize::MAX)) {
                *slot = 1;
            } else if usize::try_from(first).unwrap_or(usize::MAX) >= alphabet {
                return Err(malformed(
                    "a simple prefix code symbol outside the alphabet",
                ));
            }
            if num_symbols == 2 {
                let second = br.read_bits(8)?;
                if usize::try_from(second).unwrap_or(usize::MAX) >= alphabet {
                    return Err(malformed(
                        "a simple prefix code symbol outside the alphabet",
                    ));
                }
                if let Some(slot) = lengths.get_mut(usize::try_from(second).unwrap_or(usize::MAX)) {
                    *slot = 1;
                }
            }
            return Self::from_lengths(g, lengths);
        }
        let num_code_lengths = br.read_bits(4)?.saturating_add(4);
        if usize::try_from(num_code_lengths).unwrap_or(usize::MAX) > LENGTH_ALPHABET {
            return Err(malformed("more code lengths than the 19-symbol alphabet"));
        }
        let mut outer_lengths = vec![0u8; LENGTH_ALPHABET];
        for slot in 0..usize::try_from(num_code_lengths).unwrap_or(0) {
            let symbol = LENGTH_ORDER.get(slot).copied().unwrap_or(0);
            if let Some(cell) = outer_lengths.get_mut(symbol) {
                *cell = u8::try_from(br.read_bits(3)?).unwrap_or(0);
            }
        }
        let outer = PrefixCode::from_lengths(g, outer_lengths)?;
        let use_length = br.read_bit()?;
        let max_symbol = if use_length == 1 {
            let length_nbits = 2u32.saturating_add(2u32.saturating_mul(br.read_bits(3)?));
            if length_nbits > 16 {
                return Err(malformed("a max_symbol field longer than 16 bits"));
            }
            2u32.saturating_add(br.read_bits(length_nbits)?)
        } else {
            u32::try_from(alphabet).unwrap_or(u32::MAX)
        };
        if usize::try_from(max_symbol).unwrap_or(usize::MAX) > alphabet {
            return Err(malformed("max_symbol is larger than the alphabet"));
        }
        let mut lengths = vec![0u8; alphabet];
        let mut symbol = 0usize;
        let mut tokens = 0u32;
        let mut previous: u8 = 8;
        while symbol < alphabet && tokens < max_symbol {
            tokens = tokens.saturating_add(1);
            let read = outer.read(br)?;
            if read < 16 {
                if let Some(cell) = lengths.get_mut(symbol) {
                    *cell = u8::try_from(read).unwrap_or(0);
                }
                if read > 0 {
                    previous = u8::try_from(read).unwrap_or(8);
                }
                symbol = symbol.saturating_add(1);
                continue;
            }
            let (repeat, value) = match read {
                16 => (3u32.saturating_add(br.read_bits(2)?), previous),
                17 => (3u32.saturating_add(br.read_bits(3)?), 0),
                18 => (11u32.saturating_add(br.read_bits(7)?), 0),
                _ => return Err(malformed("a code length symbol above 18")),
            };
            let repeat = usize::try_from(repeat).unwrap_or(usize::MAX);
            if symbol.saturating_add(repeat) > alphabet {
                return Err(malformed(
                    "a run of code lengths past the end of the alphabet",
                ));
            }
            for _ in 0..repeat {
                if let Some(cell) = lengths.get_mut(symbol) {
                    *cell = value;
                }
                symbol = symbol.saturating_add(1);
            }
        }
        if symbol < alphabet && tokens >= max_symbol && use_length == 1 {
            // Entries the stream never reached stay zero, which is only sound
            // if the tree is still complete.
            let nonzero = lengths.iter().filter(|&&length| length > 0).count();
            if nonzero == 0 {
                return Err(malformed("a prefix code with no symbols"));
            }
        }
        Self::from_lengths(g, lengths)
    }
}

/// One token of a code-length run: a symbol in 0..=18 and its extra bits.
#[derive(Debug, Clone, Copy)]
struct LengthToken {
    symbol: u8,
    extra: u32,
    extra_bits: u32,
}

/// Cover `lengths[..count]` with the fewest run tokens the format offers.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::indexing_slicing
)]
fn length_tokens(lengths: &[u8], count: usize) -> Vec<LengthToken> {
    let limit = count.min(lengths.len());
    let mut tokens = Vec::new();
    let mut at = 0usize;
    while at < limit {
        let value = lengths[at];
        let mut run = 1usize;
        while at + run < limit && lengths[at + run] == value {
            run += 1;
        }
        if value == 0 {
            let mut left = run;
            while left >= 11 {
                let take = left.min(138);
                tokens.push(LengthToken {
                    symbol: 18,
                    extra: u32::try_from(take - 11).unwrap_or(0),
                    extra_bits: 7,
                });
                left -= take;
            }
            while left >= 3 {
                let take = left.min(10);
                tokens.push(LengthToken {
                    symbol: 17,
                    extra: u32::try_from(take - 3).unwrap_or(0),
                    extra_bits: 3,
                });
                left -= take;
            }
            for _ in 0..left {
                tokens.push(LengthToken {
                    symbol: 0,
                    extra: 0,
                    extra_bits: 0,
                });
            }
        } else {
            tokens.push(LengthToken {
                symbol: value,
                extra: 0,
                extra_bits: 0,
            });
            let mut left = run - 1;
            while left >= 3 {
                let take = left.min(6);
                tokens.push(LengthToken {
                    symbol: 16,
                    extra: u32::try_from(take - 3).unwrap_or(0),
                    extra_bits: 2,
                });
                left -= take;
            }
            for _ in 0..left {
                tokens.push(LengthToken {
                    symbol: value,
                    extra: 0,
                    extra_bits: 0,
                });
            }
        }
        at += run;
    }
    tokens
}

/// The 3-bit index and the value width that can carry `tokens` in a
/// `max_symbol` field.
#[allow(clippy::arithmetic_side_effects)]
fn max_symbol_field(tokens: u32) -> (u32, u32) {
    let mut index = 0u32;
    while index < 8 {
        let length_nbits = 2 + 2 * index;
        if tokens.saturating_sub(2) <= mask(length_nbits) {
            return (index, length_nbits);
        }
        index += 1;
    }
    (7, 16)
}

/// Build a Huffman tree over the nonzero counts (floored to `floor`) and
/// return a length per symbol.
///
/// The two queues hold `leaves + merges` entries and every merge takes two of
/// them before adding one back, so an index taken from a queue head is always
/// inside the parallel `weight`/`left`/`right` vectors it is used against.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::indexing_slicing
)]
fn huffman_lengths(g: &mut BudgetGuard<'_>, counts: &[u32], floor: u64) -> Result<Vec<u8>> {
    let mut weights: Vec<(u64, usize)> = Vec::new();
    for (symbol, &count) in counts.iter().enumerate() {
        let weight = u64::from(count).max(floor);
        if weight > floor || count > 0 {
            weights.push((weight, symbol));
        }
    }
    weights.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let leaves = weights.len();
    if leaves < 2 {
        let mut lengths = vec![0u8; counts.len()];
        for (weight, symbol) in &weights {
            if *weight > floor || counts.get(*symbol).copied().unwrap_or(0) > 0 {
                if let Some(cell) = lengths.get_mut(*symbol) {
                    *cell = 1;
                }
            }
        }
        return Ok(lengths);
    }
    let mut weight: Vec<u64> = alloc::vec_with_capacity::<u64>(g, leaves.saturating_mul(2))?;
    let mut left: Vec<usize> = alloc::vec_with_capacity::<usize>(g, leaves.saturating_mul(2))?;
    let mut right: Vec<usize> = alloc::vec_with_capacity::<usize>(g, leaves.saturating_mul(2))?;
    for (w, _) in &weights {
        weight.push(*w);
        left.push(usize::MAX);
        right.push(usize::MAX);
    }
    // Two queues: the sorted leaves, then the internals in creation order, both
    // non-decreasing, so the smallest of the whole forest is always at one head.
    let mut leaf_at = 0usize;
    let mut internal_at = leaves;
    for _ in 1..leaves {
        let mut pick = [0usize; 2];
        for slot in pick.iter_mut() {
            let leaf = weights.get(leaf_at);
            let internal = weight.get(internal_at);
            let take_leaf = match (leaf, internal) {
                (Some((lw, _)), Some(iw)) => *lw <= *iw,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => false,
            };
            if take_leaf {
                *slot = leaf_at;
                leaf_at += 1;
            } else {
                *slot = internal_at;
                internal_at += 1;
            }
        }
        let a = pick[0];
        let b = pick[1];
        let sum = weight[a].saturating_add(weight[b]);
        weight.push(sum);
        left.push(a);
        right.push(b);
    }
    let total = weight.len();
    let mut depth: Vec<u32> = alloc::vec_with_capacity::<u32>(g, total)?;
    depth.resize(total, 0);
    // A parent is always created after its children, so walking the internals
    // backwards pushes each child's depth before its own turn comes.
    let mut node = total - 1;
    while node >= leaves {
        let d = depth[node];
        if let Some(cell) = left.get(node) {
            if *cell != usize::MAX {
                if let Some(child) = depth.get_mut(*cell) {
                    *child = d + 1;
                }
            }
        }
        if let Some(cell) = right.get(node) {
            if *cell != usize::MAX {
                if let Some(child) = depth.get_mut(*cell) {
                    *child = d + 1;
                }
            }
        }
        node -= 1;
    }
    let mut lengths = vec![0u8; counts.len()];
    for symbol in 0..leaves {
        let real = weights[symbol].1;
        if let Some(cell) = lengths.get_mut(real) {
            *cell = u8::try_from(depth[symbol]).unwrap_or(u8::MAX);
        }
    }
    Ok(lengths)
}

// ---------------------------------------------------------------------------
// LZ77 prefix coding
// ---------------------------------------------------------------------------

/// Split an LZ77 length or distance into its prefix code and extra bits.
///
/// Values 1..=4 are their own prefix codes; above that the prefix code `p`
/// covers `(2 + (p & 1)) << ((p - 2) >> 1) + 1` through the same plus
/// `2^((p - 2) >> 1)` (RFC 9649 §3.6.2.1, table 4).
#[allow(clippy::arithmetic_side_effects)]
fn prefix_encode(value: u32) -> Option<(u32, u32, u32)> {
    if value == 0 {
        return None;
    }
    if value <= 4 {
        return Some((value - 1, 0, 0));
    }
    let mut code = 4u32;
    while code < 40 {
        let extra_bits = (code - 2) >> 1;
        let offset = (2 + (code & 1)) << extra_bits;
        let high = offset + (1 << extra_bits);
        if value > offset && value <= high {
            return Some((code, extra_bits, value - offset - 1));
        }
        if high >= 1_048_576 {
            break;
        }
        code += 1;
    }
    None
}

/// Rebuild the value a prefix code and its extra bits stand for.
#[allow(clippy::arithmetic_side_effects)]
fn prefix_decode(code: u32, extra: u32) -> u32 {
    if code < 4 {
        return code + 1;
    }
    let extra_bits = (code - 2) >> 1;
    let offset = (2 + (code & 1)) << extra_bits;
    offset + extra + 1
}

/// One symbol of an entropy-coded image: a literal pixel, or a copy of a run
/// of pixels from `distance` positions back.
#[derive(Debug, Clone, Copy)]
enum Item {
    Literal(Argb),
    Copy { length: u32, distance: u32 },
}

/// The five alphabets one entropy-coded image carries, in bitstream order.
struct CodeGroup {
    green: PrefixCode,
    red: PrefixCode,
    blue: PrefixCode,
    alpha: PrefixCode,
    dist: PrefixCode,
}

/// Greedy longest-match scan over already-transformed pixels.
///
/// Every position hashes the three pixels that start there and consults up to
/// [`CHAIN_LIMIT`] earlier occurrences of the same hash, newest first, so a run
/// of one colour finds the copy starting one pixel behind it and can reach the
/// full 4096 pixels. Candidates are dropped once they are further back than
/// [`MAX_DISTANCE`], which the chain reaches monotonically.
///
/// Pixel positions are kept in `u32` chains; the caller caps a side at
/// [`MAX_SIDE`], so an image holds at most 2^28 pixels and every index fits.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation
)]
fn lz_scan(g: &mut BudgetGuard<'_>, pixels: &[u32]) -> Result<Vec<Item>> {
    let count = pixels.len();
    let mut items: Vec<Item> = Vec::new();
    if count == 0 {
        return Ok(items);
    }
    if items.capacity() == 0 {
        alloc::grow(g, &mut items, GROW_STEP)?;
    }
    let mut head: Vec<u32> = alloc::vec_filled(g, 1usize << HASH_BITS, u32::MAX)?;
    let mut prev: Vec<u32> = alloc::vec_filled(g, count, u32::MAX)?;
    let mut pos = 0usize;
    while pos < count {
        let bucket = if pos + MIN_MATCH <= count {
            Some(hash3(pixels, pos))
        } else {
            None
        };
        let mut best_len = 0usize;
        let mut best_dist = usize::MAX;
        if let Some(bucket) = bucket {
            let ceiling = (count - pos).min(MAX_MATCH);
            let mut candidate = head[bucket];
            let mut hops = 0usize;
            while candidate != u32::MAX && hops < CHAIN_LIMIT {
                let from = usize::try_from(candidate).unwrap_or(usize::MAX);
                if from >= pos {
                    break;
                }
                let distance = pos - from;
                if distance > MAX_DISTANCE {
                    break;
                }
                let mut length = 0usize;
                while length < ceiling && pixels[from + length] == pixels[pos + length] {
                    length += 1;
                }
                if length > best_len || (length == best_len && distance < best_dist) {
                    best_len = length;
                    best_dist = distance;
                }
                if length >= ceiling {
                    break;
                }
                candidate = prev[from];
                hops += 1;
            }
            prev[pos] = head[bucket];
            head[bucket] = pos as u32;
        }
        if best_len >= MIN_MATCH {
            if items.len() == items.capacity() {
                alloc::grow(g, &mut items, GROW_STEP)?;
            }
            items.push(Item::Copy {
                length: best_len as u32,
                distance: best_dist as u32,
            });
            let end = pos + best_len;
            pos += 1;
            while pos < end {
                if pos + MIN_MATCH <= count {
                    let bucket = hash3(pixels, pos);
                    prev[pos] = head[bucket];
                    head[bucket] = pos as u32;
                }
                pos += 1;
            }
        } else {
            if items.len() == items.capacity() {
                alloc::grow(g, &mut items, GROW_STEP)?;
            }
            items.push(Item::Literal(pixels[pos]));
            pos += 1;
        }
    }
    Ok(items)
}

/// The hash bucket the three pixels starting at `at` fall into.
#[allow(clippy::arithmetic_side_effects)]
fn hash3(pixels: &[u32], at: usize) -> usize {
    let mut mix = 0x9E37_79B1_u64;
    for offset in 0..MIN_MATCH {
        let pixel = u64::from(pixels.get(at + offset).copied().unwrap_or(0));
        mix = (mix ^ pixel).wrapping_mul(0x0000_0100_0000_0193);
    }
    ((mix >> 32) as usize) & ((1usize << HASH_BITS) - 1)
}

/// The five alphabets of one entropy-coded image, before they become trees.
struct Histograms {
    green: Vec<u32>,
    red: Vec<u32>,
    blue: Vec<u32>,
    alpha: Vec<u32>,
    dist: Vec<u32>,
}

/// Tally what a scan actually spends: one green symbol per item, three channel
/// symbols per literal, one distance symbol per copy.
#[allow(clippy::arithmetic_side_effects)]
fn histograms(g: &mut BudgetGuard<'_>, items: &[Item]) -> Result<Histograms> {
    let mut h = Histograms {
        green: alloc::vec_filled(g, GREEN_ALPHABET, 0)?,
        red: alloc::vec_filled(g, LITERAL_ALPHABET, 0)?,
        blue: alloc::vec_filled(g, LITERAL_ALPHABET, 0)?,
        alpha: alloc::vec_filled(g, LITERAL_ALPHABET, 0)?,
        dist: alloc::vec_filled(g, DIST_ALPHABET, 0)?,
    };
    for item in items {
        match *item {
            Item::Literal(pixel) => {
                add(&mut h.green, channel(pixel, 8));
                add(&mut h.red, channel(pixel, 16));
                add(&mut h.blue, channel(pixel, 0));
                add(&mut h.alpha, channel(pixel, 24));
            }
            Item::Copy { length, distance } => {
                let plane = distance.saturating_add(CODE_TO_PLANE);
                let Some((length_code, _, _)) = prefix_encode(length) else {
                    return Err(malformed("an LZ77 length out of range"));
                };
                let Some((dist_code, _, _)) = prefix_encode(plane) else {
                    return Err(malformed("an LZ77 distance out of range"));
                };
                add(
                    &mut h.green,
                    256usize.saturating_add(usize::try_from(length_code).unwrap_or(0)),
                );
                add(&mut h.dist, usize::try_from(dist_code).unwrap_or(0));
            }
        }
    }
    Ok(h)
}

/// Count one symbol into one histogram.
#[allow(clippy::arithmetic_side_effects)]
fn add(counts: &mut [u32], symbol: usize) {
    if let Some(cell) = counts.get_mut(symbol) {
        *cell = cell.saturating_add(1);
    }
}

/// One ARGB channel of a packed pixel.
fn channel(pixel: Argb, shift: u32) -> usize {
    usize::try_from((pixel >> shift) & 0xFF).unwrap_or(0)
}
// ---------------------------------------------------------------------------
// Entropy-coded images
// ---------------------------------------------------------------------------

/// Match the five histograms to five trees.
fn build_group(g: &mut BudgetGuard<'_>, h: &Histograms) -> Result<CodeGroup> {
    Ok(CodeGroup {
        green: PrefixCode::from_counts(g, &h.green)?,
        red: PrefixCode::from_counts(g, &h.red)?,
        blue: PrefixCode::from_counts(g, &h.blue)?,
        alpha: PrefixCode::from_counts(g, &h.alpha)?,
        dist: PrefixCode::from_counts(g, &h.dist)?,
    })
}

/// Write one entropy-coded image: the color cache marker, five prefix codes,
/// then the pixels a scan produced.
///
/// A *spatial* image is the ARGB image itself and carries a meta prefix marker
/// between the two; a sub-resolution image — a predictor's mode grid — does not
/// (RFC 9649 §3.8.3).
fn write_entropy_image(
    g: &mut BudgetGuard<'_>,
    bw: &mut BitWriter,
    pixels: &[Argb],
    spatial: bool,
) -> Result<()> {
    bw.put_bits(g, 0, 1)?;
    if spatial {
        bw.put_bits(g, 0, 1)?;
    }
    let items = lz_scan(g, pixels)?;
    let h = histograms(g, &items)?;
    let group = build_group(g, &h)?;
    group.green.write(g, bw)?;
    group.red.write(g, bw)?;
    group.blue.write(g, bw)?;
    group.alpha.write(g, bw)?;
    group.dist.write(g, bw)?;
    write_items(g, bw, pixels, &items, &group)
}

/// Read one entropy-coded image of `count` pixels back. See
/// [`write_entropy_image`] for what `spatial` means.
fn read_entropy_image(
    g: &mut BudgetGuard<'_>,
    br: &mut BitReader<'_>,
    count: usize,
    spatial: bool,
) -> Result<Vec<Argb>> {
    if br.read_bit()? == 1 {
        return Err(unsupported("a color cache"));
    }
    if spatial && br.read_bit()? == 1 {
        return Err(unsupported("meta prefix codes"));
    }
    let group = CodeGroup {
        green: PrefixCode::read_code(g, br, GREEN_ALPHABET)?,
        red: PrefixCode::read_code(g, br, LITERAL_ALPHABET)?,
        blue: PrefixCode::read_code(g, br, LITERAL_ALPHABET)?,
        alpha: PrefixCode::read_code(g, br, LITERAL_ALPHABET)?,
        dist: PrefixCode::read_code(g, br, DIST_ALPHABET)?,
    };
    read_items(g, br, count, &group)
}

/// Emit the pixels of one entropy-coded image.
#[allow(clippy::arithmetic_side_effects)]
fn write_items(
    g: &mut BudgetGuard<'_>,
    bw: &mut BitWriter,
    pixels: &[Argb],
    items: &[Item],
    group: &CodeGroup,
) -> Result<()> {
    let mut at = 0usize;
    for item in items {
        match *item {
            Item::Literal(pixel) => {
                emit(g, bw, &group.green, channel(pixel, 8))?;
                emit(g, bw, &group.red, channel(pixel, 16))?;
                emit(g, bw, &group.blue, channel(pixel, 0))?;
                emit(g, bw, &group.alpha, channel(pixel, 24))?;
                at += 1;
            }
            Item::Copy { length, distance } => {
                let plane = distance.saturating_add(CODE_TO_PLANE);
                let Some((length_code, length_bits, length_extra)) = prefix_encode(length) else {
                    return Err(malformed("an LZ77 length out of range"));
                };
                let Some((dist_code, dist_bits, dist_extra)) = prefix_encode(plane) else {
                    return Err(malformed("an LZ77 distance out of range"));
                };
                emit(
                    g,
                    bw,
                    &group.green,
                    256usize.saturating_add(usize::try_from(length_code).unwrap_or(0)),
                )?;
                bw.put_bits(g, length_extra, length_bits)?;
                emit(g, bw, &group.dist, usize::try_from(dist_code).unwrap_or(0))?;
                bw.put_bits(g, dist_extra, dist_bits)?;
                at += usize::try_from(length).unwrap_or(0);
            }
        }
    }
    if at != pixels.len() {
        return Err(malformed("the LZ77 scan did not cover the image"));
    }
    Ok(())
}

/// Write one symbol of one alphabet. A single-leaf tree spends no bit at all,
/// which is how the format reads it (RFC 9649 §3.7.2.1).
fn emit(
    g: &mut BudgetGuard<'_>,
    bw: &mut BitWriter,
    code: &PrefixCode,
    symbol: usize,
) -> Result<()> {
    if code.single.is_some() {
        return Ok(());
    }
    let length = u32::from(code.lengths.get(symbol).copied().unwrap_or(0));
    if length == 0 {
        return Err(malformed("a symbol the prefix code does not carry"));
    }
    let value = code.codes.get(symbol).copied().unwrap_or(0);
    bw.put_code(g, value, length)
}

/// Decode the pixels of one entropy-coded image.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::indexing_slicing
)]
fn read_items(
    g: &mut BudgetGuard<'_>,
    br: &mut BitReader<'_>,
    count: usize,
    group: &CodeGroup,
) -> Result<Vec<Argb>> {
    let mut pixels: Vec<Argb> = alloc::vec_with_capacity(g, count)?;
    while pixels.len() < count {
        let symbol = group.green.read(br)?;
        if symbol < 256 {
            let red = group.red.read(br)?;
            let blue = group.blue.read(br)?;
            let alpha = group.alpha.read(br)?;
            pixels.push((alpha << 24) | (red << 16) | (symbol << 8) | blue);
            continue;
        }
        if symbol >= 280 {
            return Err(unsupported("a color cache symbol"));
        }
        let length_code = symbol - 256;
        let length = prefix_decode(length_code, br.read_bits(prefix_extra_bits(length_code))?);
        let dist_code = group.dist.read(br)?;
        let plane = prefix_decode(dist_code, br.read_bits(prefix_extra_bits(dist_code))?);
        if plane <= CODE_TO_PLANE {
            return Err(unsupported("a close-range LZ77 distance code"));
        }
        let distance = usize::try_from(plane - CODE_TO_PLANE).unwrap_or(usize::MAX);
        let length = usize::try_from(length).unwrap_or(usize::MAX);
        if length == 0 || distance > pixels.len() || pixels.len().saturating_add(length) > count {
            return Err(malformed("an LZ77 copy that reaches outside the image"));
        }
        let start = pixels.len() - distance;
        for offset in 0..length {
            let pixel = pixels[start + offset];
            pixels.push(pixel);
        }
    }
    Ok(pixels)
}

/// Extra bits that follow an LZ77 prefix code.
#[allow(clippy::arithmetic_side_effects)]
fn prefix_extra_bits(code: u32) -> u32 {
    if code < 4 {
        0
    } else {
        (code - 2) >> 1
    }
}

// ---------------------------------------------------------------------------
// Transforms
// ---------------------------------------------------------------------------

/// What the encoder decided to do to the pixels before coding them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Plan {
    /// Subtract green from red and blue first.
    subtract_green: bool,
    /// Predict each pixel from its neighbours, with a mode per `1 << bits`
    /// block, or leave the pixels as they are.
    predictor: Option<u32>,
}

/// A transform list is at most four entries, each read as one bit and two more.
const MAX_TRANSFORMS: usize = 4;

/// Pixels laid out for one entropy-coded image, with whatever the plan made of
/// them, plus the predictor's mode image when the plan asked for one.
struct Coded {
    pixels: Vec<Argb>,
    modes: Vec<Argb>,
}

/// `ceil(width / (1 << size_bits))`, the mode image's width.
#[allow(clippy::arithmetic_side_effects, clippy::integer_division)]
fn subresolution(width: u32, size_bits: u32) -> usize {
    let block = 1u32 << size_bits;
    usize::try_from(width.saturating_add(block - 1) / block).unwrap_or(1)
}

/// The prediction for one pixel, given the reconstructed neighbours.
///
/// The border rules apply whatever the mode says: the first pixel of the image
/// predicts solid black, a pixel on the top row predicts its left neighbour, a
/// pixel on the left edge predicts the one above, and a pixel in the last
/// column takes the first pixel of its own row as its top-right.
#[allow(clippy::arithmetic_side_effects, clippy::integer_division)]
fn predict(mode: u32, x: usize, y: usize, src: &[Argb], width: usize) -> Argb {
    if x == 0 && y == 0 {
        return 0xFF00_0000;
    }
    let left = src
        .get(y.saturating_mul(width).saturating_add(x).saturating_sub(1))
        .copied()
        .unwrap_or(0);
    if y == 0 {
        return left;
    }
    let top = src
        .get(y.saturating_sub(1).saturating_mul(width).saturating_add(x))
        .copied()
        .unwrap_or(0);
    if x == 0 {
        return top;
    }
    let top_left = src
        .get(
            y.saturating_sub(1)
                .saturating_mul(width)
                .saturating_add(x)
                .saturating_sub(1),
        )
        .copied()
        .unwrap_or(0);
    let top_right = if x + 1 >= width {
        src.get(y.saturating_mul(width)).copied().unwrap_or(0)
    } else {
        src.get(
            y.saturating_sub(1)
                .saturating_mul(width)
                .saturating_add(x)
                .saturating_add(1),
        )
        .copied()
        .unwrap_or(0)
    };
    match mode {
        0 => 0xFF00_0000,
        1 => left,
        2 => top,
        3 => top_right,
        4 => top_left,
        5 => average2(average2(left, top_right), top),
        6 => average2(left, top_left),
        7 => average2(left, top),
        8 => average2(top_left, top),
        9 => average2(top, top_right),
        10 => average2(average2(left, top_left), average2(top, top_right)),
        11 => select(left, top, top_left),
        12 => clamp_add_subtract_full(left, top, top_left),
        _ => clamp_add_subtract_half(average2(left, top), top_left),
    }
}

/// Per-channel `(a + b) / 2`.
#[allow(clippy::arithmetic_side_effects, clippy::integer_division)]
fn average2(a: Argb, b: Argb) -> Argb {
    let mut out = 0u32;
    for shift in [0, 8, 16, 24] {
        let x = (a >> shift) & 0xFF;
        let y = (b >> shift) & 0xFF;
        out |= ((x + y) / 2) << shift;
    }
    out
}

/// Per-channel, the estimate `a + b - c` is closer to `L` than to `T`, or not.
#[allow(clippy::arithmetic_side_effects)]
fn select(l: Argb, t: Argb, top_left: Argb) -> Argb {
    let mut estimate_l = 0i64;
    let mut estimate_t = 0i64;
    for shift in [0u32, 8, 16, 24] {
        let a = i64::from((l >> shift) & 0xFF);
        let b = i64::from((t >> shift) & 0xFF);
        let c = i64::from((top_left >> shift) & 0xFF);
        let estimate = a + b - c;
        estimate_l += (estimate - a).abs();
        estimate_t += (estimate - b).abs();
    }
    if estimate_l < estimate_t {
        l
    } else {
        t
    }
}

/// Per-channel `Clamp(a + b - c)`.
#[allow(clippy::arithmetic_side_effects)]
fn clamp_add_subtract_full(a: Argb, b: Argb, c: Argb) -> Argb {
    let mut out = 0u32;
    for shift in [0u32, 8, 16, 24] {
        let value = ((a >> shift) & 0xFF)
            .saturating_add((b >> shift) & 0xFF)
            .saturating_sub((c >> shift) & 0xFF);
        out |= value.clamp(0, 255) << shift;
    }
    out
}

/// Per-channel `Clamp(a + (a - b) / 2)`.
#[allow(clippy::arithmetic_side_effects, clippy::integer_division)]
fn clamp_add_subtract_half(a: Argb, b: Argb) -> Argb {
    let mut out = 0u32;
    for shift in [0u32, 8, 16, 24] {
        let x = i64::from((a >> shift) & 0xFF);
        let y = i64::from((b >> shift) & 0xFF);
        let value = u32::try_from((x + (x - y) / 2).clamp(0, 255)).unwrap_or(0);
        out |= value << shift;
    }
    out
}

/// Residual = actual - predicted, per channel, mod 256.
#[allow(clippy::arithmetic_side_effects)]
fn residual(pixel: Argb, prediction: Argb) -> Argb {
    let mut out = 0u32;
    for shift in [0u32, 8, 16, 24] {
        let value =
            (((pixel >> shift) & 0xFF).saturating_add(256) - ((prediction >> shift) & 0xFF)) % 256;
        out |= value << shift;
    }
    out
}

/// Reconstructed = residual + prediction, per channel, mod 256.
#[allow(clippy::arithmetic_side_effects)]
fn reconstruct(pixel: Argb, prediction: Argb) -> Argb {
    let mut out = 0u32;
    for shift in [0u32, 8, 16, 24] {
        let value = (((pixel >> shift) & 0xFF) + ((prediction >> shift) & 0xFF)) % 256;
        out |= value << shift;
    }
    out
}

/// |a - b| summed over the four channels, which is what a mode is judged by.
#[allow(clippy::arithmetic_side_effects)]
fn mode_cost(actual: Argb, prediction: Argb) -> u64 {
    let mut cost = 0u64;
    for (shift, weight) in [(24u32, 100u64), (16, 60), (8, 80), (0, 40)] {
        let a = i64::from((actual >> shift) & 0xFF);
        let b = i64::from((prediction >> shift) & 0xFF);
        cost += u64::try_from((a - b).abs())
            .unwrap_or(0)
            .saturating_mul(weight);
    }
    cost
}

/// The mode image and the residual image for one predictor block size.
///
/// A mode is chosen per block by trying all fourteen on a coarse grid inside
/// that block and keeping the cheapest — the cheapest on a *sample* of the
/// block, not on the whole of it, because a page at 300 DPI has no business
/// spending fourteen passes over every pixel to pick a filter. Ties go to the
/// lower mode, so the choice is the same on every platform.
#[allow(clippy::arithmetic_side_effects, clippy::integer_division)]
fn predictor_forward(
    g: &mut BudgetGuard<'_>,
    src: &[Argb],
    width: usize,
    height: usize,
    size_bits: u32,
) -> Result<Coded> {
    let block = 1usize << size_bits;
    let across = subresolution(u32::try_from(width).unwrap_or(0), size_bits);
    let down = subresolution(u32::try_from(height).unwrap_or(0), size_bits);
    let mut modes: Vec<Argb> = alloc::vec_with_capacity(g, across.saturating_mul(down))?;
    for by in 0..down {
        for bx in 0..across {
            let x0 = bx.saturating_mul(block);
            let y0 = by.saturating_mul(block);
            let x1 = (x0.saturating_add(block)).min(width);
            let y1 = (y0.saturating_add(block)).min(height);
            let inside = x1.saturating_sub(x0).saturating_mul(y1.saturating_sub(y0));
            let step = ceil_sqrt((inside / 8).max(1));
            let mut best_mode = 0u32;
            let mut best_cost = u64::MAX;
            for mode in 0..14u32 {
                let mut cost = 0u64;
                for y in (y0..y1).step_by(step) {
                    for x in (x0..x1).step_by(step) {
                        let index = y.saturating_mul(width).saturating_add(x);
                        let Some(&actual) = src.get(index) else {
                            continue;
                        };
                        let prediction = predict(mode, x, y, src, width);
                        cost = cost.saturating_add(mode_cost(actual, prediction));
                    }
                }
                if cost < best_cost {
                    best_cost = cost;
                    best_mode = mode;
                }
            }
            modes.push(0xFF00_0000u32.saturating_add(best_mode << 8));
        }
    }
    let mut pixels: Vec<Argb> = alloc::vec_with_capacity(g, src.len())?;
    for y in 0..height {
        for x in 0..width {
            let index = y.saturating_mul(width).saturating_add(x);
            let Some(&actual) = src.get(index) else {
                continue;
            };
            let mode = mode_at(&modes, x, y, size_bits, across);
            let prediction = predict(mode, x, y, src, width);
            pixels.push(residual(actual, prediction));
        }
    }
    Ok(Coded { pixels, modes })
}

/// `ceil(sqrt(n))`, for a sample grid that covers about `n` positions.
#[allow(clippy::arithmetic_side_effects, clippy::integer_division)]
fn ceil_sqrt(n: usize) -> usize {
    let mut root = 1usize;
    while root.saturating_mul(root) < n {
        root += 1;
    }
    root.max(1)
}

/// Rebuild an image from its residual and its mode image.
#[allow(clippy::arithmetic_side_effects)]
fn predictor_inverse(
    pixels: &mut [Argb],
    modes: &[Argb],
    width: usize,
    height: usize,
    size_bits: u32,
) {
    let across = subresolution(u32::try_from(width).unwrap_or(0), size_bits);
    for y in 0..height {
        for x in 0..width {
            let index = y.saturating_mul(width).saturating_add(x);
            let Some(&value) = pixels.get(index) else {
                continue;
            };
            let mode = mode_at(modes, x, y, size_bits, across);
            if mode > 13 {
                // The pixel keeps its coded value; the caller has already been
                // told the file asks for a predictor that does not exist.
                continue;
            }
            let prediction = predict(mode, x, y, pixels, width);
            let reconstructed = reconstruct(value, prediction);
            if let Some(cell) = pixels.get_mut(index) {
                *cell = reconstructed;
            }
        }
    }
}

/// The mode that governs pixel (x, y).
#[allow(clippy::arithmetic_side_effects)]
fn mode_at(modes: &[Argb], x: usize, y: usize, size_bits: u32, across: usize) -> u32 {
    let index = (y >> usize::try_from(size_bits).unwrap_or(0))
        .saturating_mul(across)
        .saturating_add(x >> usize::try_from(size_bits).unwrap_or(0));
    let pixel = modes.get(index).copied().unwrap_or(0);
    (pixel >> 8) & 0xFF
}

/// `red -= green`, `blue -= green`, mod 256.
#[allow(clippy::arithmetic_side_effects)]
fn subtract_green(pixels: &mut [Argb]) {
    for pixel in pixels.iter_mut() {
        let green = (*pixel >> 8) & 0xFF;
        let red = ((*pixel >> 16) & 0xFF).wrapping_sub(green) & 0xFF;
        let blue = (*pixel & 0xFF).wrapping_sub(green) & 0xFF;
        *pixel = (*pixel & 0xFF00_FF00) | (red << 16) | blue;
    }
}

/// The inverse: put the green back under red and blue.
#[allow(clippy::arithmetic_side_effects)]
fn add_green(pixels: &mut [Argb]) {
    for pixel in pixels.iter_mut() {
        let green = (*pixel >> 8) & 0xFF;
        let red = ((*pixel >> 16) & 0xFF).wrapping_add(green) & 0xFF;
        let blue = (*pixel & 0xFF).wrapping_add(green) & 0xFF;
        *pixel = (*pixel & 0xFF00_FF00) | (red << 16) | blue;
    }
}

/// Apply a plan to a whole image.
fn apply_plan(
    g: &mut BudgetGuard<'_>,
    src: &[Argb],
    width: usize,
    height: usize,
    plan: Plan,
) -> Result<Coded> {
    let mut pixels = alloc::copy_slice(g, src)?;
    if plan.subtract_green {
        subtract_green(&mut pixels);
    }
    let modes = match plan.predictor {
        Some(size_bits) => {
            let coded = predictor_forward(g, &pixels, width, height, size_bits)?;
            pixels = coded.pixels;
            coded.modes
        }
        None => Vec::new(),
    };
    Ok(Coded { pixels, modes })
}

/// How many bits the entropy code would spend on a residual image, ignoring
/// LZ77. Close enough to rank plans, and cheap enough to try several.
#[allow(clippy::arithmetic_side_effects)]
fn literal_bits(g: &mut BudgetGuard<'_>, pixels: &[Argb]) -> Result<u64> {
    let items: Vec<Item> = pixels.iter().map(|&pixel| Item::Literal(pixel)).collect();
    let h = histograms(g, &items)?;
    let mut total = 0u64;
    for counts in [&h.green, &h.red, &h.blue, &h.alpha] {
        let code = PrefixCode::from_counts(g, counts)?;
        if code.single.is_some() {
            continue;
        }
        for (symbol, &count) in counts.iter().enumerate() {
            if count == 0 {
                continue;
            }
            let length = u64::from(code.lengths.get(symbol).copied().unwrap_or(0));
            total = total.saturating_add(u64::from(count).saturating_mul(length.max(1)));
        }
    }
    Ok(total)
}

/// Pick the plan the sample image likes best.
///
/// Fourteen plans — subtract green or not, against no predictor or one of six
/// block sizes — are scored on a strided sample of the page, and the winner is
/// applied to the whole image once. Scoring is the entropy the literal code
/// would spend on the residuals, which ignores what LZ77 would then win; that
/// is a heuristic, not an optimum, and it is the same heuristic on every
/// platform, which is what makes a render reproducible.
fn choose_plan(
    g: &mut BudgetGuard<'_>,
    sample: &[Argb],
    width: usize,
    height: usize,
) -> Result<Plan> {
    let mut best: Option<(u64, Plan)> = None;
    for subtract_green in [false, true] {
        for size_bits in [
            None,
            Some(2u32),
            Some(3),
            Some(4),
            Some(5),
            Some(6),
            Some(7),
        ] {
            let plan = Plan {
                subtract_green,
                predictor: size_bits,
            };
            let coded = apply_plan(g, sample, width, height, plan)?;
            let mut cost = literal_bits(g, &coded.pixels)?;
            if subtract_green {
                cost = cost.saturating_add(8);
            }
            if let Some(bits) = size_bits {
                let across = subresolution(u32::try_from(width).unwrap_or(1), bits);
                cost = cost
                    .saturating_add(64)
                    .saturating_add(u64::try_from(across).unwrap_or(0).saturating_mul(8));
                cost = cost.saturating_add(literal_bits(g, &coded.modes)?);
            }
            if best.as_ref().is_none_or(|(previous, _)| cost < *previous) {
                best = Some((cost, plan));
            }
        }
    }
    Ok(best.map_or(
        Plan {
            subtract_green: false,
            predictor: None,
        },
        |(_, plan)| plan,
    ))
}

/// Every `step`-th pixel in both directions, as its own image, plus its size.
#[allow(clippy::arithmetic_side_effects, clippy::integer_division)]
fn subsample(
    g: &mut BudgetGuard<'_>,
    src: &[Argb],
    width: usize,
    height: usize,
) -> Result<(Vec<Argb>, usize, usize)> {
    let step_x = (width / SAMPLE_MAX).max(1);
    let step_y = (height / SAMPLE_MAX).max(1);
    let across = width.saturating_add(step_x - 1) / step_x;
    let down = height.saturating_add(step_y - 1) / step_y;
    let mut out: Vec<Argb> = alloc::vec_with_capacity(g, across.saturating_mul(down))?;
    let mut y = 0usize;
    while y < height {
        let mut x = 0usize;
        while x < width {
            if let Some(&pixel) = src.get(y.saturating_mul(width).saturating_add(x)) {
                out.push(pixel);
            }
            x = x.saturating_add(step_x);
        }
        y = y.saturating_add(step_y);
    }
    Ok((out, across, down))
}

/// The largest sample side `choose_plan` is willing to evaluate.
const SAMPLE_MAX: usize = 160;
// ---------------------------------------------------------------------------
// Container and public API
// ---------------------------------------------------------------------------

/// Encode one RGBA8 image as one WebP lossless file.
///
/// The pixels go through the plan [`choose_plan`] picked — subtract green, a
/// predictor, or both — and out through an LZ77 scan coded with a prefix code
/// per channel. Everything the bitstream asserts is written from the same
/// pixels, so the file [`decode_webp`] reads back is the file a viewer sees.
///
/// Alpha is kept: a fully transparent sample stays transparent, which is the
/// point of a lossless page image, and the header's `alpha is used` bit tells
/// the truth about the samples underneath.
///
/// # Budget
///
/// Charges the ARGB copy of the pixels, the residual image, the mode grid, the
/// LZ77 hash chains and the output. A page whose working set the budget cannot
/// cover is refused rather than attempted.
///
/// # Errors
///
/// `IMAGE_MALFORMED` when either dimension is zero, when `rgba` is not exactly
/// `width * height * 4` bytes, or when either dimension is above 16384 — the
/// format codes both as 14-bit minus-one fields, so a larger page has no legal
/// header.
pub fn encode_webp(image: &Image, g: &mut BudgetGuard<'_>) -> Result<Vec<u8>> {
    let (width, height) = dimensions(image)?;
    let pixels = to_argb(g, &image.rgba)?;
    let alpha_used = pixels.iter().any(|&pixel| (pixel >> 24) != 0xFF);
    let (sample, sample_width, sample_height) = subsample(g, &pixels, width, height)?;
    let plan = choose_plan(g, &sample, sample_width, sample_height)?;
    drop(sample);
    let coded = apply_plan(g, &pixels, width, height, plan)?;
    drop(pixels);

    let expected = (width.saturating_mul(height))
        .saturating_div(4)
        .clamp(1024, 1 << 20);
    let mut bw = BitWriter::new(g, expected)?;
    bw.put_bits(g, u32::from(SIGNATURE), 8)?;
    bw.put_bits(g, u32::try_from(width.saturating_sub(1)).unwrap_or(0), 14)?;
    bw.put_bits(g, u32::try_from(height.saturating_sub(1)).unwrap_or(0), 14)?;
    bw.put_bits(g, u32::from(alpha_used), 1)?;
    bw.put_bits(g, 0, 3)?;
    // A reader undoes the transforms in the reverse of the order it read them,
    // so the one applied first — subtract green — is written first.
    if plan.subtract_green {
        bw.put_bits(g, 1, 1)?;
        bw.put_bits(g, TX_SUBTRACT_GREEN, 2)?;
    }
    if let Some(size_bits) = plan.predictor {
        bw.put_bits(g, 1, 1)?;
        bw.put_bits(g, TX_PREDICTOR, 2)?;
        bw.put_bits(g, size_bits.saturating_sub(2), 3)?;
        write_entropy_image(g, &mut bw, &coded.modes, false)?;
    }
    bw.put_bits(g, 0, 1)?;
    write_entropy_image(g, &mut bw, &coded.pixels, true)?;
    let payload = bw.finish(g)?;
    container(g, &payload)
}

/// Decode a WebP lossless file written by [`encode_webp`] back to its samples.
///
/// This is the reader that makes the lossless claim checkable: it walks the
/// container, rebuilds the prefix codes, replays the LZ77 copies and undoes the
/// transforms, and hands back exactly the pixels the encoder was given.
///
/// # Budget
///
/// Charges the prefix codes, the LZ77 hash chains and the decoded pixels it
/// holds against `Bytes`. An image whose working set the budget cannot cover
/// is refused rather than attempted.
///
/// # Malformed Input
///
/// A broken container, an incomplete prefix code, a copy that reaches outside
/// the image, or a stream that ends early is refused as malformed; a
/// construct this crate does not write is refused as unsupported.
///
/// # Errors
///
/// `IMAGE_UNSUPPORTED` for a construct this crate does not write and has no
/// test for — a color cache, meta prefix codes, a color transform, a color
/// indexing transform, a close-range LZ77 distance code, a VP8 lossy or
/// extended file, or a chunk of metadata. `IMAGE_MALFORMED` for a broken
/// container, an incomplete prefix code, a copy that reaches outside the image
/// or a stream that ends early.
pub fn decode_webp(data: &[u8], g: &mut BudgetGuard<'_>) -> Result<Image> {
    let payload = vp8l_payload(data)?;
    let mut br = BitReader::new(payload);
    if br.read_bits(8)? != u32::from(SIGNATURE) {
        return Err(malformed("a VP8L payload without its signature byte"));
    }
    let width = br.read_bits(14)?.saturating_add(1);
    let height = br.read_bits(14)?.saturating_add(1);
    let _alpha_used = br.read_bits(1)?;
    if br.read_bits(3)? != 0 {
        return Err(unsupported("a VP8L version other than 0"));
    }
    let count = usize::try_from(width.saturating_mul(height)).unwrap_or(usize::MAX);
    let mut order: Vec<u32> = Vec::new();
    let mut used = [false; 4];
    let mut size_bits = 0u32;
    let mut modes: Vec<Argb> = Vec::new();
    while br.read_bit()? == 1 {
        let kind = br.read_bits(2)?;
        let slot = usize::try_from(kind).unwrap_or(usize::MAX);
        if order.len() >= MAX_TRANSFORMS {
            return Err(malformed("more transforms than the format allows"));
        }
        if used.get(slot).copied().unwrap_or(true) {
            return Err(malformed("the same transform used twice"));
        }
        match kind {
            TX_PREDICTOR => {
                size_bits = br.read_bits(3)?.saturating_add(2);
                let across = subresolution(width, size_bits);
                let down = subresolution(height, size_bits);
                let read = read_entropy_image(g, &mut br, across.saturating_mul(down), false)?;
                for &mode in &read {
                    if ((mode >> 8) & 0xFF) > 13 {
                        return Err(malformed("a prediction mode above 13"));
                    }
                }
                modes = read;
            }
            TX_COLOR => return Err(unsupported("a color transform")),
            TX_SUBTRACT_GREEN => {}
            _ => return Err(unsupported("a color indexing transform")),
        }
        if let Some(cell) = used.get_mut(slot) {
            *cell = true;
        }
        order.push(kind);
    }
    let mut pixels = read_entropy_image(g, &mut br, count, true)?;
    if pixels.len() != count {
        return Err(malformed("the image data did not cover the whole frame"));
    }
    for &kind in order.iter().rev() {
        match kind {
            TX_PREDICTOR => predictor_inverse(
                &mut pixels,
                &modes,
                usize::try_from(width).unwrap_or(0),
                usize::try_from(height).unwrap_or(0),
                size_bits,
            ),
            TX_SUBTRACT_GREEN => add_green(&mut pixels),
            _ => {}
        }
    }
    Ok(Image {
        width,
        height,
        rgba: from_argb(g, &pixels)?,
    })
}

/// The width, height and buffer length a call may work with.
fn dimensions(image: &Image) -> Result<(usize, usize)> {
    let width = usize::try_from(image.width).unwrap_or(usize::MAX);
    let height = usize::try_from(image.height).unwrap_or(usize::MAX);
    let during = "webp-encode";
    if image.width == 0 || image.height == 0 {
        return Err(err!(
            Code::ImageMalformed,
            during = during,
            detail = "zero-sized image"
        ));
    }
    if image.width > MAX_SIDE || image.height > MAX_SIDE {
        return Err(err!(
            Code::ImageMalformed,
            during = during,
            detail = "a side above the 16384 a VP8L header can carry"
        ));
    }
    let count = width.checked_mul(height).ok_or_else(|| {
        err!(
            Code::ImageMalformed,
            during = during,
            detail = "image dimensions overflow"
        )
    })?;
    if image.rgba.len() != count.saturating_mul(4) {
        return Err(err!(
            Code::ImageMalformed,
            during = during,
            detail = "pixel buffer length does not match the dimensions"
        ));
    }
    Ok((width, height))
}

/// RGBA bytes to ARGB pixels, the order VP8L packs them in.
#[allow(clippy::integer_division)]
fn to_argb(g: &mut BudgetGuard<'_>, rgba: &[u8]) -> Result<Vec<Argb>> {
    let mut out: Vec<Argb> = alloc::vec_with_capacity(g, rgba.len() / 4)?;
    for px in rgba.chunks_exact(4) {
        let red = u32::from(px.first().copied().unwrap_or(0));
        let green = u32::from(px.get(1).copied().unwrap_or(0));
        let blue = u32::from(px.get(2).copied().unwrap_or(0));
        let alpha = u32::from(px.get(3).copied().unwrap_or(255));
        out.push((alpha << 24) | (red << 16) | (green << 8) | blue);
    }
    Ok(out)
}

/// ARGB pixels back to RGBA bytes.
fn from_argb(g: &mut BudgetGuard<'_>, pixels: &[Argb]) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = alloc::vec_with_capacity(g, pixels.len().saturating_mul(4))?;
    for &pixel in pixels {
        out.push(u8::try_from((pixel >> 16) & 0xFF).unwrap_or(0));
        out.push(u8::try_from((pixel >> 8) & 0xFF).unwrap_or(0));
        out.push(u8::try_from(pixel & 0xFF).unwrap_or(0));
        out.push(u8::try_from(pixel >> 24).unwrap_or(0));
    }
    Ok(out)
}

/// Wrap a VP8L payload in the RIFF container.
///
/// An odd payload gets its pad byte, which the chunk size does not count and
/// the enclosing RIFF size does.
fn container(g: &mut BudgetGuard<'_>, payload: &[u8]) -> Result<Vec<u8>> {
    let pad = usize::from(payload.len() % 2 == 1);
    let riff_len = 4u64
        .saturating_add(8)
        .saturating_add(u64::try_from(payload.len()).unwrap_or(u64::MAX))
        .saturating_add(u64::try_from(pad).unwrap_or(1));
    let total = usize::try_from(riff_len.saturating_add(8)).unwrap_or(usize::MAX);
    let mut out = alloc::vec_with_capacity::<u8>(g, total)?;
    out.extend_from_slice(&RIFF);
    out.extend_from_slice(&u32::try_from(riff_len).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&WEBP);
    out.extend_from_slice(&VP8L);
    out.extend_from_slice(
        &u32::try_from(payload.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    out.extend_from_slice(payload);
    if pad == 1 {
        out.push(0);
    }
    Ok(out)
}

/// Walk a RIFF container and return the body of its `VP8L` chunk.
fn vp8l_payload(data: &[u8]) -> Result<&[u8]> {
    if data.len() < 12 {
        return Err(malformed("a file shorter than a RIFF header"));
    }
    if four(data, 0) != Some(RIFF) {
        return Err(malformed("a file that is not RIFF"));
    }
    if four(data, 8) != Some(WEBP) {
        return Err(malformed("a RIFF file whose form type is not WEBP"));
    }
    let declared = u64::from(le32(data, 4).unwrap_or(0))
        .saturating_add(8)
        .min(u64::try_from(data.len()).unwrap_or(u64::MAX));
    let declared = usize::try_from(declared).unwrap_or(data.len());
    let mut at = 12usize;
    let mut found: Option<&[u8]> = None;
    while at.saturating_add(8) <= declared {
        let tag = four(data, at).unwrap_or([0; 4]);
        let size = usize::try_from(le32(data, at.saturating_add(4)).unwrap_or(u32::MAX))
            .unwrap_or(usize::MAX);
        let body = at.saturating_add(8);
        let end = body.saturating_add(size);
        if end > declared {
            return Err(malformed("a chunk longer than the file claims"));
        }
        let bytes = data
            .get(body..end)
            .ok_or_else(|| malformed("a chunk that reaches outside the file"))?;
        if tag == VP8L {
            if found.is_some() {
                return Err(malformed("two VP8L chunks in one file"));
            }
            found = Some(bytes);
        } else if tag == *b"VP8 " {
            return Err(unsupported("a VP8 lossy frame"));
        } else if tag == *b"VP8X" {
            return Err(unsupported("an extended WebP file"));
        } else if tag == *b"ICCP" {
            return Err(unsupported("an ICC profile"));
        } else if tag == *b"EXIF" || tag == *b"XMP " {
            return Err(unsupported("embedded metadata"));
        } else if tag == *b"ANIM" || tag == *b"ANMF" {
            return Err(unsupported("an animation"));
        }
        at = end.saturating_add(size % 2);
    }
    found.ok_or_else(|| malformed("a WebP file with no VP8L chunk"))
}

/// Four bytes at an offset, if the file is long enough.
fn four(data: &[u8], at: usize) -> Option<[u8; 4]> {
    let end = at.checked_add(4)?;
    let slice = data.get(at..end)?;
    let mut out = [0u8; 4];
    out.copy_from_slice(slice);
    Some(out)
}

/// A little-endian 32-bit length at an offset.
fn le32(data: &[u8], at: usize) -> Option<u32> {
    let bytes: [u8; 4] = four(data, at)?;
    Some(u32::from_le_bytes(bytes))
}

/// A file-level failure: bytes that do not describe what they claim to.
fn malformed(detail: &str) -> selis_error::Error {
    err!(
        Code::ImageMalformed,
        during = "webp-decode",
        detail = detail
    )
}

/// A construct this crate does not write, and therefore does not pretend to
/// read.
fn unsupported(detail: &str) -> selis_error::Error {
    err!(
        Code::ImageUnsupported,
        during = "webp-decode",
        detail = detail
    )
}
#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::integer_division
    )]

    use super::*;
    use crate::encode_png;
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
                rgba.push(u8::try_from((x * 3) & 0xFF).unwrap_or(0));
                rgba.push(u8::try_from((y * 5) & 0xFF).unwrap_or(0));
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

    /// White paper with black text: what a rendered page actually looks like.
    fn text_page(width: u32, height: u32) -> Image {
        let mut rgba = vec![
            0xFFu8;
            usize::try_from(width.saturating_mul(height))
                .unwrap_or(0)
                .saturating_mul(4)
        ];
        for y in 0..height {
            if y % 12 >= 6 {
                continue;
            }
            for x in 0..width {
                if x % 7 == 0 {
                    continue;
                }
                let at = usize::try_from(y.saturating_mul(width).saturating_add(x))
                    .unwrap_or(0)
                    .saturating_mul(4);
                for channel in rgba.iter_mut().skip(at).take(3) {
                    *channel = 12;
                }
            }
        }
        Image {
            width,
            height,
            rgba,
        }
    }

    /// Encode, read back, and insist every sample came home.
    fn round_trip(image: &Image) -> Vec<u8> {
        let bytes = encode_webp(image, &mut guard()).expect("encode");
        let back = decode_webp(&bytes, &mut guard()).expect("decode");
        assert_eq!(back.width, image.width);
        assert_eq!(back.height, image.height);
        assert_eq!(&back.rgba, &image.rgba, "lossless means every sample");
        bytes
    }

    /// A payload writer for the streams a decoder must say no to.
    fn payload(bits: &[(u32, u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut acc: u64 = 0;
        let mut used = 0u32;
        for (value, n) in bits {
            acc |= u64::from(*value & mask(*n)) << used;
            used += n;
            while used >= 8 {
                out.push((acc & 0xFF) as u8);
                acc >>= 8;
                used -= 8;
            }
        }
        if used > 0 {
            out.push((acc & 0xFF) as u8);
        }
        out
    }

    fn header(extra: &[(u32, u32)]) -> Vec<u8> {
        let mut bits = vec![(0x2Fu32, 8u32), (3, 14), (1, 14), (0, 1), (0, 3)];
        bits.extend_from_slice(extra);
        payload(&bits)
    }

    #[test]
    fn a_varying_page_round_trips_to_its_own_samples() {
        round_trip(&gradient(9, 5));
        round_trip(&gradient(64, 37));
        round_trip(&solid(1, 1, [10, 20, 30, 255]));
    }

    #[test]
    fn a_document_page_round_trips_and_is_not_a_liability() {
        let page = text_page(240, 320);
        let bytes = round_trip(&page);
        let png = encode_png(&page, 6, &mut guard()).expect("png");
        assert!(
            bytes.len() <= png.len(),
            "a lossless WebP of a text page measured {} B against a PNG of {} B",
            bytes.len(),
            png.len()
        );
    }

    /// A page of nothing but noise, which is what a scanned or dithered region
    /// looks like to the coder: almost no matches, and every channel byte
    /// straddles the point where a subtraction goes negative.
    fn noise(width: u32, height: u32) -> Image {
        let count = usize::try_from(width.saturating_mul(height)).unwrap_or(0);
        let mut rgba = vec![0u8; count.saturating_mul(4)];
        let mut seed = 0x1234_5678u32;
        for byte in &mut rgba {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *byte = (seed >> 24) as u8;
        }
        for alpha in rgba.chunks_exact_mut(4) {
            alpha[3] = 255;
        }
        Image {
            width,
            height,
            rgba,
        }
    }

    #[test]
    fn a_page_of_noise_round_trips_channel_for_channel() {
        let image = noise(97, 61);
        let bytes = round_trip(&image);
        assert!(
            bytes.len() > image.rgba.len() / 4,
            "noise is not compressible, measured {}",
            bytes.len()
        );
    }

    #[test]
    fn a_flat_page_costs_its_headers_and_little_else() {
        let bytes = round_trip(&solid(300, 300, [255, 255, 255, 255]));
        assert!(
            bytes.len() < 512,
            "a white page should cost almost nothing, measured {}",
            bytes.len()
        );
    }

    #[test]
    fn the_container_is_a_riff_wrapping_one_vp8l_chunk() {
        let bytes = round_trip(&gradient(7, 3));
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
        assert_eq!(&bytes[12..16], b"VP8L");
        let declared = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!(usize::try_from(declared).unwrap() + 8, bytes.len());
        let chunk = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
        assert!(usize::try_from(chunk).unwrap() + 20 <= bytes.len());
        assert_eq!(bytes[20], SIGNATURE);
    }

    #[test]
    fn alpha_survives_and_says_so_in_the_header() {
        let mut image = solid(8, 8, [0, 0, 0, 0]);
        image.rgba[4] = 255;
        image.rgba[7] = 128;
        let bytes = round_trip(&image);
        let mut br = BitReader::new(vp8l_payload(&bytes).expect("payload"));
        br.read_bits(8).unwrap();
        br.read_bits(14).unwrap();
        br.read_bits(14).unwrap();
        assert_eq!(br.read_bits(1).unwrap(), 1, "the alpha bit must be honest");
        let opaque = decode_webp(&bytes, &mut guard()).expect("decode");
        assert_eq!(opaque.rgba[6], 0, "a transparent sample stays transparent");
        assert_eq!(opaque.rgba[7], 128);
    }

    #[test]
    fn the_same_pixels_always_produce_the_same_file() {
        let page = text_page(40, 60);
        let first = encode_webp(&page, &mut guard()).expect("first");
        let second = encode_webp(&page, &mut guard()).expect("second");
        assert_eq!(first, second);
    }

    #[test]
    fn a_long_run_copies_itself_in_blocks_of_4096() {
        let row = solid(1, 9000, [7, 7, 7, 255]);
        let bytes = round_trip(&row);
        assert!(
            bytes.len() < 600,
            "a single colour row measured {}",
            bytes.len()
        );
    }

    #[test]
    fn greyscale_uses_the_green_subtraction() {
        let mut rgba = Vec::new();
        for y in 0..40u32 {
            for x in 0..40u32 {
                let value = u8::try_from(((x * 5 + y * 3) % 200) & 0xFF).unwrap_or(0);
                rgba.extend_from_slice(&[value, value, value, 255]);
            }
        }
        round_trip(&Image {
            width: 40,
            height: 40,
            rgba,
        });
    }

    #[test]
    fn every_prediction_mode_is_defined_and_bounded() {
        let src: Vec<Argb> = (0..16u32)
            .map(|i| 0xFF00_0000u32 | (i << 8) | (i << 16) | i)
            .collect();
        let corner = predict(0, 0, 0, &src, 4);
        assert_eq!(corner, 0xFF00_0000, "the first pixel predicts solid black");
        assert_eq!(
            predict(9, 1, 0, &src, 4),
            *src.first().unwrap(),
            "a top row predicts L"
        );
        assert_eq!(
            predict(2, 0, 1, &src, 4),
            *src.first().unwrap(),
            "a left edge predicts T"
        );
        // The rightmost column borrows the first pixel of its own row as TR.
        assert_eq!(
            predict(3, 3, 1, &src, 4),
            *src.get(4).unwrap(),
            "mode 3 on the right edge predicts the row's first pixel"
        );
        // Interior of a 4x4 grid at (x=2, y=1): L is src[5], T src[2], TL
        // src[1] and TR src[3], so every mode has one right answer. This is
        // the table the format fixes (RFC 9649 §4.3, table 2), and the order
        // of it is part of the file format, not a private choice.
        let expected: [(u32, Argb); 14] = [
            (0, 0xFF00_0000),  // Black
            (1, 0xFF05_0505),  // L
            (2, 0xFF02_0202),  // T
            (3, 0xFF03_0303),  // TR
            (4, 0xFF01_0101),  // TL
            (5, 0xFF03_0303),  // Average2(Average2(L, TR), T)
            (6, 0xFF03_0303),  // Average2(L, TL)
            (7, 0xFF03_0303),  // Average2(L, T)
            (8, 0xFF01_0101),  // Average2(TL, T)
            (9, 0xFF02_0202),  // Average2(T, TR)
            (10, 0xFF02_0202), // Average2(Average2(L, TL), Average2(T, TR))
            (11, 0xFF05_0505), // Select(L, T, TL)
            (12, 0xFF06_0606), // ClampAddSubtractFull(L, T, TL)
            (13, 0xFF04_0404), // ClampAddSubtractHalf(Average2(L, T), TL)
        ];
        for (mode, want) in expected {
            assert_eq!(
                predict(mode, 2, 1, &src, 4),
                want,
                "mode {mode} predicted the wrong neighbour blend"
            );
        }
        // The two clamped blends are the only ones that can leave a byte, and
        // they are the reason they are clamped.
        assert_eq!(
            clamp_add_subtract_full(0xFF00_0000, 0xFF00_0000, 0xFF00_0101),
            0xFF00_0000,
            "a subtraction below zero lands on zero, not on 255"
        );
        assert_eq!(
            clamp_add_subtract_full(0xFFFFFFFF, 0xFFFFFFFF, 0),
            0xFFFF_FFFF,
            "an addition past a byte lands on a full byte"
        );
        assert_eq!(
            clamp_add_subtract_half(0x0000_0000, 0x0000_000A),
            0x0000_0000,
            "a half blend below zero is clamped"
        );
        assert_eq!(
            average2(0x0000_0003, 0x0000_0004),
            0x0000_0003,
            "the average of two channels is a whole division, not a rounded one"
        );
        // Residual plus prediction is the pixel again, mod 256 per channel.
        for (actual, prediction) in [
            (0xFF10_2030u32, 0xFF70_8090u32),
            (0x0000_0000u32, 0xFFFF_FFFFu32),
            (0xFF80_8080u32, 0xFF7F_FFFFu32),
        ] {
            assert_eq!(
                reconstruct(residual(actual, prediction), prediction),
                actual
            );
        }
    }

    #[test]
    fn subtract_green_and_add_green_are_inverse() {
        let start = vec![
            0xFF12_3456u32,
            0x00FF_FFFFu32,
            0xFF7F_0080u32,
            0xFF00_0001u32,
            // Both differences go negative here, which is the case that has to
            // stay inside its own byte: the green channel must not pick up the
            // borrow, and neither must the alpha.
            0xFF75_CD25u32,
            0x80A6_8120u32,
            0x0102_0304u32,
        ];
        let mut pixels = start.clone();
        subtract_green(&mut pixels);
        add_green(&mut pixels);
        assert_eq!(pixels, start);
        // A grey sample always lands on zero red and blue.
        let mut grey = vec![0xFF40_4040u32];
        subtract_green(&mut grey);
        assert_eq!(grey[0] & 0x00FF_00FF, 0);
        // Only the two bytes the transform owns ever change.
        let mut one = vec![0xFF75_CD25u32];
        subtract_green(&mut one);
        assert_eq!(one[0], 0xFFA8_CD58, "the borrow stays in its channel");
        assert_eq!(one[0] >> 24, 0xFF, "alpha is not a scratch byte");
    }

    #[test]
    fn lz77_prefix_coding_covers_its_whole_range() {
        for value in [
            1u32, 2, 3, 4, 5, 6, 7, 8, 9, 12, 13, 16, 17, 4095, 4096, 1_048_456, 1_048_576,
        ] {
            let (code, bits, extra) = prefix_encode(value).unwrap_or_else(|| panic!("{value}"));
            assert_eq!(prefix_decode(code, extra), value, "value {value}");
            assert!(code < 40);
            assert!(bits <= 18);
            assert!(extra < (1 << bits) || bits == 0);
        }
        assert!(prefix_encode(0).is_none());
        assert!(prefix_encode(1_048_577).is_none());
        // A length must fit 24 codes; 4096 is the ceiling.
        let (code, _, _) = prefix_encode(4096).unwrap();
        assert_eq!(code, 23);
        assert!(
            prefix_encode(4097).is_some(),
            "the code itself reaches further"
        );
    }

    #[test]
    fn a_code_whose_tree_is_deep_get_a_floor_under_its_counts() {
        // Fibonacci counts are the classic way to make a Huffman tree deeper
        // than the alphabet is wide, and VP8L allows fifteen bits at most.
        let mut counts = vec![0u32; 256];
        let (mut a, mut b) = (1u32, 1u32);
        for slot in counts.iter_mut() {
            *slot = a;
            let next = a.saturating_add(b);
            a = b;
            b = next;
        }
        let mut g = guard();
        let code = PrefixCode::from_counts(&mut g, &counts).expect("tree");
        assert!(
            code.lengths
                .iter()
                .all(|&length| length <= MAX_CODE_LEN as u8),
            "the floor must pull every code inside 15 bits"
        );
        assert_eq!(code.single, None);

        let symbols: Vec<usize> = code
            .lengths
            .iter()
            .enumerate()
            .filter_map(|(symbol, &length)| if length > 0 { Some(symbol) } else { None })
            .collect();
        let mut bw = BitWriter::new(&mut g, 64).expect("writer");
        code.write(&mut g, &mut bw).expect("write code");
        for &symbol in &symbols {
            emit(&mut g, &mut bw, &code, symbol).expect("emit");
        }
        let bytes = bw.finish(&mut g).expect("finish");
        let mut br = BitReader::new(&bytes);
        let read = PrefixCode::read_code(&mut g, &mut br, 256).expect("read code");
        assert_eq!(
            read.lengths, code.lengths,
            "the tree survives its own coding"
        );
        for &symbol in &symbols {
            assert_eq!(
                read.read(&mut br).expect("a symbol of the deep tree"),
                u32::try_from(symbol).unwrap_or(0),
                "symbol {symbol} spends {} bits",
                code.lengths.get(symbol).copied().unwrap_or(0)
            );
        }
    }

    #[test]
    fn a_single_symbol_code_spends_no_bits() {
        let mut counts = vec![0u32; 256];
        counts[200] = 5000;
        let code = PrefixCode::from_counts(&mut guard(), &counts).expect("tree");
        assert_eq!(code.single, Some(200));
        let mut g = guard();
        let mut bw = BitWriter::new(&mut g, 1).unwrap();
        code.write(&mut g, &mut bw).unwrap();
        for _ in 0..10 {
            emit(&mut g, &mut bw, &code, 200).unwrap();
        }
        let bytes = bw.finish(&mut g).unwrap();
        // The code itself is the simple form: five bits, rounded up to one byte.
        assert!(bytes.len() <= 2, "measured {bytes:?}");
        let mut br = BitReader::new(&bytes);
        let read = PrefixCode::read_code(&mut g, &mut br, 256).unwrap();
        assert_eq!(read.single, Some(200));
        for _ in 0..10 {
            assert_eq!(
                read.read(&mut br).unwrap(),
                200,
                "a single leaf reads without bits"
            );
        }
    }

    #[test]
    fn a_two_symbol_code_takes_the_simple_path() {
        let mut counts = vec![0u32; 256];
        counts[3] = 100;
        counts[200] = 900;
        let code = PrefixCode::from_counts(&mut guard(), &counts).expect("tree");
        assert_eq!(code.single, None);
        let mut g = guard();
        let mut bw = BitWriter::new(&mut g, 1).unwrap();
        code.write(&mut g, &mut bw).unwrap();
        emit(&mut g, &mut bw, &code, 200).unwrap();
        emit(&mut g, &mut bw, &code, 3).unwrap();
        emit(&mut g, &mut bw, &code, 200).unwrap();
        let bytes = bw.finish(&mut g).unwrap();
        let mut br = BitReader::new(&bytes);
        let read = PrefixCode::read_code(&mut g, &mut br, 256).unwrap();
        assert_eq!(read.lengths[3], 1);
        assert_eq!(read.lengths[200], 1);
        assert_eq!(
            read.read(&mut br).unwrap(),
            200,
            "the rarer symbol still reads"
        );
        assert_eq!(read.read(&mut br).unwrap(), 3);
        assert_eq!(read.read(&mut br).unwrap(), 200);
    }

    #[test]
    fn a_long_zero_run_shrinks_to_the_two_repeat_codes() {
        // A complete tree over 251 symbols whose last entry is 249 slots down:
        // the run of empty slots is what the repeat codes are for.
        let mut lengths = vec![0u8; 251];
        lengths[0] = 2;
        lengths[1] = 2;
        lengths[250] = 1;
        let tokens = length_tokens(&lengths, 251);
        assert!(
            tokens.len() < 8,
            "248 zeros should cost a handful of tokens, measured {}",
            tokens.len()
        );
        assert!(tokens.iter().any(|t| t.symbol == 18), "{tokens:?}");

        // Write it with the crate's own writer and read it back: the array the
        // decoder rebuilds has to be the one the encoder had.
        let mut g = guard();
        let code = PrefixCode::from_lengths(&mut g, lengths.clone()).expect("complete tree");
        assert_eq!(code.single, None);
        let mut bw = BitWriter::new(&mut g, 16).expect("writer");
        code.write(&mut g, &mut bw).expect("write");
        let bytes = bw.finish(&mut g).expect("finish");
        assert!(
            bytes.len() < 32,
            "a 251-symbol code should not cost its alphabet in bits, measured {}",
            bytes.len()
        );
        let mut br = BitReader::new(&bytes);
        let read = PrefixCode::read_code(&mut g, &mut br, 251).expect("rebuild");
        assert_eq!(read.lengths, lengths);
        for &symbol in &[0usize, 1, 250] {
            let mut emit_guard = guard();
            let mut writer = BitWriter::new(&mut emit_guard, 4).expect("writer");
            emit(&mut emit_guard, &mut writer, &read, symbol).expect("emit");
            let coded = writer.finish(&mut emit_guard).expect("finish");
            let mut reader = BitReader::new(&coded);
            assert_eq!(
                read.read(&mut reader).expect("symbol"),
                u32::try_from(symbol).unwrap_or(0)
            );
        }
    }

    /// A VP8L payload inside a real container, for the streams a reader has to
    /// say no to.
    fn wrapped(payload: &[u8]) -> Vec<u8> {
        container(&mut guard(), payload).expect("a container for a small payload")
    }

    /// A chunk with its size and pad byte.
    fn chunk(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(tag);
        out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    /// A `RIFF`/`WEBP` file carrying exactly the chunks given.
    fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut body = Vec::new();
        for part in chunks {
            body.extend_from_slice(part);
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&u32::try_from(body.len() + 4).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend_from_slice(&body);
        out
    }

    /// A code that carries one symbol. The writer spends no bit on it and the
    /// reader returns it unconditionally, which is why a hand-built group can
    /// stand in for a decoded one when the point is what the *items* say.
    fn one_symbol(g: &mut BudgetGuard<'_>, alphabet: usize, symbol: usize) -> PrefixCode {
        let mut counts = vec![0u32; alphabet];
        if let Some(cell) = counts.get_mut(symbol) {
            *cell = 1;
        }
        PrefixCode::from_counts(g, &counts).expect("a single-symbol code")
    }

    /// Assert a refusal by code and by the words the module chose for it, so a
    /// renamed construct shows up here rather than silently passing.
    fn refused(file: &[u8], expected_code: Code, expected_detail: &str) {
        let error = decode_webp(file, &mut guard())
            .err()
            .unwrap_or_else(|| panic!("{expected_detail} must be refused"));
        assert_eq!(error.code(), expected_code, "{expected_detail}: {error:?}");
        assert!(
            error.to_string().contains(expected_detail),
            "{expected_detail} refused with other words: {error}"
        );
    }

    #[test]
    fn a_color_cache_is_refused_by_name() {
        // The transform list ends, and the entropy-coded image opens with its
        // color cache marker set.
        refused(
            &wrapped(&header(&[(0, 1), (1, 1)])),
            Code::ImageUnsupported,
            "a color cache",
        );
    }

    #[test]
    fn meta_prefix_codes_are_refused_by_name() {
        refused(
            &wrapped(&header(&[(0, 1), (0, 1), (1, 1)])),
            Code::ImageUnsupported,
            "meta prefix codes",
        );
    }

    #[test]
    fn the_transforms_this_crate_does_not_write_are_refused_by_name() {
        refused(
            &wrapped(&header(&[(1, 1), (TX_COLOR, 2)])),
            Code::ImageUnsupported,
            "a color transform",
        );
        refused(
            &wrapped(&header(&[(1, 1), (3, 2)])),
            Code::ImageUnsupported,
            "a color indexing transform",
        );
        // A version this writer never emits is not a stream it can promise.
        refused(
            &wrapped(&payload(&[
                (u32::from(SIGNATURE), 8),
                (3, 14),
                (1, 14),
                (0, 1),
                (1, 3),
            ])),
            Code::ImageUnsupported,
            "a VP8L version other than 0",
        );
        // The same transform twice is a stream no conforming writer produces.
        let error = decode_webp(
            &wrapped(&header(&[
                (1, 1),
                (TX_SUBTRACT_GREEN, 2),
                (1, 1),
                (TX_SUBTRACT_GREEN, 2),
            ])),
            &mut guard(),
        )
        .expect_err("a repeated transform is refused");
        assert_eq!(error.code(), Code::ImageMalformed, "{error:?}");
        assert!(error.to_string().contains("twice"), "{error}");
    }

    #[test]
    fn a_container_this_writer_does_not_produce_is_refused_at_the_chunk_level() {
        let vp8l = chunk(b"VP8L", &header(&[]));
        let cases: Vec<(Vec<u8>, Code, &str)> = vec![
            (
                riff(&[chunk(b"VP8 ", &[0u8; 8])]),
                Code::ImageUnsupported,
                "a VP8 lossy frame",
            ),
            (
                riff(&[chunk(b"VP8X", &[0u8; 10])]),
                Code::ImageUnsupported,
                "an extended WebP file",
            ),
            (
                riff(&[chunk(b"ICCP", &[0u8; 4])]),
                Code::ImageUnsupported,
                "an ICC profile",
            ),
            (
                riff(&[chunk(b"EXIF", &[0u8; 4]), vp8l.clone()]),
                Code::ImageUnsupported,
                "embedded metadata",
            ),
            (
                riff(&[chunk(b"XMP ", &[0u8; 4]), vp8l.clone()]),
                Code::ImageUnsupported,
                "embedded metadata",
            ),
            (
                riff(&[chunk(b"ANIM", &[0u8; 4]), vp8l.clone()]),
                Code::ImageUnsupported,
                "an animation",
            ),
            (
                riff(&[vp8l.clone(), vp8l]),
                Code::ImageMalformed,
                "two VP8L chunks in one file",
            ),
            (
                riff(&[]),
                Code::ImageMalformed,
                "a WebP file with no VP8L chunk",
            ),
            (
                b"RIFF\x14\x00\x00\x00JPEG".to_vec(),
                Code::ImageMalformed,
                "a RIFF file whose form type is not WEBP",
            ),
        ];
        for (file, expected_code, expected_detail) in cases {
            refused(&file, expected_code, expected_detail);
        }
        assert_eq!(
            decode_webp(b"webp", &mut guard())
                .err()
                .map(|error| error.code()),
            Some(Code::ImageMalformed),
            "a file shorter than a RIFF header"
        );
    }

    #[test]
    fn a_close_range_distance_code_is_refused_by_name() {
        // The first 120 distance codes name a neighbourhood rather than a
        // scan-line offset. This writer only ever emits offsets, so the table
        // that would turn a neighbourhood into one is not in the crate, and a
        // stream that uses it is refused by name instead of guessed at.
        let mut g = guard();
        let pixel: Argb = 0xFF10_2030;
        let mut green_counts = vec![0u32; GREEN_ALPHABET];
        green_counts[channel(pixel, 8)] = 1;
        green_counts[257] = 1; // the length code that means "two pixels"
        let group = CodeGroup {
            green: PrefixCode::from_counts(&mut g, &green_counts).expect("green code"),
            red: one_symbol(&mut g, LITERAL_ALPHABET, channel(pixel, 16)),
            blue: one_symbol(&mut g, LITERAL_ALPHABET, channel(pixel, 0)),
            alpha: one_symbol(&mut g, LITERAL_ALPHABET, channel(pixel, 24)),
            dist: one_symbol(&mut g, DIST_ALPHABET, 0),
        };
        let mut bw = BitWriter::new(&mut g, 16).expect("writer");
        emit(&mut g, &mut bw, &group.green, channel(pixel, 8)).expect("literal green");
        emit(&mut g, &mut bw, &group.red, channel(pixel, 16)).expect("literal red");
        emit(&mut g, &mut bw, &group.blue, channel(pixel, 0)).expect("literal blue");
        emit(&mut g, &mut bw, &group.alpha, channel(pixel, 24)).expect("literal alpha");
        emit(&mut g, &mut bw, &group.green, 257).expect("length symbol");
        bw.put_bits(&mut g, 0, prefix_extra_bits(1))
            .expect("length extras");
        emit(&mut g, &mut bw, &group.dist, 0).expect("distance symbol");
        bw.put_bits(&mut g, 0, prefix_extra_bits(0))
            .expect("distance extras");
        let bytes = bw.finish(&mut g).expect("finish");
        let mut br = BitReader::new(&bytes);
        let error = read_items(&mut g, &mut br, 3, &group)
            .expect_err("a neighbourhood distance is refused");
        assert_eq!(error.code(), Code::ImageUnsupported, "{error:?}");
        assert!(
            error.to_string().contains("close-range"),
            "refused with other words: {error}"
        );
    }

    #[test]
    fn an_overlapping_backward_reference_rebuilds_the_pixels_it_repeats() {
        // A copy whose length runs past its own source: the last two pixels of
        // the frame are the first two, one at a time.
        let mut g = guard();
        let pixels: Vec<Argb> = vec![0xFF10_2030, 0xFF11_2233];
        let items = vec![
            Item::Literal(pixels[0]),
            Item::Literal(pixels[1]),
            Item::Copy {
                length: 2,
                distance: 2,
            },
        ];
        let h = histograms(&mut g, &items).expect("histograms");
        let group = build_group(&mut g, &h).expect("code group");
        let mut bw = BitWriter::new(&mut g, 16).expect("writer");
        let wanted = vec![0xFF10_2030u32, 0xFF11_2233, 0xFF10_2030, 0xFF11_2233];
        write_items(&mut g, &mut bw, &wanted, &items, &group).expect("write");
        let bytes = bw.finish(&mut g).expect("finish");
        let mut br = BitReader::new(&bytes);
        let back = read_items(&mut g, &mut br, 4, &group).expect("four pixels");
        assert_eq!(back, wanted, "the copy replays itself across its own end");
    }

    #[test]
    fn a_copy_that_reaches_outside_the_frame_is_malformed() {
        let hostile: Vec<Argb> = vec![0xFF10_2030, 0xFF11_2233, 0xFF14_2536];
        let mut g = guard();

        // A source range the decoder has not written yet.
        let items = vec![
            Item::Literal(hostile[0]),
            Item::Copy {
                length: 2,
                distance: 9,
            },
        ];
        let h = histograms(&mut g, &items).expect("histograms");
        let group = build_group(&mut g, &h).expect("code group");
        let mut bw = BitWriter::new(&mut g, 16).expect("writer");
        write_items(&mut g, &mut bw, &hostile, &items, &group).expect("write");
        let bytes = bw.finish(&mut g).expect("finish");
        let mut br = BitReader::new(&bytes);
        let error = read_items(&mut g, &mut br, 4, &group)
            .expect_err("a copy from beyond the frame is refused");
        assert_eq!(error.code(), Code::ImageMalformed, "{error:?}");
        assert!(
            error.to_string().contains("outside the image"),
            "refused with other words: {error}"
        );

        // And a copy whose length runs past the last pixel the frame holds: the
        // stream asks for three pixels from an image that only has two.
        let items = vec![
            Item::Literal(hostile[0]),
            Item::Copy {
                length: 2,
                distance: 1,
            },
        ];
        let h = histograms(&mut g, &items).expect("histograms");
        let group = build_group(&mut g, &h).expect("code group");
        let mut bw = BitWriter::new(&mut g, 16).expect("writer");
        write_items(&mut g, &mut bw, &hostile, &items, &group).expect("write");
        let bytes = bw.finish(&mut g).expect("finish");
        let mut br = BitReader::new(&bytes);
        let error = read_items(&mut g, &mut br, 2, &group)
            .expect_err("a copy past the end of the frame is refused");
        assert_eq!(error.code(), Code::ImageMalformed, "{error:?}");
    }

    #[test]
    fn a_truncated_or_broken_stream_is_malformed_not_unsupported() {
        let bytes = round_trip(&gradient(9, 5));
        // Shortening the file breaks the container walk, which counts its
        // chunks against the declared RIFF length. Losing only the last byte
        // would not: a byte of zero padding is a legal file.
        for cut in [4, bytes.len() / 2] {
            let error =
                decode_webp(&bytes[..cut], &mut guard()).expect_err("a short file is refused");
            assert_eq!(
                error.code(),
                Code::ImageMalformed,
                "cut at {cut}: {error:?}"
            );
        }
        // A chunk the container hands over intact, but whose bitstream stops
        // early, fails inside the reader.
        let payload = vp8l_payload(&bytes).expect("payload");
        for keep in [8, payload.len().saturating_sub(6)] {
            let error = decode_webp(&wrapped(&payload[..keep]), &mut guard())
                .expect_err("a short bitstream is refused");
            assert_eq!(error.code(), Code::ImageMalformed, "keep {keep}: {error:?}");
        }
        let mut wrong_signature = wrapped(&header(&[]));
        if let Some(byte) = wrong_signature.get_mut(20) {
            *byte = SIGNATURE.wrapping_sub(1);
        }
        refused(
            &wrong_signature,
            Code::ImageMalformed,
            "a VP8L payload without its signature byte",
        );
    }

    #[test]
    fn zero_sized_and_oversized_images_are_refused_before_any_work() {
        let empty = Image {
            width: 0,
            height: 4,
            rgba: Vec::new(),
        };
        assert_eq!(
            encode_webp(&empty, &mut guard()).err().map(|e| e.code()),
            Some(Code::ImageMalformed)
        );
        let wide = Image {
            width: MAX_SIDE.saturating_add(1),
            height: 1,
            rgba: vec![0u8; usize::try_from(MAX_SIDE + 1).unwrap_or(0).saturating_mul(4)],
        };
        let error = encode_webp(&wide, &mut guard()).expect_err("refusal");
        assert_eq!(error.code(), Code::ImageMalformed, "{error:?}");
        assert!(error.to_string().contains("16384"), "{error}");
        let short = Image {
            width: 4,
            height: 4,
            rgba: vec![0u8; 60],
        };
        assert_eq!(
            encode_webp(&short, &mut guard())
                .err()
                .map(|error| error.code()),
            Some(Code::ImageMalformed)
        );
    }

    #[test]
    fn a_budget_too_small_for_the_working_set_refuses_the_page() {
        let page = gradient(400, 400);
        let budget = Budget {
            bytes: 1 << 16,
            ..Budget::unlimited()
        };
        let mut g = budget.guard_with(&FixedClock(0), CancelToken::new());
        let error = encode_webp(&page, &mut g).expect_err("refusal");
        assert_eq!(error.code(), Code::BudgetBytes, "{error:?}");
    }
}
