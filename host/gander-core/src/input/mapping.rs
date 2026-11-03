// SPDX-License-Identifier: Apache-2.0
//! Normalised picture coordinates -> screen pixels -> `SendInput` absolute coordinates.
//!
//! The phone sends pointer positions normalised (0..1, origin top-left) to the **picture** it
//! shows, which is the captured window scaled to the encode size. The host maps them back onto the
//! window's **live** geometry (re-read before each event, so a window that moved or was resized
//! between frames is still hit where the phone aimed), in physical pixels throughout (the
//! injecting thread is per-monitor v2 DPI aware, so nothing here depends on the monitor scale).
//!
//! 1. Picture rectangle: the capture area of the window on the virtual screen ([`PictureArea`]):
//!    the DWM visible frame bounds (what Windows Graphics Capture delivers for a window, title bar
//!    and borders included; stream-spike WP-44) or the client rectangle (if the engine crops to
//!    it).
//! 2. Pixel: `floor(n * len)`, clamped to the last pixel, so `1.0` is the last pixel and every
//!    pixel has a non-empty range of normalised values (round trip 0 px, [`normalise`]).
//! 3. Absolute: `SendInput` with `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` takes 0..65535
//!    over the **virtual desktop** (the bounding box of every monitor, which can start at negative
//!    coordinates). Windows maps an absolute value `a` back to pixel `origin + a * len / 65536`,
//!    so the centre of pixel `p` is sent as `((2 (p - origin) + 1) * 65536) / (2 len)`
//!    ([`to_absolute`], inverse [`from_absolute`]).

use crate::launch::discovery::Rect;

/// A point in physical screen pixels (virtual-screen coordinates).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Point {
    /// X.
    pub x: i32,
    /// Y.
    pub y: i32,
}

impl Point {
    /// A point.
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// `true` when `p` lies inside `r` (right and bottom exclusive).
pub const fn contains(r: &Rect, p: Point) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

/// The live geometry of the captured window, physical pixels, read by the platform layer before
/// each pointer event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGeometry {
    /// DWM extended frame bounds (the visible window, without the invisible resize borders).
    pub frame: Rect,
    /// Client area on the screen.
    pub client: Rect,
    /// `GetDpiForWindow` (information only: the mapping does not depend on it).
    pub dpi: u32,
    /// Bounding rectangle of all monitors (`SM_XVIRTUALSCREEN` ..).
    pub desktop: Rect,
}

/// Which part of the window the streamed picture shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PictureArea {
    /// The visible frame (title bar and borders included): what a WGC window capture delivers.
    #[default]
    Frame,
    /// The client area only (the engine crops the frame to it).
    Client,
}

impl WindowGeometry {
    /// The picture rectangle for `area`.
    pub const fn picture(&self, area: PictureArea) -> Rect {
        match area {
            PictureArea::Frame => self.frame,
            PictureArea::Client => self.client,
        }
    }
}

/// Largest wheel delta per axis and message (50 notches), so one message cannot scroll forever.
pub const MAX_WHEEL: i32 = 120 * 50;

/// The `SendInput` wheel data for a `scroll` message: `(vertical, horizontal)`, each `None` when
/// that delta is 0, clamped to [`MAX_WHEEL`].
///
/// Convention (Conductor decision, 2026-11-02, Windows wheel convention; the protocol unit is
/// wheel-delta units, 120 per notch): positive `dy` means the wheel turned AWAY from the user,
/// so the content scrolls up (`MOUSEEVENTF_WHEEL` with `+WHEEL_DELTA`); positive `dx` means a
/// tilt to the right (`MOUSEEVENTF_HWHEEL` with a positive delta). Both pass through unchanged.
pub fn wheel_data(dx: i32, dy: i32) -> (Option<i32>, Option<i32>) {
    let clamp = |d: i32| (d != 0).then(|| d.clamp(-MAX_WHEEL, MAX_WHEEL));
    (clamp(dy), clamp(dx))
}

/// Why a pointer position cannot be mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MapError {
    /// NaN or infinite coordinate.
    #[error("the coordinate is not a finite number")]
    NotFinite,
    /// Outside 0..1 where clamping is not allowed (a press or a scroll on a letterbox bar).
    #[error("the point is outside the picture")]
    OutsidePicture,
    /// The window has no area (minimised, or not yet shown).
    #[error("the window has no visible area")]
    EmptyWindow,
}

