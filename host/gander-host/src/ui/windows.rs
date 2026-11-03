// SPDX-License-Identifier: Apache-2.0
//! The content of each window as a free function over `(egui::Context, state, Cx)`, so the same
//! drawing code runs in the live application, in the offscreen screenshots and in the tests.
//!
//! - [`main_window`]: the NavigationView (status block, pages, Add a device and Disconnect
//!   everyone at the bottom), the orange live band while a device streams, the kill-switch result
//!   band, and the current page on the flat Mica page layer (mockups 38 to 45, 48).
//! - [`add_device_window`]: Add a device (mockups 41, 42).
//! - [`toast_stack`]: the notifications that need buttons (pairing request, Stream prompts).
//! - [`tray_flyout`]: the tray flyout (mockup 37). `tray-icon` only offers a native menu, so the
//!   flyout is a small borderless always-on-top egui window above the notification area.
//! - [`viewing_pill`]: the orange top-of-screen pill with Stop while a device streams (mockup 46).

use std::time::Duration;

use egui::{Align, CornerRadius, Layout, Margin, Sense, Stroke, StrokeKind, Vec2};

use super::controller::{HostStatus, StreamSessionView};
use super::format;
use super::glyphs::{self, Icon};
use super::model::{Page, UiData};
use super::pages::{self, Cx};
use super::theme::{self, Palette, pal, pal_ctx};
use super::toast::{Toast, ToastAction, ToastKind};
use super::widgets::{self, Btn, Dot, Kind};

/// What the kill switch did, for the result band (mockup 48).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillReport {
    /// Local clock time it was used, `14:07`.
    pub at: String,
    /// Connections open when it was used.
    pub connections: usize,
    /// Whether a stream was running.
    pub stream: bool,
}

impl KillReport {
    /// A report for the status just before the kill switch, at local `now`.
    pub fn new(status: &HostStatus, now: time::OffsetDateTime) -> Self {
        Self {
            at: format!("{:02}:{:02}", now.hour(), now.minute()),
            connections: status.connected.len(),
            stream: status.stream.is_some(),
        }
    }

    /// "1 stream and 2 connections ended."
    pub fn summary(&self) -> String {
        let conns = match self.connections {
            0 => "No device was connected".to_owned(),
            1 => "1 connection ended".to_owned(),
            n => format!("{n} connections ended"),
        };
        if self.stream {
            format!("The stream and {}.", conns.to_lowercase())
        } else {
            format!("{conns}.")
        }
    }
}

/// What each page remembers.
#[derive(Default, Debug)]
pub struct PageStates {
    /// Shares page.
    pub shares: pages::shares::State,
    /// Devices page.
    pub devices: pages::devices::State,
    /// Add device window.
    pub add_device: pages::add_device::State,
    /// Activity page.
    pub activity: pages::activity::State,
    /// Settings page.
    pub settings: pages::settings::State,
    /// The kill switch's result band, until dismissed.
    pub kill: Option<KillReport>,
}

/// What the main window asks the application to do.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MainOut {
    /// The owner picked another page.
    pub next_page: Option<Page>,
    /// The owner pressed Disconnect everyone.
    pub kill: bool,
}

/// The Gander window: live band, navigation and the current page.
pub fn main_window(
    ctx: &egui::Context,
    page: Page,
    states: &mut PageStates,
    cx: &mut Cx<'_>,
) -> MainOut {
    let mut out = MainOut::default();
    let p = pal_ctx(ctx);
    live_band(ctx, cx);
    egui::SidePanel::left("nav")
        .exact_width(212.0)
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(p.mica).inner_margin(Margin {
            left: 6,
            right: 6,
            top: 6,
            bottom: 10,
        }))
        .show(ctx, |ui| nav(ui, page, cx, &mut out));
    page_layer(ctx, |ui| {
        kill_band(ui, states, cx);
        match page {
            Page::Shares => pages::shares::show(ui, &mut states.shares, cx),
            Page::Devices => pages::devices::show(ui, &mut states.devices, cx),
            Page::Activity => pages::activity::show(ui, &mut states.activity, cx),
            Page::Network => pages::network::show(ui, cx),
            Page::Settings => pages::settings::show(ui, &mut states.settings, cx),
        }
    });
    out
}

