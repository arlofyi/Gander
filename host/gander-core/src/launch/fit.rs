// SPDX-License-Identifier: Apache-2.0
//! Fit-to-phone math (stream-launch-policy spec, "Launch and window discovery", step 5).
//!
//! Goal: one window pixel per phone pixel at the phone's natural scale. The phone's viewport is
//! `width x height` physical pixels at device pixel ratio `dpr`, so its natural (CSS-like) size is
//! `width / dpr x height / dpr` logical pixels. A window on a monitor at `dpi` shows a logical
//! pixel as `dpi / 96` physical pixels, so the target client area is
//! `viewport / dpr * dpi / 96` physical pixels, rounded.
//!
//! Then, in this order:
//!
//! 1. **Minimum** 640x480 logical pixels (scaled by `dpi / 96`, so an app's layout gets the same
//!    room at every scale), oriented like the viewport: a portrait phone gets at least 480 wide
//!    and 640 tall. The size grows uniformly (the aspect ratio stays the phone's) until both
//!    minimums hold.
//! 2. **Work area**: the outer window (client plus the frame the window already has) must fit
//!    the monitor's work area; the size shrinks uniformly until it does. A 2280-pixel-tall phone
//!    window cannot exist on a 1600-pixel monitor (stream-spike findings, encode section); the
//!    encoder scales instead.
//! 3. **Position**: the window stays where it is when it fits, otherwise it is moved the least
//!    distance that brings it inside the work area of the monitor it is on.
//!
//! The app's own `WM_GETMINMAXINFO` limits are applied by Windows itself during `SetWindowPos`
//! (`DefWindowProc` clamps in `WM_WINDOWPOSCHANGING`); the platform side reads the size back
//! instead of predicting it.

use super::discovery::Rect;
use crate::protocol::stream::Viewport;

/// Logical minimum of the longer side.
pub const MIN_LONG_SIDE: f64 = 640.0;
/// Logical minimum of the shorter side.
pub const MIN_SHORT_SIDE: f64 = 480.0;

/// A size in physical pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Size {
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

impl Size {
    /// A size.
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// What the window looks like now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGeometry {
    /// Outer rectangle (`GetWindowRect`, restored state), physical pixels.
    pub outer: Rect,
    /// Client size (`GetClientRect`), physical pixels.
    pub client: Size,
    /// The dpi of the window's monitor (`GetDpiForWindow`).
    pub dpi: u32,
    /// The work area of the window's monitor.
    pub work_area: Rect,
}

/// Which limit shaped the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FitLimit {
    /// Exactly the phone's natural size.
    Natural,
    /// Grown to the minimum size.
    Minimum,
    /// Shrunk to the work area.
    WorkArea,
}

/// The planned window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitPlan {
    /// Target client size.
    pub client: Size,
    /// Target outer rectangle (`SetWindowPos` arguments).
    pub outer: Rect,
    /// Client pixels per phone pixel at natural scale: 1.0 is the goal; above 1 the window was
    /// grown to the minimum, below 1 shrunk to the work area. The phone may use it to zoom.
    pub scale: f64,
    /// The limit that decided.
    pub limit: FitLimit,
}

/// Why no plan could be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FitError {
    /// Zero width or height, or a device pixel ratio that is not a positive finite number.
    #[error("the phone viewport is not usable")]
    BadViewport,
    /// A dpi of 0.
    #[error("the monitor dpi is not usable")]
    BadDpi,
    /// The work area cannot hold even the window frame.
    #[error("the monitor work area is too small")]
    WorkAreaTooSmall,
}

/// The phone's natural size on a monitor at `dpi`, unrounded: `viewport / dpr * dpi / 96`.
pub fn natural_client(viewport: Viewport, dpi: u32) -> Result<(f64, f64), FitError> {
    if viewport.width == 0
        || viewport.height == 0
        || !(viewport.dpr.is_finite() && viewport.dpr > 0.0)
    {
        return Err(FitError::BadViewport);
    }
    if dpi == 0 {
        return Err(FitError::BadDpi);
    }
    let scale = f64::from(dpi) / 96.0;
    Ok((
        f64::from(viewport.width) / viewport.dpr * scale,
        f64::from(viewport.height) / viewport.dpr * scale,
    ))
}

fn round_px(v: f64) -> u32 {
    // Bounded by the work area or a 2^31 viewport, so the cast cannot overflow meaningfully.
    v.round().clamp(1.0, f64::from(i32::MAX)) as u32
}