/// How out-of-range coordinates are treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutOfRange {
    /// Clamp into the picture (moves and releases: a drag that leaves the picture still ends).
    Clamp,
    /// Refuse (presses and scrolls: aim at the picture, not the letterbox bars).
    Refuse,
}

fn axis(n: f64, start: i32, len: i32, mode: OutOfRange) -> Result<i32, MapError> {
    if !n.is_finite() {
        return Err(MapError::NotFinite);
    }
    if !(0.0..=1.0).contains(&n) && mode == OutOfRange::Refuse {
        return Err(MapError::OutsidePicture);
    }
    let n = n.clamp(0.0, 1.0);
    // `as` saturates for floats; the clamp keeps 1.0 on the last pixel.
    let px = ((n * f64::from(len)).floor() as i32).clamp(0, len - 1);
    Ok(start + px)
}

/// Maps a normalised picture point onto the screen.
pub fn map_normalised(
    nx: f64,
    ny: f64,
    picture: Rect,
    mode: OutOfRange,
) -> Result<Point, MapError> {
    if picture.is_empty() {
        return Err(MapError::EmptyWindow);
    }
    Ok(Point {
        x: axis(nx, picture.left, picture.width(), mode)?,
        y: axis(ny, picture.top, picture.height(), mode)?,
    })
}

/// The normalised coordinates of the centre of screen pixel `p` in `picture` (the inverse of
/// [`map_normalised`]; what a phone that aims perfectly sends).
pub fn normalise(p: Point, picture: Rect) -> (f64, f64) {
    (
        (f64::from(p.x - picture.left) + 0.5) / f64::from(picture.width().max(1)),
        (f64::from(p.y - picture.top) + 0.5) / f64::from(picture.height().max(1)),
    )
}

fn abs_axis(p: i32, origin: i32, len: i32) -> i32 {
    let len = i64::from(len.max(1));
    let off = (i64::from(p) - i64::from(origin)).clamp(0, len - 1);
    (((2 * off + 1) * 65536) / (2 * len)) as i32
}

/// `SendInput` absolute coordinates (0..=65535 per axis over the virtual desktop) of the centre of
/// pixel `p`. Points outside the desktop are clamped onto its edge.
pub fn to_absolute(p: Point, desktop: Rect) -> (i32, i32) {
    (
        abs_axis(p.x, desktop.left, desktop.width()),
        abs_axis(p.y, desktop.top, desktop.height()),
    )
}

