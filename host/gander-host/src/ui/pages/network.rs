// SPDX-License-Identifier: Apache-2.0
//! The Network page (mockup 44, `hNetwork`): the sockets that are bound, the trust adapter and
//! interface, how each connected device reaches this computer (direct, peer relay, DERP), and the
//! connectivity self-check: Windows Firewall Block rules that name Gander and Tailscale Shields Up
//! (pairing spec, "Windows Firewall" and "Shields Up").
//!
//! The check only reports. Removing a Block rule needs elevation and is not done from here.

use egui::{Align, Layout, Vec2};
use gander_core::protocol::host::{RelayState, TrustAdapterKind};

use super::Cx;
use crate::ui::format;
use crate::ui::glyphs::{self, Icon};
use crate::ui::theme::{self, pal};
use crate::ui::widgets::{self, Bar, Btn, Dot};

/// Draws the Network page.
pub fn show(ui: &mut egui::Ui, cx: &mut Cx<'_>) {
    let net = &cx.data.network;
    let running = net.self_check.running;
    widgets::page_header(
        ui,
        "Network",
        "Gander only listens on this computer and on your private network, never on the internet.",
        |ui| {
            if running {
                ui.label("Checking\u{2026}");
                ui.spinner();
            } else if ui
                .add(Btn::new("Check again").icon(Icon::Refresh))
                .clicked()
            {
                cx.out.refresh_network = true;
            }
        },
    );
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if let Some(problem) = &cx.data.settings.previews_problem {
                widgets::infobar(
                    ui,
                    Bar::Bad,
                    Icon::Error,
                    "Previews and thumbnails are off",
                    problem,
                );
            }
            if let Some(left) = net.paused_for {
                widgets::infobar(
                    ui,
                    Bar::Warn,
                    Icon::Pause,
                    "The listener is paused",
                    &format!(
                        "No device can reach this computer for another {}. Resume from the tray \
                         or the Gander window.",
                        format::countdown(left)
                    ),
                );
            }
            ui.columns(2, |cols| {
                listening(&mut cols[0], cx);
                connected(&mut cols[1], cx);
            });
            self_check(ui, cx);
        });
}

fn listening(ui: &mut egui::Ui, cx: &Cx<'_>) {
    let p = pal(ui);
    let net = &cx.data.network;
    widgets::card(ui, |ui| {
        ui.label(theme::semi(ui, "Listening on", theme::BODY, p.text));
        ui.add_space(2.0);
        if net.bound.is_empty() {
            ui.label(theme::text("Nothing is bound.", theme::BODY, p.bad));
        }
        for a in &net.bound {
            ui.horizontal(|ui| {
                ui.label(theme::mono(format::socket(*a), 12.0, p.text));
                ui.label(theme::text(
                    format::address_scope(a.ip()),
                    theme::CAPTION,
                    p.text_3,
                ));
            });
        }
        ui.add_space(4.0);
        let adapter = match net.adapter {
            TrustAdapterKind::Tailscale => {
                "Adapter: Tailscale. Devices are identified by their tailnet login."
            }
            TrustAdapterKind::Generic => {
                "Adapter: generic private network. Devices are identified by their token only."
            }
        };
        ui.label(theme::text(adapter, theme::CAPTION, p.text_2));
        let interface = net.interface.as_deref().map_or_else(
            || "Interface: none found".to_owned(),
            |i| format!("Interface: {i}"),
        );
        ui.label(theme::text(interface, theme::CAPTION, p.text_2));
    });
}

