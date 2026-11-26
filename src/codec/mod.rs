//! Video codecs the server can stream and the packetizer that frames encoder output for the client.

mod annexb;
mod av1;
mod bits;
mod h264;
mod hevc;
mod packetizer;

use std::fmt;

pub use packetizer::Packetizer;
#[cfg(test)]
pub use packetizer::{FLAG_CONFIG, FLAG_KEY, MESSAGE_VERSION};

/// The values are the wire codec id and the C `codec` int: they must match `CODEC_*` in
/// lib/encode_video.c.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Codec {
    H264 = 0,
    Hevc = 1,
    Av1 = 2,
}

impl Codec {
    pub fn wire_id(self) -> u8 {
        self as u8
    }

    /// Parses the wire name used in the client's `codecs` list.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "h264" => Some(Self::H264),
            "hevc" => Some(Self::Hevc),
            "av1" => Some(Self::Av1),
            _ => None,
        }
    }

    /// H.264 and HEVC are sent as length-prefixed NAL units, AV1 as raw OBUs.
    pub(super) fn length_prefixed(self) -> bool {
        matches!(self, Self::H264 | Self::Hevc)
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::H264 => "H.264",
            Self::Hevc => "H.265",
            Self::Av1 => "AV1",
        })
    }
}

/// The encoder candidates, best first. Always ends with H.264.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecOrder([Option<Codec>; 3]);

impl Default for CodecOrder {
    fn default() -> Self {
        Self([Some(Codec::H264), None, None])
    }
}

impl CodecOrder {
    /// Unknown names and duplicates are skipped; H.264 is appended if the client didn't list it.
    pub fn from_names(names: &[String]) -> Self {
        let mut slots = [None; 3];
        let mut len = 0;
        let wanted = names
            .iter()
            .filter_map(|n| Codec::from_name(n))
            .chain(std::iter::once(Codec::H264));
        for codec in wanted {
            if !slots[..len].contains(&Some(codec)) {
                slots[len] = Some(codec);
                len += 1;
            }
        }
        Self(slots)
    }

    pub fn iter(self) -> impl Iterator<Item = Codec> {
        self.0.into_iter().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(names: &[&str]) -> Vec<Codec> {
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        CodecOrder::from_names(&names).iter().collect()
    }

    #[test]
    fn unknown_names_are_ignored_and_h264_appended() {
        assert_eq!(order(&["vp9", "av1"]), [Codec::Av1, Codec::H264]);
    }

    #[test]
    fn duplicates_are_dropped() {
        assert_eq!(
            order(&["hevc", "hevc", "av1", "hevc"]),
            [Codec::Hevc, Codec::Av1, Codec::H264]
        );
    }

    #[test]
    fn h264_keeps_its_place_when_listed() {
        assert_eq!(order(&["h264", "av1"]), [Codec::H264, Codec::Av1]);
        assert_eq!(
            order(&["av1", "h264", "hevc"]),
            [Codec::Av1, Codec::H264, Codec::Hevc]
        );
    }

    #[test]
    fn empty_is_h264_only() {
        assert_eq!(order(&[]), [Codec::H264]);
        assert_eq!(
            CodecOrder::default().iter().collect::<Vec<_>>(),
            [Codec::H264]
        );
    }

    #[test]
    fn names_are_exact_and_ids_match_the_wire() {
        assert_eq!(Codec::from_name("H264"), None);
        assert_eq!(Codec::from_name("hevc"), Some(Codec::Hevc));
        assert_eq!(
            [Codec::H264, Codec::Hevc, Codec::Av1].map(Codec::wire_id),
            [0, 1, 2]
        );
        assert_eq!(Codec::Hevc.to_string(), "H.265");
    }
}
