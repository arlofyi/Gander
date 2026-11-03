// SPDX-License-Identifier: Apache-2.0
//! The Devices page (mockup 40, `hDevices`): one card per paired device with its identity,
//! address and path, the shares it can use and what it is doing now, with Stop stream and Revoke;
//! then the revoked devices, kept 30 days for the activity log (pairing spec, "Host-side
//! visibility and audit").

use egui::{Align, Layout};
use gander_core::protocol::DeviceId;
use gander_core::protocol::host::RelayState;

use super::Cx;
use super::shares::device_icon;
use crate::ui::controller::DeviceRow;
use crate::ui::format;
use crate::ui::glyphs::{self, Icon};
use crate::ui::theme::{self, pal};
use crate::ui::widgets::{self, Bar, Btn, Dot, Kind};

/// What the page remembers.
#[derive(Debug, Default)]
pub struct State {
    /// The device whose Revoke button was clicked once; the second click confirms.
    pub confirm_revoke: Option<DeviceId>,
    /// The last error, shown until the next action.
    pub error: Option<String>,
}

/// Draws the Devices page.
pub fn show(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>) {
    widgets::page_header(
        ui,
        "Devices",
        "Phones and tablets paired with this computer. Revoking a device cuts it off at once.",
        |ui| {
            if ui
                .add(
                    Btn::new("Add a device")
                        .kind(Kind::Primary)
                        .icon(Icon::Plus),
                )
                .clicked()
            {
                cx.out.open_add_device = true;
            }
        },
    );
    if let Some(e) = &st.error {
        widgets::infobar(ui, Bar::Bad, Icon::Error, "Could not revoke the device", e);
    }

    let mut active: Vec<&DeviceRow> = cx
        .data
        .devices
        .iter()
        .filter(|d| d.revoked_at.is_none())
        .collect();
    active.sort_by_key(|d| d.name.to_lowercase());
    let revoked: Vec<&DeviceRow> = cx
        .data
        .devices
        .iter()
        .filter(|d| d.revoked_at.is_some())
        .collect();

    if active.is_empty() && revoked.is_empty() {
        widgets::card(ui, |ui| {
            let p = pal(ui);
            ui.label(theme::semi(
                ui,
                "No devices are paired yet.",
                theme::BODY_STRONG,
                p.text,
            ));
            ui.label(theme::text(
                "Click Add a device, then scan the code with the Gander app on your phone or tablet.",
                theme::BODY,
                p.text_2,
            ));
        });
        return;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for d in active {
                ui.push_id(d.id.as_str().to_owned(), |ui| device_card(ui, st, cx, d));
            }
            if !revoked.is_empty() {
                widgets::section(ui, "Revoked, kept 30 days for the activity log");
                for d in revoked {
                    revoked_row(ui, cx, d);
                }
            }
        });
}

