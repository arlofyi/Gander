// SPDX-License-Identifier: Apache-2.0
//! Software rendering of the test window's content, plus the readers that recover values from
//! captured pixels.
//!
//! The frame is drawn straight into a `0x00RRGGBB` pixel buffer (the memory layout of a 32-bit
//! GDI DIB), with no GDI, no anti-aliasing and no font dependency, so every colour is exact and
//! the output is identical on every machine. The decimal digits use a built-in 5x7 dot font.
//!
//! What a capture can verify (all positions from [`crate::layout::Layout`]):
//!
//! - **Colour patches.** Patch A (top-left) is exactly `RGB(255,0,255)` and patch B (bottom-right)
//!   exactly `RGB(0,255,0)`, 64x64 logical pixels each. Use [`patch_matches`].
//! - **Frame counter.** `frame & 0xFFFFFF` is drawn as 8 decimal digits (for people and photos)
//!   and as 24 black/white cells, most significant bit first (for programs). Use
//!   [`decode_counter`], which tolerates codec noise by thresholding luma at 128.
//! - **Box.** A square of the current palette colour at [`Layout::box_rect`] for the frame.
//! - **Clock.** Local time `HH:MM:SS.mmm` in the same dot font.

use crate::layout::{
    ARENA_BORDER_RGB, ARENA_RGB, BG_RGB, COUNTER_BITS, COUNTER_MASK, Layout, PATCH_A_RGB,
    PATCH_B_RGB, Rect, Rgb, STRIP_ONE_RGB, STRIP_ZERO_RGB, TEXT_RGB, TextSpec,
};

/// Wall-clock time drawn in the clock row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClockTime {
    /// Hour, 0..=23.
    pub hour: u8,
    /// Minute, 0..=59.
    pub minute: u8,
    /// Second, 0..=59.
    pub second: u8,
    /// Millisecond, 0..=999.
    pub milli: u16,
}

impl ClockTime {
    /// `HH:MM:SS.mmm`.
    pub fn text(&self) -> String {
        format!(
            "{:02}:{:02}:{:02}.{:03}",
            self.hour, self.minute, self.second, self.milli
        )
    }
}

/// Everything that varies from frame to frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameState {
    /// Frame number (`floor(elapsed * 60)`); only the low 24 bits are shown.
    pub frame: u32,
    /// Time shown in the clock row.
    pub clock: ClockTime,
    /// Current box colour.
    pub box_rgb: Rgb,
}

/// A mutable view of a `0x00RRGGBB` pixel buffer with tightly packed rows.
#[derive(Debug)]
pub struct Canvas<'a> {
    pixels: &'a mut [u32],
    width: usize,
    height: usize,
}

impl<'a> Canvas<'a> {
    /// Wraps `pixels`; `None` when the slice is shorter than `width * height`.
    pub fn new(pixels: &'a mut [u32], width: usize, height: usize) -> Option<Canvas<'a>> {
        (pixels.len() >= width.checked_mul(height)?).then_some(Canvas {
            pixels,
            width,
            height,
        })
    }

    /// Width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// Pixel at `(x, y)`, `None` outside the canvas.
    pub fn get(&self, x: i32, y: i32) -> Option<Rgb> {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return None;
        }
        let p = self.pixels[y as usize * self.width + x as usize];
        Some(Rgb((p >> 16) as u8, (p >> 8) as u8, p as u8))
    }

    /// Fills the whole canvas.
    pub fn clear(&mut self, colour: Rgb) {
        self.pixels[..self.width * self.height].fill(colour.to_u32());
    }

    /// Fills `rect`, clipped to the canvas.
    pub fn fill_rect(&mut self, rect: Rect, colour: Rgb) {
        let x0 = rect.x.max(0) as usize;
        let y0 = rect.y.max(0) as usize;
        let x1 = (rect.right().max(0) as usize).min(self.width);
        let y1 = (rect.bottom().max(0) as usize).min(self.height);
        if x0 >= x1 {
            return;
        }
        let value = colour.to_u32();
        for y in y0..y1 {
            self.pixels[y * self.width + x0..y * self.width + x1].fill(value);
        }
    }

