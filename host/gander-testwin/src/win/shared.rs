// SPDX-License-Identifier: Apache-2.0
//! State shared between the window thread, the ticker thread and the pipe thread.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

use gander_testwin::layout::{BOX_PALETTE, FPS, Rgb};

/// `last_click` value meaning "no click yet".
const NO_CLICK: u64 = u64::MAX;

/// Everything the three threads exchange. Plain atomics and short mutex sections only: the window
/// thread must never wait on another thread, and a stuck window thread must not wedge the pipe.
#[derive(Debug)]
pub struct Shared {
    /// Process start; the frame number is derived from this clock.
    pub start: Instant,
    /// Title of the main window.
    pub title: String,
    /// Newest frame number published by the ticker.
    pub latest_frame: AtomicU32,
    /// Frame number the main window last painted.
    pub painted_frame: AtomicU32,
    /// Counts colour changes; the box colour is `BOX_PALETTE[idx % len]`.
    pub colour_idx: AtomicUsize,
    /// Clicks that landed on the box.
    pub hits: AtomicU32,
    /// All left clicks.
    pub clicks: AtomicU32,
    last_click: AtomicU64,
    /// Client width, height and dpi of the main window.
    pub client: Mutex<(i32, i32, u32)>,
    /// Handles (as integers) of the open windows, main first.
    pub windows: Mutex<Vec<isize>>,
    /// A popup menu is open (modal on the window thread).
    pub menu_open: AtomicBool,
    /// The tracking tooltip is shown.
    pub tooltip_visible: AtomicBool,
    /// A modal dialog is open.
    pub dialog_open: AtomicBool,
    /// A tick message is already queued for the window thread (coalesces ticks).
    pub tick_pending: AtomicBool,
    /// The main window handle as an integer, 0 before creation.
    pub main_hwnd: AtomicIsize,
    /// Thread id of the window thread, 0 before creation.
    pub ui_thread: AtomicU32,
    /// Shutdown requested: ticker and pipe threads exit.
    pub stop: AtomicBool,
    /// A clean exit was requested (`quit` command); the ticker keeps nudging the window thread
    /// until it leaves its message loop.
    pub quit_requested: AtomicBool,
    /// `freeze`: the ticker stops asking for repaints, so the content stays static.
    pub frozen: AtomicBool,
    /// `park`: every paint shows frame 0 (the box at its start position) until `animate`.
    pub parked: AtomicBool,
    /// Title for the next `open-second`.
    pub pending_title: Mutex<Option<String>>,
    /// `WM_KEYDOWN` and `WM_SYSKEYDOWN` messages received (`typed?`).
    pub keydowns: AtomicU32,
    /// `WM_CHAR` UTF-16 units received, the last [`TYPED_KEEP`] kept (`typed?`).
    pub typed: Mutex<Vec<u16>>,
}

/// How many UTF-16 units of typed text `typed?` keeps.
pub const TYPED_KEEP: usize = 256;

static SHARED: OnceLock<Shared> = OnceLock::new();

/// Creates the process-wide state. Call once, before any thread starts.
pub fn init(title: String) -> &'static Shared {
    SHARED.get_or_init(|| Shared {
        start: Instant::now(),
        title,
        latest_frame: AtomicU32::new(0),
        painted_frame: AtomicU32::new(0),
        colour_idx: AtomicUsize::new(0),
        hits: AtomicU32::new(0),
        clicks: AtomicU32::new(0),
        last_click: AtomicU64::new(NO_CLICK),
        client: Mutex::new((0, 0, 96)),
        windows: Mutex::new(Vec::new()),
        menu_open: AtomicBool::new(false),
        tooltip_visible: AtomicBool::new(false),
        dialog_open: AtomicBool::new(false),
        tick_pending: AtomicBool::new(false),
        main_hwnd: AtomicIsize::new(0),
        ui_thread: AtomicU32::new(0),
        stop: AtomicBool::new(false),
        quit_requested: AtomicBool::new(false),
        frozen: AtomicBool::new(false),
        parked: AtomicBool::new(false),
        pending_title: Mutex::new(None),
        keydowns: AtomicU32::new(0),
        typed: Mutex::new(Vec::new()),
    })
}

/// The process-wide state; panics if [`init`] has not run (a programming error).
pub fn shared() -> &'static Shared {
    SHARED.get().expect("shared state not initialised")
}

/// Locks a mutex, ignoring poisoning (the data is plain values and always consistent).
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    /// Current box colour.
    pub fn box_rgb(&self) -> Rgb {
        BOX_PALETTE[self.colour_idx.load(Ordering::Relaxed) % BOX_PALETTE.len()]
    }

    /// Index into the palette of the current colour.
    pub fn box_index(&self) -> usize {
        self.colour_idx.load(Ordering::Relaxed) % BOX_PALETTE.len()
    }

    /// Moves to the next palette colour.
    pub fn cycle_colour(&self) {
        self.colour_idx.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a click position (client pixels).
    pub fn set_last_click(&self, x: i32, y: i32) {
        let packed = (u64::from(x as u32) << 32) | u64::from(y as u32);
        self.last_click.store(packed, Ordering::Relaxed);
    }

    /// The last click position, if any.
    pub fn last_click(&self) -> Option<(i32, i32)> {
        let v = self.last_click.load(Ordering::Relaxed);
        (v != NO_CLICK).then_some(((v >> 32) as u32 as i32, v as u32 as i32))
    }

    /// Frame number for the current instant: `floor(elapsed * 60)`.
    pub fn frame_now(&self) -> u32 {
        frame_at_nanos(self.start.elapsed().as_nanos())
    }
}

/// Frame number at `nanos` nanoseconds after start.
pub fn frame_at_nanos(nanos: u128) -> u32 {
    (nanos * u128::from(FPS) / 1_000_000_000) as u32
}

/// First instant of `frame` in nanoseconds after process start (rounded up, so
/// `frame_at_nanos(frame_start_nanos(f)) == f`).
pub fn frame_start_nanos(frame: u64) -> u64 {
    (frame * 1_000_000_000).div_ceil(u64::from(FPS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_numbering_matches_sixty_hertz() {
        assert_eq!(frame_at_nanos(0), 0);
        assert_eq!(frame_at_nanos(16_666_666), 0);
        assert_eq!(frame_at_nanos(16_666_667), 1);
        assert_eq!(frame_at_nanos(1_000_000_000), 60);
        assert_eq!(frame_start_nanos(60), 1_000_000_000);
        assert_eq!(frame_start_nanos(1), 16_666_667);
        for f in 0..1000u64 {
            assert_eq!(
                u64::from(frame_at_nanos(u128::from(frame_start_nanos(f)))),
                f
            );
        }
    }

    #[test]
    fn click_position_round_trips_including_negatives() {
        let s = init("t".into());
        assert_eq!(s.last_click(), None);
        s.set_last_click(-5, 17);
        assert_eq!(s.last_click(), Some((-5, 17)));
        s.set_last_click(640, 479);
        assert_eq!(s.last_click(), Some((640, 479)));
    }
}
