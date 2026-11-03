// SPDX-License-Identifier: Apache-2.0
//! Offscreen screenshots of every window in both themes, for `docs/specs/assets/ui/` and the
//! smoke test: `<name>.png` (light) and `<name>-dark.png`.
//!
//! Each window's real drawing code ([`super::windows`]) runs against [`DemoController`] sample
//! data in a headless egui context and is rasterised by [`super::raster`]. No window is opened and
//! nothing is read from the screen, so a locked session or a busy desktop cannot leak into an
//! image.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use gander_core::pairing::window::Flow;

use super::controller::{FolderPicker, HostController, StreamSessionView};
use super::demo::{DemoController, sample_pending};
use super::model::{Page, UiData};
use super::pages::{Cx, Outbox};
use super::picker::ScriptedPicker;
use super::raster::{self, Image};
use super::toast::{NewToast, ToastAction, ToastKind, ToastQueue};
use super::windows::{self, KillReport, PageStates};

/// The clock every screenshot shows (UTC), so the images are the same on every run and machine.
pub fn fixed_now() -> time::OffsetDateTime {
    let date = time::Date::from_calendar_date(2026, time::Month::November, 2)
        .expect("a valid constant date");
    let at = time::Time::from_hms(14, 30, 0).expect("a valid constant time");
    time::PrimitiveDateTime::new(date, at).assume_utc()
}

/// A screenshot has at least this many pixels that differ from the background, or it is blank.
pub const MIN_DRAWN_PIXELS: usize = 1500;

/// What one screenshot shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shot {
    /// A page of the Gander window.
    Page(Page),
    /// The Shares page with the Share a folder dialog open.
    ShareDialog,
    /// The Devices page while a device streams (live band).
    DevicesLive,
    /// The Add a device window with the QR code.
    AddDeviceQr,
    /// The Add a device window with a request waiting for Allow or Deny.
    AddDevicePending,
    /// The tray flyout.
    Flyout,
    /// The top-of-screen pill while a device streams.
    Pill,
    /// The notifications that need buttons.
    Toasts,
    /// The Gander window right after Disconnect everyone, paused.
    Kill,
}

impl Shot {
    /// Every screenshot, in the order they are written.
    pub const ALL: [Shot; 13] = [
        Shot::Page(Page::Shares),
        Shot::ShareDialog,
        Shot::Page(Page::Devices),
        Shot::DevicesLive,
        Shot::AddDeviceQr,
        Shot::AddDevicePending,
        Shot::Page(Page::Activity),
        Shot::Page(Page::Network),
        Shot::Page(Page::Settings),
        Shot::Flyout,
        Shot::Pill,
        Shot::Toasts,
        Shot::Kill,
    ];

    /// File stem (light theme; the dark one adds `-dark`).
    pub fn name(self) -> &'static str {
        match self {
            Shot::Page(Page::Shares) => "shares",
            Shot::Page(Page::Devices) => "devices",
            Shot::Page(Page::Activity) => "activity",
            Shot::Page(Page::Network) => "network",
            Shot::Page(Page::Settings) => "settings",
            Shot::ShareDialog => "share-folder",
            Shot::DevicesLive => "devices-live",
            Shot::AddDeviceQr => "add-device",
            Shot::AddDevicePending => "add-device-pending",
            Shot::Flyout => "tray-flyout",
            Shot::Pill => "viewing-pill",
            Shot::Toasts => "toasts",
            Shot::Kill => "kill-switch",
        }
    }

    /// Image size in pixels.
    pub fn size(self) -> (u32, u32) {
        match self {
            Shot::Page(_) | Shot::ShareDialog | Shot::DevicesLive | Shot::Kill => (1040, 700),
            Shot::AddDeviceQr => (720, 600),
            Shot::AddDevicePending => (720, 520),
            Shot::Flyout => (windows::FLYOUT_WIDTH as u32, 350),
            Shot::Pill => (windows::PILL_SIZE.x as u32, windows::PILL_SIZE.y as u32),
            Shot::Toasts => (364, 3 * (windows::TOAST_HEIGHT as u32 + 8)),
        }
    }
}

