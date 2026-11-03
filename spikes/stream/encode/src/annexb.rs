// SPDX-License-Identifier: Apache-2.0
//! Annex-B byte-stream helpers: split on start codes and classify NAL units, so the spike can
//! check that the encoder emitted parameter sets and IDR pictures where it was asked to.

/// Which bitstream a buffer holds (the NAL header layout differs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bitstream {
    /// H.264 / AVC: `nal_unit_type` is the low 5 bits of the first header byte.
    H264,
    /// H.265 / HEVC: `nal_unit_type` is bits 1..=6 of the first header byte.
    Hevc,
}

/// Payloads of the NAL units in an Annex-B buffer (start codes stripped). Bytes before the first
/// start code are ignored.
pub fn nal_units(buf: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (n, &s) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map_or(buf.len(), |&next| next - 3);
        // A 4-byte start code leaves a zero byte at the end of the previous unit; trailing zero
        // bytes are not part of a NAL unit either.
        while end > s && buf[end - 1] == 0 {
            end -= 1;
        }
        if end > s {
            out.push(&buf[s..end]);
        }
    }
    out
}

/// `nal_unit_type` of a NAL unit payload.
pub fn nal_type(kind: Bitstream, nal: &[u8]) -> Option<u8> {
    let b = *nal.first()?;
    Some(match kind {
        Bitstream::H264 => b & 0x1F,
        Bitstream::Hevc => (b >> 1) & 0x3F,
    })
}

/// Summary of one access unit (one encoder output sample).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnitSummary {
    /// Number of NAL units.
    pub nals: usize,
    /// Contains an IDR (H.264 type 5; HEVC IDR_W_RADL 19 or IDR_N_LP 20).
    pub idr: bool,
    /// Contains a sequence parameter set (H.264 7, HEVC 33).
    pub sps: bool,
    /// Contains a picture parameter set (H.264 8, HEVC 34).
    pub pps: bool,
    /// Contains a video parameter set (HEVC 32 only).
    pub vps: bool,
}

/// Classifies the NAL units of one access unit.
pub fn summarize(kind: Bitstream, buf: &[u8]) -> UnitSummary {
    let mut s = UnitSummary::default();
    for nal in nal_units(buf) {
        s.nals += 1;
        match (kind, nal_type(kind, nal)) {
            (Bitstream::H264, Some(5)) | (Bitstream::Hevc, Some(19 | 20)) => s.idr = true,
            (Bitstream::H264, Some(7)) | (Bitstream::Hevc, Some(33)) => s.sps = true,
            (Bitstream::H264, Some(8)) | (Bitstream::Hevc, Some(34)) => s.pps = true,
            (Bitstream::Hevc, Some(32)) => s.vps = true,
            _ => {}
        }
    }
    s
}

/// True when the buffer starts with an Annex-B start code (3 or 4 bytes).
pub fn starts_with_start_code(buf: &[u8]) -> bool {
    buf.starts_with(&[0, 0, 1]) || buf.starts_with(&[0, 0, 0, 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let buf = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9, 9,
        ];
        let units = nal_units(&buf);
        assert_eq!(
            units,
            vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 9, 9][..]]
        );
        let s = summarize(Bitstream::H264, &buf);
        assert_eq!(
            s,
            UnitSummary {
                nals: 3,
                idr: true,
                sps: true,
                pps: true,
                vps: false
            }
        );
    }

    #[test]
    fn hevc_types() {
        // VPS 32 -> 0x40, SPS 33 -> 0x42, PPS 34 -> 0x44, IDR_W_RADL 19 -> 0x26, TRAIL_R 1 -> 0x02
        let buf = [
            0, 0, 1, 0x40, 1, 0, 0, 1, 0x42, 1, 0, 0, 1, 0x44, 1, 0, 0, 1, 0x26, 1,
        ];
        let s = summarize(Bitstream::Hevc, &buf);
        assert!(s.vps && s.sps && s.pps && s.idr);
        assert!(!summarize(Bitstream::Hevc, &[0, 0, 1, 0x02, 1]).idr);
    }

    #[test]
    fn non_idr_and_garbage() {
        assert!(!summarize(Bitstream::H264, &[0, 0, 1, 0x41, 1, 2]).idr);
        assert_eq!(nal_units(&[1, 2, 3]).len(), 0);
        assert!(starts_with_start_code(&[0, 0, 0, 1, 0x67]));
        assert!(!starts_with_start_code(&[0, 0, 2]));
    }
}
