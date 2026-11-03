// SPDX-License-Identifier: Apache-2.0
//! Geometry and the fixed colours of the test window's client area.
//!
//! Everything here is a pure function of `(client width, client height, dpi)` and the frame
//! number, so a capture, an injector or a test can recompute exactly where each element is
//! without asking the window (the `layout?` pipe command returns the same numbers for a live
//! window).
//!
//! Coordinates are client-area pixels with the origin at the top-left of the client area (not
//! the window frame). Logical sizes below are for 96 dpi; at other dpi values they are scaled by
//! `dpi / 96` and rounded to the nearest pixel.
//!
//! ```text
//!  +------+-------------------------------------------+
//!  |  A   |  frame counter, 8 digits (decimal)         |   A = patch A  (255,0,255), 64x64
//!  |      |  clock HH:MM:SS.mmm                        |
//!  +------+-------------------------------------------+
//!  |  frame counter again as 24 black/white cells     |   strip: 24 cells of 16x16
//!  +--------------------------------------------------+
//!  |  arena: the box bounces inside this rectangle    |   arena: y from 128 down to
//!  |                                                  |   (client height - 64)
//!  +--------------------------------------+-----------+
//!  |  caption (title, pid, hits)          |    B      |   B = patch B  (0,255,0), 64x64
//!  +--------------------------------------+-----------+
//! ```

/// The dpi at which logical sizes are exact.
pub const BASE_DPI: u32 = 96;
/// Frames per second of the content. The frame number is `floor(elapsed_seconds * FPS)`.
pub const FPS: u32 = 60;

/// Initial client width at 96 dpi.
pub const LOGICAL_CLIENT_W: i32 = 640;
/// Initial client height at 96 dpi.
pub const LOGICAL_CLIENT_H: i32 = 480;

/// A colour with 8 bits per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `0x00RRGGBB`, which is also the in-memory layout (`B, G, R, 0`) of a 32-bit GDI DIB.
    pub const fn to_u32(self) -> u32 {
        ((self.0 as u32) << 16) | ((self.1 as u32) << 8) | self.2 as u32
    }

    /// Upper-case `RRGGBB`, the form used on the pipe.
    pub fn hex(self) -> String {
        format!("{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }

    /// Largest per-channel absolute difference to `other`.
    pub fn max_delta(self, other: Rgb) -> u8 {
        self.0
            .abs_diff(other.0)
            .max(self.1.abs_diff(other.1))
            .max(self.2.abs_diff(other.2))
    }

    /// Integer luma (Rec. 601) in 0..=255, used to read black/white strip cells.
    pub fn luma(self) -> u8 {
        ((299 * self.0 as u32 + 587 * self.1 as u32 + 114 * self.2 as u32) / 1000) as u8
    }
}

/// Patch A: top-left corner of the client area, 64x64 logical pixels, solid magenta.
pub const PATCH_A_RGB: Rgb = Rgb(255, 0, 255);
/// Patch B: bottom-right corner of the client area, 64x64 logical pixels, solid green.
pub const PATCH_B_RGB: Rgb = Rgb(0, 255, 0);
/// Side of both patches (logical pixels).
pub const PATCH_SIZE: i32 = 64;

/// Window background.
pub const BG_RGB: Rgb = Rgb(24, 24, 28);
/// Arena fill.
pub const ARENA_RGB: Rgb = Rgb(44, 44, 56);
/// One-pixel arena border.
pub const ARENA_BORDER_RGB: Rgb = Rgb(96, 96, 120);
/// Digits and clock.
pub const TEXT_RGB: Rgb = Rgb(255, 255, 255);
/// Strip cell for a 0 bit.
pub const STRIP_ZERO_RGB: Rgb = Rgb(0, 0, 0);
/// Strip cell for a 1 bit.
pub const STRIP_ONE_RGB: Rgb = Rgb(255, 255, 255);

/// Number of data cells in the counter strip.
pub const COUNTER_BITS: u32 = 24;
/// The frame counter shown on screen is `frame & COUNTER_MASK`.
pub const COUNTER_MASK: u32 = (1 << COUNTER_BITS) - 1;
/// Side of one strip cell (logical pixels).
pub const STRIP_CELL: i32 = 16;

/// Side of the moving box (logical pixels).
pub const BOX_SIZE: i32 = 64;
/// Box speed in logical pixels per frame, horizontal.
pub const BOX_STEP_X: i32 = 2;
/// Box speed in logical pixels per frame, vertical.
pub const BOX_STEP_Y: i32 = 1;
/// Box colours; a click on the box advances to the next one (wrapping). Index 0 is the start
/// colour. None of them equals a patch, background, arena or strip colour.
pub const BOX_PALETTE: [Rgb; 4] = [
    Rgb(255, 128, 0),
    Rgb(0, 128, 255),
    Rgb(255, 255, 0),
    Rgb(255, 0, 0),
];

/// An axis-aligned rectangle in client pixels (`x`, `y` is the top-left corner).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width, never negative.
    pub w: i32,
    /// Height, never negative.
    pub h: i32,
}

