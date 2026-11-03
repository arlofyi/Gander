// SPDX-License-Identifier: Apache-2.0
//! Deep-zoom tile pyramid geometry (WP-67).
//!
//! A page (an image, or one page of a PDF or Office document) is served as 512 x 512 tiles on a
//! pyramid, so a phone can zoom in past the width the preview endpoint renders (at most 2048).
//! The geometry is pure arithmetic shared by the host (which validates a tile request and tells
//! the worker what to cut out), the render worker and, through `protocol/test-vectors/tiles.json`,
//! the Android client:
//!
//! - The **top level** is the page at its natural resolution: an image's own pixels (after EXIF
//!   orientation), a PDF page at [`PDF_TILE_DPI`] dots per inch (reduced if that would exceed
//!   [`MAX_TOP_SIDE`]).
//! - Level `levels - 1` is the top level; every level below it is half the size of the one above
//!   (rounded up), down to level 0, the smallest level whose longest side fits in [`TILE_SIZE`]
//!   (so level 0 is a single tile and "the whole page fitted in 512").
//! - A level is cut into [`TILE_SIZE`] squares from the top left; tiles on the right and bottom
//!   edge are smaller (never padded).
//!
//! See `docs/specs/2026-11-01-preview-renderers.md` ("Deep-zoom tiles") for the endpoint.

use serde::{Deserialize, Serialize};

/// Edge length of a full tile in pixels.
pub const TILE_SIZE: u32 = 512;

/// Longest side of a top level; a larger page cannot be tiled (images) or is rendered at a lower
/// resolution (PDF).
pub const MAX_TOP_SIDE: u32 = 32_768;

/// Most levels a pyramid has: `TILE_SIZE * 2^6 = MAX_TOP_SIDE`.
pub const MAX_LEVELS: u32 = 7;

/// Resolution of the top level of a PDF page.
pub const PDF_TILE_DPI: f64 = 300.0;

/// Device-independent units per inch (`Windows.Data.Pdf` reports page sizes in 1/96 inch).
pub const DIPS_PER_INCH: f64 = 96.0;

/// Most pages `preview/info` lists tile sizes for; later pages of a longer document are not
/// offered deep zoom.
pub const MAX_TILE_PAGES: usize = 512;

/// One page's entry in `preview/info`: size in pixels at the top level, and level count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TilePage {
    /// Width of the top level in pixels.
    pub width: u32,
    /// Height of the top level in pixels.
    pub height: u32,
    /// Number of levels (`level` runs from 0 to `levels - 1`).
    pub levels: u32,
}

/// The tile pyramid of one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pyramid {
    top_width: u32,
    top_height: u32,
    levels: u32,
}

/// Where one tile sits and what it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    /// Pyramid level.
    pub level: u32,
    /// Column.
    pub x: u32,
    /// Row.
    pub y: u32,
    /// Width of the tile image in pixels (512, or less on the right edge).
    pub width: u32,
    /// Height of the tile image in pixels (512, or less on the bottom edge).
    pub height: u32,
    /// Left edge of the area the tile shows, in top-level pixels.
    pub src_x: u32,
    /// Top edge of the area the tile shows, in top-level pixels.
    pub src_y: u32,
    /// Width of the area the tile shows, in top-level pixels.
    pub src_width: u32,
    /// Height of the area the tile shows, in top-level pixels.
    pub src_height: u32,
}

fn ceil_div(a: u32, b: u32) -> u32 {
    a.div_ceil(b)
}

impl Pyramid {
    /// The pyramid of a page whose top level is `top_width` x `top_height` pixels. `None` for an
    /// empty page or one whose longest side exceeds [`MAX_TOP_SIDE`].
    pub fn new(top_width: u32, top_height: u32) -> Option<Self> {
        let longest = top_width.max(top_height);
        if top_width == 0 || top_height == 0 || longest > MAX_TOP_SIDE {
            return None;
        }
        let mut levels = 1;
        while ceil_div(longest, 1 << (levels - 1)) > TILE_SIZE {
            levels += 1;
        }
        Some(Self {
            top_width,
            top_height,
            levels,
        })
    }

    /// Number of levels, at least 1.
    pub fn levels(&self) -> u32 {
        self.levels
    }

    /// Size of the top level.
    pub fn top_size(&self) -> (u32, u32) {
        (self.top_width, self.top_height)
    }

