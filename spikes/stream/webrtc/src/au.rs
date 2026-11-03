// SPDX-License-Identifier: Apache-2.0
//! H.264 access-unit helpers: a frame-id SEI stamped into every access unit by the sender and
//! read back by the receiver, so latency is matched per frame even when packets are dropped.
//!
//! The SEI is `user_data_unregistered` (payload type 5): a 16-byte UUID followed by ASCII text
//! `id=<n>;cap=<qpc 100 ns>`. Neither part contains a zero byte, so no emulation-prevention bytes
//! are needed. Decoders ignore it (an Android `MediaCodec` too), which makes it usable by WP-44.

/// UUID of the frame-id SEI (ASCII, no zero bytes).
pub const SEI_UUID: [u8; 16] = *b"GanderWP43Frame!";

/// An access unit delimiter NAL unit with a start code (`primary_pic_type` 7 = any).
pub const AUD: [u8; 6] = [0, 0, 0, 1, 0x09, 0xF0];

/// Offsets of the NAL units in an Annex-B buffer: (start of the start code, start of the NAL
/// header). A 4-byte start code is reported from its leading zero.
pub fn nal_offsets(buf: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1 {
            let sc = if i > 0 && buf[i - 1] == 0 { i - 1 } else { i };
            out.push((sc, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    out
}

/// `nal_unit_type` of every NAL unit in the buffer.
pub fn nal_types(buf: &[u8]) -> Vec<u8> {
    nal_offsets(buf)
        .into_iter()
        .filter_map(|(_, h)| buf.get(h).map(|b| b & 0x1F))
        .collect()
}

/// True when the access unit holds an IDR slice (type 5).
pub fn is_idr(buf: &[u8]) -> bool {
    nal_types(buf).contains(&5)
}

/// The frame-id SEI NAL unit, with a 4-byte start code.
pub fn sei_nal(id: u64, capture_100ns: i64) -> Vec<u8> {
    let text = format!("id={id};cap={capture_100ns}");
    let size = SEI_UUID.len() + text.len();
    debug_assert!(size < 255);
    let mut v = vec![0, 0, 0, 1, 0x06, 0x05, size as u8];
    v.extend_from_slice(&SEI_UUID);
    v.extend_from_slice(text.as_bytes());
    v.push(0x80); // rbsp_trailing_bits
    v
}

/// The access unit with `sei` inserted before its first slice (H.264 requires SEI before the
/// first VCL NAL unit of the access unit). Appended when there is no slice.
pub fn insert_sei(au: &[u8], sei: &[u8]) -> Vec<u8> {
    let at = nal_offsets(au)
        .into_iter()
        .find(|&(_, h)| matches!(au.get(h).map(|b| b & 0x1F), Some(1 | 5)))
        .map_or(au.len(), |(sc, _)| sc);
    let mut v = Vec::with_capacity(au.len() + sei.len());
    v.extend_from_slice(&au[..at]);
    v.extend_from_slice(sei);
    v.extend_from_slice(&au[at..]);
    v
}

/// Reads the frame-id SEI back: (id, capture time).
pub fn parse_sei(au: &[u8]) -> Option<(u64, i64)> {
    for (_, h) in nal_offsets(au) {
        let nal = &au[h..];
        if nal.len() < 3 + SEI_UUID.len() || nal[0] & 0x1F != 6 || nal[1] != 5 {
            continue;
        }
        let size = usize::from(nal[2]);
        let body = nal.get(3..3 + size)?;
        if body[..16] != SEI_UUID {
            continue;
        }
        let text = std::str::from_utf8(&body[16..]).ok()?;
        let mut id = None;
        let mut cap = None;
        for part in text.split(';') {
            if let Some(v) = part.strip_prefix("id=") {
                id = v.parse().ok();
            } else if let Some(v) = part.strip_prefix("cap=") {
                cap = v.parse().ok();
            }
        }
        return Some((id?, cap?));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn au() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0xE0, 0x32]); // SPS
        v.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE, 0x3C, 0x80]); // PPS
        v.extend_from_slice(&[0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0x33]); // IDR slice
        v
    }

    #[test]
    fn sei_round_trips_and_sits_before_the_slice() {
        let stamped = insert_sei(&au(), &sei_nal(42, -7_000_123));
        assert_eq!(nal_types(&stamped), vec![7, 8, 6, 5]);
        assert_eq!(parse_sei(&stamped), Some((42, -7_000_123)));
        assert!(is_idr(&stamped));
    }

    #[test]
    fn sei_has_no_start_code_emulation() {
        let sei = sei_nal(u64::MAX, i64::MIN);
        assert_eq!(nal_offsets(&sei).len(), 1);
        assert!(!sei[4..].windows(2).any(|w| w == [0, 0]));
    }

    #[test]
    fn no_sei_no_id() {
        assert_eq!(parse_sei(&au()), None);
        assert_eq!(parse_sei(&[]), None);
    }
}