impl Rect {
    /// Rectangle from corner and size; negative sizes become 0.
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect {
            x,
            y,
            w: if w < 0 { 0 } else { w },
            h: if h < 0 { 0 } else { h },
        }
    }

    /// Exclusive right edge.
    pub const fn right(&self) -> i32 {
        self.x + self.w
    }

    /// Exclusive bottom edge.
    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    /// Centre point (rounded down).
    pub const fn centre(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    /// True when the point lies inside (left/top inclusive, right/bottom exclusive).
    pub const fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }

    /// True when the two rectangles share at least one pixel.
    pub const fn intersects(&self, other: &Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    /// `x,y,w,h`, the form used on the pipe.
    pub fn csv(&self) -> String {
        format!("{},{},{},{}", self.x, self.y, self.w, self.h)
    }
}

/// Position and pixel size of a run of bitmap-font text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSpec {
    /// Left edge of the first glyph.
    pub x: i32,
    /// Top edge of the first glyph.
    pub y: i32,
    /// Pixels per font dot (a glyph is 5x7 dots plus one dot of spacing).
    pub scale: i32,
}

impl TextSpec {
    /// Bounding box of `chars` glyphs (the trailing spacing dot is excluded).
    pub const fn bounds(&self, chars: i32) -> Rect {
        Rect::new(self.x, self.y, (chars * 6 - 1) * self.scale, 7 * self.scale)
    }
}

/// All element positions for one `(client size, dpi)` combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Client width in pixels.
    pub client_w: i32,
    /// Client height in pixels.
    pub client_h: i32,
    /// Dpi the layout was computed for.
    pub dpi: u32,
    /// Patch A rectangle (solid [`PATCH_A_RGB`]).
    pub patch_a: Rect,
    /// Patch B rectangle (solid [`PATCH_B_RGB`]).
    pub patch_b: Rect,
    /// The decimal frame counter, 8 digits.
    pub counter_text: TextSpec,
    /// The wall clock, `HH:MM:SS.mmm`.
    pub clock_text: TextSpec,
    /// The binary counter strip: `COUNTER_BITS` cells, most significant bit first.
    pub strip: Rect,
    /// Side of one strip cell in pixels.
    pub strip_cell: i32,
    /// The rectangle the box moves in.
    pub arena: Rect,
    /// Side of the box in pixels.
    pub box_size: i32,
    /// Box speed in pixels per frame (horizontal).
    pub box_step_x: i32,
    /// Box speed in pixels per frame (vertical).
    pub box_step_y: i32,
    /// Where the caption is drawn (left of patch B, below the arena).
    pub caption: Rect,
}