    /// Draws `text` in the dot font. Characters the font lacks are left blank.
    pub fn draw_text(&mut self, spec: TextSpec, text: &str, colour: Rgb) {
        for (i, ch) in text.chars().enumerate() {
            let Some(rows) = glyph(ch) else { continue };
            let gx = spec.x + (i as i32) * 6 * spec.scale;
            for (row, bits) in rows.iter().enumerate() {
                for col in 0..5 {
                    if bits & (0b10000 >> col) != 0 {
                        self.fill_rect(
                            Rect::new(
                                gx + col * spec.scale,
                                spec.y + row as i32 * spec.scale,
                                spec.scale,
                                spec.scale,
                            ),
                            colour,
                        );
                    }
                }
            }
        }
    }
}

/// 5x7 dot font for the characters the window draws. Each row is 5 bits, bit 4 leftmost.
fn glyph(ch: char) -> Option<[u8; 7]> {
    Some(match ch {
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        ':' => [
            0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000,
        ],
        '.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ],
        ' ' => [0; 7],
        _ => return None,
    })
}

/// The 8-digit decimal text of the frame counter.
pub fn counter_digits(frame: u32) -> String {
    format!("{:08}", frame & COUNTER_MASK)
}

/// Draws one complete frame (everything except the GDI caption) into `canvas`.
///
/// `layout` must have been computed for the canvas size. The patches are drawn last so nothing
/// can ever cover them.
pub fn draw_frame(canvas: &mut Canvas<'_>, layout: &Layout, state: &FrameState) {
    canvas.clear(BG_RGB);

    // Arena with a one pixel border, then the box.
    canvas.fill_rect(layout.arena, ARENA_BORDER_RGB);
    canvas.fill_rect(
        Rect::new(
            layout.arena.x + 1,
            layout.arena.y + 1,
            layout.arena.w - 2,
            layout.arena.h - 2,
        ),
        ARENA_RGB,
    );
    canvas.fill_rect(layout.box_rect(state.frame), state.box_rgb);

    // Text rows.
    canvas.draw_text(layout.counter_text, &counter_digits(state.frame), TEXT_RGB);
    canvas.draw_text(layout.clock_text, &state.clock.text(), TEXT_RGB);

    // Binary strip, most significant bit first.
    let counter = state.frame & COUNTER_MASK;
    for i in 0..COUNTER_BITS {
        let bit = (counter >> (COUNTER_BITS - 1 - i)) & 1;
        canvas.fill_rect(
            Rect::new(
                layout.strip.x + layout.strip_cell * i as i32,
                layout.strip.y,
                layout.strip_cell,
                layout.strip_cell,
            ),
            if bit == 1 {
                STRIP_ONE_RGB
            } else {
                STRIP_ZERO_RGB
            },
        );
    }

    // Calibration patches last.
    canvas.fill_rect(layout.patch_a, PATCH_A_RGB);
    canvas.fill_rect(layout.patch_b, PATCH_B_RGB);
}

/// Reads the frame counter back from pixels. `sample(x, y)` returns the pixel at a client
/// coordinate (add the client origin yourself when reading from a window capture) or `None`
/// when it cannot be read.
///
/// Returns `None` if any cell is unreadable or ambiguous (luma between 64 and 192), so noise
/// never yields a wrong counter silently. The result is `frame & 0xFFFFFF`.
pub fn decode_counter(layout: &Layout, sample: impl Fn(i32, i32) -> Option<Rgb>) -> Option<u32> {
    let mut value = 0u32;
    for i in 0..COUNTER_BITS {
        let (x, y) = layout.strip_cell_centre(i);
        let luma = sample(x, y)?.luma();
        let bit = match luma {
            0..64 => 0,
            192..=255 => 1,
            _ => return None,
        };
        value = (value << 1) | bit;
    }
    Some(value)
}

