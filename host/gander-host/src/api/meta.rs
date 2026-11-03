// SPDX-License-Identifier: Apache-2.0
//! `GET /stat?meta=1`: image metadata from a header sniff.
//!
//! The render worker (WP-15) has no metadata operation yet, so the host reads only what is cheap
//! and safe in-process: at most [`SNIFF_BYTES`] from the start of the file, parsed with bounds-
//! checked slice access (no decoder, no allocation proportional to file content). Supported:
//! PNG, JPEG (SOF dimensions plus EXIF orientation, capture time and camera), GIF, BMP and WebP.
//! Video, audio, PDF page counts and Office page counts are not reported yet (`meta` is omitted);
//! they belong to a worker operation (WP-16 or later).

use std::fs::File;
use std::os::windows::fs::FileExt as _;

use gander_core::protocol::files::{EntryMeta, ImageMeta};

/// Bytes read from the start of the file.
pub const SNIFF_BYTES: usize = 256 * 1024;

/// Extensions worth sniffing.
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "dib", "webp",
];

/// Metadata for `file` (an open regular file with extension `ext`), when cheaply known.
pub fn sniff(file: &File, ext: Option<&str>) -> Option<EntryMeta> {
    if !ext.is_some_and(|e| IMAGE_EXTS.contains(&e)) {
        return None;
    }
    let mut buf = vec![0u8; SNIFF_BYTES];
    let mut filled = 0;
    while filled < buf.len() {
        match file.seek_read(&mut buf[filled..], filled as u64) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => return None,
        }
    }
    buf.truncate(filled);
    image_meta(&buf).map(EntryMeta::Image)
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn le24(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 3)?;
    Some(u32::from(s[0]) | u32::from(s[1]) << 8 | u32::from(s[2]) << 16)
}

fn plain(width: u32, height: u32) -> Option<ImageMeta> {
    (width > 0 && height > 0).then_some(ImageMeta {
        width,
        height,
        orientation: None,
        taken_at: None,
        camera: None,
    })
}

/// Parses image dimensions (and JPEG EXIF facts) from the first bytes of a file.
pub fn image_meta(b: &[u8]) -> Option<ImageMeta> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        if b.get(12..16)? != b"IHDR" {
            return None;
        }
        return plain(be32(b, 16)?, be32(b, 20)?);
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return plain(u32::from(le16(b, 6)?), u32::from(le16(b, 8)?));
    }
    if b.starts_with(b"BM") {
        let dib = le32(b, 14)?;
        return if dib == 12 {
            plain(u32::from(le16(b, 18)?), u32::from(le16(b, 20)?))
        } else {
            let w = i32::from_le_bytes(b.get(18..22)?.try_into().ok()?);
            let h = i32::from_le_bytes(b.get(22..26)?.try_into().ok()?);
            plain(w.unsigned_abs(), h.unsigned_abs())
        };
    }
    if b.starts_with(b"RIFF") && b.get(8..12)? == b"WEBP" {
        return match b.get(12..16)? {
            b"VP8 " => plain(
                u32::from(le16(b, 26)? & 0x3FFF),
                u32::from(le16(b, 28)? & 0x3FFF),
            ),
            b"VP8L" => {
                if *b.get(20)? != 0x2F {
                    return None;
                }
                let s = b.get(21..25)?;
                let w = 1 + (u32::from(s[0]) | (u32::from(s[1]) & 0x3F) << 8);
                let h = 1
                    + (u32::from(s[1]) >> 6
                        | u32::from(s[2]) << 2
                        | (u32::from(s[3]) & 0x0F) << 10);
                plain(w, h)
            }
            b"VP8X" => plain(le24(b, 24)? + 1, le24(b, 27)? + 1),
            _ => None,
        };
    }
    if b.starts_with(&[0xFF, 0xD8]) {
        return jpeg(b);
    }
    None
}

fn jpeg(b: &[u8]) -> Option<ImageMeta> {
    let mut exif = Exif::default();
    let mut i = 2;
    loop {
        // Skip to the next marker (fill bytes 0xFF are allowed).
        if *b.get(i)? != 0xFF {
            return None;
        }
        while *b.get(i)? == 0xFF {
            i += 1;
        }
        let marker = *b.get(i)?;
        i += 1;
        if marker == 0x01 || (0xD0..=0xD9).contains(&marker) {
            continue;
        }
        let len = usize::from(be16(b, i)?);
        if len < 2 {
            return None;
        }
        let data = b.get(i + 2..i + len)?;
        match marker {
            0xE1 if data.starts_with(b"Exif\0\0") => exif = parse_exif(&data[6..]),
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                let height = u32::from(be16(data, 1)?);
                let width = u32::from(be16(data, 3)?);
                let mut meta = plain(width, height)?;
                meta.orientation = exif.orientation.filter(|o| (1..=8).contains(o));
                meta.taken_at = exif.taken_at.or(exif.date_time);
                meta.camera = match (exif.make, exif.model) {
                    (Some(make), Some(model)) if model.starts_with(&make) => Some(model),
                    (Some(make), Some(model)) => Some(format!("{make} {model}")),
                    (make, model) => model.or(make),
                };
                return Some(meta);
            }
            0xDA => return None, // start of scan before any frame header
            _ => {}
        }
        i += len;
    }
}

