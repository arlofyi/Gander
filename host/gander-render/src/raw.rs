// SPDX-License-Identifier: Apache-2.0
//! Camera RAW thumbnails from the embedded JPEG preview (never demosaiced in v1).
//!
//! - TIFF-based RAWs (CR2, NEF, ARW, DNG, ORF, RW2, PEF...): walk IFD0, the IFD chain and
//!   SubIFDs; candidates are `JPEGInterchangeFormat`/`Length` pairs and single-strip JPEG
//!   (compression 6 or 7) images. Only baseline/progressive JPEGs (SOF0/1/2) qualify, which skips
//!   the lossless-JPEG raw data some formats also store. The largest candidate wins; orientation
//!   comes from IFD0 tag 274.
//! - RAF: the header points straight at the preview JPEG.
//! - CR3 (ISO BMFF): the `PRVW` box holds the preview JPEG.
//!
//! When no preview is found the file goes to WIC, which decodes RAW only with the Raw Image
//! Extension installed; otherwise the result is "unsupported".

use std::fs::File;
use std::ops::Range;

use image::RgbaImage;

use crate::{Failure, Rendered, Target, read_all, wic};

/// Most IFDs visited (defends against loops and absurd files).
const MAX_IFDS: usize = 32;
/// Most entries read per IFD.
const MAX_ENTRIES: usize = 1024;

/// An embedded preview: byte range of the JPEG and the RAW's orientation, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// Byte range of the JPEG inside the file.
    pub range: Range<usize>,
    /// EXIF orientation from the RAW container.
    pub orientation: Option<u16>,
}

/// Thumbnail for a RAW file.
pub fn thumb(file: &File, size: u32) -> Rendered<RgbaImage> {
    decode(file, Target::Box(size))
}

/// Preview image for a RAW file, `width` pixels wide at most.
pub fn preview(file: &File, width: u32) -> Rendered<RgbaImage> {
    decode(file, Target::Width(width))
}

/// Deep-zoom tile of the preview image of a RAW file (the embedded JPEG, else the WIC frame).
pub fn tile(file: &File, level: u32, x: u32, y: u32) -> Rendered<RgbaImage> {
    let bytes = read_all(file)?;
    if let Some(p) = find_preview(&bytes) {
        return wic::tile_bytes(&bytes[p.range], p.orientation.or(Some(1)), level, x, y);
    }
    wic::tile_file(file, level, x, y).map_err(|e| match e {
        Failure::Unsupported(_) => Failure::unsupported("no embedded preview and no RAW codec"),
        other => other,
    })
}

/// Dimensions of the image a RAW preview would show (the embedded JPEG, else the WIC frame).
pub fn frame_size(file: &File) -> Rendered<(u32, u32)> {
    let bytes = read_all(file)?;
    if let Some(p) = find_preview(&bytes) {
        return wic::frame_size_bytes(&bytes[p.range], p.orientation.or(Some(1)));
    }
    wic::frame_size_file(file).map_err(|e| match e {
        Failure::Unsupported(_) => Failure::unsupported("no embedded preview and no RAW codec"),
        other => other,
    })
}

fn decode(file: &File, target: Target) -> Rendered<RgbaImage> {
    let bytes = read_all(file)?;
    if let Some(p) = find_preview(&bytes) {
        return wic::decode_bytes_target(&bytes[p.range], target, p.orientation.or(Some(1)));
    }
    wic::decode_file_target(file, target).map_err(|e| match e {
        Failure::Unsupported(_) => Failure::unsupported("no embedded preview and no RAW codec"),
        other => other,
    })
}