fn clamp_axis(start: i32, len: i32, lo: i32, hi: i32) -> i32 {
    if len >= hi - lo {
        lo
    } else {
        start.clamp(lo, hi - len)
    }
}

/// Plans the fit of a window to the phone viewport.
pub fn plan_fit(viewport: Viewport, geometry: &WindowGeometry) -> Result<FitPlan, FitError> {
    let (nat_w, nat_h) = natural_client(viewport, geometry.dpi)?;
    let scale = f64::from(geometry.dpi) / 96.0;
    let (min_w, min_h) = if viewport.height > viewport.width {
        (MIN_SHORT_SIDE * scale, MIN_LONG_SIDE * scale)
    } else {
        (MIN_LONG_SIDE * scale, MIN_SHORT_SIDE * scale)
    };
    let grow = (min_w / nat_w).max(min_h / nat_h).max(1.0);

    let inset_w = (geometry.outer.width() - geometry.client.width as i32).max(0);
    let inset_h = (geometry.outer.height() - geometry.client.height as i32).max(0);
    let avail_w = f64::from(geometry.work_area.width() - inset_w);
    let avail_h = f64::from(geometry.work_area.height() - inset_h);
    if avail_w < 1.0 || avail_h < 1.0 {
        return Err(FitError::WorkAreaTooSmall);
    }
    let (w, h) = (nat_w * grow, nat_h * grow);
    let shrink = (avail_w / w).min(avail_h / h).min(1.0);
    let k = grow * shrink;
    let client = Size::new(
        round_px(nat_w * k).min(avail_w as u32),
        round_px(nat_h * k).min(avail_h as u32),
    );
    let limit = if shrink < 1.0 {
        FitLimit::WorkArea
    } else if grow > 1.0 {
        FitLimit::Minimum
    } else {
        FitLimit::Natural
    };

    let outer_w = client.width as i32 + inset_w;
    let outer_h = client.height as i32 + inset_h;
    let wa = geometry.work_area;
    let x = clamp_axis(geometry.outer.left, outer_w, wa.left, wa.right);
    let y = clamp_axis(geometry.outer.top, outer_h, wa.top, wa.bottom);
    Ok(FitPlan {
        client,
        outer: Rect::from_xywh(x, y, outer_w, outer_h),
        scale: k,
        limit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A large monitor so only the scale shows; the frame is 16 x 39 pixels.
    fn big(dpi: u32) -> WindowGeometry {
        WindowGeometry {
            outer: Rect::from_xywh(100, 100, 656, 519),
            client: Size::new(640, 480),
            dpi,
            work_area: Rect::from_xywh(0, 0, 7680, 4320),
        }
    }

    /// A tablet in portrait: 1600x2560 at dpr 2, natural 800x1280 (above the minimum).
    const TABLET: Viewport = Viewport {
        width: 1600,
        height: 2560,
        dpr: 2.0,
    };
    /// A phone in portrait: 1080x2400 at dpr 2.625, natural 411.4x914.3 (below the minimum
    /// width).
    const PHONE: Viewport = Viewport {
        width: 1080,
        height: 2400,
        dpr: 2.625,
    };

    const SCALES: [(u32, u32); 4] = [(96, 100), (120, 125), (144, 150), (192, 200)];

    #[test]
    fn one_to_one_at_every_scale() {
        let want = [(800, 1280), (1000, 1600), (1200, 1920), (1600, 2560)];
        for ((dpi, pct), (w, h)) in SCALES.into_iter().zip(want) {
            let p = plan_fit(TABLET, &big(dpi)).unwrap();
            assert_eq!(p.client, Size::new(w, h), "{pct} %");
            assert_eq!(p.limit, FitLimit::Natural, "{pct} %");
            assert!((p.scale - 1.0).abs() < 1e-9);
            assert_eq!(
                p.outer,
                Rect::from_xywh(100, 100, w as i32 + 16, h as i32 + 39)
            );
        }
    }

    #[test]
    fn phone_grows_to_the_portrait_minimum_at_every_scale() {
        // 480 logical wide, phone aspect kept: 480 x 1066.7 logical.
        let want = [(480, 1067), (600, 1333), (720, 1600), (960, 2133)];
        for ((dpi, pct), (w, h)) in SCALES.into_iter().zip(want) {
            let p = plan_fit(PHONE, &big(dpi)).unwrap();
            assert_eq!(p.client, Size::new(w, h), "{pct} %");
            assert_eq!(p.limit, FitLimit::Minimum, "{pct} %");
            assert!((p.scale - 480.0 / (1080.0 / 2.625)).abs() < 1e-9);
        }
    }

    #[test]
    fn landscape_minimum_is_640_by_480() {
        let landscape = Viewport {
            width: 2400,
            height: 1080,
            dpr: 2.625,
        };
        let want = [(1067, 480), (1333, 600), (1600, 720), (2133, 960)];
        for ((dpi, pct), (w, h)) in SCALES.into_iter().zip(want) {
            let p = plan_fit(landscape, &big(dpi)).unwrap();
            assert_eq!(p.client, Size::new(w, h), "{pct} %");
        }
        // A small landscape viewport grows on both axes to 640x480 logical when the aspect is
        // 4:3, at 150 %: 960x720.
        let small = Viewport {
            width: 400,
            height: 300,
            dpr: 1.0,
        };
        assert_eq!(
            plan_fit(small, &big(144)).unwrap().client,
            Size::new(960, 720)
        );
    }

    #[test]
    fn shrinks_to_the_work_area_and_keeps_the_aspect() {
        // The development laptop's primary monitor: 2560x1600 at 150 %, work area 2560x1528.
        let g = WindowGeometry {
            outer: Rect::from_xywh(200, 300, 976, 759),
            client: Size::new(960, 720),
            dpi: 144,
            work_area: Rect::from_xywh(0, 0, 2560, 1528),
        };
        let p = plan_fit(PHONE, &g).unwrap();
        assert_eq!(p.limit, FitLimit::WorkArea);
        // Available client height 1528 - 39 = 1489; width keeps the phone's aspect.
        assert_eq!(p.client.height, 1489);
        assert_eq!(
            p.client.width,
            (1489.0_f64 * 1080.0 / 2400.0).round() as u32
        );
        // Moved up the least distance that fits (top = 0, the window is exactly as tall as
        // the work area).
        assert_eq!(p.outer.top, 0);
        assert_eq!(p.outer.left, 200);
        assert_eq!(p.outer.height(), 1528);
    }

    #[test]
    fn position_is_clamped_into_the_monitor_it_is_on() {
        // A secondary monitor to the right at 100 %.
        let g = WindowGeometry {
            outer: Rect::from_xywh(3000, 900, 656, 519),
            client: Size::new(640, 480),
            dpi: 96,
            work_area: Rect::from_xywh(2560, 426, 1920, 1032),
        };
        let p = plan_fit(TABLET, &g).unwrap();
        // 1280 + 39 > 1032: shrunk to 1032 - 39 = 993 tall.
        assert_eq!(p.client.height, 993);
        assert_eq!(p.client.width, 621);
        assert_eq!(p.outer.left, 3000);
        assert_eq!(p.outer.right, 3000 + 621 + 16);
        assert_eq!(p.outer.top, 426);
        assert!(p.outer.right <= 2560 + 1920);
        // Half off the right edge: pulled in.
        let g2 = WindowGeometry {
            outer: Rect::from_xywh(4400, 500, 656, 519),
            ..g
        };
        let p2 = plan_fit(TABLET, &g2).unwrap();
        assert_eq!(p2.outer.right, 2560 + 1920);
    }

    #[test]
    fn bad_inputs() {
        let g = big(96);
        for vp in [
            Viewport {
                width: 0,
                height: 10,
                dpr: 1.0,
            },
            Viewport {
                width: 10,
                height: 0,
                dpr: 1.0,
            },
            Viewport {
                width: 10,
                height: 10,
                dpr: 0.0,
            },
            Viewport {
                width: 10,
                height: 10,
                dpr: f64::NAN,
            },
            Viewport {
                width: 10,
                height: 10,
                dpr: -1.0,
            },
        ] {
            assert_eq!(plan_fit(vp, &g), Err(FitError::BadViewport));
        }
        assert_eq!(
            plan_fit(TABLET, &WindowGeometry { dpi: 0, ..g }),
            Err(FitError::BadDpi)
        );
        let tiny = WindowGeometry {
            work_area: Rect::from_xywh(0, 0, 10, 10),
            ..g
        };
        assert_eq!(plan_fit(TABLET, &tiny), Err(FitError::WorkAreaTooSmall));
    }

    #[test]
    fn natural_size_formula() {
        let (w, h) = natural_client(PHONE, 144).unwrap();
        assert!((w - 1080.0 / 2.625 * 1.5).abs() < 1e-9);
        assert!((h - 2400.0 / 2.625 * 1.5).abs() < 1e-9);
    }
}
