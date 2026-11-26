//! AV1: OBU walk and codec string from the in-band sequence header.

use super::bits::BitReader;
use super::packetizer::{Frame, StreamConfig};

const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_TEMPORAL_DELIMITER: u8 = 2;
const OBU_PADDING: u8 = 15;

/// Reads a leb128 value (at most 8 bytes); returns it and the bytes used.
fn leb128(data: &[u8]) -> Option<(usize, usize)> {
    let mut value = 0u64;
    for (i, &b) in data.iter().take(8).enumerate() {
        value |= u64::from(b & 0x7F) << (7 * i);
        if b & 0x80 == 0 {
            return Some((usize::try_from(value).ok()?, i + 1));
        }
    }
    None
}

pub(super) fn parse(data: &[u8]) -> Frame<'_> {
    let mut units = Vec::new();
    let mut config = None;
    let mut rest = data;
    while let Some(&header) = rest.first() {
        let kind = (header >> 3) & 0xF;
        let mut pos = 1;
        if header & 4 != 0 {
            pos += 1;
        }
        let size = if header & 2 != 0 {
            let Some((size, used)) = rest.get(pos..).and_then(leb128) else {
                break;
            };
            pos += used;
            size
        } else {
            rest.len().saturating_sub(pos)
        };
        let Some(end) = pos.checked_add(size).filter(|&e| e <= rest.len()) else {
            break;
        };
        let (obu, next) = rest.split_at(end);
        rest = next;
        match kind {
            OBU_TEMPORAL_DELIMITER | OBU_PADDING => continue,
            OBU_SEQUENCE_HEADER => {
                if let Some((profile, level, tier)) = parse_sequence_header(&obu[pos..]) {
                    config = Some(StreamConfig {
                        // 8-bit: every encoder path here is fed NV12/YUV420P
                        codec_string: format!(
                            "av01.{profile}.{level:02}{}.08",
                            if tier == 0 { 'M' } else { 'H' }
                        ),
                        description: Vec::new(),
                        source: obu[pos..].to_vec(),
                    });
                }
            }
            _ => {}
        }
        units.push(obu);
    }
    Frame { config, units }
}