/// The content page: `layer_2` with a 1 px stroke on its top and left edges and an 8 px top-left
/// radius (the flat Mica approximation), padded 20 x 26.
fn page_layer(ctx: &egui::Context, add: impl FnOnce(&mut egui::Ui)) {
    let p = pal_ctx(ctx);
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(p.mica))
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            // Past the right and bottom edges, so only the top and left strokes show.
            let layer = egui::Rect::from_min_max(rect.min, rect.max + Vec2::splat(4.0));
            ui.painter().rect(
                layer,
                CornerRadius {
                    nw: 8,
                    ne: 0,
                    sw: 0,
                    se: 0,
                },
                p.layer_2,
                Stroke::new(1.0, p.stroke),
                StrokeKind::Inside,
            );
            let inner = rect.shrink2(Vec2::new(26.0, 20.0));
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(inner)
                    .layout(Layout::top_down(Align::Min)),
            );
            add(&mut child);
        });
}

fn nav(ui: &mut egui::Ui, page: Page, cx: &mut Cx<'_>, out: &mut MainOut) {
    let p = pal(ui);
    let status = &cx.data.status;
    egui::Frame::new()
        .inner_margin(Margin {
            left: 10,
            right: 10,
            top: 8,
            bottom: 12,
        })
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(theme::semi(
                ui,
                &status.host_name,
                theme::BODY_STRONG,
                p.text,
            ));
            let (dot, text) = status_line(cx.data);
            widgets::status(ui, dot, &text, theme::CAPTION);
        });
    let net = &cx.data.network;
    let active_devices = cx
        .data
        .devices
        .iter()
        .filter(|d| d.revoked_at.is_none())
        .count();
    for pg in Page::ALL {
        let (icon, count, warn) = match pg {
            Page::Shares => (Icon::Folder, cx.data.shares.len().to_string(), false),
            Page::Devices => (Icon::Phone, active_devices.to_string(), false),
            Page::Activity => (Icon::Pulse, String::new(), false),
            Page::Network => {
                let bad = net.self_check.has_problems() || net.interface.is_none();
                (
                    Icon::Net,
                    if bad { "!".to_owned() } else { String::new() },
                    bad,
                )
            }
            Page::Settings => (Icon::Gear, String::new(), false),
        };
        if nav_item(ui, p, icon, pg.title(), &count, warn, page == pg).clicked() {
            out.next_page = Some(pg);
        }
    }
    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        egui::Frame::new()
            .inner_margin(Margin::symmetric(4, 0))
            .show(ui, |ui| {
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    let kill = ui
                        .add(
                            Btn::new("Disconnect everyone")
                                .kind(Kind::DangerOutline)
                                .icon(Icon::Power)
                                .full_width(),
                        )
                        .on_hover_text(
                            "Kill switch: drops every session and closes pairing. Paired devices \
                             can reconnect; revoke a device to lock it out.",
                        );
                    if kill.clicked() {
                        out.kill = true;
                    }
                    if ui
                        .add(
                            Btn::new("Add a device")
                                .kind(Kind::Primary)
                                .icon(Icon::Plus)
                                .full_width(),
                        )
                        .clicked()
                    {
                        cx.out.open_add_device = true;
                    }
                });
            });
    });
}