fn device_card(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>, d: &DeviceRow) {
    let p = pal(ui);
    let streaming = cx
        .data
        .status
        .stream
        .as_ref()
        .filter(|s| s.device_name == d.name);
    let peer = cx
        .data
        .network
        .peers
        .iter()
        .find(|pp| pp.device_name == d.name);
    widgets::card_with(ui, egui::Margin::same(14), |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            widgets::tile(ui, device_icon(&d.name, &d.model), 52.0);
            widgets::trailing(
                ui,
                false,
                |ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        ui.label(theme::semi(ui, &d.name, 15.0, p.text));
                        if let Some(s) = streaming {
                            let time = cx
                                .data
                                .stream_elapsed
                                .map(|e| format!(" \u{b7} {}", format::countdown(e)))
                                .unwrap_or_default();
                            widgets::status(
                                ui,
                                Dot::Live,
                                &format!("Streaming {}{time}", s.app_name),
                                12.0,
                            );
                        } else if d.connected {
                            widgets::status(ui, Dot::Ok, "Connected \u{b7} browsing", 12.0);
                        } else {
                            let seen = d.last_seen_at.map_or_else(
                                || "never connected".to_owned(),
                                |at| {
                                    format!(
                                        "last seen {}",
                                        format::ago(cx.data.now, at, cx.data.offset)
                                    )
                                },
                            );
                            widgets::status(ui, Dot::Off, &format!("Offline \u{b7} {seen}"), 12.0);
                        }
                    });
                    let platform = match d.platform.as_str() {
                        "android" => "Android".to_owned(),
                        other => other.to_owned(),
                    };
                    let mut meta = platform;
                    if !d.model.is_empty() && d.model != d.name {
                        meta.push_str(&format!(" \u{b7} {}", d.model));
                    }
                    if !d.app_version.is_empty() {
                        meta.push_str(&format!(" \u{b7} Gander {}", d.app_version));
                    }
                    meta.push_str(&format!(
                        " \u{b7} paired {}",
                        format::date_time(d.created_at, cx.data.offset)
                            .chars()
                            .take(10)
                            .collect::<String>()
                    ));
                    ui.label(theme::text(meta, 12.5, p.text_2));
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 18.0;
                        let identity = d
                            .identity
                            .as_ref()
                            .map(|(login, node)| (login.as_str(), node.as_str()));
                        fact(ui, Icon::Shield, &format::identity_line(identity), p.text_2);
                        if let Some(addr) = d.last_address {
                            let (path, color) = match peer.and_then(|pp| pp.path) {
                                Some(RelayState::Direct) => (" \u{b7} direct", p.text_2),
                                Some(RelayState::Derp) => (" \u{b7} relayed (DERP)", p.warn),
                                Some(_) => (" \u{b7} relayed", p.warn),
                                None => ("", p.text_2),
                            };
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                glyphs::icon(ui, Icon::Net, 13.0, p.text_2);
                                ui.label(theme::text(addr.to_string(), theme::CAPTION, p.text_2));
                                if !path.is_empty() {
                                    ui.label(theme::text(path, theme::CAPTION, color));
                                }
                            });
                        }
                        let shares: Vec<&str> = cx
                            .data
                            .shares
                            .iter()
                            .filter(|s| s.grants.iter().any(|g| g.device == d.id))
                            .map(|s| s.name.as_str())
                            .collect();
                        let shares = if shares.is_empty() {
                            "no shares yet".to_owned()
                        } else {
                            shares.join(", ")
                        };
                        fact(ui, Icon::Folder, &shares, p.text_2);
                    });
                },
                |ui| {
                    ui.with_layout(Layout::top_down(Align::Max), |ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        {
                            if streaming.is_some()
                                && ui
                                    .add(
                                        Btn::new("Stop stream")
                                            .name(format!("Stop stream on {}", d.name)),
                                    )
                                    .clicked()
                            {
                                cx.ctrl.stop_stream();
                            }
                            let revoke = ui.add(
                                Btn::new("Revoke")
                                    .kind(Kind::DangerOutline)
                                    .name(format!("Revoke {}", d.name)),
                            );
                            if revoke.clicked() {
                                st.confirm_revoke = Some(d.id.clone());
                            }
                        }
                    });
                },
            );
        });
        if st.confirm_revoke.as_ref() == Some(&d.id) {
            ui.add_space(8.0);
            widgets::infobar_with(
                ui,
                Bar::Bad,
                Icon::Warn,
                &format!("Revoke {}?", d.name),
                "It is disconnected at once and its token stops working. To use it again it has \
                 to be paired again.",
                |ui| {
                    if ui.add(Btn::new("Cancel")).clicked() {
                        st.confirm_revoke = None;
                    }
                    if ui.add(Btn::new("Yes, revoke").kind(Kind::Danger)).clicked() {
                        match cx.ctrl.revoke_device(&d.id) {
                            Ok(()) => st.error = None,
                            Err(e) => st.error = Some(e.to_string()),
                        }
                        st.confirm_revoke = None;
                    }
                },
            );
        }
    });
    ui.add_space(2.0);
}

fn fact(ui: &mut egui::Ui, icon: Icon, text: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        glyphs::icon(ui, icon, 13.0, color);
        ui.label(theme::text(text, theme::CAPTION, color));
    });
}

