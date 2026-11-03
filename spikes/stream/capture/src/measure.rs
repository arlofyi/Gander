// SPDX-License-Identifier: Apache-2.0
//! Frame collection and statistics over the test window's known content.

use std::time::{Duration, Instant};

use anyhow::Result;
use gander_testwin::layout::{ARENA_RGB, Layout, PATCH_A_RGB, PATCH_B_RGB, Rect, Rgb};
use gander_testwin::render::{decode_counter, patch_matches};
use spike_capture::readback::{Image, Readback};
use spike_capture::wgc::{Capture, Frame, WindowGeometry};
use windows::Win32::Foundation::{HWND, RECT};

/// What one frame showed.
#[derive(Debug, Clone)]
pub struct FrameRec {
    pub srt_100ns: i64,
    pub arrival_100ns: i64,
    pub size: (u32, u32),
    pub counter: Option<u32>,
    pub patch_a: bool,
    pub patch_b: bool,
    /// Item-specific measurement (occluder pixels, foreign fraction, ...).
    pub extra: f64,
}

/// Everything one collection produced.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub frames: Vec<FrameRec>,
    pub secs: f64,
    pub dropped_full: u64,
}

/// Per-window analysis context: geometry and layout cached per captured size.
#[derive(Debug)]
pub struct Analyzer {
    pub hwnd: HWND,
    cache: Option<((u32, u32), WindowGeometry, Layout)>,
    /// Offset of the client origin in the frame, from DWM geometry.
    pub offset: (i32, i32),
}

impl Analyzer {
    pub fn new(hwnd: HWND) -> Analyzer {
        Analyzer {
            hwnd,
            cache: None,
            offset: (0, 0),
        }
    }

    /// Geometry and layout for a frame of `size` (refreshed when the size changes).
    pub fn context(&mut self, size: (u32, u32)) -> Option<(WindowGeometry, Layout)> {
        if self.cache.as_ref().is_none_or(|(s, _, _)| *s != size) {
            let g = WindowGeometry::of(self.hwnd).ok()?;
            let layout = Layout::new(g.client_w, g.client_h, g.dpi);
            self.offset = g.client_offset();
            self.cache = Some((size, g, layout));
        }
        self.cache.as_ref().map(|(_, g, l)| (*g, *l))
    }

    /// Counter and patches of an image, in client coordinates via the DWM offset.
    pub fn basic(&mut self, img: &Image, size: (u32, u32)) -> (Option<u32>, bool, bool) {
        let Some((_, layout)) = self.context(size) else {
            return (None, false, false);
        };
        let (ox, oy) = self.offset;
        let at = |x: i32, y: i32| img.at(x + ox, y + oy);
        (
            decode_counter(&layout, at),
            patch_matches(layout.patch_a, PATCH_A_RGB, 0, at),
            patch_matches(layout.patch_b, PATCH_B_RGB, 0, at),
        )
    }

    pub fn layout(&self) -> Option<Layout> {
        self.cache.as_ref().map(|(_, _, l)| *l)
    }
}

/// Collects frames from several captures for `dur`; `analyse(index, frame, image)` returns the
/// counter, the two patch checks and an extra number.
pub fn collect(
    caps: &[&Capture],
    rb: &mut Readback,
    dur: Duration,
    mut analyse: impl FnMut(usize, &Frame, &Image) -> (Option<u32>, bool, bool, f64),
) -> Result<Vec<Run>> {
    let mut runs: Vec<Run> = caps.iter().map(|_| Run::default()).collect();
    let start = Instant::now();
    let base: Vec<u64> = caps.iter().map(|c| c.dropped_full()).collect();
    while start.elapsed() < dur {
        let mut got = false;
        for (i, cap) in caps.iter().enumerate() {
            let wait = if caps.len() == 1 {
                Duration::from_millis(20)
            } else {
                Duration::ZERO
            };
            if let Some(f) = cap.recv_timeout(wait) {
                got = true;
                let (w, h) = f.size();
                let img = rb.read(f.texture(), w, h)?;
                let (counter, a, b, extra) = analyse(i, &f, &img);
                runs[i].frames.push(FrameRec {
                    srt_100ns: f.system_relative_time_100ns(),
                    arrival_100ns: f.arrival_100ns(),
                    size: (w, h),
                    counter,
                    patch_a: a,
                    patch_b: b,
                    extra,
                });
            }
        }
        if !got && caps.len() > 1 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let secs = start.elapsed().as_secs_f64();
    for (i, r) in runs.iter_mut().enumerate() {
        r.secs = secs;
        r.dropped_full = caps[i].dropped_full() - base[i];
    }
    Ok(runs)
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let idx = ((v.len() - 1) as f64 * p).round() as usize;
    v[idx]
}

/// Summary numbers of a run.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub frames: usize,
    pub fps: f64,
    /// SystemRelativeTime -> FrameArrived handler, ms.
    pub lat_p50: f64,
    pub lat_p95: f64,
    /// Interval between consecutive frames (by SystemRelativeTime), ms.
    pub int_p50: f64,
    pub int_p95: f64,
    pub int_max: f64,
    pub decoded: usize,
    /// Source frames (60 Hz counter values) not seen between the first and last decoded.
    pub missed_source: u64,
    /// Repeated counter values.
    pub repeats: u64,
    pub patch_a_ok: usize,
    pub patch_b_ok: usize,
}