fn nav_item(
    ui: &mut egui::Ui,
    p: &Palette,
    icon: Icon,
    title: &str,
    count: &str,
    warn: bool,
    selected: bool,
) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, title)
    });
    let painter = ui.painter();
    if selected || resp.hovered() {
        painter.rect_filled(rect, 5, p.stroke);
    }
    if selected {
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.top() + 10.0),
            Vec2::new(3.0, rect.height() - 20.0),
        );
        painter.rect_filled(bar, 2, p.accent);
    }
    glyphs::paint(
        painter,
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + 18.5, rect.center().y),
            Vec2::splat(17.0),
        ),
        icon,
        p.text,
    );
    painter.text(
        egui::pos2(rect.left() + 39.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(theme::BODY),
        p.text,
    );
    if !count.is_empty() {
        let (font, color) = if warn {
            (egui::FontId::new(11.0, theme::semibold(ui.ctx())), p.warn)
        } else {
            (egui::FontId::proportional(11.0), p.text_3)
        };
        painter.text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            count,
            font,
            color,
        );
    }
    resp
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The status line under the computer's name, in the nav and the tray flyout.
pub fn status_line(data: &UiData) -> (Dot, String) {
    let status = &data.status;
    if let Some(s) = &status.stream {
        let by = if s.audio {
            "Being viewed with sound"
        } else {
            "Being viewed"
        };
        return (Dot::Live, format!("{by} by {}", s.device_name));
    }
    if let Some(left) = status.paused_for {
        return (
            Dot::Off,
            format!("Paused \u{b7} {} left", format::countdown(left)),
        );
    }
    let net = &data.network;
    if net.interface.is_none() {
        return (
            Dot::Warn,
            "Only this computer: no private network".to_owned(),
        );
    }
    match net.adapter {
        gander_core::protocol::host::TrustAdapterKind::Tailscale => {
            (Dot::Ok, "Listening on Tailscale".to_owned())
        }
        gander_core::protocol::host::TrustAdapterKind::Generic => {
            (Dot::Ok, "Listening on your private network".to_owned())
        }
    }
}

/// "Pixel 9 is viewing PowerPoint" with the file, time and Stop stream, across the top of a
/// window while a device streams (design doc 9.3).
fn live_band(ctx: &egui::Context, cx: &Cx<'_>) {
    let Some(s) = &cx.data.status.stream else {
        return;
    };
    let p = pal_ctx(ctx);
    egui::TopBottomPanel::top("live-band")
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(p.accent).inner_margin(Margin {
            left: 14,
            right: 10,
            top: 7,
            bottom: 7,
        }))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                glyphs::icon(ui, Icon::Preview, 18.0, p.on_accent);
                ui.label(theme::semi(
                    ui,
                    format!("{} is viewing {}", s.device_name, s.viewing(&s.app_name)),
                    theme::BODY,
                    p.on_accent,
                ));
                let mut detail = s.file_name.clone();
                if let Some(e) = cx.data.stream_elapsed {
                    detail.push_str(&format!(" \u{b7} {}", format::countdown(e)));
                }
                detail.push_str(" \u{b7} it can click and type in that window");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(Btn::new("Stop stream").kind(Kind::OnAccent).height(26.0))
                        .clicked()
                    {
                        cx.ctrl.stop_stream();
                    }
                    ui.add(
                        egui::Label::new(theme::text(detail, theme::BODY, p.on_accent)).truncate(),
                    );
                });
            });
        });
}

/// The kill-switch result band (mockup 48) and the paused band ("paused from the menu").
fn kill_band(ui: &mut egui::Ui, states: &mut PageStates, cx: &Cx<'_>) {
    let paused = cx.data.status.paused_for;
    if states.kill.is_none() && paused.is_none() {
        return;
    }
    let p = pal(ui);
    let (title, mut body) = match &states.kill {
        Some(k) => (
            format!("Everyone was disconnected at {}.", k.at),
            format!(
                "{} Pairing is closed. Paired devices can reconnect; revoke a device to lock it out.",
                k.summary()
            ),
        ),
        None => (
            "Gander is paused.".to_owned(),
            "No device can connect and pairing is closed.".to_owned(),
        ),
    };
    if let Some(left) = paused {
        body.push_str(&format!(
            " Gander is not listening for {} more.",
            format::countdown(left)
        ));
    }
    egui::Frame::new()
        .fill(p.primary)
        .corner_radius(6)
        .inner_margin(Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                glyphs::icon(ui, Icon::Power, 18.0, p.on_primary);
                widgets::trailing(
                    ui,
                    true,
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(theme::semi(ui, &title, theme::BODY_STRONG, p.on_primary));
                        ui.label(theme::text(&body, theme::BODY, p.on_primary));
                    },
                    |ui| {
                        if paused.is_some() {
                            if ui
                                .add(Btn::new("Resume now").kind(Kind::OnAccent))
                                .clicked()
                            {
                                cx.ctrl.resume_listener();
                                states.kill = None;
                            }
                        } else if ui.add(Btn::new("Dismiss").kind(Kind::OnAccent)).clicked() {
                            states.kill = None;
                        }
                    },
                );
            });
        });
    ui.add_space(14.0);
}

