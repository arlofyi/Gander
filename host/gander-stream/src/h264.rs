// SPDX-License-Identifier: Apache-2.0
//! H.264 Annex-B helpers: NAL unit scanning, IDR detection and the optional frame-id SEI (the
//! WP-43 latency probe; decoders ignore it).

/// `profile-level-id` the host offers first: constrained baseline, level 5.0. 1080x2280 is 9724
/// macroblocks, over level 4.x's 8192 (WP-42 finding).
pub const PROFILE_LEVEL_5_0: &str = "42e032";
/// Constrained baseline level 3.1, offered second for receivers that only match exact levels.
pub const PROFILE_LEVEL_3_1: &str = "42e01f";

/// The H.264 `fmtp` line for a `profile-level-id`.
pub fn fmtp(profile_level_id: &str) -> String {
    format!("level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={profile_level_id}")
}

/// UUID of the frame-id SEI (ASCII, no zero bytes).
pub const SEI_UUID: [u8; 16] = *b"GanderWP43Frame!";

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

/// The frame-id SEI NAL unit (`id=<n>;cap=<t>`), with a 4-byte start code.
pub fn sei_nal(id: u64, capture_100ns: i64) -> Vec<u8> {
    let text = format!("id={id};cap={capture_100ns}");
    let size = SEI_UUID.len() + text.len();
    let mut v = vec![0, 0, 0, 1, 0x06, 0x05, size.min(254) as u8];
    v.extend_from_slice(&SEI_UUID);
    v.extend_from_slice(text.as_bytes());
    v.push(0x80);
    v
}

/// The access unit with `sei` inserted before its first slice.
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
        v.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0xE0, 0x32]);
        v.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE, 0x3C, 0x80]);
        v.extend_from_slice(&[0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0x33]);
        v
    }

    #[test]
    fn sei_round_trips_before_the_slice() {
        let stamped = insert_sei(&au(), &sei_nal(42, -7));
        assert_eq!(nal_types(&stamped), vec![7, 8, 6, 5]);
        assert_eq!(parse_sei(&stamped), Some((42, -7)));
        assert!(is_idr(&stamped));
        assert!(!is_idr(&[0, 0, 1, 0x41, 1]));
    }

    #[test]
    fn fmtp_names_level_asymmetry() {
        assert_eq!(
            fmtp(PROFILE_LEVEL_5_0),
            "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e032"
        );
    }
}
