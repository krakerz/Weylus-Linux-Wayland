//! MSB-first bit reader for parameter set parsing. Every read returns None on truncation.

pub(super) struct BitReader<'a> {
    data: &'a [u8],
    /// position in bits
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Starts at a bit offset.
    pub(super) fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    pub(super) fn bits(&mut self, n: u32) -> Option<u32> {
        if n > 32 || self.pos + n as usize > self.data.len() * 8 {
            return None;
        }
        let mut value = 0u64;
        for _ in 0..n {
            let byte = self.data[self.pos / 8];
            value = (value << 1) | u64::from((byte >> (7 - self.pos % 8)) & 1);
            self.pos += 1;
        }
        Some(value as u32)
    }

    pub(super) fn skip(&mut self, n: usize) -> Option<()> {
        let end = self.pos.checked_add(n)?;
        if end > self.data.len() * 8 {
            return None;
        }
        self.pos = end;
        Some(())
    }

    /// Unsigned Exp-Golomb code ue(v).
    pub(super) fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bits(1)? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let rest = self.bits(zeros)?;
        Some(((1u64 << zeros) - 1 + u64::from(rest)) as u32)
    }

    /// AV1 uvlc() (spec 4.10.3).
    pub(super) fn uvlc(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bits(1)? == 0 {
            zeros += 1;
            if zeros >= 32 {
                return Some(u32::MAX);
            }
        }
        let rest = self.bits(zeros)?;
        Some(rest + ((1u64 << zeros) - 1) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_read_msb_first_across_bytes() {
        let mut r = BitReader::new(&[0b1010_1100, 0b0101_0000]);
        assert_eq!(r.bits(3), Some(0b101));
        assert_eq!(r.bits(7), Some(0b011_0001));
        assert_eq!(r.bits(0), Some(0));
        assert_eq!(r.bits(6), Some(0b010000));
        assert_eq!(r.bits(1), None);
    }

    #[test]
    fn bits_rejects_more_than_32() {
        assert_eq!(BitReader::new(&[0; 8]).bits(33), None);
        assert_eq!(BitReader::new(&[0xFF; 4]).bits(32), Some(u32::MAX));
    }

    #[test]
    fn skip_stops_at_the_end() {
        let mut r = BitReader::new(&[0xFF, 0x00]);
        assert_eq!(r.skip(9), Some(()));
        assert_eq!(r.bits(7), Some(0));
        assert_eq!(r.skip(1), None);
        assert_eq!(BitReader::new(&[0]).skip(usize::MAX), None);
    }

    #[test]
    fn ue_decodes_exp_golomb() {
        // 1 | 010 | 011 | 00100 | 0001000 => 0, 1, 2, 3, 7
        let mut r = BitReader::new(&[0b1010_0110, 0b0100_0001, 0b0000_0000]);
        assert_eq!(r.ue(), Some(0));
        assert_eq!(r.ue(), Some(1));
        assert_eq!(r.ue(), Some(2));
        assert_eq!(r.ue(), Some(3));
        assert_eq!(r.ue(), Some(7));
    }

    #[test]
    fn ue_rejects_overlong_and_truncated() {
        assert_eq!(BitReader::new(&[0, 0, 0, 0, 1]).ue(), None);
        assert_eq!(BitReader::new(&[0b0000_0001]).ue(), None);
    }

    #[test]
    fn uvlc_decodes_and_saturates() {
        let mut r = BitReader::new(&[0b1010_0110]);
        assert_eq!(r.uvlc(), Some(0));
        assert_eq!(r.uvlc(), Some(1));
        assert_eq!(r.uvlc(), Some(2));
        assert_eq!(BitReader::new(&[0, 0, 0, 0, 0xFF]).uvlc(), Some(u32::MAX));
        assert_eq!(BitReader::new(&[0, 0]).uvlc(), None);
    }

    #[test]
    fn at_starts_mid_stream() {
        let mut r = BitReader::at(&[0x0F, 0xF0], 4);
        assert_eq!(r.bits(8), Some(0xFF));
    }
}
