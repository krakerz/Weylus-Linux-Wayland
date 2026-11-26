//! H.264: avcC record and codec string from the in-band SPS/PPS.

use super::annexb::nal_units;
use super::packetizer::{Frame, StreamConfig};

const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const NAL_AUD: u8 = 9;

pub(super) fn parse(data: &[u8]) -> Frame<'_> {
    let nals = nal_units(data);
    let nal_type = |nal: &[u8]| nal[0] & 0x1f;

    let sps = nals.iter().find(|n| nal_type(n) == NAL_SPS);
    let pps = nals.iter().find(|n| nal_type(n) == NAL_PPS);
    let config = if let (Some(sps), Some(pps)) = (sps, pps)
        && let Some(record) = avcc_record(sps, pps)
    {
        Some(StreamConfig {
            codec_string: format!("avc1.{:02X}{:02X}{:02X}", record[1], record[2], record[3]),
            description: record,
            source: Vec::new(),
        })
    } else {
        None
    };
    let units = nals
        .into_iter()
        .filter(|n| !matches!(nal_type(n), NAL_SPS | NAL_PPS | NAL_AUD))
        .collect();
    Frame { config, units }
}

/// Builds an AVCDecoderConfigurationRecord (ISO 14496-15) with 4-byte NAL lengths.
fn avcc_record(sps: &[u8], pps: &[u8]) -> Option<Vec<u8>> {
    if sps.len() < 4 || sps.len() > u16::MAX as usize || pps.len() > u16::MAX as usize {
        return None;
    }
    let mut record = Vec::with_capacity(11 + sps.len() + pps.len() + 4);
    record.extend_from_slice(&[1, sps[1], sps[2], sps[3], 0xFF, 0xE1]);
    record.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    record.extend_from_slice(sps);
    record.push(1);
    record.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    record.extend_from_slice(pps);
    if matches!(sps[1], 100 | 110 | 122 | 144) {
        // every encoder path here outputs 4:2:0 8-bit (NV12/YUV420P), and High-profile avcC
        // requires these fields: chroma_format_idc 1, 8-bit luma/chroma, no SPS extensions
        record.extend_from_slice(&[0xFD, 0xF8, 0xF8, 0x00]);
    }
    Some(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: [u8; 5] = [0x67, 0x42, 0xC0, 0x1E, 0xAB];
    const PPS: [u8; 4] = [0x68, 0xCE, 0x3C, 0x80];

    #[test]
    fn high_profile_record_has_extension_bytes() {
        let sps = [0x67, 0x64, 0x00, 0x1F, 0xAB];
        let record = avcc_record(&sps, &PPS).unwrap();
        assert_eq!(record[record.len() - 4..], [0xFD, 0xF8, 0xF8, 0x00]);
        let baseline = avcc_record(&SPS, &PPS).unwrap();
        assert_eq!(baseline.last(), Some(&0x80));
    }

    #[test]
    fn codec_string_comes_from_the_sps() {
        let mut data = Vec::new();
        for nal in [&SPS[..], &PPS[..]] {
            data.extend_from_slice(&[0, 0, 0, 1]);
            data.extend_from_slice(nal);
        }
        let frame = parse(&data);
        assert_eq!(frame.config.unwrap().codec_string, "avc1.42C01E");
        assert!(frame.units.is_empty());
    }
}