/// The Add a device window.
pub fn add_device_window(ctx: &egui::Context, states: &mut PageStates, cx: &mut Cx<'_>) {
    live_band(ctx, cx);
    page_layer(ctx, |ui| {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                pages::add_device::show(ui, &mut states.add_device, cx);
            });
    });
}

/// Height of one toast with buttons.
pub const TOAST_HEIGHT: f32 = 156.0;

/// The toast window: one card per toast. Returns the buttons the owner pressed.
pub fn toast_stack(ctx: &egui::Context, items: &[Toast]) -> Vec<(u64, ToastAction)> {
    let mut clicked = Vec::new();
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            for t in items {
                ui.push_id(t.id, |ui| toast_card(ui, t, &mut clicked));
            }
        });
    clicked
}

fn toast_card(ui: &mut egui::Ui, t: &Toast, clicked: &mut Vec<(u64, ToastAction)>) {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.mica)
        .stroke(Stroke::new(1.0, p.stroke_2))
        .corner_radius(8)
        .inner_margin(Margin {
            left: 14,
            right: 14,
            top: 12,
            bottom: 14,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(TOAST_HEIGHT - 34.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                glyphs::lens(ui, 16.0);
                ui.label(theme::text("Gander", theme::CAPTION, p.text_2));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let close = ui.add(
                        Btn::new("")
                            .icon(Icon::Close)
                            .kind(Kind::Subtle)
                            .height(22.0)
                            .min_width(26.0)
                            .name("Dismiss"),
                    );
                    if close.clicked() {
                        clicked.push((t.id, ToastAction::None));
                    }
                    ui.label(theme::text("now", theme::CAPTION, p.text_2));
                });
            });
            ui.add_space(2.0);
            if matches!(t.kind, ToastKind::StreamStarted) {
                ui.horizontal(|ui| {
                    widgets::status(ui, Dot::Live, &t.title, theme::BODY_STRONG);
                });
            } else {
                ui.label(theme::semi(ui, &t.title, theme::BODY_STRONG, p.text));
            }
            ui.label(theme::text(&t.body, 12.5, p.text_2));
            ui.add_space(6.0);
            let buttons = toast_buttons(t);
            if buttons.is_empty() {
                return;
            }
            ui.columns(buttons.len(), |cols| {
                for (col, (label, kind, action)) in cols.iter_mut().zip(buttons) {
                    if col.add(Btn::new(label).kind(kind).full_width()).clicked() {
                        clicked.push((t.id, action));
                    }
                }
            });
        });
}

fn toast_buttons(t: &Toast) -> Vec<(&'static str, Kind, ToastAction)> {
    use super::controller::StreamPromptAnswer as A;
    match t.kind {
        ToastKind::StreamPrompt { id, approval: true } => vec![
            (
                "Allow once",
                Kind::Primary,
                ToastAction::Answer(id, A::AllowOnce),
            ),
            ("Always", Kind::Standard, ToastAction::Answer(id, A::Always)),
            ("Deny", Kind::Standard, ToastAction::Answer(id, A::Deny)),
        ],
        ToastKind::StreamPrompt {
            id,
            approval: false,
        } => vec![
            (
                "Allow",
                Kind::Primary,
                ToastAction::Answer(id, A::AllowOnce),
            ),
            ("Not now", Kind::Standard, ToastAction::Answer(id, A::Deny)),
        ],
        ToastKind::PairingRequest(_) => vec![(
            "Compare the code",
            Kind::Primary,
            ToastAction::OpenAddDevice,
        )],
        ToastKind::StreamStarted => vec![
            ("Stop stream", Kind::Danger, ToastAction::StopStream),
            ("Open Gander", Kind::Standard, ToastAction::OpenDevices),
        ],
        ToastKind::DeviceConnected => {
            vec![("Open Gander", Kind::Standard, ToastAction::OpenDevices)]
        }
        ToastKind::Notice => Vec::new(),
    }
}