/// Returns (seq_profile, seq_level_idx[0], seq_tier[0]) from a sequence header payload (spec 5.5.1).
fn parse_sequence_header(payload: &[u8]) -> Option<(u8, u8, u8)> {
    let mut r = BitReader::new(payload);
    let profile = r.bits(3)? as u8;
    r.skip(1)?; // still_picture
    if r.bits(1)? == 1 {
        // reduced_still_picture_header
        let level = r.bits(5)? as u8;
        return Some((profile, level, 0));
    }
    if r.bits(1)? == 1 {
        // timing_info_present_flag
        r.skip(64)?;
        if r.bits(1)? == 1 {
            r.uvlc()?; // num_ticks_per_picture_minus_1
        }
        if r.bits(1)? == 1 {
            // decoder_model_info
            r.skip(5 + 32 + 5 + 5)?;
        }
    }
    r.skip(1)?; // initial_display_delay_present_flag
    r.skip(5)?; // operating_points_cnt_minus_1
    r.skip(12)?; // operating_point_idc[0]
    let level = r.bits(5)? as u8;
    let tier = if level > 7 { r.bits(1)? as u8 } else { 0 };
    Some((profile, level, tier))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEQ: [u8; 11] = [
        0x00, 0x00, 0x00, 0x42, 0xa7, 0x5b, 0xd9, 0xe0, 0x10, 0xe0, 0x02,
    ];

    fn obu(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![(kind << 3) | 2, payload.len() as u8];
        v.extend_from_slice(payload);
        v
    }

    #[derive(Default)]
    struct BitWriter {
        bits: Vec<bool>,
    }

    impl BitWriter {
        fn put(&mut self, value: u64, n: u32) {
            for i in (0..n).rev() {
                self.bits.push((value >> i) & 1 == 1);
            }
        }

        fn bytes(&self) -> Vec<u8> {
            self.bits
                .chunks(8)
                .map(|c| {
                    c.iter()
                        .enumerate()
                        .fold(0u8, |acc, (i, &b)| acc | (u8::from(b) << (7 - i)))
                })
                .collect()
        }
    }

    #[test]
    fn real_sequence_header_gives_profile_0_level_8() {
        let mut data = vec![0x12, 0x00, 0x0a, 0x0b];
        data.extend_from_slice(&SEQ);
        data.extend_from_slice(&[0x32, 0x02, 0xaa, 0xbb]);
        let frame = parse(&data);
        let config = frame.config.unwrap();
        assert_eq!(config.codec_string, "av01.0.08M.08");
        assert!(config.description.is_empty());
        assert_eq!(config.source, SEQ);
        assert_eq!(frame.units.len(), 2);
        assert_eq!(frame.units[0][0], 0x0a);
        assert_eq!(frame.units[1], [0x32, 0x02, 0xaa, 0xbb]);
    }

    #[test]
    fn timing_info_and_decoder_model_are_skipped() {
        let mut w = BitWriter::default();
        w.put(0, 3); // profile
        w.put(0, 2); // still_picture, reduced
        w.put(1, 1); // timing_info_present
        w.put(0, 64);
        w.put(1, 1); // equal_picture_interval
        w.put(0b00100, 5); // uvlc = 3
        w.put(1, 1); // decoder_model_info_present
        w.put(0, 5 + 32 + 5 + 5);
        w.put(0, 1); // initial_display_delay_present
        w.put(0, 5); // operating_points_cnt_minus_1
        w.put(0, 12);
        w.put(12, 5); // level 12
        w.put(1, 1); // tier H
        assert_eq!(parse_sequence_header(&w.bytes()), Some((0, 12, 1)));
        let data = obu(1, &w.bytes());
        let frame = parse(&data);
        assert_eq!(frame.config.unwrap().codec_string, "av01.0.12H.08");
    }

    #[test]
    fn reduced_still_picture_header() {
        let mut w = BitWriter::default();
        w.put(1, 3);
        w.put(1, 1);
        w.put(1, 1);
        w.put(4, 5);
        assert_eq!(parse_sequence_header(&w.bytes()), Some((1, 4, 0)));
    }

    #[test]
    fn last_obu_without_size_field_takes_the_rest() {
        let mut data = obu(6, &[1, 2]);
        data.extend_from_slice(&[6 << 3, 9, 9, 9]);
        let frame = parse(&data);
        assert_eq!(frame.units.len(), 2);
        assert_eq!(frame.units[1], [6 << 3, 9, 9, 9]);
    }

    #[test]
    fn padding_and_delimiters_are_dropped() {
        let mut data = obu(2, &[]);
        data.extend_from_slice(&obu(15, &[0, 0]));
        data.extend_from_slice(&obu(6, &[7]));
        assert_eq!(parse(&data).units, vec![&obu(6, &[7])[..]]);
    }

    #[test]
    fn extension_byte_is_part_of_the_obu() {
        let data = [(6 << 3) | 4 | 2, 0x20, 1, 0xEE];
        assert_eq!(parse(&data).units, vec![&data[..]]);
    }

    #[test]
    fn truncated_input_keeps_what_was_collected() {
        let mut data = obu(6, &[1]);
        data.extend_from_slice(&[(6 << 3) | 2, 0x80]); // leb128 never ends
        assert_eq!(parse(&data).units.len(), 1);
        let mut data = obu(6, &[1]);
        data.extend_from_slice(&[(6 << 3) | 2, 5, 1]); // size past the end
        assert_eq!(parse(&data).units.len(), 1);
    }

    #[test]
    fn leb128_reads_multi_byte_values() {
        assert_eq!(leb128(&[0xE5, 0x8E, 0x26]), Some((624_485, 3)));
        assert_eq!(leb128(&[0x80; 9]), None);
        assert_eq!(leb128(&[]), None);
    }
}
