//! HEVC: hvcC record and codec string from the in-band VPS/SPS/PPS.

use super::annexb::{nal_units, rbsp};
use super::bits::BitReader;
use super::packetizer::{Frame, StreamConfig};

const NAL_VPS: u8 = 32;
const NAL_SPS: u8 = 33;
const NAL_PPS: u8 = 34;
const NAL_AUD: u8 = 35;

fn nal_type(nal: &[u8]) -> u8 {
    (nal[0] >> 1) & 0x3F
}

pub(super) fn parse(data: &[u8]) -> Frame<'_> {
    let nals = nal_units(data);
    let find = |t: u8| nals.iter().copied().find(|n| nal_type(n) == t);
    let config = if let (Some(vps), Some(sps), Some(pps)) =
        (find(NAL_VPS), find(NAL_SPS), find(NAL_PPS))
        && let Some((record, codec_string)) = hvcc_record(vps, sps, pps)
    {
        Some(StreamConfig {
            codec_string,
            description: record,
            source: Vec::new(),
        })
    } else {
        None
    };
    let units = nals
        .into_iter()
        .filter(|n| !matches!(nal_type(n), NAL_VPS | NAL_SPS | NAL_PPS | NAL_AUD))
        .collect();
    Frame { config, units }
}

/// Fields of the SPS that the hvcC record repeats.
struct SpsInfo {
    chroma_format_idc: u8,
    bit_depth_luma_minus8: u8,
    bit_depth_chroma_minus8: u8,
}

/// Skips sub-layer profile/level data and reads the fields after it (H.265 7.3.2.2.1).
fn parse_sps_tail(rbsp: &[u8], max_sub_layers_minus1: u8) -> Option<SpsInfo> {
    // 8 bits of vps id/sub-layers/nesting + 12 bytes of general profile_tier_level
    let mut r = BitReader::at(rbsp, 104);
    if max_sub_layers_minus1 > 0 {
        let mut present = Vec::with_capacity(max_sub_layers_minus1 as usize);
        for _ in 0..max_sub_layers_minus1 {
            present.push((r.bits(1)? == 1, r.bits(1)? == 1));
        }
        r.skip(2 * (8 - max_sub_layers_minus1 as usize))?;
        for (profile, level) in present {
            if profile {
                r.skip(88)?;
            }
            if level {
                r.skip(8)?;
            }
        }
    }
    r.ue()?; // sps_seq_parameter_set_id
    let chroma_format_idc = r.ue()?;
    if chroma_format_idc == 3 {
        r.skip(1)?;
    }
    r.ue()?; // width
    r.ue()?; // height
    if r.bits(1)? == 1 {
        for _ in 0..4 {
            r.ue()?; // conformance window offsets
        }
    }
    let luma = r.ue()?;
    let chroma = r.ue()?;
    Some(SpsInfo {
        chroma_format_idc: u8::try_from(chroma_format_idc).ok()?,
        bit_depth_luma_minus8: u8::try_from(luma).ok()?,
        bit_depth_chroma_minus8: u8::try_from(chroma).ok()?,
    })
}

/// Builds an HEVCDecoderConfigurationRecord (ISO 14496-15 8.3.3.1) and the `hvc1.` codec string.
fn hvcc_record(vps: &[u8], sps: &[u8], pps: &[u8]) -> Option<(Vec<u8>, String)> {
    if [vps, sps, pps].iter().any(|n| n.len() > u16::MAX as usize) || sps.len() < 3 {
        return None;
    }
    let rbsp = rbsp(&sps[2..]);
    if rbsp.len() < 13 {
        return None;
    }
    let max_sub_layers_minus1 = (rbsp[0] >> 1) & 7;
    let nesting = rbsp[0] & 1;
    let ptl = &rbsp[1..=12];
    let info = parse_sps_tail(&rbsp, max_sub_layers_minus1)?;

    let mut record = Vec::with_capacity(23 + 3 * 5 + vps.len() + sps.len() + pps.len());
    record.push(1);
    record.extend_from_slice(ptl);
    record.extend_from_slice(&[0xF0, 0x00, 0xFC]);
    record.push(0xFC | info.chroma_format_idc);
    record.push(0xF8 | info.bit_depth_luma_minus8);
    record.push(0xF8 | info.bit_depth_chroma_minus8);
    record.extend_from_slice(&[0x00, 0x00]);
    record.push(((max_sub_layers_minus1 + 1) << 3) | (nesting << 2) | 3);
    record.push(3);
    for (kind, nal) in [(NAL_VPS, vps), (NAL_SPS, sps), (NAL_PPS, pps)] {
        record.push(0x80 | kind);
        record.extend_from_slice(&[0x00, 0x01]);
        record.extend_from_slice(&(nal.len() as u16).to_be_bytes());
        record.extend_from_slice(nal);
    }
    Some((record, codec_string(ptl)))
}

