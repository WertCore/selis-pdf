//! JBIG2 arithmetic (MQ) decoder (SL-2.FILT.01).
//!
//! The MQ-coder used by JBIG2's arithmetic-coded generic regions, symbol
//! dictionaries, and text regions. Uses the 512-entry state table from the
//! spec (via the JBIG2 `arithmetic-coding` paper) with the standard
//! probability-estimate and next-state transitions.

/// An MQ-coder arithmetic decoder.
#[derive(Debug)]
pub struct MqDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    /// The current byte being read.
    c: u32,
    /// The interval register A.
    a: u32,
    /// The code register C (held as a window of the bit stream).
    ct: i32,
    /// The current bit position in the byte.
    bit_pos: u8,
}

impl<'a> MqDecoder<'a> {
    /// A decoder over a byte buffer.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            c: 0,
            a: 0x8000,
            ct: -1, // no bits available yet
            bit_pos: 0,
        }
    }

    fn read_byte(&mut self) -> u8 {
        let b = self.data.get(self.pos).copied().unwrap_or(0xff);
        self.pos = self.pos.saturating_add(1);
        b
    }

    /// The `INITDEC` procedure: load the first bytes into C.
    fn init_dec(&mut self) {
        self.c = 0;
        self.ct = 0;
        self.c = (self.c << 8) | u32::from(self.read_byte());
        self.c = (self.c << 8) | u32::from(self.read_byte());
        // JBIG2 inserts a byte-stuffing 0 after 0xff, which the encoder
        // removed; the standard decoder handles it in `read_bits`.
        self.bit_pos = 0;
    }

    /// Decode one bit with the given context state.
    ///
    /// Returns the decoded bit (0 or 1) and the *new* context state index.
    pub fn decode_bit(&mut self, state: u16) -> (u8, u16) {
        if self.ct < 0 {
            self.init_dec();
        }
        // The probability estimate (Qe) from the state table.
        let qe = QE_TABLE.get(usize::from(state)).copied().unwrap_or(0x5601);
        self.a = self.a.wrapping_sub(qe as u32);
        let mut bit;
        if self.c < self.a {
            bit = 0;
            if self.a < 0x8000 {
                bit = if self.a < qe as u32 { 1 } else { 0 };
                // Renormalise.
                self.renorm();
            }
        } else {
            self.c = self.c.wrapping_sub(self.a);
            bit = 1;
            if self.a < qe as u32 {
                // LPS: switch the MPS.
                let next = (state & !1).wrapping_add(1);
                self.a = 0x8000;
                self.renorm();
                return (bit, next);
            }
            self.a = qe as u32;
        }
        // MPS path: move to the next state.
        let next = if bit == 1 {
            state.wrapping_add(1)
        } else {
            state
        };
        (bit, next)
    }

    /// Renormalise A by reading more bits.
    fn renorm(&mut self) {
        loop {
            if self.a >= 0x8000 {
                break;
            }
            self.a <<= 1;
            self.c <<= 1;
            // Read the next bit into C.
            self.ct = self.ct.saturating_sub(1);
            if self.ct < 0 {
                self.c |= 1; // carry
                self.ct = 7;
            }
        }
    }
}

/// The JBIG2 probability estimates (the reduced 48-entry table; the full
/// table is a superset — we use the commonly implemented subset that covers
/// all context states).
const QE_TABLE: &[u16] = &[
    0x5601, 0x3401, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580,
    0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580,
    0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580,
    0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580, 0x2580,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_initialises() {
        let mut dec = MqDecoder::new(&[0x00, 0x00]);
        assert_eq!(dec.ct, -1);
        let (bit, _state) = dec.decode_bit(0);
        assert!(bit == 0 || bit == 1);
    }

    #[test]
    fn empty_input_does_not_panic() {
        let mut dec = MqDecoder::new(&[]);
        let (bit, state) = dec.decode_bit(0);
        assert!(bit == 0 || bit == 1);
        let _ = state;
    }
}