fn connected(ui: &mut egui::Ui, cx: &Cx<'_>) {
    let p = pal(ui);
    let net = &cx.data.network;
    widgets::card(ui, |ui| {
        ui.label(theme::semi(ui, "Connected now", theme::BODY, p.text));
        ui.add_space(2.0);
        if net.peers.is_empty() {
            ui.label(theme::text(
                "No device is connected.",
                theme::BODY,
                p.text_2,
            ));
        }
        let mut relayed = false;
        for peer in &net.peers {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                glyphs::icon(ui, Icon::Phone, 14.0, p.text_2);
                ui.label(theme::semi(ui, &peer.device_name, 12.5, p.text));
                ui.label(theme::mono(peer.address.to_string(), 11.5, p.text_3));
                let dot = match peer.path {
                    Some(RelayState::Direct) => Dot::Ok,
                    None => Dot::Off,
                    Some(_) => {
                        relayed = true;
                        Dot::Warn
                    }
                };
                let short = match peer.path {
                    Some(RelayState::Direct) => "direct",
                    Some(RelayState::PeerRelay) => "relayed (peer relay)",
                    Some(RelayState::Derp) => "relayed (DERP)",
                    None => "path unknown",
                };
                widgets::status(ui, dot, short, theme::CAPTION)
                    .on_hover_text(format::relay_text(peer.path));
            });
        }
        if relayed {
            ui.add_space(4.0);
            ui.label(theme::text(
                "A relay carries only a few Mbit/s. Browsing and previews work; streaming will be \
                 blurry. A direct path usually appears within a minute; if it never does, a router \
                 or firewall is blocking UDP.",
                theme::CAPTION,
                p.text_2,
            ));
        }
    });
}

/// One row of the connectivity check: a status icon, a title and what to do.
fn check_row(ui: &mut egui::Ui, bar: Bar, title: &str, detail: &str) {
    let p = pal(ui);
    let (icon, color) = match bar {
        Bar::Ok => (Icon::OkCircle, p.ok),
        Bar::Bad => (Icon::Error, p.bad),
        Bar::Warn => (Icon::Warn, p.warn),
        Bar::Neutral => (Icon::Info, p.text_2),
    };
    egui::Frame::new()
        .fill(p.layer)
        .stroke(egui::Stroke::new(1.0, p.stroke))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                glyphs::icon(ui, icon, 18.0, color);
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), 0.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(theme::text(title, 13.5, p.text));
                        if !detail.is_empty() {
                            ui.label(theme::text(detail, theme::CAPTION, p.text_2));
                        }
                    },
                );
            });
        });
    ui.add_space(4.0);
}