impl Layout {
    /// Layout for a client area of `client_w` x `client_h` pixels at `dpi`.
    ///
    /// Total for any input: tiny client areas give empty arenas and clamped rectangles rather
    /// than a panic.
    pub fn new(client_w: i32, client_h: i32, dpi: u32) -> Layout {
        let dpi = dpi.max(BASE_DPI / 2);
        let scale_pct = i64::from(dpi) * 100 / i64::from(BASE_DPI);
        let s = |v: i32| -> i32 { ((i64::from(v) * scale_pct + 50) / 100) as i32 };
        let client_w = client_w.max(0);
        let client_h = client_h.max(0);

        let patch = s(PATCH_SIZE);
        let strip_cell = s(STRIP_CELL).max(1);
        let arena_top = s(128);
        let bottom_band = patch;
        let arena = Rect::new(0, arena_top, client_w, client_h - arena_top - bottom_band);

        Layout {
            client_w,
            client_h,
            dpi,
            patch_a: Rect::new(0, 0, patch, patch),
            patch_b: Rect::new(
                (client_w - patch).max(0),
                (client_h - patch).max(0),
                patch,
                patch,
            ),
            counter_text: TextSpec {
                x: s(80),
                y: s(8),
                scale: s(6).max(1),
            },
            clock_text: TextSpec {
                x: s(80),
                y: s(58),
                scale: s(4).max(1),
            },
            strip: Rect::new(s(8), s(96), strip_cell * COUNTER_BITS as i32, strip_cell),
            strip_cell,
            arena,
            box_size: s(BOX_SIZE).max(1),
            box_step_x: s(BOX_STEP_X).max(1),
            box_step_y: s(BOX_STEP_Y).max(1),
            caption: Rect::new(
                s(8),
                client_h - bottom_band,
                (client_w - patch - s(16)).max(0),
                bottom_band,
            ),
        }
    }

    /// The layout of a freshly created window at `dpi`.
    pub fn initial(dpi: u32) -> Layout {
        let scale_pct = i64::from(dpi.max(BASE_DPI / 2)) * 100 / i64::from(BASE_DPI);
        let s = |v: i32| -> i32 { ((i64::from(v) * scale_pct + 50) / 100) as i32 };
        Layout::new(s(LOGICAL_CLIENT_W), s(LOGICAL_CLIENT_H), dpi)
    }

    /// Centre of strip cell `index` (0 is the most significant bit).
    pub fn strip_cell_centre(&self, index: u32) -> (i32, i32) {
        let x = self.strip.x + self.strip_cell * index as i32 + self.strip_cell / 2;
        (x, self.strip.y + self.strip_cell / 2)
    }

    /// Where the box is on `frame`. Deterministic: each axis is a triangle wave (constant speed,
    /// reflecting at the arena edges), so the position needs no state.
    pub fn box_rect(&self, frame: u32) -> Rect {
        let range_x = i64::from((self.arena.w - self.box_size).max(0));
        let range_y = i64::from((self.arena.h - self.box_size).max(0));
        let x = triangle(i64::from(frame) * i64::from(self.box_step_x), range_x);
        let y = triangle(i64::from(frame) * i64::from(self.box_step_y), range_y);
        Rect::new(
            self.arena.x + x as i32,
            self.arena.y + y as i32,
            self.box_size,
            self.box_size,
        )
    }
}