/// One image written.
#[derive(Debug, Clone)]
pub struct Written {
    /// File stem.
    pub name: String,
    /// The PNG's path.
    pub path: PathBuf,
    /// Image width.
    pub width: u32,
    /// Image height.
    pub height: u32,
    /// PNG size on disk.
    pub bytes: usize,
    /// Pixels that differ from the background colour.
    pub drawn_pixels: usize,
}

fn demo_stream() -> StreamSessionView {
    StreamSessionView {
        device_name: "Pixel 9".into(),
        file_name: "Q3 review deck.pptx".into(),
        app_name: "PowerPoint".into(),
        audio: false,
    }
}

fn toasts() -> ToastQueue {
    let mut toasts = ToastQueue::new();
    let now = Instant::now();
    toasts.push(
        NewToast {
            kind: ToastKind::PairingRequest("pair_demo".into()),
            title: "Pixel 9 wants to pair".into(),
            body: "alice@example.com's pixel-9. Open Add a device and compare the code.".into(),
            action: ToastAction::OpenAddDevice,
        },
        now,
    );
    for (id, kind) in [
        (1, crate::lifecycle::PromptKind::Approval),
        (2, crate::lifecycle::PromptKind::Attended),
    ] {
        toasts.push(
            super::toast::prompt_toast(&crate::lifecycle::PromptView {
                id,
                kind,
                device_name: "Pixel 9".into(),
                file_name: "Q3 review deck.pptx".into(),
                app_name: "PowerPoint".into(),
                handler: "POWERPNT.EXE".into(),
                publisher: Some("Microsoft Corporation".into()),
                expires_at: now,
            }),
            now,
        );
    }
    toasts
}

/// Renders one screenshot, light or `dark`.
pub fn render(shot: Shot, dark: bool) -> Image {
    let ctrl = Arc::new(DemoController::sample_at(fixed_now()));
    let picker: Arc<dyn FolderPicker> = Arc::new(ScriptedPicker::new(Vec::new()));
    let mut elapsed = None;
    match shot {
        Shot::AddDeviceQr => {
            ctrl.open_pairing_window();
        }
        Shot::AddDevicePending => {
            ctrl.open_pairing_window();
            ctrl.receive_pairing_request(sample_pending(
                "pair_demo",
                "Pixel 9",
                Flow::Manual,
                true,
            ));
        }
        Shot::DevicesLive | Shot::Pill => {
            ctrl.set_stream(Some(demo_stream()));
            elapsed = Some(Duration::from_secs(if shot == Shot::Pill {
                41
            } else {
                252
            }));
        }
        Shot::Kill => {
            ctrl.pause_listener(Duration::from_secs(592));
        }
        _ => {}
    }
    let mut data = UiData::fetch(&*ctrl);
    data.now = fixed_now();
    data.offset = time::UtcOffset::UTC;
    data.stream_elapsed = elapsed;
    let mut states = PageStates::default();
    if shot == Shot::Kill {
        states.kill = Some(KillReport {
            at: "14:07".into(),
            connections: 2,
            stream: true,
        });
    }
    let mut out = Outbox::default();
    let toasts = toasts();
    raster::render(shot.size(), 1.0, dark, |ctx| {
        let mut cx = Cx {
            data: &data,
            ctrl: &*ctrl,
            picker: &picker,
            out: &mut out,
        };
        match shot {
            Shot::Page(p) => {
                windows::main_window(ctx, p, &mut states, &mut cx);
            }
            Shot::ShareDialog => {
                if states.shares.dialog.is_none() {
                    let root = if cfg!(windows) {
                        PathBuf::from("D:\\Projects\\Clients\\Kestrel house")
                    } else {
                        PathBuf::from("/srv/projects/clients/kestrel house")
                    };
                    super::pages::shares::open_dialog(&mut states.shares, &cx, root);
                }
                windows::main_window(ctx, Page::Shares, &mut states, &mut cx);
            }
            Shot::DevicesLive => {
                windows::main_window(ctx, Page::Devices, &mut states, &mut cx);
            }
            Shot::Kill => {
                windows::main_window(ctx, Page::Shares, &mut states, &mut cx);
            }
            Shot::AddDeviceQr | Shot::AddDevicePending => {
                windows::add_device_window(ctx, &mut states, &mut cx);
                // Past the anti-reflex delay, so Allow shows as it does after two seconds.
                states
                    .add_device
                    .backdate_request(super::pages::add_device::ALLOW_DELAY);
            }
            Shot::Flyout => {
                windows::tray_flyout(ctx, &data, super::tray::PAUSE_MINUTES);
            }
            Shot::Pill => {
                windows::viewing_pill(ctx, &demo_stream(), elapsed);
            }
            Shot::Toasts => {
                windows::toast_stack(ctx, toasts.items());
            }
        }
    })
}

