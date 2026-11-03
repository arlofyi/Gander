// SPDX-License-Identifier: Apache-2.0
//! Where the toast window goes: the bottom-right corner of the primary monitor's work area (the
//! screen minus the taskbar), like a system notification.

use egui::{Pos2, Vec2};

/// Gap between a toast and the screen edge, in points.
pub const MARGIN: f32 = 14.0;

/// A rectangle in physical pixels: `(left, top, right, bottom)`.
pub type PixelRect = (i32, i32, i32, i32);

/// The primary monitor's work area in physical pixels. `None` when the system cannot say.
#[cfg(windows)]
pub fn work_area() -> Option<PixelRect> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETWORKAREA, SystemParametersInfoW};

    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: SPI_GETWORKAREA writes one RECT through the pointer, which refers to a live,
    // properly aligned RECT for the duration of the call; the other arguments are unused by it.
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETWORKAREA, 0, std::ptr::from_mut(&mut rect).cast(), 0)
    };
    (ok != 0).then_some((rect.left, rect.top, rect.right, rect.bottom))
}

/// No work area query on this platform.
#[cfg(not(windows))]
pub fn work_area() -> Option<PixelRect> {
    None
}

/// The top-left corner (in points) for a window of `size` points in the bottom-right of `work`
/// (physical pixels) at `pixels_per_point`. Without a work area, a 1920x1080 screen is assumed.
pub fn bottom_right(work: Option<PixelRect>, pixels_per_point: f32, size: Vec2) -> Pos2 {
    let (_, _, right, bottom) = work.unwrap_or((0, 0, 1920, 1080));
    let ppp = pixels_per_point.max(0.5);
    Pos2::new(
        right as f32 / ppp - size.x - MARGIN,
        bottom as f32 / ppp - size.y - MARGIN,
    )
}

/// The top-left corner (in points) for a window of `size` points centred at the top of `work`,
/// [`PILL_GAP`] below its top edge (out of the way of a maximised app's title bar buttons).
pub fn top_centre(work: Option<PixelRect>, pixels_per_point: f32, size: Vec2) -> Pos2 {
    let (left, top, right, _) = work.unwrap_or((0, 0, 1920, 1080));
    let ppp = pixels_per_point.max(0.5);
    let centre = (left + right) as f32 / 2.0 / ppp;
    Pos2::new(centre - size.x / 2.0, top as f32 / ppp + PILL_GAP)
}

/// Gap between the top of the screen and the viewing pill, in points.
pub const PILL_GAP: f32 = 6.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_toast_sits_in_the_corner_with_a_margin() {
        let p = bottom_right(Some((0, 0, 1920, 1040)), 1.0, Vec2::new(360.0, 100.0));
        assert_eq!(
            p,
            Pos2::new(1920.0 - 360.0 - MARGIN, 1040.0 - 100.0 - MARGIN)
        );
    }

    #[test]
    fn scaling_converts_pixels_to_points() {
        let p = bottom_right(Some((0, 0, 3840, 2080)), 2.0, Vec2::new(360.0, 100.0));
        assert_eq!(
            p,
            Pos2::new(1920.0 - 360.0 - MARGIN, 1040.0 - 100.0 - MARGIN)
        );
    }

    #[test]
    fn the_pill_sits_top_centre() {
        let p = top_centre(Some((0, 0, 1920, 1040)), 1.0, Vec2::new(480.0, 44.0));
        assert_eq!(p, Pos2::new(960.0 - 240.0, PILL_GAP));
    }

    #[test]
    fn a_missing_work_area_assumes_full_hd() {
        let p = bottom_right(None, 1.0, Vec2::new(360.0, 100.0));
        assert_eq!(p.x, 1920.0 - 360.0 - MARGIN);
        assert_eq!(p.y, 1080.0 - 100.0 - MARGIN);
    }

    #[cfg(windows)]
    #[test]
    fn the_real_work_area_is_a_sane_rectangle_when_the_system_answers() {
        if let Some((l, t, r, b)) = work_area() {
            assert!(r > l && b > t, "{l},{t},{r},{b}");
        }
    }
}