/// Finds the best embedded JPEG preview.
pub fn find_preview(b: &[u8]) -> Option<Preview> {
    if b.len() > 92 && b.starts_with(b"FUJIFILMCCD-RAW") {
        let off = be32(b, 84)? as usize;
        let len = be32(b, 88)? as usize;
        return jpeg_at(b, off, len).map(|range| Preview {
            range,
            orientation: None,
        });
    }
    if b.len() > 12 && &b[4..12] == b"ftypcrx " {
        return cr3_preview(b);
    }
    tiff_preview(b)
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Validates that `off..off+len` is in bounds and is a JPEG of a kind WIC decodes.
fn jpeg_at(b: &[u8], off: usize, len: usize) -> Option<Range<usize>> {
    let end = off.checked_add(len)?;
    let data = b.get(off..end)?;
    if len < 4 || data[0] != 0xFF || data[1] != 0xD8 || !is_displayable_jpeg(data) {
        return None;
    }
    Some(off..end)
}

/// Scans JPEG markers up to the first SOF and accepts SOF0/1/2 only.
fn is_displayable_jpeg(j: &[u8]) -> bool {
    let mut i = 2;
    while i + 4 <= j.len() {
        if j[i] != 0xFF {
            return false;
        }
        let marker = j[i + 1];
        match marker {
            0xC0..=0xC2 => return true,
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return false,
            0xD8 | 0x01 | 0xD0..=0xD7 | 0xFF => i += if marker == 0xFF { 1 } else { 2 },
            _ => {
                let seg = usize::from(u16::from_be_bytes([j[i + 2], j[i + 3]]));
                i += 2 + seg;
            }
        }
    }
    false
}

fn cr3_preview(b: &[u8]) -> Option<Preview> {
    let pos = b.windows(4).position(|w| w == b"PRVW")?;
    // Layout after the box type: u32, u16, u16 width, u16 height, u16, u32 jpeg length, JPEG.
    let len = be32(b, pos + 4 + 12)? as usize;
    let off = pos + 4 + 16;
    jpeg_at(b, off, len).map(|range| Preview {
        range,
        orientation: None,
    })
}

#[derive(Clone, Copy)]
struct Tiff<'a> {
    b: &'a [u8],
    le: bool,
}

impl Tiff<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        let s: [u8; 2] = self.b.get(at..at + 2)?.try_into().ok()?;
        Some(if self.le {
            u16::from_le_bytes(s)
        } else {
            u16::from_be_bytes(s)
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let s: [u8; 4] = self.b.get(at..at + 4)?.try_into().ok()?;
        Some(if self.le {
            u32::from_le_bytes(s)
        } else {
            u32::from_be_bytes(s)
        })
    }

    /// Value(s) of an entry as u32s (SHORT or LONG), following the offset when they do not fit
    /// in the entry.
    fn values(&self, entry: usize) -> Vec<u32> {
        let (Some(ty), Some(count)) = (self.u16(entry + 2), self.u32(entry + 4)) else {
            return Vec::new();
        };
        let count = (count as usize).min(64);
        let width = match ty {
            3 => 2,
            4 | 13 => 4,
            _ => return Vec::new(),
        };
        let base = if count * width <= 4 {
            entry + 8
        } else {
            match self.u32(entry + 8) {
                Some(o) => o as usize,
                None => return Vec::new(),
            }
        };
        (0..count)
            .filter_map(|i| {
                let at = base + i * width;
                if width == 2 {
                    self.u16(at).map(u32::from)
                } else {
                    self.u32(at)
                }
            })
            .collect()
    }
}