/// What the tray flyout asks the application to do.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FlyoutOut {
    /// Open the Gander window on this page.
    pub open: Option<Page>,
    /// Open Add a device.
    pub add_device: bool,
    /// Pause for 10 minutes, or resume.
    pub toggle_pause: bool,
    /// Disconnect everyone.
    pub kill: bool,
    /// Stop the stream.
    pub stop_stream: bool,
    /// Quit Gander.
    pub quit: bool,
}

/// The flyout's height for a status (it grows with the connected devices).
pub fn flyout_height(status: &HostStatus) -> f32 {
    let rows = status.connected.len().max(1) as f32;
    300.0 + rows * 50.0
}

/// Width of the tray flyout.
pub const FLYOUT_WIDTH: f32 = 340.0;

/// The tray flyout (mockup 37): status, who is connected, quick actions and the kill switch.
pub fn tray_flyout(ctx: &egui::Context, data: &UiData, pause_minutes: u64) -> FlyoutOut {
    let mut out = FlyoutOut::default();
    let p = pal_ctx(ctx);
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(p.mica)
                .stroke(Stroke::new(1.0, p.stroke_2))
                .corner_radius(8)
                .inner_margin(Margin::same(14)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                glyphs::lens(ui, 30.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(theme::semi(ui, "Gander", theme::BODY_STRONG, p.text));
                    ui.label(theme::text(
                        &data.status.host_name,
                        theme::CAPTION,
                        p.text_2,
                    ));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let gear = Btn::new("")
                        .icon(Icon::Gear)
                        .kind(Kind::Subtle)
                        .min_width(30.0)
                        .name("Settings");
                    if ui.add(gear).clicked() {
                        out.open = Some(Page::Settings);
                    }
                });
            });
            ui.add_space(8.0);
            let (dot, text) = status_line(data);
            let text = if dot == Dot::Ok {
                format!("Ready \u{b7} {}", lower_first(&text))
            } else {
                text
            };
            widgets::status(ui, dot, &text, 12.5);
            ui.add_space(8.0);
            ui.label(theme::text("Connected now", theme::CAPTION, p.text_2));
            if data.status.connected.is_empty() {
                widgets::card_with(ui, Margin::symmetric(10, 10), |ui| {
                    ui.label(theme::text("No device is connected.", 12.5, p.text_2));
                });
            }
            for d in &data.status.connected {
                let stream = data
                    .status
                    .stream
                    .as_ref()
                    .filter(|s| s.device_name == d.name);
                let path = data
                    .network
                    .peers
                    .iter()
                    .find(|pp| pp.device_name == d.name)
                    .map(|pp| match pp.path {
                        Some(gander_core::protocol::host::RelayState::Direct) => " \u{b7} direct",
                        Some(_) => " \u{b7} relayed",
                        None => "",
                    })
                    .unwrap_or("");
                let model = data
                    .devices
                    .iter()
                    .find(|r| r.id == d.id)
                    .map(|r| r.model.clone())
                    .unwrap_or_default();
                widgets::card_with(ui, Margin::symmetric(10, 8), |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        glyphs::icon(
                            ui,
                            pages::shares::device_icon(&d.name, &model),
                            17.0,
                            p.text_2,
                        );
                        widgets::trailing(
                            ui,
                            true,
                            |ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.label(theme::text(&d.name, 13.5, p.text));
                                let what = match stream {
                                    Some(s) => format!("Streaming {}{path}", s.app_name),
                                    None => format!("Browsing{path}"),
                                };
                                let color = if stream.is_some() {
                                    p.accent_text
                                } else {
                                    p.text_2
                                };
                                ui.label(theme::text(what, theme::CAPTION, color));
                            },
                            |ui| {
                                if stream.is_some()
                                    && ui
                                        .add(
                                            Btn::new("Stop")
                                                .kind(Kind::Subtle)
                                                .height(26.0)
                                                .name("Stop stream"),
                                        )
                                        .clicked()
                                {
                                    out.stop_stream = true;
                                }
                            },
                        );
                    });
                });
            }
            ui.add_space(6.0);
            let paused = data.status.paused_for.is_some();
            let pause_label = if paused {
                "Resume now".to_owned()
            } else {
                format!("Pause {pause_minutes} min")
            };
            ui.columns(3, |cols| {
                if tile_button(&mut cols[0], Icon::Plus, "Add a device").clicked() {
                    out.add_device = true;
                }
                if tile_button(&mut cols[1], Icon::Pause, &pause_label).clicked() {
                    out.toggle_pause = true;
                }
                if tile_button(&mut cols[2], Icon::Window, "Open Gander").clicked() {
                    out.open = Some(Page::Shares);
                }
            });
            ui.add_space(8.0);
            if ui
                .add(
                    Btn::new("Disconnect everyone")
                        .kind(Kind::Danger)
                        .icon(Icon::Power)
                        .height(36.0)
                        .full_width(),
                )
                .clicked()
            {
                out.kill = true;
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(theme::text(
                    "Ends every session and closes pairing",
                    theme::CAPTION,
                    p.text_2,
                ));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(
                            Btn::new("Quit")
                                .kind(Kind::Subtle)
                                .height(24.0)
                                .name("Quit Gander"),
                        )
                        .clicked()
                    {
                        out.quit = true;
                    }
                });
            });
        });
    out
}