impl Run {
    pub fn stats(&self) -> Stats {
        let f = &self.frames;
        let mut lat: Vec<f64> = f
            .iter()
            .map(|r| (r.arrival_100ns - r.srt_100ns) as f64 / 10_000.0)
            .collect();
        let mut int: Vec<f64> = f
            .windows(2)
            .map(|w| (w[1].srt_100ns - w[0].srt_100ns) as f64 / 10_000.0)
            .collect();
        let counters: Vec<u32> = f.iter().filter_map(|r| r.counter).collect();
        let (mut missed, mut repeats) = (0u64, 0u64);
        for w in counters.windows(2) {
            let d = w[1].wrapping_sub(w[0]) & 0xFF_FFFF;
            match d {
                0 => repeats += 1,
                1 => {}
                d if d < 0x80_0000 => missed += u64::from(d - 1),
                _ => {}
            }
        }
        Stats {
            frames: f.len(),
            fps: if self.secs > 0.0 {
                f.len() as f64 / self.secs
            } else {
                0.0
            },
            lat_p50: percentile(&mut lat, 0.5),
            lat_p95: percentile(&mut lat, 0.95),
            int_p50: percentile(&mut int, 0.5),
            int_p95: percentile(&mut int, 0.95),
            int_max: int.iter().copied().fold(f64::NAN, f64::max),
            decoded: counters.len(),
            missed_source: missed,
            repeats,
            patch_a_ok: f.iter().filter(|r| r.patch_a).count(),
            patch_b_ok: f.iter().filter(|r| r.patch_b).count(),
        }
    }
}

/// Exact-colour pixel count of an image.
pub fn count_colour(img: &Image, rgb: Rgb) -> usize {
    let v = (u32::from(rgb.0) << 16) | (u32::from(rgb.1) << 8) | u32::from(rgb.2);
    img.px.iter().filter(|&&p| p & 0xFF_FFFF == v).count()
}

/// Top-left of the first run of `run` exact `rgb` pixels in a row (patch locator).
pub fn find_run(img: &Image, rgb: Rgb, run: i32) -> Option<(i32, i32)> {
    for y in 0..img.h {
        let mut n = 0;
        for x in 0..img.w {
            if img.at(x, y) == Some(rgb) {
                n += 1;
                if n >= run {
                    return Some((x + 1 - n, y));
                }
            } else {
                n = 0;
            }
        }
    }
    None
}

/// Fraction of arena pixels inside `screen_rect` that are NOT the arena colour, ignoring the
/// moving box (with a margin) and the arena border: how much of a popup shows in the frame.
pub fn foreign_fraction(
    img: &Image,
    g: &WindowGeometry,
    layout: &Layout,
    counter: Option<u32>,
    screen_rect: RECT,
) -> f64 {
    let (ox, oy) = g.client_offset();
    // Popup rectangle in client coordinates, clipped to the arena interior.
    let a = layout.arena;
    let x0 = (screen_rect.left - g.client_origin.x).max(a.x + 2);
    let y0 = (screen_rect.top - g.client_origin.y).max(a.y + 2);
    let x1 = (screen_rect.right - g.client_origin.x).min(a.right() - 2);
    let y1 = (screen_rect.bottom - g.client_origin.y).min(a.bottom() - 2);
    if x1 <= x0 || y1 <= y0 {
        return f64::NAN;
    }
    // Without a decoded counter the box position is unknown and would count as foreign.
    let Some(c) = counter else {
        return f64::NAN;
    };
    let margin = layout.box_size / 4 + 4;
    let b = layout.box_rect(c);
    let boxr = Some(Rect::new(
        b.x - margin,
        b.y - margin,
        b.w + 2 * margin,
        b.h + 2 * margin,
    ));
    let (mut total, mut foreign) = (0u64, 0u64);
    for y in (y0..y1).step_by(2) {
        for x in (x0..x1).step_by(2) {
            if boxr.is_some_and(|b| b.contains(x, y)) {
                continue;
            }
            if let Some(p) = img.at(x + ox, y + oy) {
                total += 1;
                if p.max_delta(ARENA_RGB) > 2 {
                    foreign += 1;
                }
            }
        }
    }
    if total == 0 {
        f64::NAN
    } else {
        foreign as f64 / total as f64
    }
}
