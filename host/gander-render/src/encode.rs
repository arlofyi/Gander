// SPDX-License-Identifier: Apache-2.0
//! Output encoding: lossy WebP (alpha preserved when present) or baseline JPEG (alpha composited
//! over white). Only pixels are written: no EXIF, ICC or XMP reaches the output.

use gander_win::render::job::OutputFormat;
use image::codecs::jpeg::JpegEncoder;
use image::{RgbImage, RgbaImage};

use crate::{Failure, Rendered};

/// Encodes `img` in `format` at `quality` (1..=100).
pub fn encode(img: &RgbaImage, format: OutputFormat, quality: u8) -> Rendered<Vec<u8>> {
    let (w, h) = img.dimensions();
    let opaque = img.pixels().all(|p| p.0[3] == 255);
    match format {
        OutputFormat::Webp => {
            let encoded = if opaque {
                let rgb = to_rgb(img);
                webp::Encoder::from_rgb(rgb.as_raw(), w, h).encode_simple(false, f32::from(quality))
            } else {
                webp::Encoder::from_rgba(img.as_raw(), w, h)
                    .encode_simple(false, f32::from(quality))
            };
            encoded
                .map(|m| m.to_vec())
                .map_err(|e| Failure::Decode(format!("webp encoder: {e:?}")))
        }
        OutputFormat::Jpeg => {
            let rgb = to_rgb(img);
            let mut out = Vec::new();
            JpegEncoder::new_with_quality(&mut out, quality)
                .encode_image(&rgb)
                .map_err(Failure::decode)?;
            Ok(out)
        }
    }
}

/// Drops alpha by compositing over white.
fn to_rgb(img: &RgbaImage) -> RgbImage {
    RgbImage::from_fn(img.width(), img.height(), |x, y| {
        let [r, g, b, a] = img.get_pixel(x, y).0;
        let a = u16::from(a);
        let blend = |c: u8| ((u16::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
        image::Rgb([blend(r), blend(g), blend(b)])
    })
}
