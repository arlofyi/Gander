// SPDX-License-Identifier: Apache-2.0
//! `image` crate decoding for formats WIC lacks on this machine. Bounded by explicit decoder
//! limits on top of the worker's job-object memory cap.

use std::fs::File;
use std::io::Cursor;

use gander_core::tiles::Pyramid;
use gander_win::render::job::TILE_OUT_OF_RANGE_MSG;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits, RgbaImage};

use crate::{Failure, MAX_SOURCE_PIXELS, Rendered, Target, read_all};

/// Allocation limit for the `image` crate decoder.
const MAX_ALLOC: u64 = 384 * 1024 * 1024;

/// Decodes `file` with the `image` crate into a `size` box.
pub fn decode_file(file: &File, size: u32) -> Rendered<RgbaImage> {
    decode_file_target(file, Target::Box(size))
}

/// Decodes `file` with the `image` crate.
pub fn decode_file_target(file: &File, target: Target) -> Rendered<RgbaImage> {
    let bytes = read_all(file)?;
    decode_bytes(&bytes, target)
}

/// Dimensions (after orientation) of an image the `image` crate can read, without decoding the
/// pixels.
pub fn frame_size_file(file: &File) -> Rendered<(u32, u32)> {
    let bytes = read_all(file)?;
    let mut reader = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(Failure::decode)?;
    if reader.format().is_none() {
        return Err(Failure::unsupported("unknown image format"));
    }
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(map_err)?;
    let (w, h) = decoder.dimensions();
    let swapped = decoder.orientation().map_err(map_err)?.to_exif() >= 5;
    Ok(if swapped { (h, w) } else { (w, h) })
}

/// Decodes in-memory bytes with the `image` crate, applying EXIF orientation.
pub fn decode_bytes(bytes: &[u8], target: Target) -> Rendered<RgbaImage> {
    let rgba = decode_oriented(bytes)?;
    let (tw, th) = target.fit(rgba.width(), rgba.height());
    let out = if (tw, th) == rgba.dimensions() {
        rgba
    } else {
        image::imageops::thumbnail(&rgba, tw, th)
    };
    Ok(out)
}

/// Deep-zoom tile `(level, x, y)` of the image behind `file`. The `image` crate has no region
/// decode, so the whole image is decoded (within the decoder limits) and the tile cut from it:
/// only for formats WIC cannot read.
pub fn tile_file(file: &File, level: u32, x: u32, y: u32) -> Rendered<RgbaImage> {
    let bytes = read_all(file)?;
    let rgba = decode_oriented(&bytes)?;
    let pyramid = Pyramid::new(rgba.width(), rgba.height())
        .ok_or_else(|| Failure::unsupported("image too large to tile"))?;
    let tile = pyramid
        .tile(level, x, y)
        .ok_or_else(|| Failure::decode(TILE_OUT_OF_RANGE_MSG))?;
    let area = image::imageops::crop_imm(
        &rgba,
        tile.src_x,
        tile.src_y,
        tile.src_width,
        tile.src_height,
    )
    .to_image();
    Ok(if area.dimensions() == (tile.width, tile.height) {
        area
    } else {
        image::imageops::thumbnail(&area, tile.width, tile.height)
    })
}

/// The decoded image as RGBA with its EXIF orientation applied.
fn decode_oriented(bytes: &[u8]) -> Rendered<RgbaImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(Failure::decode)?;
    if reader.format().is_none() {
        return Err(Failure::unsupported("unknown image format"));
    }
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_ALLOC);
    limits.max_image_width = Some(65_535);
    limits.max_image_height = Some(65_535);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(map_err)?;
    let (w, h) = decoder.dimensions();
    if u64::from(w) * u64::from(h) > MAX_SOURCE_PIXELS {
        return Err(Failure::Decode(format!(
            "declared {w}x{h} exceeds the pixel limit"
        )));
    }
    let orientation = decoder.orientation().map_err(map_err)?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(map_err)?;
    img.apply_orientation(orientation);
    let rgba = img.into_rgba8();
    Ok(rgba)
}

fn map_err(e: image::ImageError) -> Failure {
    match e {
        image::ImageError::Unsupported(u) => Failure::unsupported(u),
        other => Failure::decode(other),
    }
}
