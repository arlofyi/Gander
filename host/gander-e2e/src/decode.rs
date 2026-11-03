// SPDX-License-Identifier: Apache-2.0
//! Decoding what the loopback viewer received (WP-57): the access units (Annex B, as the
//! viewer's depacketiser hands them over) go to `ffmpeg` in a file; ffmpeg reports one
//! `framecrc` line per decoded picture and writes the last picture as a 24-bit BMP, which is read
//! here to find the test window's box by its colour.
//!
//! ffmpeg is the decoder because it is already the optional decode tool of the preflight and the
//! stream spike; without it the decode steps are deferred, not failed.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};
use bytes::Bytes;

/// An RGB colour (8 bits per channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Parses `RRGGBB` (the test window's pipe form).
    pub fn from_hex(s: &str) -> Option<Rgb> {
        if s.len() != 6 {
            return None;
        }
        let c = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok();
        Some(Rgb(c(0)?, c(2)?, c(4)?))
    }

    /// Largest per-channel difference.
    pub fn max_delta(self, o: Rgb) -> u8 {
        self.0
            .abs_diff(o.0)
            .max(self.1.abs_diff(o.1))
            .max(self.2.abs_diff(o.2))
    }

    /// `RRGGBB`.
    pub fn hex(self) -> String {
        format!("{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
}

/// A decoded picture, top-down rows of RGB.
#[derive(Debug, Clone)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pixels: Vec<Rgb>,
}

impl Picture {
    pub fn at(&self, x: usize, y: usize) -> Rgb {
        self.pixels[y * self.width + x]
    }

    /// Mean colour of the `(2r+1)^2` square around the normalised point `(nx, ny)`.
    pub fn sample(&self, nx: f64, ny: f64, r: usize) -> Rgb {
        let cx = ((nx * self.width as f64) as usize).min(self.width.saturating_sub(1));
        let cy = ((ny * self.height as f64) as usize).min(self.height.saturating_sub(1));
        let (mut sr, mut sg, mut sb, mut n) = (0u64, 0u64, 0u64, 0u64);
        for y in cy.saturating_sub(r)..=(cy + r).min(self.height - 1) {
            for x in cx.saturating_sub(r)..=(cx + r).min(self.width - 1) {
                let p = self.at(x, y);
                sr += u64::from(p.0);
                sg += u64::from(p.1);
                sb += u64::from(p.2);
                n += 1;
            }
        }
        let n = n.max(1);
        Rgb((sr / n) as u8, (sg / n) as u8, (sb / n) as u8)
    }

    /// The region of pixels within `tolerance` of `colour`: pixel count, bounding box and
    /// centroid (normalised to the picture).
    pub fn find(&self, colour: Rgb, tolerance: u8) -> Option<Blob> {
        let (mut n, mut sx, mut sy) = (0usize, 0usize, 0usize);
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
        for y in 0..self.height {
            for x in 0..self.width {
                if self.at(x, y).max_delta(colour) <= tolerance {
                    n += 1;
                    sx += x;
                    sy += y;
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        (n > 0).then(|| Blob {
            pixels: n,
            bbox: (x0, y0, x1 - x0 + 1, y1 - y0 + 1),
            centre: (
                (sx as f64 / n as f64 + 0.5) / self.width as f64,
                (sy as f64 / n as f64 + 0.5) / self.height as f64,
            ),
        })
    }
}

/// Pixels of one colour found in a picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Blob {
    pub pixels: usize,
    /// x, y, width, height in picture pixels.
    pub bbox: (usize, usize, usize, usize),
    /// Normalised centroid (0..1 on each axis).
    pub centre: (f64, f64),
}

impl Blob {
    /// Square-ish and mostly filled: a box, not scattered pixels of a similar colour.
    pub fn is_box(&self, min_side: usize) -> bool {
        let (_, _, w, h) = self.bbox;
        let aspect = w as f64 / h.max(1) as f64;
        let fill = self.pixels as f64 / (w * h).max(1) as f64;
        w >= min_side && h >= min_side && (0.6..=1.6).contains(&aspect) && fill > 0.6
    }
}

/// What ffmpeg made of a run of access units.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// Pictures decoded.
    pub frames: usize,
    /// The last one.
    pub last: Picture,
}

/// Decodes `aus` (Annex B access units, the first an IDR) with `ffmpeg`, using `dir` for files.
pub fn decode(ffmpeg: &Path, aus: &[Bytes], dir: &Path, tag: &str) -> Result<Decoded> {
    if aus.is_empty() {
        bail!("no access units to decode");
    }
    std::fs::create_dir_all(dir)?;
    let input = dir.join(format!("{tag}.h264"));
    let crc = dir.join(format!("{tag}.framecrc.txt"));
    let bmp = dir.join(format!("{tag}.last.bmp"));
    let mut bytes = Vec::with_capacity(aus.iter().map(Bytes::len).sum());
    for au in aus {
        bytes.extend_from_slice(au);
    }
    std::fs::write(&input, &bytes)?;
    let _ = std::fs::remove_file(&bmp);
    let out = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "h264",
            "-i",
        ])
        .arg(&input)
        .args(["-map", "0:v", "-f", "framecrc"])
        .arg(&crc)
        .args([
            "-map", "0:v", "-update", "1", "-pix_fmt", "bgr24", "-f", "image2",
        ])
        .arg(&bmp)
        .stdin(Stdio::null())
        .output()
        .context("run ffmpeg")?;
    if !out.status.success() {
        bail!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("")
        );
    }
    let frames = std::fs::read_to_string(&crc)
        .context("framecrc output")?
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .count();
    let last = parse_bmp(&std::fs::read(&bmp).context("the last picture")?)?;
    let _ = std::fs::remove_file(&input);
    Ok(Decoded { frames, last })
}