/// The pixel Windows moves the cursor to for absolute coordinates `a` over `desktop`
/// (`origin + a * len / 65536`). Used by the tests to prove [`to_absolute`] lands exactly.
pub fn from_absolute(a: (i32, i32), desktop: Rect) -> Point {
    let back =
        |a: i32, origin: i32, len: i32| origin + (i64::from(a) * i64::from(len) / 65536) as i32;
    Point {
        x: back(a.0, desktop.left, desktop.width()),
        y: back(a.1, desktop.top, desktop.height()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A monitor of a synthetic desktop: position in physical pixels and its scale.
    #[derive(Debug, Clone, Copy)]
    struct Monitor {
        rect: Rect,
        scale_percent: u32,
    }

    impl Monitor {
        fn dpi(&self) -> u32 {
            96 * self.scale_percent / 100
        }
    }

    fn union(ms: &[Monitor]) -> Rect {
        let mut r = ms[0].rect;
        for m in ms {
            r.left = r.left.min(m.rect.left);
            r.top = r.top.min(m.rect.top);
            r.right = r.right.max(m.rect.right);
            r.bottom = r.bottom.max(m.rect.bottom);
        }
        r
    }

    /// A window of a logical client size placed on monitor `m` the way a per-monitor v2 app
    /// would be: every metric scaled by the monitor's dpi.
    fn window_on(
        m: &Monitor,
        desktop: Rect,
        logical: (i32, i32),
        at: (i32, i32),
    ) -> WindowGeometry {
        let s = |v: i32| v * m.scale_percent as i32 / 100;
        let border = s(1).max(1);
        let caption = s(31);
        let left = m.rect.left + at.0;
        let top = m.rect.top + at.1;
        let client = Rect::from_xywh(left + border, top + caption, s(logical.0), s(logical.1));
        let frame = Rect {
            left,
            top,
            right: client.right + border,
            bottom: client.bottom + border,
        };
        WindowGeometry {
            frame,
            client,
            dpi: m.dpi(),
            desktop,
        }
    }

    /// The four scales of the WP-52 verify, each on a desktop that includes a monitor left of and
    /// above the primary (negative virtual-screen origin) and one of another scale.
    fn desktops() -> Vec<(u32, Vec<Monitor>)> {
        let mut out = Vec::new();
        for scale in [100u32, 125, 150, 200] {
            let primary = Monitor {
                rect: Rect::from_xywh(0, 0, 2560, 1600),
                scale_percent: scale,
            };
            let left = Monitor {
                rect: Rect::from_xywh(-1920, -300, 1920, 1080),
                scale_percent: 100,
            };
            let right = Monitor {
                rect: Rect::from_xywh(2560, 200, 3840, 2160),
                scale_percent: if scale == 200 { 150 } else { 200 },
            };
            out.push((scale, vec![primary, left, right]));
        }
        out
    }

    fn sample_points(r: Rect) -> Vec<Point> {
        let mut v = Vec::new();
        let xs = [
            r.left,
            r.left + 1,
            (r.left + r.right) / 2,
            r.right - 2,
            r.right - 1,
        ];
        let ys = [
            r.top,
            r.top + 1,
            (r.top + r.bottom) / 2,
            r.bottom - 2,
            r.bottom - 1,
        ];
        for &x in &xs {
            for &y in &ys {
                v.push(Point::new(x, y));
            }
        }
        v
    }

    /// Normalise -> map -> absolute -> back to a pixel: 0 px error for every sampled pixel
    /// (first and last included) of windows on every monitor of the 100/125/150/200 % desktops,
    /// for both picture areas.
    #[test]
    fn round_trip_is_exact_at_four_scales_on_every_monitor() {
        for (scale, monitors) in desktops() {
            let desktop = union(&monitors);
            for m in &monitors {
                let g = window_on(m, desktop, (600, 640), (37, 23));
                assert_eq!(g.dpi, m.dpi());
                for area in [PictureArea::Frame, PictureArea::Client] {
                    let pic = g.picture(area);
                    for p in sample_points(pic) {
                        let (nx, ny) = normalise(p, pic);
                        let mapped = map_normalised(nx, ny, pic, OutOfRange::Refuse).unwrap();
                        assert_eq!(mapped, p, "scale {scale} monitor {m:?} {area:?}");
                        let a = to_absolute(mapped, desktop);
                        assert!((0..=65535).contains(&a.0) && (0..=65535).contains(&a.1));
                        assert_eq!(
                            from_absolute(a, desktop),
                            p,
                            "absolute round trip, scale {scale} monitor {m:?}"
                        );
                        assert!(contains(&m.rect, p), "point left its monitor");
                    }
                }
            }
        }
    }

    /// The picture is a scaled copy of the window: the phone's 1080x2280 picture of a window at
    /// any scale maps every phone pixel centre into the window, monotonically.
    #[test]
    fn phone_pixels_land_inside_the_window_at_every_scale() {
        for (scale, monitors) in desktops() {
            let desktop = union(&monitors);
            let g = window_on(&monitors[0], desktop, (411, 868), (100, 50));
            let pic = g.picture(PictureArea::Frame);
            let (pw, ph) = (1080, 2280);
            let mut last = Point::new(i32::MIN, i32::MIN);
            for i in 0..pw {
                let nx = (f64::from(i) + 0.5) / f64::from(pw);
                let p = map_normalised(nx, 0.5, pic, OutOfRange::Refuse).unwrap();
                assert!(contains(&pic, p), "scale {scale}: {p:?} outside {pic:?}");
                assert!(p.x >= last.x, "not monotonic at {i}");
                last = p;
            }
            for j in 0..ph {
                let ny = (f64::from(j) + 0.5) / f64::from(ph);
                let p = map_normalised(0.5, ny, pic, OutOfRange::Refuse).unwrap();
                assert!(contains(&pic, p));
            }
        }
    }

    /// A tap aimed at a client pixel lands on that client pixel even when the client area does
    /// not start at the frame origin (title bar and border in the picture).
    #[test]
    fn frame_picture_offsets_reach_the_client_pixel() {
        for (scale, monitors) in desktops() {
            let desktop = union(&monitors);
            let g = window_on(&monitors[0], desktop, (640, 480), (0, 0));
            let pic = g.picture(PictureArea::Frame);
            let target = Point::new(g.client.left + 10, g.client.top + 10);
            let (nx, ny) = normalise(target, pic);
            let p = map_normalised(nx, ny, pic, OutOfRange::Refuse).unwrap();
            assert_eq!(p, target, "scale {scale}");
            assert_eq!(p.x - g.client.left, 10);
        }
    }

    /// Live rect tracking: the same normalised point follows the window when it moves to
    /// another monitor and is resized.
    #[test]
    fn mapping_follows_a_moved_and_resized_window() {
        let (_, monitors) = desktops().remove(2); // 150 %
        let desktop = union(&monitors);
        let before = window_on(&monitors[0], desktop, (640, 480), (10, 10));
        let after = window_on(&monitors[1], desktop, (800, 600), (5, 5));
        let p1 = map_normalised(0.5, 0.5, before.frame, OutOfRange::Refuse).unwrap();
        let p2 = map_normalised(0.5, 0.5, after.frame, OutOfRange::Refuse).unwrap();
        assert!(contains(&monitors[0].rect, p1));
        assert!(contains(&monitors[1].rect, p2));
        assert!(
            p2.x < 0,
            "the left monitor has negative coordinates: {p2:?}"
        );
    }

    #[test]
    fn edges_and_bad_values() {
        let pic = Rect::from_xywh(100, 200, 50, 40);
        assert_eq!(
            map_normalised(0.0, 0.0, pic, OutOfRange::Refuse),
            Ok(Point::new(100, 200))
        );
        assert_eq!(
            map_normalised(1.0, 1.0, pic, OutOfRange::Refuse),
            Ok(Point::new(149, 239))
        );
        assert_eq!(
            map_normalised(1.2, 0.5, pic, OutOfRange::Refuse),
            Err(MapError::OutsidePicture)
        );
        assert_eq!(
            map_normalised(-0.1, 0.5, pic, OutOfRange::Clamp),
            Ok(Point::new(100, 220))
        );
        assert_eq!(
            map_normalised(f64::NAN, 0.5, pic, OutOfRange::Clamp),
            Err(MapError::NotFinite)
        );
        assert_eq!(
            map_normalised(0.5, f64::INFINITY, pic, OutOfRange::Clamp),
            Err(MapError::NotFinite)
        );
        assert_eq!(
            map_normalised(0.5, 0.5, Rect::default(), OutOfRange::Clamp),
            Err(MapError::EmptyWindow)
        );
    }

    /// Every pixel of every plausible virtual-desktop width round-trips through the absolute
    /// encoding, including desktops that start at negative coordinates.
    #[test]
    fn absolute_round_trips_for_every_pixel() {
        for (origin, len) in [(0, 1280), (0, 1920), (-1920, 4480), (-2560, 7680), (0, 1)] {
            let desktop = Rect::from_xywh(origin, 0, len, 1);
            for p in origin..origin + len {
                let a = to_absolute(Point::new(p, 0), desktop);
                assert!((0..=65535).contains(&a.0));
                assert_eq!(from_absolute(a, desktop).x, p, "len {len} pixel {p}");
            }
        }
        let desktop = Rect::from_xywh(-10, -10, 20, 20);
        assert_eq!(
            to_absolute(Point::new(-500, 500), desktop),
            to_absolute(Point::new(-10, 9), desktop)
        );
    }

    #[test]
    fn scroll_follows_the_windows_wheel_convention() {
        // One notch away from the user: content scrolls up, +WHEEL_DELTA, unchanged.
        assert_eq!(wheel_data(0, 120), (Some(120), None));
        // Toward the user: content scrolls down.
        assert_eq!(wheel_data(0, -240), (Some(-240), None));
        // Tilt right: positive horizontal delta.
        assert_eq!(wheel_data(120, 0), (None, Some(120)));
        assert_eq!(wheel_data(-120, 0), (None, Some(-120)));
        assert_eq!(wheel_data(0, 0), (None, None));
        assert_eq!(
            wheel_data(i32::MAX, i32::MIN),
            (Some(-MAX_WHEEL), Some(MAX_WHEEL))
        );
    }
}