fn tiff_preview(b: &[u8]) -> Option<Preview> {
    let le = match b.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let t = Tiff { b, le };
    let first = t.u32(4)? as usize;
    let mut pending = vec![first];
    let mut seen = Vec::new();
    let mut best: Option<Range<usize>> = None;
    let mut orientation = None;

    while let Some(ifd) = pending.pop() {
        if ifd == 0 || seen.contains(&ifd) || seen.len() >= MAX_IFDS {
            continue;
        }
        seen.push(ifd);
        let Some(n) = t.u16(ifd) else { continue };
        let n = usize::from(n).min(MAX_ENTRIES);
        let (mut jif, mut jif_len) = (None, None);
        let (mut strip, mut strip_len, mut compression) = (None, None, None);
        for i in 0..n {
            let e = ifd + 2 + i * 12;
            let Some(tag) = t.u16(e) else { break };
            let first_value = || t.values(e).first().copied();
            match tag {
                0x0103 => compression = first_value(),
                0x0111 => strip = (t.values(e).len() == 1).then(first_value).flatten(),
                0x0117 => strip_len = (t.values(e).len() == 1).then(first_value).flatten(),
                0x0112 if seen.len() == 1 => orientation = first_value().map(|v| v as u16),
                0x0201 => jif = first_value(),
                0x0202 => jif_len = first_value(),
                0x014A => pending.extend(t.values(e).into_iter().map(|v| v as usize)),
                _ => {}
            }
        }
        let mut consider = |off: Option<u32>, len: Option<u32>| {
            if let (Some(off), Some(len)) = (off, len)
                && let Some(r) = jpeg_at(b, off as usize, len as usize)
                && best.as_ref().is_none_or(|cur| r.len() > cur.len())
            {
                best = Some(r);
            }
        };
        consider(jif, jif_len);
        if matches!(compression, Some(6 | 7)) {
            consider(strip, strip_len);
        }
        if let Some(next) = t.u32(ifd + 2 + n * 12) {
            pending.push(next as usize);
        }
    }
    best.map(|range| Preview { range, orientation })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal baseline-looking JPEG header (SOI, APP0 stub, SOF0) and EOI.
    fn fake_jpeg(extra: usize) -> Vec<u8> {
        let mut j = vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x02,
        ];
        j.extend(std::iter::repeat_n(0u8, extra));
        j.extend([0xFF, 0xD9]);
        j
    }

    fn le_tiff_with_preview(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        let mut b = b"II*\0".to_vec();
        b.extend(8u32.to_le_bytes());
        let entries: [(u16, u16, u32, u32); 3] = [
            (0x0112, 3, 1, u32::from(orientation)),
            (0x0201, 4, 1, 0), // patched below
            (0x0202, 4, 1, jpeg.len() as u32),
        ];
        b.extend((entries.len() as u16).to_le_bytes());
        let data_off = 8 + 2 + entries.len() * 12 + 4;
        for (tag, ty, count, value) in entries {
            b.extend(tag.to_le_bytes());
            b.extend(ty.to_le_bytes());
            b.extend(count.to_le_bytes());
            let v = if tag == 0x0201 {
                data_off as u32
            } else {
                value
            };
            b.extend(v.to_le_bytes());
        }
        b.extend(0u32.to_le_bytes());
        b.extend_from_slice(jpeg);
        b
    }

    #[test]
    fn tiff_preview_found_with_orientation() {
        let jpeg = fake_jpeg(10);
        let b = le_tiff_with_preview(&jpeg, 6);
        let p = find_preview(&b).unwrap();
        assert_eq!(&b[p.range.clone()], &jpeg[..]);
        assert_eq!(p.orientation, Some(6));
    }

    #[test]
    fn lossless_jpeg_is_skipped() {
        let mut jpeg = fake_jpeg(10);
        jpeg[9] = 0xC3; // SOF3: lossless raw data, not a preview
        let b = le_tiff_with_preview(&jpeg, 1);
        assert_eq!(find_preview(&b), None);
    }

    #[test]
    fn raf_header_points_at_preview() {
        let jpeg = fake_jpeg(4);
        let mut b = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
        b.resize(100, 0);
        let off = b.len() as u32;
        b[84..88].copy_from_slice(&off.to_be_bytes());
        b[88..92].copy_from_slice(&(jpeg.len() as u32).to_be_bytes());
        b.extend_from_slice(&jpeg);
        let p = find_preview(&b).unwrap();
        assert_eq!(&b[p.range], &jpeg[..]);
    }

    #[test]
    fn garbage_and_loops_do_not_panic() {
        assert_eq!(find_preview(b""), None);
        assert_eq!(find_preview(b"II*\0\x08\0\0\0"), None);
        // IFD pointing at itself.
        let mut b = b"II*\0\x08\0\0\0".to_vec();
        b.extend(0u16.to_le_bytes());
        b.extend(8u32.to_le_bytes());
        assert_eq!(find_preview(&b), None);
        let mut cr3 = vec![0u8; 4];
        cr3.extend(b"ftypcrx PRVW");
        assert_eq!(find_preview(&cr3), None);
    }
}
