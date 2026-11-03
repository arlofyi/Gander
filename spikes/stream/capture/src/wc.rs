// SPDX-License-Identifier: Apache-2.0
//! Comparison run through `windows-capture` 2.0.1 (MIT): same window, its own device. Records
//! frame timing, which adapter its device is on and with which creation flags, to answer
//! "can the encoder share this device?" from measurement rather than from reading the source.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use spike_capture::clock;
use spike_capture::d3d;
use windows::Win32::Foundation::HWND;
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

use crate::measure::{FrameRec, Run};

/// Facts about the device windows-capture created.
#[derive(Debug, Clone, Default)]
pub struct DeviceFacts {
    pub luid: i64,
    pub video_support: bool,
    pub multithread_protected: bool,
}

#[derive(Debug, Default)]
struct Shared {
    frames: Vec<FrameRec>,
    device: Option<DeviceFacts>,
}

struct Handler {
    shared: Arc<Mutex<Shared>>,
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Arc<Mutex<Shared>>;
    type Error = String;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let facts = DeviceFacts {
            luid: d3d::device_luid(&ctx.device).unwrap_or(0),
            video_support: d3d::has_video_support(&ctx.device),
            multithread_protected: d3d::is_multithread_protected(&ctx.device),
        };
        ctx.flags
            .lock()
            .map_err(|e| e.to_string())?
            .device
            .replace(facts);
        Ok(Handler { shared: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame<'_>,
        _control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let arrival = clock::now_100ns();
        let srt = frame.timestamp().map_err(|e| e.to_string())?.Duration;
        let rec = FrameRec {
            srt_100ns: srt,
            arrival_100ns: arrival,
            size: (frame.width(), frame.height()),
            counter: None,
            patch_a: false,
            patch_b: false,
            extra: 0.0,
        };
        self.shared
            .lock()
            .map_err(|e| e.to_string())?
            .frames
            .push(rec);
        Ok(())
    }
}

/// Captures `hwnd` for `dur` with windows-capture's own device.
pub fn run(hwnd: HWND, dur: Duration) -> Result<(Run, DeviceFacts)> {
    let shared = Arc::new(Mutex::new(Shared::default()));
    let settings = Settings::new(
        Window::from_raw_hwnd(hwnd.0),
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        shared.clone(),
    );
    let control =
        Handler::start_free_threaded(settings).map_err(|e| anyhow!("windows-capture: {e:?}"))?;
    let started = std::time::Instant::now();
    std::thread::sleep(dur);
    let secs = started.elapsed().as_secs_f64();
    control
        .stop()
        .map_err(|e| anyhow!("windows-capture stop: {e:?}"))?;
    let mut s = shared.lock().map_err(|e| anyhow!("{e}"))?;
    let frames = std::mem::take(&mut s.frames);
    let facts = s.device.clone().unwrap_or_default();
    Ok((
        Run {
            frames,
            secs,
            dropped_full: 0,
        },
        facts,
    ))
}