#[derive(Debug, Default)]
struct Exif {
    orientation: Option<u8>,
    make: Option<String>,
    model: Option<String>,
    date_time: Option<String>,
    taken_at: Option<String>,
}

/// A minimal TIFF reader for IFD0 and the EXIF sub-IFD.
fn parse_exif(t: &[u8]) -> Exif {
    let mut out = Exif::default();
    let little = match t.get(0..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return out,
    };
    let u16_at = |at: usize| if little { le16(t, at) } else { be16(t, at) };
    let u32_at = |at: usize| if little { le32(t, at) } else { be32(t, at) };
    if u16_at(2) != Some(42) {
        return out;
    }
    let ascii = |count: u32, field: usize| -> Option<String> {
        let count = count as usize;
        let start = if count <= 4 {
            field
        } else {
            u32_at(field)? as usize
        };
        let raw = t.get(start..start.checked_add(count.min(128))?)?;
        let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        let s = String::from_utf8_lossy(&raw[..end]).trim().to_owned();
        (!s.is_empty()).then_some(s)
    };
    let mut ifds = vec![u32_at(4).map(|o| o as usize)];
    let mut visited = 0;
    while let Some(Some(ifd)) = ifds.pop() {
        visited += 1;
        if visited > 2 {
            break;
        }
        let Some(count) = u16_at(ifd) else { break };
        for n in 0..usize::from(count.min(256)) {
            let e = ifd + 2 + n * 12;
            let (Some(tag), Some(count)) = (u16_at(e), u32_at(e + 4)) else {
                break;
            };
            let field = e + 8;
            match tag {
                0x0112 => out.orientation = u16_at(field).and_then(|v| u8::try_from(v).ok()),
                0x010F => out.make = ascii(count, field),
                0x0110 => out.model = ascii(count, field),
                0x0132 => out.date_time = ascii(count, field),
                0x9003 => out.taken_at = ascii(count, field),
                0x8769 => ifds.push(u32_at(field).map(|o| o as usize)),
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_gif_bmp() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        let m = image_meta(&png).unwrap();
        assert_eq!((m.width, m.height), (640, 480));
        let gif = b"GIF89a\x20\x00\x10\x00".to_vec();
        let m = image_meta(&gif).unwrap();
        assert_eq!((m.width, m.height), (32, 16));
        let mut bmp = vec![0u8; 26];
        bmp[0..2].copy_from_slice(b"BM");
        bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
        bmp[18..22].copy_from_slice(&100i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&(-50i32).to_le_bytes());
        let m = image_meta(&bmp).unwrap();
        assert_eq!((m.width, m.height), (100, 50));
    }

    fn exif_jpeg() -> Vec<u8> {
        // TIFF, little endian: IFD0 with Make, Model, Orientation=6, ExifIFD -> DateTimeOriginal.
        let mut t = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
        let entries: u16 = 4;
        t.extend_from_slice(&entries.to_le_bytes());
        let ifd0_end = 8 + 2 + 12 * 4 + 4;
        let make_at = ifd0_end as u32;
        let model_at = make_at + 6;
        let exif_ifd_at = model_at + 8;
        let dto_at = exif_ifd_at + 2 + 12 + 4;
        let entry = |t: &mut Vec<u8>, tag: u16, ty: u16, count: u32, value: u32| {
            t.extend_from_slice(&tag.to_le_bytes());
            t.extend_from_slice(&ty.to_le_bytes());
            t.extend_from_slice(&count.to_le_bytes());
            t.extend_from_slice(&value.to_le_bytes());
        };
        entry(&mut t, 0x010F, 2, 6, make_at);
        entry(&mut t, 0x0110, 2, 8, model_at);
        entry(&mut t, 0x0112, 3, 1, 6);
        entry(&mut t, 0x8769, 4, 1, exif_ifd_at);
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(b"Canon\0");
        t.extend_from_slice(b"EOS R5\0\0");
        t.extend_from_slice(&1u16.to_le_bytes());
        entry(&mut t, 0x9003, 2, 20, dto_at);
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(b"2026:10:30 09:12:44\0");

        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE1];
        let app1_len = (2 + 6 + t.len()) as u16;
        j.extend_from_slice(&app1_len.to_be_bytes());
        j.extend_from_slice(b"Exif\0\0");
        j.extend_from_slice(&t);
        j.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        j.extend_from_slice(&3000u16.to_be_bytes());
        j.extend_from_slice(&4000u16.to_be_bytes());
        j.extend_from_slice(&[0; 10]);
        j
    }

    #[test]
    fn jpeg_with_exif() {
        let m = image_meta(&exif_jpeg()).unwrap();
        assert_eq!((m.width, m.height), (4000, 3000));
        assert_eq!(m.orientation, Some(6));
        assert_eq!(m.camera.as_deref(), Some("Canon EOS R5"));
        assert_eq!(m.taken_at.as_deref(), Some("2026:10:30 09:12:44"));
    }

    #[test]
    fn truncated_and_garbage_input_never_panics() {
        let j = exif_jpeg();
        for n in 0..j.len() {
            let _ = image_meta(&j[..n]);
        }
        let mut x: u32 = 0x1234_5678;
        for _ in 0..2000 {
            let mut buf = vec![0xFF, 0xD8];
            for _ in 0..64 {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                buf.push(x as u8);
            }
            let _ = image_meta(&buf);
        }
    }
}