fn self_check(ui: &mut egui::Ui, cx: &Cx<'_>) {
    let net = &cx.data.network;
    let check = &net.self_check;
    widgets::section(ui, "Connectivity check");
    if net.interface.is_none() {
        check_row(
            ui,
            Bar::Bad,
            "No private network found",
            "Only this computer can connect. Start Tailscale (or your VPN) and Gander picks the \
             address up within a few seconds.",
        );
    }
    if !check.done && !check.running {
        check_row(ui, Bar::Neutral, "Not run yet", "Click Check again.");
        return;
    }
    if !check.done {
        return;
    }
    match check.shields_up {
        Some(true) => check_row(
            ui,
            Bar::Bad,
            "Tailscale Shields Up is on",
            "Tailscale is blocking every incoming connection, so no phone can reach this computer \
             over your tailnet. Turn on \"Allow incoming connections\" in the Tailscale menu.",
        ),
        Some(false) => check_row(
            ui,
            Bar::Ok,
            "Incoming connections are allowed",
            "Tailscale \"Shields Up\" is off.",
        ),
        None => {}
    }
    for rule in &check.blocking_rules {
        let profiles = if rule.profiles.is_empty() {
            "all profiles".to_owned()
        } else {
            rule.profiles.join(", ")
        };
        let proto = match rule.protocol {
            Some(6) => "TCP",
            Some(17) => "UDP",
            _ => "any protocol",
        };
        check_row(
            ui,
            Bar::Bad,
            &format!("A Windows Firewall rule blocks Gander: {}", rule.name),
            &format!(
                "Inbound Block rule for {proto} on {profiles}. A Block rule overrides every Allow \
                 rule, so devices cannot connect. Remove it in Windows Defender Firewall with \
                 Advanced Security (Inbound Rules)."
            ),
        );
    }
    if check.blocking_rules.is_empty() {
        check_row(ui, Bar::Ok, "No Windows Firewall rule blocks Gander", "");
    }
    for e in &check.errors {
        check_row(ui, Bar::Warn, "The check could not finish", e);
    }
    if !check.has_problems() && check.errors.is_empty() {
        let detail = match check.shields_up {
            Some(false) => "Nothing on this computer stops your devices from connecting.",
            _ => "Tailscale's incoming-connections setting could not be read.",
        };
        check_row(ui, Bar::Ok, "No problems found", detail);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    use super::*;
    use crate::ui::controller::{FolderPicker, SelfCheckView};
    use crate::ui::demo::DemoController;
    use crate::ui::model::UiData;
    use crate::ui::pages::Outbox;
    use crate::ui::picker::ScriptedPicker;

    fn harness(ctrl: Arc<DemoController>) -> Harness<'static, (Arc<DemoController>, Outbox)> {
        let picker: Arc<dyn FolderPicker> = Arc::new(ScriptedPicker::new(Vec::new()));
        let mut h = Harness::new_ui_state(
            move |ui, (ctrl, out): &mut (Arc<DemoController>, Outbox)| {
                let data = UiData::fetch(&**ctrl);
                let mut cx = Cx {
                    data: &data,
                    ctrl: &**ctrl,
                    picker: &picker,
                    out,
                };
                show(ui, &mut cx);
            },
            (ctrl, Outbox::default()),
        );
        h.set_size(egui::vec2(800.0, 1000.0));
        h
    }

    #[test]
    fn problems_are_called_out_with_what_to_do() {
        let mut h = harness(Arc::new(DemoController::sample()));
        h.run();
        assert!(h.query_by_label("Tailscale Shields Up is on").is_some());
        assert!(
            h.query_by_label("A Windows Firewall rule blocks Gander: gander-host.exe")
                .is_some()
        );
        assert!(
            h.query_by_label_contains("Allow incoming connections")
                .is_some()
        );
        assert!(h.query_by_label("No problems found").is_none());
        // Relay information for the connected phone.
        assert!(h.query_by_label("relayed (DERP)").is_some());
        // Bound sockets, loopback labelled.
        assert!(h.query_by_label("127.0.0.1:42637").is_some());
        assert!(h.query_by_label("this computer only").is_some());
        assert!(h.query_all_by_label("Tailscale").count() > 0);
    }

    #[test]
    fn a_clean_check_says_so_and_check_again_asks_for_a_refresh() {
        let ctrl = Arc::new(DemoController::empty());
        ctrl.edit_network(|n| {
            n.self_check = SelfCheckView {
                running: false,
                done: true,
                blocking_rules: Vec::new(),
                shields_up: Some(false),
                errors: Vec::new(),
            }
        });
        let mut h = harness(ctrl);
        h.run();
        assert!(h.query_by_label("No problems found").is_some());
        assert!(h.query_by_label("No device is connected.").is_some());
        h.get_by_label("Check again").click();
        h.run();
        assert!(h.state().1.refresh_network);
    }

    #[test]
    fn a_running_check_shows_progress_and_no_button() {
        let ctrl = Arc::new(DemoController::empty());
        ctrl.edit_network(|n| {
            n.self_check = SelfCheckView {
                running: true,
                done: false,
                ..SelfCheckView::default()
            }
        });
        let mut h = harness(ctrl);
        h.run_steps(3); // the spinner repaints forever
        assert!(h.query_by_label("Checking\u{2026}").is_some());
        assert!(h.query_by_label("Check again").is_none());
    }

    #[test]
    fn a_paused_listener_is_announced() {
        let ctrl = Arc::new(DemoController::empty());
        ctrl.edit_network(|n| n.paused_for = Some(std::time::Duration::from_secs(425)));
        let mut h = harness(ctrl);
        h.run();
        assert!(h.query_by_label("The listener is paused").is_some());
        assert!(h.query_by_label_contains("another 7:05").is_some());
    }
}