fn revoked_row(ui: &mut egui::Ui, cx: &Cx<'_>, d: &DeviceRow) {
    let mut desc = String::new();
    if let Some(at) = d.revoked_at {
        desc.push_str(&format!(
            "Revoked {}",
            format::ago(cx.data.now, at, cx.data.offset)
        ));
    }
    if let Some(at) = d.last_seen_at {
        desc.push_str(&format!(
            " \u{b7} last seen {}",
            format::ago(cx.data.now, at, cx.data.offset)
        ));
    }
    if let Some(a) = d.last_address {
        desc.push_str(&format!(" from {a}"));
    }
    ui.scope(|ui| {
        ui.multiply_opacity(0.7);
        widgets::setting_row(
            ui,
            Some(device_icon(&d.name, &d.model)),
            &d.name,
            &desc,
            |_| {},
        );
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    use super::*;
    use crate::ui::controller::{HostController as _, StreamSessionView};
    use crate::ui::demo::{Call, DemoController, TABLET};
    use crate::ui::model::UiData;
    use crate::ui::pages::Outbox;
    use crate::ui::picker::ScriptedPicker;

    struct Fixture {
        ctrl: Arc<DemoController>,
        st: State,
        out: Outbox,
    }

    fn harness(ctrl: Arc<DemoController>) -> Harness<'static, Fixture> {
        let mut h = Harness::new_ui_state(
            |ui, f: &mut Fixture| {
                let data = UiData::fetch(&*f.ctrl);
                let picker: Arc<dyn crate::ui::controller::FolderPicker> =
                    Arc::new(ScriptedPicker::new(Vec::new()));
                let mut cx = Cx {
                    data: &data,
                    ctrl: &*f.ctrl,
                    picker: &picker,
                    out: &mut f.out,
                };
                show(ui, &mut f.st, &mut cx);
            },
            Fixture {
                ctrl,
                st: State::default(),
                out: Outbox::default(),
            },
        );
        h.set_size(egui::vec2(1000.0, 900.0));
        h
    }

    #[test]
    fn lists_active_and_revoked_devices_without_a_revoke_button_on_revoked_ones() {
        let ctrl = Arc::new(DemoController::sample());
        let mut h = harness(ctrl);
        h.run();
        assert!(h.query_by_label("Pixel 9").is_some());
        assert!(h.query_by_label("Galaxy Tab S9").is_some());
        assert!(h.query_by_label("Old Galaxy").is_some());
        assert!(h.query_by_label("Connected \u{b7} browsing").is_some());
        assert!(
            h.query_by_label_contains("alice@example.com's pixel-9")
                .is_some()
        );
        assert!(h.query_by_label("Revoke Pixel 9").is_some());
        assert!(h.query_by_label("Revoke Galaxy Tab S9").is_some());
        assert!(h.query_by_label("Revoke Old Galaxy").is_none());
    }

    #[test]
    fn a_streaming_device_says_so_and_can_be_stopped() {
        let ctrl = Arc::new(DemoController::sample());
        ctrl.set_stream(Some(StreamSessionView {
            device_name: "Pixel 9".into(),
            file_name: "Deck.pptx".into(),
            app_name: "PowerPoint".into(),
            audio: false,
        }));
        let mut h = harness(ctrl);
        h.run();
        assert!(h.query_by_label_contains("Streaming PowerPoint").is_some());
        assert!(h.query_by_label("Stop stream on Pixel 9").is_some());
    }

    #[test]
    fn revoke_needs_a_second_click_and_calls_the_controller_once() {
        let ctrl = Arc::new(DemoController::sample());
        let mut h = harness(Arc::clone(&ctrl));
        h.run();
        h.get_by_label("Revoke Galaxy Tab S9").click();
        h.run();
        assert!(ctrl.calls().is_empty(), "one click only asks");
        assert!(h.query_by_label("Yes, revoke").is_some());
        h.get_by_label("Cancel").click();
        h.run();
        h.step();
        assert!(ctrl.calls().is_empty());

        h.get_by_label("Revoke Galaxy Tab S9").click();
        h.run();
        h.get_by_label("Yes, revoke").click();
        h.run();
        assert_eq!(ctrl.calls().len(), 1);
        assert_eq!(ctrl.calls()[0], Call::Revoke(DeviceId::from(TABLET)));
        let revoked = ctrl
            .devices()
            .into_iter()
            .filter(|d| d.revoked_at.is_some())
            .count();
        assert_eq!(revoked, 2);
    }

    #[test]
    fn add_a_device_asks_the_application_to_open_the_window() {
        let ctrl = Arc::new(DemoController::empty());
        let mut h = harness(ctrl);
        h.run();
        assert!(h.query_by_label("No devices are paired yet.").is_some());
        h.get_by_label("Add a device").click();
        h.run();
        assert!(h.state().out.open_add_device);
    }
}