/// The access units from the last IDR at or before index `end` (exclusive) on; `None` without
/// an IDR.
pub fn from_last_idr<T>(frames: &[T], idr: impl Fn(&T) -> bool) -> Option<usize> {
    frames.iter().rposition(idr)
}

/// Parses a 24-bit uncompressed BMP (bottom-up or top-down).
pub fn parse_bmp(b: &[u8]) -> Result<Picture> {
    let u16_at = |o: usize| -> Result<u16> {
        Ok(u16::from_le_bytes(
            b.get(o..o + 2).context("short BMP")?.try_into()?,
        ))
    };
    let u32_at = |o: usize| -> Result<u32> {
        Ok(u32::from_le_bytes(
            b.get(o..o + 4).context("short BMP")?.try_into()?,
        ))
    };
    if b.get(0..2) != Some(b"BM") {
        bail!("not a BMP");
    }
    let offset = u32_at(10)? as usize;
    let width = u32_at(18)? as i32;
    let height = u32_at(22)? as i32;
    let bpp = u16_at(28)?;
    let compression = u32_at(30)?;
    if bpp != 24 || compression != 0 || width <= 0 || height == 0 {
        bail!("unsupported BMP: {bpp} bpp, compression {compression}, {width}x{height}");
    }
    let (w, h) = (width as usize, height.unsigned_abs() as usize);
    let row = (w * 3).div_ceil(4) * 4;
    if b.len() < offset + row * h {
        bail!("BMP pixel data is short");
    }
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        let src_row = if height > 0 { h - 1 - y } else { y };
        let r = &b[offset + src_row * row..];
        for x in 0..w {
            let p = &r[x * 3..x * 3 + 3];
            pixels.push(Rgb(p[2], p[1], p[0]));
        }
    }
    Ok(Picture {
        width: w,
        height: h,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bottom-up 24-bit BMP of `w` x `h` filled by `f(x, y)` (top-down coordinates).
    fn bmp(w: usize, h: usize, f: impl Fn(usize, usize) -> Rgb) -> Vec<u8> {
        let row = (w * 3).div_ceil(4) * 4;
        let mut b = Vec::new();
        b.extend_from_slice(b"BM");
        b.extend_from_slice(&((54 + row * h) as u32).to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&54u32.to_le_bytes());
        b.extend_from_slice(&40u32.to_le_bytes());
        b.extend_from_slice(&(w as i32).to_le_bytes());
        b.extend_from_slice(&(h as i32).to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&24u16.to_le_bytes());
        b.extend_from_slice(&[0; 24]);
        for y in (0..h).rev() {
            for x in 0..w {
                let p = f(x, y);
                b.extend_from_slice(&[p.2, p.1, p.0]);
            }
            b.resize(b.len() + row - w * 3, 0);
        }
        b
    }

    #[test]
    fn bmp_rows_come_out_top_down_and_the_box_is_found() {
        let orange = Rgb(255, 128, 0);
        let grey = Rgb(44, 44, 56);
        // 30x20 picture, box at x 10..20, y 4..14.
        let pic = parse_bmp(&bmp(30, 20, |x, y| {
            if (10..20).contains(&x) && (4..14).contains(&y) {
                Rgb(250, 131, 6)
            } else {
                grey
            }
        }))
        .unwrap();
        assert_eq!((pic.width, pic.height), (30, 20));
        assert_eq!(pic.at(0, 0), grey);
        assert_eq!(pic.at(10, 4), Rgb(250, 131, 6));
        let blob = pic.find(orange, 40).unwrap();
        assert_eq!(blob.pixels, 100);
        assert_eq!(blob.bbox, (10, 4, 10, 10));
        assert!((blob.centre.0 - 15.0 / 30.0).abs() < 1e-9, "{blob:?}");
        assert!((blob.centre.1 - 9.0 / 20.0).abs() < 1e-9, "{blob:?}");
        assert!(blob.is_box(8));
        assert!(!blob.is_box(11));
        assert!(pic.find(Rgb(0, 128, 255), 40).is_none());
        assert!(pic.sample(0.5, 0.45, 2).max_delta(orange) <= 6);
    }

    #[test]
    fn hex_round_trip_and_bad_input() {
        assert_eq!(Rgb::from_hex("FF8000"), Some(Rgb(255, 128, 0)));
        assert_eq!(Rgb(0, 128, 255).hex(), "0080FF");
        assert_eq!(Rgb::from_hex("FF80"), None);
        assert_eq!(Rgb::from_hex("GG8000"), None);
        assert!(parse_bmp(b"BMxx").is_err());
    }

    #[test]
    fn last_idr_index() {
        let f = [true, false, true, false, false];
        assert_eq!(from_last_idr(&f, |x| *x), Some(2));
        assert_eq!(from_last_idr(&[false, false], |x: &bool| *x), None);
    }
}