/// ISO 14496-15 Annex E.3 codec string from the general profile_tier_level bytes.
fn codec_string(ptl: &[u8]) -> String {
    let space = match ptl[0] >> 6 {
        0 => "",
        1 => "A",
        2 => "B",
        _ => "C",
    };
    let tier = if ptl[0] & 0x20 != 0 { 'H' } else { 'L' };
    let compat = u32::from_be_bytes([ptl[1], ptl[2], ptl[3], ptl[4]]);
    let mut s = format!(
        "hvc1.{space}{}.{:X}.{tier}{}",
        ptl[0] & 0x1F,
        compat.reverse_bits(),
        ptl[11]
    );
    let constraints = &ptl[5..11];
    if let Some(last) = constraints.iter().rposition(|&b| b != 0) {
        for b in &constraints[..=last] {
            s.push_str(&format!(".{b:X}"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const VPS: &str = "40010c01ffff016000000300b000000300000300782c0c0000030004000003007ba0";
    const SPS: &str =
        "420101016000000300b00000030000030078a00360802d1cbe24b924295a30168080000003008000000f62";
    const PPS: &str = "4401c0758112";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn record_and_codec_string_match_ffmpeg() {
        let (vps, sps, pps) = (hex(VPS), hex(SPS), hex(PPS));
        let (record, codec) = hvcc_record(&vps, &sps, &pps).unwrap();
        assert_eq!(codec, "hvc1.1.6.L120.B0");
        assert_eq!(
            record[..23],
            hex("010160000000b0000000000078f000fcfdf8f800000f03")[..]
        );
        let mut tail = hex("a00001");
        tail.extend_from_slice(&(vps.len() as u16).to_be_bytes());
        tail.extend_from_slice(&vps);
        tail.extend_from_slice(&hex("a10001"));
        tail.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        tail.extend_from_slice(&sps);
        tail.extend_from_slice(&hex("a20001"));
        tail.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        tail.extend_from_slice(&pps);
        assert_eq!(record[23..], tail[..]);
        assert_eq!(vps.len(), 0x22);
        assert_eq!(sps.len(), 0x2b);
    }

    #[test]
    fn parse_strips_parameter_sets_and_keeps_slices() {
        let mut data = Vec::new();
        for nal in [hex(VPS), hex(SPS), hex(PPS), hex("2601af68")] {
            data.extend_from_slice(&[0, 0, 0, 1]);
            data.extend_from_slice(&nal);
        }
        let frame = parse(&data);
        assert_eq!(frame.config.unwrap().codec_string, "hvc1.1.6.L120.B0");
        assert_eq!(frame.units, vec![&hex("2601af68")[..]]);
    }

    #[test]
    fn codec_string_handles_tier_space_and_constraints() {
        // profile_space 1, tier high, profile 2, compat bit 1 set, level 93
        let mut ptl = [0u8; 12];
        ptl[0] = 0x40 | 0x20 | 2;
        ptl[1..5].copy_from_slice(&[0x40, 0, 0, 0]);
        ptl[11] = 93;
        assert_eq!(codec_string(&ptl), "hvc1.A2.2.H93");
        ptl[5] = 0x90;
        ptl[7] = 0x01;
        assert_eq!(codec_string(&ptl), "hvc1.A2.2.H93.90.0.1");
    }

    #[test]
    fn short_sps_is_rejected() {
        let (vps, pps) = (hex(VPS), hex(PPS));
        let sps = hex("42010101600000030000");
        assert!(hvcc_record(&vps, &sps, &pps).is_none());
        assert!(hvcc_record(&vps, &[0x42], &pps).is_none());
    }
}