/// Number of pixels that differ from the top-left pixel (the background of every window).
pub fn drawn_pixels(img: &Image) -> usize {
    let bg = img.pixel(0, 0);
    img.rgba.chunks_exact(4).filter(|p| **p != bg).count()
}

/// Renders every screenshot in both themes into `dir` (created if missing).
pub fn render_all(dir: &Path) -> anyhow::Result<Vec<Written>> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut written = Vec::new();
    for shot in Shot::ALL {
        for dark in [false, true] {
            let img = render(shot, dark);
            let png = img.to_png().context("encoding a screenshot")?;
            let name = if dark {
                format!("{}-dark", shot.name())
            } else {
                shot.name().to_owned()
            };
            let path = dir.join(format!("{name}.png"));
            std::fs::write(&path, &png).with_context(|| format!("writing {}", path.display()))?;
            written.push(Written {
                name,
                path,
                width: img.width,
                height: img.height,
                bytes: png.len(),
                drawn_pixels: drawn_pixels(&img),
            });
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_window_renders_something_in_both_themes() {
        for shot in Shot::ALL {
            for dark in [false, true] {
                let img = render(shot, dark);
                assert_eq!((img.width, img.height), shot.size());
                let drawn = drawn_pixels(&img);
                let min = if shot == Shot::Pill {
                    300
                } else {
                    MIN_DRAWN_PIXELS
                };
                assert!(
                    drawn >= min,
                    "{} (dark {dark}) drew only {drawn} pixels",
                    shot.name()
                );
            }
        }
    }

    #[test]
    fn the_themes_differ() {
        let light = render(Shot::Page(Page::Settings), false);
        let dark = render(Shot::Page(Page::Settings), true);
        assert_ne!(light.pixel(5, 5), dark.pixel(5, 5));
    }

    #[test]
    fn the_add_device_screenshot_has_a_qr_code_dark_and_light_modules() {
        let img = render(Shot::AddDeviceQr, true);
        let dark = img
            .rgba
            .chunks_exact(4)
            .filter(|p| p[0] < 40 && p[1] < 40 && p[2] < 40)
            .count();
        let white = img
            .rgba
            .chunks_exact(4)
            .filter(|p| p[0] > 250 && p[1] > 250 && p[2] > 250)
            .count();
        assert!(dark > 1500, "{dark} near-black pixels: the QR modules");
        assert!(
            white > 1500,
            "{white} white pixels: the QR stays on white in dark mode"
        );
    }

    #[test]
    fn rendering_is_deterministic_so_the_committed_images_do_not_churn() {
        for shot in [
            Shot::Page(Page::Shares),
            Shot::AddDevicePending,
            Shot::Flyout,
        ] {
            assert_eq!(
                render(shot, false),
                render(shot, false),
                "{} differs between runs",
                shot.name()
            );
        }
    }

    #[test]
    fn render_all_writes_both_themes_as_pngs() {
        let dir = std::env::temp_dir().join(format!("gander-shots-{}", std::process::id()));
        let written = render_all(&dir).unwrap();
        assert_eq!(written.len(), Shot::ALL.len() * 2);
        assert!(written.iter().any(|w| w.name == "shares-dark"));
        for w in &written {
            let bytes = std::fs::read(&w.path).unwrap();
            assert_eq!(&bytes[..4], b"\x89PNG");
            assert_eq!(bytes.len(), w.bytes);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