    /// The `preview/info` entry for this page.
    pub fn page(&self) -> TilePage {
        TilePage {
            width: self.top_width,
            height: self.top_height,
            levels: self.levels,
        }
    }

    /// Size of `level` in pixels; `None` past the top level.
    pub fn level_size(&self, level: u32) -> Option<(u32, u32)> {
        if level >= self.levels {
            return None;
        }
        let k = self.levels - 1 - level;
        Some((
            ceil_div(self.top_width, 1 << k),
            ceil_div(self.top_height, 1 << k),
        ))
    }

    /// Tile columns and rows of `level`.
    pub fn grid(&self, level: u32) -> Option<(u32, u32)> {
        let (w, h) = self.level_size(level)?;
        Some((ceil_div(w, TILE_SIZE), ceil_div(h, TILE_SIZE)))
    }

    /// Tile `(x, y)` of `level`; `None` when the level or the position does not exist.
    pub fn tile(&self, level: u32, x: u32, y: u32) -> Option<Tile> {
        let (lw, lh) = self.level_size(level)?;
        let (cols, rows) = self.grid(level)?;
        if x >= cols || y >= rows {
            return None;
        }
        let k = self.levels - 1 - level;
        let (x0, y0) = (x * TILE_SIZE, y * TILE_SIZE);
        let (x1, y1) = ((x0 + TILE_SIZE).min(lw), (y0 + TILE_SIZE).min(lh));
        // Top-level coordinates; the last row and column are clipped to the page because a
        // rounded-up level can be a little larger than the page it stands for.
        let (sx0, sy0) = (x0 << k, y0 << k);
        let (sx1, sy1) = (
            (u64::from(x1) << k).min(u64::from(self.top_width)) as u32,
            (u64::from(y1) << k).min(u64::from(self.top_height)) as u32,
        );
        Some(Tile {
            level,
            x,
            y,
            width: x1 - x0,
            height: y1 - y0,
            src_x: sx0,
            src_y: sy0,
            src_width: sx1 - sx0,
            src_height: sy1 - sy0,
        })
    }
}

/// Top-level pixel size of a PDF page of `width_dip` x `height_dip` device-independent units
/// (1/96 inch): [`PDF_TILE_DPI`] dots per inch, reduced to fit [`MAX_TOP_SIDE`]. `None` for a
/// degenerate page.
pub fn pdf_top_size(width_dip: f64, height_dip: f64) -> Option<(u32, u32)> {
    if !(width_dip.is_finite() && height_dip.is_finite() && width_dip > 0.0 && height_dip > 0.0) {
        return None;
    }
    let scale = PDF_TILE_DPI / DIPS_PER_INCH;
    let (mut w, mut h) = (width_dip * scale, height_dip * scale);
    let longest = w.max(h);
    if longest > f64::from(MAX_TOP_SIDE) {
        let k = f64::from(MAX_TOP_SIDE) / longest;
        (w, h) = (w * k, h * k);
    }
    let clamp = |v: f64| (v.round() as u32).clamp(1, MAX_TOP_SIDE);
    Some((clamp(w), clamp(h)))
}

