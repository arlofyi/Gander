// SPDX-License-Identifier: Apache-2.0
//! The tray icon and the settings windows (WP-18; ADR-0007: `tray-icon` + `egui`/`eframe`).
//!
//! Behind the cargo feature `ui` (on by default for the binary). Without it the host runs
//! headless, which is what `cargo xtask e2e-host` and the integration tests use.
//!
//! # Layout
//!
//! | Module | What |
//! | --- | --- |
//! | [`controller`] | [`HostController`], the one seam between the UI and the host, and the view types |
//! | [`live`] | [`LiveController`]: the real controller over `Server`, `PairingService`, the registry, audit log, `ApiState` (which owns the events link) and config |
//! | [`demo`] | [`DemoController`]: in-memory sample data and call recording, for `--ui-smoke` and tests |
//! | [`app`] | the application: tray, polling, window lifecycle, toasts, eframe sessions |
//! | [`idle`] | the tray-only message loop between sessions |
//! | [`windows`], [`pages`] | the drawing code of each window |
//! | [`tray`], [`tray_state`], [`icons`] | the tray icon, its menu, its four states and the lens pixels |
//! | [`toast`] | what the owner is told about pairing requests and sessions |
//! | [`share_editor`] | the Shares window's draft logic (deny by default, validation) |
//! | [`autostart`], [`picker`] | the autostart switch and folder dialog, behind traits |
//! | [`raster`], [`shot`], [`smoke`] | offscreen screenshots and `gander-host --ui-smoke` |
//!
//! # Wiring (application)
//!
//! ```ignore
//! let pause = gander_host::server::ListenerPause::new();
//! // plan source: wrap the real one so "Pause listener" can close the private-network sockets
//! let plan = Arc::new(PausablePlan::new(Arc::new(SystemPlan::new(listener)), pause.clone()));
//! // ... start the server, then, on the thread that owns the event loop (the main thread):
//! let controller = LiveController::new(LiveParts { server: server.handle(), pairing, store,
//!     config_dir, config, clipboard, pause, api, caches, autostart: system_autostart(),
//!     runtime: tokio::runtime::Handle::current() });
//! gander_host::ui::run(Arc::new(controller))?;   // returns when the owner picks Quit (or `run_live(parts)`)
//! ```
//!
//! # Security properties
//!
//! - The pairing window is open exactly while the Add a device window is open; closing the window,
//!   the kill switch, pausing the listener and quitting all close it.
//! - The QR payload is only ever rendered as an image, dropped when the window closes, and never
//!   logged or formatted as text; the secret is never displayed.
//! - Tray state follows `security.md` principle 7: hollow lens (idle), green lens (connected),
//!   solid orange lens (streaming), lens with bars (paused); toasts on pairing requests and session
//!   starts.
//! - Share edits are drafts until Save; a new share allows nobody; granting Stream shows the
//!   warning that the streamed app's own dialogs reach other folders.

pub mod app;
pub mod autostart;
pub mod controller;
pub mod demo;
pub mod format;
pub mod glyphs;
pub mod icons;
pub mod idle;
pub mod live;
pub mod model;
pub mod pages;
pub mod picker;
pub mod qr;
pub mod raster;
pub mod screen;
pub mod share_editor;
pub mod shot;
pub mod smoke;
pub mod theme;
pub mod toast;
pub mod tray;
pub mod tray_state;
pub mod widgets;
pub mod windows;

use std::sync::Arc;

pub use app::request_open_window;
pub use autostart::{Autostart, StubAutostart, TaskSchedulerAutostart, system_autostart};
pub use controller::{FolderPicker, HostController};
pub use demo::DemoController;
pub use live::{LiveController, LiveParts};

/// The folder dialog the application uses.
#[cfg(windows)]
fn system_picker() -> Arc<dyn FolderPicker> {
    Arc::new(picker::SystemPicker)
}

/// There is no folder dialog on this platform yet: Add folder does nothing.
#[cfg(not(windows))]
fn system_picker() -> Arc<dyn FolderPicker> {
    Arc::new(picker::ScriptedPicker::new(Vec::new()))
}

/// Runs the tray and windows until the owner picks Quit. Must be called on the main thread (the
/// tray icon and the event loop need it) and blocks. Only the tray exists until a window or a
/// toast must show (see [`app`]); a window that cannot open (no window system or OpenGL context)
/// is reported and the tray keeps running.
pub fn run(controller: Arc<dyn HostController>) -> anyhow::Result<()> {
    run_app(controller, system_picker(), None)
}

/// [`run`] over the real host: builds the [`LiveController`] from `parts` first.
pub fn run_live(parts: LiveParts) -> anyhow::Result<()> {
    run(Arc::new(LiveController::new(parts)))
}

/// Holds the hybrid-GPU decision for a window session's OpenGL context (`[ui] gpu`). The host
/// exports `NvOptimusEnablement` (and AMD's equivalent) with the value 0; the Stream engine
/// raises it to 1 only while it sets up its encoder, so the default (`integrated`) context lands
/// on the integrated GPU. The guard is held from just before `run_native` until the context
/// exists, and it serialises with the Stream engine's encoder setup so neither sees the
/// other's value. Measured: `docs/specs/2026-11-03-igpu-ui-findings.md`.
#[cfg(windows)]
fn gpu_choice(choice: gander_core::config::UiGpu) -> gander_stream::win::optimus::GpuDecision {
    let on_battery = gander_win::session::power::power_status().is_ok_and(|s| s.on_battery());
    gander_stream::win::optimus::GpuDecision::take(choice.wants_discrete(on_battery))
}

/// No hybrid-GPU switch on this platform.
#[cfg(not(windows))]
fn gpu_choice(_choice: gander_core::config::UiGpu) -> NoGpuChoice {
    NoGpuChoice
}

#[cfg(not(windows))]
#[derive(Debug)]
struct NoGpuChoice;

pub(crate) fn run_app(
    controller: Arc<dyn HostController>,
    picker: Arc<dyn FolderPicker>,
    smoke: Option<smoke::SmokeDriver>,
) -> anyhow::Result<()> {
    let mut app = app::GanderApp::new(controller, picker, smoke);
    loop {
        // Tray only: no window, no GL context, no egui.
        loop {
            app.idle_tick();
            if app.quitting() || app.needs_session() {
                break;
            }
            idle::pump(app.idle_wait());
        }
        if app.quitting() {
            break;
        }
        // The GPU decision is taken while the GL context is created (the creator closure runs
        // just after it): see `gpu_choice`.
        let mut gpu = Some(gpu_choice(app.ui_gpu()));
        let options = eframe::NativeOptions {
            viewport: app::root_viewport(),
            // Return when the session's root closes; eframe reuses its event loop next time.
            run_and_return: true,
            ..Default::default()
        };
        let result = eframe::run_native(
            "Gander",
            options,
            Box::new(|cc| {
                let _ = gpu.take();
                Ok(Box::new(app::Session::new(cc, &mut app)))
            }),
        );
        app.session_ended(result.err().map(|e| e.to_string()));
        idle::trim_working_set();
    }
    app.shutdown();
    Ok(())
}