/// True when a 3x3 grid of points inside `rect` all equal `expected` within `tolerance` per
/// channel (0 for an exact match; use 8-16 after lossy video coding).
pub fn patch_matches(
    rect: Rect,
    expected: Rgb,
    tolerance: u8,
    sample: impl Fn(i32, i32) -> Option<Rgb>,
) -> bool {
    if rect.w < 3 || rect.h < 3 {
        return false;
    }
    for gy in 1..=3 {
        for gx in 1..=3 {
            let x = rect.x + rect.w * gx / 4;
            let y = rect.y + rect.h * gy / 4;
            match sample(x, y) {
                Some(c) if c.max_delta(expected) <= tolerance => {}
                _ => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::BOX_PALETTE;

    fn render(layout: &Layout, frame: u32, box_rgb: Rgb) -> Vec<u32> {
        let (w, h) = (layout.client_w as usize, layout.client_h as usize);
        let mut buf = vec![0u32; w * h];
        let mut canvas = Canvas::new(&mut buf, w, h).unwrap();
        let state = FrameState {
            frame,
            clock: ClockTime {
                hour: 13,
                minute: 7,
                second: 59,
                milli: 42,
            },
            box_rgb,
        };
        draw_frame(&mut canvas, layout, &state);
        buf
    }

    fn reader(buf: &[u32], layout: &Layout) -> impl Fn(i32, i32) -> Option<Rgb> {
        let (w, h) = (layout.client_w, layout.client_h);
        let buf = buf.to_vec();
        move |x, y| {
            if x < 0 || y < 0 || x >= w || y >= h {
                return None;
            }
            let p = buf[(y * w + x) as usize];
            Some(Rgb((p >> 16) as u8, (p >> 8) as u8, p as u8))
        }
    }

    #[test]
    fn patches_are_exact_rgb_at_every_pixel_for_several_dpis() {
        for dpi in [96, 120, 144, 192] {
            let l = Layout::initial(dpi);
            let buf = render(&l, 12345, BOX_PALETTE[0]);
            let at = reader(&buf, &l);
            for (rect, rgb) in [(l.patch_a, PATCH_A_RGB), (l.patch_b, PATCH_B_RGB)] {
                for y in rect.y..rect.bottom() {
                    for x in rect.x..rect.right() {
                        assert_eq!(at(x, y), Some(rgb), "({x},{y}) at {dpi} dpi");
                    }
                }
                assert!(patch_matches(rect, rgb, 0, &at));
            }
            // The documented corners at 96 dpi.
            if dpi == 96 {
                assert_eq!(at(0, 0), Some(Rgb(255, 0, 255)));
                assert_eq!(at(63, 63), Some(Rgb(255, 0, 255)));
                assert_eq!(at(639, 479), Some(Rgb(0, 255, 0)));
                assert_eq!(at(576, 416), Some(Rgb(0, 255, 0)));
            }
        }
    }

    #[test]
    fn patch_matcher_rejects_wrong_colour_and_honours_tolerance() {
        let l = Layout::initial(96);
        let buf = render(&l, 1, BOX_PALETTE[0]);
        let at = reader(&buf, &l);
        assert!(!patch_matches(l.patch_a, PATCH_B_RGB, 0, &at));
        assert!(patch_matches(l.patch_a, Rgb(250, 6, 252), 8, &at));
        assert!(!patch_matches(l.patch_a, Rgb(250, 6, 252), 4, &at));
        assert!(!patch_matches(l.patch_a, PATCH_A_RGB, 0, |_, _| None));
    }

    #[test]
    fn counter_round_trips_through_pixels() {
        for dpi in [96, 125, 150, 200] {
            let l = Layout::initial(dpi);
            for frame in [0, 1, 2, 59, 60, 1234, 0xAAAAAA, 0x555555, COUNTER_MASK] {
                let buf = render(&l, frame, BOX_PALETTE[1]);
                assert_eq!(
                    decode_counter(&l, reader(&buf, &l)),
                    Some(frame),
                    "{frame} @{dpi}"
                );
            }
        }
    }

    #[test]
    fn counter_wraps_at_24_bits_and_digits_match() {
        let l = Layout::initial(96);
        let buf = render(&l, COUNTER_MASK + 5, BOX_PALETTE[0]);
        assert_eq!(decode_counter(&l, reader(&buf, &l)), Some(4));
        assert_eq!(counter_digits(4), "00000004");
        assert_eq!(counter_digits(COUNTER_MASK), "16777215");
        assert_eq!(counter_digits(COUNTER_MASK + 1), "00000000");
    }

    #[test]
    fn decoder_refuses_ambiguous_cells() {
        let l = Layout::initial(96);
        let buf = render(&l, 99, BOX_PALETTE[0]);
        let at = reader(&buf, &l);
        let (bad_x, bad_y) = l.strip_cell_centre(10);
        let grey = move |x, y| {
            if (x, y) == (bad_x, bad_y) {
                Some(Rgb(128, 128, 128))
            } else {
                at(x, y)
            }
        };
        assert_eq!(decode_counter(&l, grey), None);
        assert_eq!(decode_counter(&l, |_, _| None), None);
    }

    #[test]
    fn decoder_tolerates_lossy_noise() {
        let l = Layout::initial(96);
        let buf = render(&l, 4242, BOX_PALETTE[0]);
        let at = reader(&buf, &l);
        let noisy = move |x: i32, y: i32| {
            at(x, y).map(|c| {
                let n = (x * 7 + y * 13) % 41 - 20;
                let adj = |v: u8| (i32::from(v) + n).clamp(0, 255) as u8;
                Rgb(adj(c.0), adj(c.1), adj(c.2))
            })
        };
        assert_eq!(decode_counter(&l, noisy), Some(4242));
    }

    #[test]
    fn box_is_drawn_at_its_deterministic_position_in_the_current_colour() {
        let l = Layout::initial(96);
        for frame in [0, 100, 288, 5000] {
            for colour in BOX_PALETTE {
                let buf = render(&l, frame, colour);
                let at = reader(&buf, &l);
                let b = l.box_rect(frame);
                let (cx, cy) = b.centre();
                assert_eq!(at(cx, cy), Some(colour));
                assert_eq!(at(b.x, b.y), Some(colour));
                assert_eq!(at(b.right() - 1, b.bottom() - 1), Some(colour));
            }
        }
    }

    #[test]
    fn digits_change_with_the_frame_and_use_text_colour() {
        let l = Layout::initial(96);
        let text = l.counter_text.bounds(8);
        let pixels = |frame: u32| -> Vec<Option<Rgb>> {
            let buf = render(&l, frame, BOX_PALETTE[0]);
            let at = reader(&buf, &l);
            (text.y..text.bottom())
                .flat_map(|y| (text.x..text.right()).map(move |x| (x, y)))
                .map(|(x, y)| at(x, y))
                .collect()
        };
        let lit = |p: &[Option<Rgb>]| p.iter().filter(|c| **c == Some(TEXT_RGB)).count();
        assert!(lit(&pixels(0)) > 100);
        assert_ne!(
            pixels(1),
            pixels(2),
            "frame 1 and 2 must differ in the digit row"
        );
    }

    #[test]
    fn clock_text_format() {
        let c = ClockTime {
            hour: 1,
            minute: 2,
            second: 3,
            milli: 4,
        };
        assert_eq!(c.text(), "01:02:03.004");
    }

    #[test]
    fn canvas_clips_and_rejects_short_buffers() {
        let mut small = [0u32; 4];
        assert!(Canvas::new(&mut small, 3, 3).is_none());
        let mut buf = [0u32; 16];
        let mut c = Canvas::new(&mut buf, 4, 4).unwrap();
        c.fill_rect(Rect::new(-5, -5, 100, 100), Rgb(1, 2, 3));
        assert_eq!(c.get(0, 0), Some(Rgb(1, 2, 3)));
        assert_eq!(c.get(4, 0), None);
        assert_eq!(c.get(-1, 0), None);
        c.fill_rect(Rect::new(10, 10, 5, 5), Rgb(9, 9, 9));
        c.draw_text(
            TextSpec {
                x: -100,
                y: -100,
                scale: 3,
            },
            "12:34.56",
            TEXT_RGB,
        );
    }

    #[test]
    fn small_clients_render_without_panicking() {
        for (w, h) in [(1, 1), (60, 60), (300, 150), (640, 130)] {
            let l = Layout::new(w, h, 96);
            let _ = render(&l, 777, BOX_PALETTE[2]);
        }
    }
}
