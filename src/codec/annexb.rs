//! Annex B byte stream helpers shared by the H.264 and HEVC parsers.

/// Splits an Annex B stream into NAL units without start codes.
pub(super) fn nal_units(annexb: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    // start of the NAL after the most recent start code
    let mut start = None;
    let mut i = 0;
    while i + 3 <= annexb.len() {
        if annexb[i..i + 3] == [0, 0, 1] {
            if let Some(s) = start {
                push_nal(&mut nals, &annexb[s..i]);
            }
            start = Some(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        push_nal(&mut nals, &annexb[s..]);
    }
    nals
}

fn push_nal<'a>(nals: &mut Vec<&'a [u8]>, nal: &'a [u8]) {
    // the last byte of a NAL holds the RBSP stop bit, so trailing zeros belong to the next start code
    let end = nal.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
    if end > 0 {
        nals.push(&nal[..end]);
    }
}

/// Removes emulation prevention bytes (`00 00 03` -> `00 00`) from a NAL payload.
pub(super) fn rbsp(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len());
    for &b in payload {
        if b == 3 && out.ends_with(&[0, 0]) {
            continue;
        }
        out.push(b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: [u8; 5] = [0x67, 0x42, 0xC0, 0x1E, 0xAB];
    const IDR: [u8; 3] = [0x65, 0x88, 0x84];
    const AUD: [u8; 2] = [0x09, 0xF0];

    #[test]
    fn splits_mixed_start_codes_and_trailing_zeros() {
        let mut data = vec![0xAA, 0, 0, 0, 1];
        data.extend_from_slice(&AUD);
        data.extend_from_slice(&[0, 0, 1]);
        data.extend_from_slice(&SPS);
        data.extend_from_slice(&[0, 0, 0, 0, 0, 1]);
        data.extend_from_slice(&IDR);
        data.extend_from_slice(&[0, 0]);
        let nals = nal_units(&data);
        assert_eq!(nals, vec![&AUD[..], &SPS[..], &IDR[..]]);
    }

    #[test]
    fn rbsp_drops_emulation_prevention_bytes() {
        assert_eq!(rbsp(&[0x60, 0, 0, 3, 0, 0xB0]), [0x60, 0, 0, 0, 0xB0]);
        assert_eq!(rbsp(&[0, 0, 3, 0, 0, 3]), [0, 0, 0, 0]);
        assert_eq!(rbsp(&[3, 0, 3]), [3, 0, 3]);
    }
}