fn tile_button(ui: &mut egui::Ui, icon: Icon, label: &str) -> egui::Response {
    let p = pal(ui);
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 62.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let fill = if resp.hovered() {
        theme::mix(p.layer, p.text, 0.05)
    } else {
        p.layer
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        6,
        fill,
        Stroke::new(1.0, p.stroke),
        StrokeKind::Inside,
    );
    glyphs::paint(
        painter,
        egui::Rect::from_center_size(rect.center() - Vec2::new(0.0, 10.0), Vec2::splat(17.0)),
        icon,
        p.text,
    );
    painter.text(
        rect.center() + Vec2::new(0.0, 13.0),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(theme::CAPTION),
        p.text,
    );
    resp
}

/// Size of the viewing pill's window.
pub const PILL_SIZE: Vec2 = Vec2::new(480.0, 44.0);

/// The always-on-top pill: "Pixel 9 is viewing PowerPoint · 00:41" with Stop. Returns whether
/// Stop was clicked.
pub fn viewing_pill(
    ctx: &egui::Context,
    stream: &StreamSessionView,
    elapsed: Option<Duration>,
) -> bool {
    let p = pal_ctx(ctx);
    let mut stop = false;
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT))
        .show(ctx, |ui| {
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                egui::Frame::new()
                    .fill(p.accent)
                    .corner_radius(18)
                    .inner_margin(Margin {
                        left: 14,
                        right: 6,
                        top: 6,
                        bottom: 6,
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            glyphs::icon(ui, Icon::Preview, 16.0, p.on_accent);
                            let mut text = format!(
                                "{} is viewing {}",
                                stream.device_name,
                                stream.viewing(&stream.app_name)
                            );
                            if let Some(e) = elapsed {
                                text.push_str(&format!(" \u{b7} {}", format::countdown(e)));
                            }
                            ui.label(theme::semi(ui, text, 12.5, p.on_accent));
                            if ui
                                .add(
                                    Btn::new("Stop")
                                        .kind(Kind::OnAccent)
                                        .height(24.0)
                                        .name("Stop stream"),
                                )
                                .clicked()
                            {
                                stop = true;
                            }
                        });
                    });
            });
        });
    stop
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kill_report_counts_what_ended() {
        let k = KillReport {
            at: "14:07".into(),
            connections: 2,
            stream: true,
        };
        assert_eq!(k.summary(), "The stream and 2 connections ended.");
        let k = KillReport {
            at: "14:07".into(),
            connections: 0,
            stream: false,
        };
        assert_eq!(k.summary(), "No device was connected.");
    }
}