/// Triangle wave with period `2 * range`: 0, 1, .., range, range-1, .., 1, 0, 1, ..
fn triangle(pos: i64, range: i64) -> i64 {
    if range <= 0 {
        return 0;
    }
    let period = 2 * range;
    let m = pos.rem_euclid(period);
    if m <= range { m } else { period - m }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DPIS: [u32; 6] = [96, 120, 144, 168, 192, 288];

    #[test]
    fn triangle_wave_reflects() {
        let v: Vec<i64> = (0..9).map(|p| triangle(p, 3)).collect();
        assert_eq!(v, [0, 1, 2, 3, 2, 1, 0, 1, 2]);
        assert_eq!(triangle(5, 0), 0);
        assert_eq!(triangle(-1, 3), 1);
    }

    #[test]
    fn default_layout_matches_the_documented_numbers() {
        let l = Layout::initial(96);
        assert_eq!((l.client_w, l.client_h), (640, 480));
        assert_eq!(l.patch_a, Rect::new(0, 0, 64, 64));
        assert_eq!(l.patch_b, Rect::new(576, 416, 64, 64));
        assert_eq!(l.strip, Rect::new(8, 96, 384, 16));
        assert_eq!(l.arena, Rect::new(0, 128, 640, 288));
        assert_eq!(l.box_size, 64);
        assert_eq!((l.box_step_x, l.box_step_y), (2, 1));
        assert_eq!(
            l.counter_text,
            TextSpec {
                x: 80,
                y: 8,
                scale: 6
            }
        );
        assert_eq!(
            l.clock_text,
            TextSpec {
                x: 80,
                y: 58,
                scale: 4
            }
        );
    }

    #[test]
    fn elements_do_not_overlap_at_any_dpi() {
        for dpi in DPIS {
            let l = Layout::initial(dpi);
            let counter = l.counter_text.bounds(8);
            let clock = l.clock_text.bounds(12);
            let regions = [
                ("patch_a", l.patch_a),
                ("counter", counter),
                ("clock", clock),
                ("strip", l.strip),
                ("arena", l.arena),
                ("patch_b", l.patch_b),
            ];
            for (i, (na, a)) in regions.iter().enumerate() {
                assert!(
                    a.right() <= l.client_w && a.bottom() <= l.client_h,
                    "{na} @{dpi}"
                );
                for (nb, b) in &regions[i + 1..] {
                    assert!(!a.intersects(b), "{na} overlaps {nb} at {dpi} dpi");
                }
            }
        }
    }

    #[test]
    fn box_stays_inside_the_arena_and_moves() {
        for (w, h, dpi) in [
            (640, 480, 96),
            (800, 300, 96),
            (960, 720, 144),
            (1280, 1024, 192),
        ] {
            let l = Layout::new(w, h, dpi);
            let mut seen = std::collections::HashSet::new();
            for frame in 0..20_000u32 {
                let b = l.box_rect(frame);
                assert!(
                    b.x >= l.arena.x && b.right() <= l.arena.right(),
                    "x at {frame}"
                );
                assert!(
                    b.y >= l.arena.y && b.bottom() <= l.arena.bottom(),
                    "y at {frame}"
                );
                seen.insert((b.x, b.y));
            }
            assert!(seen.len() > 1000, "box barely moves: {}", seen.len());
        }
    }

    #[test]
    fn box_position_is_a_pure_function_of_the_frame() {
        let l = Layout::initial(96);
        assert_eq!(l.box_rect(0), Rect::new(0, 128, 64, 64));
        assert_eq!(l.box_rect(1), Rect::new(2, 129, 64, 64));
        assert_eq!(l.box_rect(777), l.box_rect(777));
        // Reflection at the right edge: arena width 640 - box 64 = 576 -> x peaks at frame 288.
        assert_eq!(l.box_rect(288).x, 576);
        assert_eq!(l.box_rect(289).x, 574);
    }

    #[test]
    fn tiny_clients_do_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (50, 50), (200, 100), (-5, -5)] {
            let l = Layout::new(w, h, 96);
            assert!(l.arena.w >= 0 && l.arena.h >= 0);
            let _ = l.box_rect(123);
        }
    }

    #[test]
    fn box_palette_is_distinct_from_every_fixed_colour() {
        let fixed = [
            PATCH_A_RGB,
            PATCH_B_RGB,
            BG_RGB,
            ARENA_RGB,
            ARENA_BORDER_RGB,
            TEXT_RGB,
            STRIP_ZERO_RGB,
            STRIP_ONE_RGB,
        ];
        for (i, c) in BOX_PALETTE.iter().enumerate() {
            assert!(
                !fixed.contains(c),
                "palette[{i}] collides with a fixed colour"
            );
            for d in &BOX_PALETTE[i + 1..] {
                assert_ne!(c, d);
            }
        }
    }

    #[test]
    fn rgb_helpers() {
        assert_eq!(Rgb(255, 0, 255).to_u32(), 0x00FF00FF);
        assert_eq!(Rgb(1, 2, 3).hex(), "010203");
        assert_eq!(Rgb(10, 20, 30).max_delta(Rgb(12, 20, 25)), 5);
        assert_eq!(Rgb(0, 0, 0).luma(), 0);
        assert_eq!(Rgb(255, 255, 255).luma(), 255);
    }
}
