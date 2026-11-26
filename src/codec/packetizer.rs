//! Packs encoder output into the binary video message of the web client (wire format v2).
//!
//! Little-endian: version (u8), flags (u8), codec id (u8: 0 H.264, 1 HEVC, 2 AV1), reserved (u8),
//! pts in ms (u32). If `FLAG_CONFIG`: codec string length (u8) and the ASCII codec string for
//! `VideoDecoder.configure`, then the description length (u32; 0 = none) and the description
//! (avcC / hvcC). The rest is the access unit: H.264/HEVC NAL units each prefixed with their
//! big-endian u32 length (parameter sets and delimiters travel in the description), AV1 OBUs back
//! to back without temporal delimiters (the sequence header stays in-band).

use tracing::warn;

use super::{Codec, av1, h264, hevc};

pub const MESSAGE_VERSION: u8 = 2;
pub const FLAG_KEY: u8 = 1;
pub const FLAG_CONFIG: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StreamConfig {
    pub codec_string: String,
    /// sent as the decoder description; empty for AV1
    pub description: Vec<u8>,
    /// not sent; compared with `description` to detect a change (the AV1 sequence header)
    pub source: Vec<u8>,
}

pub(super) struct Frame<'a> {
    pub config: Option<StreamConfig>,
    /// NAL units (H.264/HEVC, written length-prefixed) or whole OBUs (AV1, written as-is)
    pub units: Vec<&'a [u8]>,
}

/// Tracks the last sent stream config so it's only resent when it changes.
#[derive(Debug)]
pub struct Packetizer {
    codec: Codec,
    config: Option<StreamConfig>,
}