/// Maps `rect` (x, y, width, height), given in the coordinates of an image *as displayed* after
/// applying the EXIF `orientation` (1 to 8; anything else is 1) to a stored image of `stored`
/// (width, height) pixels, back to the stored pixels it covers. Used to cut a tile out of the
/// frame a decoder holds, before the orientation is applied to the (small) tile.
pub fn displayed_rect_to_stored(
    orientation: u16,
    stored: (u32, u32),
    rect: (u32, u32, u32, u32),
) -> (u32, u32, u32, u32) {
    let (w, h) = stored;
    let (x0, y0, x1, y1) = (rect.0, rect.1, rect.0 + rect.2, rect.1 + rect.3);
    // The inverse of the display transform on edge coordinates, for one corner.
    let inv = |dx: u32, dy: u32| -> (u32, u32) {
        match orientation {
            2 => (w.saturating_sub(dx), dy),
            3 => (w.saturating_sub(dx), h.saturating_sub(dy)),
            4 => (dx, h.saturating_sub(dy)),
            5 => (dy, dx),
            6 => (dy, h.saturating_sub(dx)),
            7 => (w.saturating_sub(dy), h.saturating_sub(dx)),
            8 => (w.saturating_sub(dy), dx),
            _ => (dx, dy),
        }
    };
    let (ax, ay) = inv(x0, y0);
    let (bx, by) = inv(x1, y1);
    (ax.min(bx), ay.min(by), ax.abs_diff(bx), ay.abs_diff(by))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn vectors() -> Value {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../protocol/test-vectors/tiles.json");
        let text = std::fs::read_to_string(path).expect("read protocol/test-vectors/tiles.json");
        serde_json::from_str(&text).expect("tiles.json parses")
    }

    fn pair(v: &Value) -> (u32, u32) {
        let a = v.as_array().expect("pair");
        (a[0].as_u64().unwrap() as u32, a[1].as_u64().unwrap() as u32)
    }

    fn quad(v: &Value) -> (u32, u32, u32, u32) {
        let a = v.as_array().expect("quad");
        let n = |i: usize| a[i].as_u64().unwrap() as u32;
        (n(0), n(1), n(2), n(3))
    }

    #[test]
    fn shared_vectors_match() {
        let doc = vectors();
        assert_eq!(doc["version"], 1);
        let list = doc["vectors"].as_array().expect("vectors");
        assert!(list.len() >= 8, "expected the full vector set");
        for v in list {
            let id = v["id"].as_str().unwrap();
            let (w, h) = pair(&v["top"]);
            let Some(p) = Pyramid::new(w, h) else {
                assert!(v["levels"].is_null(), "{id}: no pyramid expected");
                continue;
            };
            assert_eq!(
                Some(p.levels()),
                v["levels"].as_u64().map(|l| l as u32),
                "{id} levels"
            );
            let sizes = v["level_sizes"].as_array().unwrap();
            let grids = v["grids"].as_array().unwrap();
            assert_eq!(sizes.len() as u32, p.levels(), "{id} level count");
            for level in 0..p.levels() {
                assert_eq!(
                    p.level_size(level),
                    Some(pair(&sizes[level as usize])),
                    "{id} level {level} size"
                );
                assert_eq!(
                    p.grid(level),
                    Some(pair(&grids[level as usize])),
                    "{id} level {level} grid"
                );
            }
            assert_eq!(p.level_size(p.levels()), None, "{id}: past the top level");
            for t in v["tiles"].as_array().unwrap() {
                let (level, x, y) = (
                    t["level"].as_u64().unwrap() as u32,
                    t["x"].as_u64().unwrap() as u32,
                    t["y"].as_u64().unwrap() as u32,
                );
                let got = p.tile(level, x, y);
                if t["expect"].is_null() {
                    assert_eq!(got, None, "{id} tile {level}/{x}/{y} must not exist");
                    continue;
                }
                let tile = got.unwrap_or_else(|| panic!("{id} tile {level}/{x}/{y} must exist"));
                let e = &t["expect"];
                assert_eq!(
                    (tile.width, tile.height),
                    pair(&e["size"]),
                    "{id} {level}/{x}/{y} size"
                );
                assert_eq!(
                    (tile.src_x, tile.src_y, tile.src_width, tile.src_height),
                    quad(&e["src"]),
                    "{id} {level}/{x}/{y} source"
                );
            }
        }
    }

    #[test]
    fn every_pixel_of_the_top_level_is_covered_exactly_once() {
        for (w, h) in [
            (6000, 4500),
            (513, 1),
            (7016, 9933),
            (1, 1),
            (512, 512),
            (33, 4097),
        ] {
            let p = Pyramid::new(w, h).unwrap();
            let top = p.levels() - 1;
            let (cols, rows) = p.grid(top).unwrap();
            let mut covered = 0u64;
            for y in 0..rows {
                for x in 0..cols {
                    let t = p.tile(top, x, y).unwrap();
                    assert!(
                        t.width >= 1
                            && t.height >= 1
                            && t.width <= TILE_SIZE
                            && t.height <= TILE_SIZE
                    );
                    assert_eq!(
                        (t.width, t.height),
                        (t.src_width, t.src_height),
                        "top level is 1:1"
                    );
                    covered += u64::from(t.width) * u64::from(t.height);
                }
            }
            assert_eq!(covered, u64::from(w) * u64::from(h), "{w}x{h}");
        }
    }

    #[test]
    fn level_zero_is_one_tile_and_each_level_doubles() {
        for (w, h) in [
            (6000, 4500),
            (4000, 3000),
            (32_768, 32_768),
            (100, 100),
            (9933, 7016),
        ] {
            let p = Pyramid::new(w, h).unwrap();
            assert_eq!(p.grid(0), Some((1, 1)), "{w}x{h}");
            let (lw, lh) = p.level_size(0).unwrap();
            assert!(lw.max(lh) <= TILE_SIZE);
            if p.levels() > 1 {
                assert!(
                    lw.max(lh) > TILE_SIZE / 2,
                    "level 0 is the largest size that fits"
                );
            }
            for level in 1..p.levels() {
                let (a, b) = (
                    p.level_size(level - 1).unwrap(),
                    p.level_size(level).unwrap(),
                );
                assert_eq!(
                    a,
                    (b.0.div_ceil(2), b.1.div_ceil(2)),
                    "{w}x{h} level {level}"
                );
            }
            assert!(p.levels() <= MAX_LEVELS);
        }
    }

    #[test]
    fn unusable_pages_have_no_pyramid() {
        assert!(Pyramid::new(0, 10).is_none());
        assert!(Pyramid::new(10, 0).is_none());
        assert!(Pyramid::new(MAX_TOP_SIDE + 1, 10).is_none());
        assert_eq!(Pyramid::new(MAX_TOP_SIDE, 1).unwrap().levels(), MAX_LEVELS);
        assert_eq!(Pyramid::new(512, 512).unwrap().levels(), 1);
        assert_eq!(Pyramid::new(513, 1).unwrap().levels(), 2);
    }

    /// The display transform of pixel indices for an EXIF orientation (the definition the
    /// decoders and `image::DynamicImage::apply_orientation` follow).
    fn display(orientation: u16, (w, h): (u32, u32), (sx, sy): (u32, u32)) -> (u32, u32) {
        match orientation {
            2 => (w - 1 - sx, sy),
            3 => (w - 1 - sx, h - 1 - sy),
            4 => (sx, h - 1 - sy),
            5 => (sy, sx),
            6 => (h - 1 - sy, sx),
            7 => (h - 1 - sy, w - 1 - sx),
            8 => (sy, w - 1 - sx),
            _ => (sx, sy),
        }
    }

    #[test]
    fn a_displayed_rect_maps_back_to_exactly_the_stored_pixels_it_shows() {
        let stored = (7u32, 5u32);
        for orientation in 0..=9u16 {
            let swapped = (5..=8).contains(&orientation);
            let (dw, dh) = if swapped {
                (stored.1, stored.0)
            } else {
                stored
            };
            for rect in [
                (0, 0, dw, dh),
                (1, 0, 2, dh),
                (0, 2, dw, 3),
                (2, 1, 3, 2),
                (dw - 1, dh - 1, 1, 1),
            ] {
                let (rx, ry, rw, rh) = rect;
                let mut shown = Vec::new();
                for sy in 0..stored.1 {
                    for sx in 0..stored.0 {
                        let (dx, dy) = display(orientation, stored, (sx, sy));
                        if dx >= rx && dx < rx + rw && dy >= ry && dy < ry + rh {
                            shown.push((sx, sy));
                        }
                    }
                }
                let (x, y, w, h) = displayed_rect_to_stored(orientation, stored, rect);
                let mut expected: Vec<(u32, u32)> = Vec::new();
                for sy in y..y + h {
                    for sx in x..x + w {
                        expected.push((sx, sy));
                    }
                }
                shown.sort_unstable();
                expected.sort_unstable();
                assert_eq!(shown, expected, "orientation {orientation}, rect {rect:?}");
            }
        }
    }

    #[test]
    fn pdf_pages_are_tiled_at_300_dpi() {
        // A4: 8.27 x 11.69 in -> 794 x 1123 DIP -> 2481 x 3508 px (rounded).
        assert_eq!(pdf_top_size(793.7, 1122.5), Some((2480, 3508)));
        // A1 is 594 x 841 mm: 2245.0 x 3178.6 DIP -> 7016 x 9933 px.
        assert_eq!(pdf_top_size(2244.96, 3178.58), Some((7016, 9933)));
        // A huge banner is reduced to the longest side limit, keeping the aspect ratio.
        let (w, h) = pdf_top_size(19_200.0, 960.0).unwrap();
        assert_eq!(w, MAX_TOP_SIDE);
        assert_eq!(h, 1638);
        assert_eq!(pdf_top_size(0.0, 10.0), None);
        assert_eq!(pdf_top_size(f64::NAN, 10.0), None);
        assert_eq!(pdf_top_size(0.1, 0.1), Some((1, 1)));
    }
}