impl Packetizer {
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            config: None,
        }
    }

    /// Returns the WS message for one encoded packet, or None if nothing decodable is left
    /// (e.g. a packet holding only parameter sets).
    pub fn packetize(&mut self, data: &[u8], pts_ms: u32, keyframe: bool) -> Option<Vec<u8>> {
        let Frame { config, units } = match self.codec {
            Codec::H264 => h264::parse(data),
            Codec::Hevc => hevc::parse(data),
            Codec::Av1 => av1::parse(data),
        };
        if units.is_empty() {
            return None;
        }

        let mut flags = if keyframe { FLAG_KEY } else { 0 };
        let mut new_config = None;
        if let Some(config) = config
            && self.config.as_ref() != Some(&config)
        {
            if config.codec_string.len() > usize::from(u8::MAX) {
                warn!("Codec string too long, not sending the config");
            } else {
                flags |= FLAG_CONFIG;
                new_config = Some(config);
            }
        }

        let prefixed = self.codec.length_prefixed();
        let units_len: usize = units
            .iter()
            .map(|u| u.len() + 4 * usize::from(prefixed))
            .sum();
        let config_len = new_config
            .as_ref()
            .map_or(0, |c| 1 + c.codec_string.len() + 4 + c.description.len());
        let mut message = Vec::with_capacity(8 + config_len + units_len);
        message.extend_from_slice(&[MESSAGE_VERSION, flags, self.codec.wire_id(), 0]);
        message.extend_from_slice(&pts_ms.to_le_bytes());
        if let Some(config) = new_config {
            message.push(config.codec_string.len() as u8);
            message.extend_from_slice(config.codec_string.as_bytes());
            message.extend_from_slice(&(config.description.len() as u32).to_le_bytes());
            message.extend_from_slice(&config.description);
            self.config = Some(config);
        }
        for unit in units {
            if prefixed {
                message.extend_from_slice(&(unit.len() as u32).to_be_bytes());
            }
            message.extend_from_slice(unit);
        }
        Some(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: [u8; 5] = [0x67, 0x42, 0xC0, 0x1E, 0xAB];
    const PPS: [u8; 4] = [0x68, 0xCE, 0x3C, 0x80];
    const IDR: [u8; 3] = [0x65, 0x88, 0x84];
    const AUD: [u8; 2] = [0x09, 0xF0];
    const P: [u8; 2] = [0x41, 0x9A];

    fn annexb(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in nals {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
        out
    }

    fn keyframe() -> Vec<u8> {
        annexb(&[&AUD, &SPS, &PPS, &IDR])
    }

    #[test]
    fn keyframe_has_config_and_avcc_access_unit() {
        let mut p = Packetizer::new(Codec::H264);
        let msg = p.packetize(&keyframe(), 0x0102_0304, true).unwrap();
        assert_eq!(msg[0], MESSAGE_VERSION);
        assert_eq!(msg[1], FLAG_KEY | FLAG_CONFIG);
        assert_eq!(msg[2], 0);
        assert_eq!(msg[3], 0);
        assert_eq!(msg[4..8], [4, 3, 2, 1]);
        let record = [
            1, 0x42, 0xC0, 0x1E, 0xFF, 0xE1, 0, 5, 0x67, 0x42, 0xC0, 0x1E, 0xAB, 1, 0, 4, 0x68,
            0xCE, 0x3C, 0x80,
        ];
        assert_eq!(msg[8], 11);
        assert_eq!(&msg[9..20], b"avc1.42C01E");
        assert_eq!(msg[20..24], (record.len() as u32).to_le_bytes());
        assert_eq!(msg[24..24 + record.len()], record);
        assert_eq!(msg[24 + record.len()..], [0, 0, 0, 3, 0x65, 0x88, 0x84]);
    }

    #[test]
    fn identical_config_is_not_resent() {
        let mut p = Packetizer::new(Codec::H264);
        p.packetize(&keyframe(), 0, true).unwrap();
        let msg = p.packetize(&keyframe(), 10, true).unwrap();
        assert_eq!(msg[1], FLAG_KEY);
        assert_eq!(msg[8..], [0, 0, 0, 3, 0x65, 0x88, 0x84]);
    }

    #[test]
    fn changed_sps_resends_config() {
        let mut p = Packetizer::new(Codec::H264);
        p.packetize(&keyframe(), 0, true).unwrap();
        let sps2 = [0x67, 0x42, 0xC0, 0x28, 0xAB];
        let msg = p
            .packetize(&annexb(&[&AUD, &sps2, &PPS, &IDR]), 10, true)
            .unwrap();
        assert_eq!(msg[1], FLAG_KEY | FLAG_CONFIG);
    }

    #[test]
    fn parameter_sets_only_yield_none_and_delta_has_no_flags() {
        let mut p = Packetizer::new(Codec::H264);
        assert!(p.packetize(&annexb(&[&SPS, &PPS]), 0, true).is_none());
        let msg = p.packetize(&annexb(&[&P]), 5, false).unwrap();
        assert_eq!(msg[1], 0);
        assert_eq!(msg[8..], [0, 0, 0, 2, 0x41, 0x9A]);
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn hevc_keyframe_has_hvcc_and_length_prefixed_slice() {
        let vps = hex("40010c01ffff016000000300b000000300000300782c0c0000030004000003007ba0");
        let sps = hex(
            "420101016000000300b00000030000030078a00360802d1cbe24b924295a30168080000003008000000f62",
        );
        let pps = hex("4401c0758112");
        let idr = hex("2601af68");
        let mut p = Packetizer::new(Codec::Hevc);
        let msg = p
            .packetize(&annexb(&[&vps, &sps, &pps, &idr]), 0, true)
            .unwrap();
        assert_eq!(msg[1], FLAG_KEY | FLAG_CONFIG);
        assert_eq!(msg[2], 1);
        assert_eq!(msg[8], 16);
        assert_eq!(&msg[9..25], b"hvc1.1.6.L120.B0");
        assert!(msg[25..].ends_with(&hex("000000042601af68")));
        let msg = p
            .packetize(&annexb(&[&vps, &sps, &pps, &idr]), 1, true)
            .unwrap();
        assert_eq!(msg[1], FLAG_KEY);
        assert_eq!(msg[8..], hex("000000042601af68")[..]);
    }

    #[test]
    fn av1_keyframe_and_delta_pass_obus_through() {
        let seq = hex("00000042a75bd9e010e002");
        let mut key = vec![0x12, 0x00, 0x0a, 0x0b];
        key.extend_from_slice(&seq);
        key.extend_from_slice(&[0x32, 0x02, 0xaa, 0xbb]);
        let mut payload = vec![0x0a, 0x0b];
        payload.extend_from_slice(&seq);
        payload.extend_from_slice(&[0x32, 0x02, 0xaa, 0xbb]);

        let mut p = Packetizer::new(Codec::Av1);
        let msg = p.packetize(&key, 7, true).unwrap();
        assert_eq!(msg[1], FLAG_KEY | FLAG_CONFIG);
        assert_eq!(msg[2], 2);
        assert_eq!(msg[8], 13);
        assert_eq!(&msg[9..22], b"av01.0.08M.08");
        assert_eq!(msg[22..26], [0, 0, 0, 0]);
        assert_eq!(msg[26..], payload[..]);

        let msg = p
            .packetize(&[0x12, 0x00, 0x32, 0x02, 0xcc, 0xdd], 8, false)
            .unwrap();
        assert_eq!(msg[1], 0);
        assert_eq!(msg[2], 2);
        assert_eq!(msg[8..], [0x32, 0x02, 0xcc, 0xdd]);

        let msg = p.packetize(&key, 9, true).unwrap();
        assert_eq!(msg[1], FLAG_KEY);
        assert_eq!(msg[8..], payload[..]);
    }
}
