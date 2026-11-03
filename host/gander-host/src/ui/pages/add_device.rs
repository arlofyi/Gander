// SPDX-License-Identifier: Apache-2.0
//! The Add a device window (pairing spec, "Pairing", flows A and B; mockups 41 `hAdd` and 42
//! `hAddApprove`).
//!
//! The pairing window exists only while this window is open: the application opens it when this
//! window opens and closes it when this window closes (`HostController::open_pairing_window` and
//! `close_pairing_window`). While it is open the window shows the QR code (flow A), three steps,
//! and the addresses and port for flow B. When a device asks to pair it shows who, the identity,
//! the large six-digit code, and Deny / Allow, with the warning text the spec requires.
//!
//! **Allow waits [`ALLOW_DELAY`] after a request appears** (owner decision, anti-reflex): the
//! button is disabled and counts down, so a click meant for something else cannot approve.
//!
//! The QR payload carries the window secret. It is rendered as an image only, the matrix is dropped
//! as soon as the window is no longer open, and nothing here formats it as text or logs it.

use std::time::{Duration, Instant};

use egui::{Align, Layout, Margin, Vec2};
use gander_core::pairing::window::{CloseReason, Flow, MAX_FAILURES, PendingView};
use gander_core::protocol::PairingId;

use super::Cx;
use super::shares::device_icon;
use crate::ui::format;
use crate::ui::glyphs::{self, Icon};
use crate::ui::qr::QrMatrix;
use crate::ui::theme::{self, pal};
use crate::ui::widgets::{self, Bar, Btn, Kind};

/// Pixels per QR module.
const QR_SCALE: usize = 6;

/// How long Allow stays disabled after a request appears.
pub const ALLOW_DELAY: Duration = Duration::from_secs(2);

/// What the window remembers.
#[derive(Default)]
pub struct State {
    texture: Option<egui::TextureHandle>,
    /// The pairing window generation and the addresses the texture was made for.
    generation: Option<(u64, Vec<std::net::IpAddr>)>,
    /// The last error from Allow / Deny.
    pub error: Option<String>,
    /// The request on screen and when it first appeared.
    seen: Option<(PairingId, Instant)>,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State")
            .field("has_qr", &self.texture.is_some())
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl State {
    /// Forgets the QR code (the pairing window closed or this window closed).
    pub fn clear_qr(&mut self) {
        self.texture = None;
        self.generation = None;
    }

    /// Whether a QR image is held.
    pub fn has_qr(&self) -> bool {
        self.texture.is_some()
    }

    /// Makes the request on screen look older by `by` (tests and screenshots skip the delay).
    pub fn backdate_request(&mut self, by: Duration) {
        if let Some((_, at)) = &mut self.seen {
            *at = at.checked_sub(by).unwrap_or(*at);
        }
    }

    /// How long Allow still waits for the request `id` (zero once it may be clicked).
    fn allow_wait(&mut self, id: &PairingId, now: Instant) -> Duration {
        if self.seen.as_ref().is_none_or(|(seen, _)| seen != id) {
            self.seen = Some((id.clone(), now));
        }
        let at = self.seen.as_ref().map_or(now, |(_, at)| *at);
        ALLOW_DELAY.saturating_sub(now.saturating_duration_since(at))
    }
}

/// Draws the Add a device window.
pub fn show(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>) {
    let snap = &cx.data.pairing;
    if !snap.open {
        st.clear_qr();
    }
    if let Some(e) = &st.error {
        widgets::infobar(ui, Bar::Bad, Icon::Error, "That did not work", e);
    }

    if let Some(pending) = &snap.pending {
        pending_view(ui, st, cx, pending);
        return;
    }
    st.seen = None;

    widgets::page_header(
        ui,
        "Add a device",
        "Pairing is only possible while this window is open.",
        |_| {},
    );
    if snap.open {
        qr_view(ui, st, cx);
    } else {
        closed_card(ui, st, cx);
    }
    footer(ui, cx);
}

fn footer(ui: &mut egui::Ui, cx: &mut Cx<'_>) {
    let p = pal(ui);
    let snap = &cx.data.pairing;
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        if snap.open
            && let Some(left) = snap.expires_in
        {
            glyphs::icon(ui, Icon::Clock, 13.0, p.text_2);
            ui.label(theme::text(
                format!("This window closes in {}", format::countdown(left)),
                12.5,
                p.text_2,
            ));
            ui.label(theme::text("\u{b7}", 12.5, p.text_3));
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
        if snap.open {
            attempts(ui, cx);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add(Btn::new("Close")).clicked() {
                cx.out.close_add_device = true;
            }
        });
    });
}

fn qr_view(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>) {
    let p = pal(ui);
    let snap = &cx.data.pairing;
    // A code without an address is refused by the phone ("damaged"): show why instead.
    if let Some(payload) = &cx.data.qr
        && payload.addresses.is_empty()
    {
        st.texture = None;
        st.generation = None;
        widgets::infobar(
            ui,
            Bar::Warn,
            Icon::Warn,
            "Not on your private network yet.",
            "Gander has no Tailscale (or other private network) address to put in the code. Check              that Tailscale is connected on this computer; the code appears here as soon as it is.",
        );
        return;
    }
    if let Some(payload) = &cx.data.qr
        && st.generation.as_ref().map(|(g, a)| (*g, a))
            != Some((snap.generation, &payload.addresses))
    {
        if let Some(matrix) = QrMatrix::for_payload(payload) {
            st.texture = Some(ui.ctx().load_texture(
                "pairing-qr",
                matrix.to_image(QR_SCALE),
                egui::TextureOptions::NEAREST,
            ));
            st.generation = Some((snap.generation, payload.addresses.clone()));
        } else {
            st.error = Some("The pairing code could not be drawn.".to_owned());
        }
    }
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 24.0;
        if let Some(tex) = &st.texture {
            // The QR sits on white in both themes, as scanners expect.
            egui::Frame::new()
                .fill(egui::Color32::WHITE)
                .stroke(egui::Stroke::new(1.0, p.stroke_2))
                .corner_radius(8)
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.add(egui::Image::new(tex).fit_to_exact_size(Vec2::splat(196.0)));
                });
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            let step = |ui: &mut egui::Ui, n: &str, text: &str| {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.add_sized(
                        [18.0, 18.0],
                        egui::Label::new(theme::semi(ui, n, theme::BODY, p.text)),
                    );
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.label(theme::text(text, theme::BODY, p.text));
                        },
                    );
                });
            };
            step(
                ui,
                "1",
                "On the phone or tablet, open Gander and tap Add a computer.",
            );
            step(
                ui,
                "2",
                "Scan this code. It only works on this computer and only while this window is \
                 open.",
            );
            step(
                ui,
                "3",
                "Check the name and the six-digit code here, then click Allow.",
            );
            if let Some(qr) = &cx.data.qr {
                widgets::card(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.label(theme::text(
                        "No camera? Type one of these addresses in the app instead",
                        theme::CAPTION,
                        p.text_2,
                    ));
                    for a in &qr.addresses {
                        ui.label(theme::mono(a.to_string(), 12.5, p.text));
                    }
                    if let Some(m) = &qr.magic_dns {
                        ui.label(theme::mono(m, 12.5, p.text));
                    }
                    ui.horizontal(|ui| {
                        ui.label(theme::text("port", 12.5, p.text_3));
                        ui.label(theme::mono(qr.port.to_string(), 12.5, p.text));
                    });
                    ui.label(theme::text(
                        format!("Computer: {}", qr.host_name),
                        theme::CAPTION,
                        p.text_3,
                    ));
                });
            }
        });
    });
    ui.add_space(12.0);
    widgets::infobar(
        ui,
        Bar::Warn,
        Icon::Warn,
        "Only add a device you own.",
        "If someone asked you to do this, close this window. Anyone who can see the code can \
         start a pairing request, but nothing is allowed until you click Allow here.",
    );
}

fn pending_view(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>, pend: &PendingView) {
    let p = pal(ui);
    let name = &pend.device.name;
    ui.label(theme::semi(
        ui,
        format!("{name} wants to pair"),
        22.0,
        p.text,
    ));
    ui.label(theme::text(
        "Check that this is your device and that the code matches the one on its screen.",
        theme::BODY,
        p.text_2,
    ));
    ui.add_space(12.0);
    let device_word = if device_icon(name, &pend.device.model) == Icon::Tablet {
        "tablet"
    } else {
        "phone"
    };
    widgets::card_with(ui, Margin::same(14), |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            widgets::tile(ui, device_icon(name, &pend.device.model), 52.0);
            widgets::trailing(
                ui,
                false,
                |ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.label(theme::semi(ui, name, 15.0, p.text));
                    let platform = if pend.device.platform == "android" {
                        "Android"
                    } else {
                        pend.device.platform.as_str()
                    };
                    let mut meta = platform.to_owned();
                    if pend.device.model != *name {
                        meta.push_str(&format!(" \u{b7} {}", pend.device.model));
                    }
                    meta.push_str(&format!(" \u{b7} Gander {}", pend.device.app_version));
                    ui.label(theme::text(meta, 12.5, p.text_2));
                    ui.add_space(4.0);
                    let identity = pend
                        .identity
                        .as_ref()
                        .map(|i| (i.login.as_str(), i.node_name.as_str()));
                    let color = if identity.is_some() { p.ok } else { p.warn };
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        glyphs::icon(
                            ui,
                            if identity.is_some() {
                                Icon::Shield
                            } else {
                                Icon::Warn
                            },
                            14.0,
                            color,
                        );
                        ui.label(theme::text(format::identity_line(identity), 12.5, color));
                    });
                    if pend.identity.as_ref().is_some_and(|i| i.shared_in) {
                        ui.label(theme::text("Shared from another tailnet", 12.5, p.warn));
                    }
                    ui.label(theme::text(
                        format!("From {}", pend.source),
                        theme::CAPTION,
                        p.text_3,
                    ));
                },
                |ui| {
                    ui.with_layout(Layout::top_down(Align::Max), |ui| {
                        ui.label(theme::text(
                            format!("Code on the {device_word} should be"),
                            theme::CAPTION,
                            p.text_2,
                        ));
                        ui.add_space(4.0);
                        let code = egui::RichText::new(pend.sas.to_string())
                            .font(egui::FontId::new(theme::CODE, egui::FontFamily::Monospace))
                            .color(p.text);
                        ui.label(code);
                    });
                },
            );
        });
    });
    ui.add_space(4.0);
    match pend.flow {
        Flow::Qr => widgets::infobar(
            ui,
            Bar::Warn,
            Icon::Warn,
            &format!("Only approve if the code on your {device_word} matches."),
            "This device scanned the code on this screen. If someone asked you to do this, click \
             Deny.",
        ),
        Flow::Manual => widgets::infobar(
            ui,
            Bar::Warn,
            Icon::Warn,
            &format!("Only approve if the code on your {device_word} matches."),
            "If someone is telling you a code over the phone, click Deny. If someone asked you to \
             do this, click Deny.",
        ),
    }
    ui.add_space(8.0);
    let wait = st.allow_wait(&pend.pairing_id, Instant::now());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        glyphs::icon(ui, Icon::Clock, 13.0, p.text_2);
        ui.label(theme::text(
            format!("Request expires in {}", format::countdown(pend.expires_in)),
            12.5,
            p.text_2,
        ));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let allow_text = if wait.is_zero() {
                format!("Allow {name}")
            } else {
                format!("Allow {name} ({})", wait.as_secs() + 1)
            };
            let allow = Btn::new(allow_text)
                .kind(Kind::Primary)
                .min_width(200.0)
                .name(format!("Allow {name}"));
            if ui.add_enabled(wait.is_zero(), allow).clicked() {
                match cx.ctrl.approve_pairing(&pend.pairing_id) {
                    Ok(_) => st.error = None,
                    Err(e) => st.error = Some(e.to_string()),
                }
            }
            if ui.add(Btn::new("Deny").min_width(120.0)).clicked() {
                match cx.ctrl.deny_pairing(&pend.pairing_id) {
                    Ok(()) => st.error = None,
                    Err(e) => st.error = Some(e.to_string()),
                }
            }
        });
    });
    if !wait.is_zero() {
        ui.ctx()
            .request_repaint_after(wait.min(Duration::from_millis(250)));
    } else {
        ui.ctx().request_repaint_after(Duration::from_millis(500));
    }
}

fn attempts(ui: &mut egui::Ui, cx: &Cx<'_>) {
    let p = pal(ui);
    let snap = &cx.data.pairing;
    let text = if snap.failures == 0 {
        "No failed attempts".to_owned()
    } else {
        format!(
            "{} failed attempt{}",
            snap.failures,
            if snap.failures == 1 { "" } else { "s" }
        )
    };
    let color = if snap.failures == 0 { p.text_2 } else { p.warn };
    ui.label(theme::text(text, 12.5, color));
    if snap.rate_limited_sources > 0 {
        ui.label(theme::text(
            format!(
                "\u{b7} {} source{} blocked after {MAX_FAILURES} failures",
                snap.rate_limited_sources,
                if snap.rate_limited_sources == 1 {
                    ""
                } else {
                    "s"
                }
            ),
            12.5,
            p.bad,
        ));
    }
}

fn closed_card(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>) {
    let (bar, icon, text) = match cx.data.pairing.closed_reason {
        Some(CloseReason::Approved) => (
            Bar::Ok,
            Icon::OkCircle,
            "Device approved. It finishes pairing in a moment.",
        ),
        Some(CloseReason::Denied) => (Bar::Neutral, Icon::Info, "Denied. Nothing was added."),
        Some(CloseReason::Expired) => (
            Bar::Warn,
            Icon::Clock,
            "The code expired. Show a new one to try again.",
        ),
        Some(CloseReason::Owner) => (Bar::Neutral, Icon::Info, "Pairing is off."),
        None => (Bar::Neutral, Icon::Info, "Starting\u{2026}"),
    };
    let mut again = false;
    widgets::infobar_with(
        ui,
        bar,
        icon,
        text,
        "No device can pair while pairing is off.",
        |ui| {
            again = ui
                .add(Btn::new("Show a new code").kind(Kind::Primary))
                .clicked();
        },
    );
    if again {
        st.clear_qr();
        cx.ctrl.open_pairing_window();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;
    use gander_core::protocol::PairingId;

    use super::*;
    use crate::ui::controller::{FolderPicker, HostController as _};
    use crate::ui::demo::{Call, DemoController, sample_pending};
    use crate::ui::model::UiData;
    use crate::ui::pages::Outbox;
    use crate::ui::picker::ScriptedPicker;

    struct Fixture {
        ctrl: Arc<DemoController>,
        picker: Arc<dyn FolderPicker>,
        st: State,
        out: Outbox,
    }

    fn harness(ctrl: Arc<DemoController>) -> Harness<'static, Fixture> {
        let mut h = Harness::new_ui_state(
            |ui, f: &mut Fixture| {
                let data = UiData::fetch(&*f.ctrl);
                let mut cx = Cx {
                    data: &data,
                    ctrl: &*f.ctrl,
                    picker: &f.picker,
                    out: &mut f.out,
                };
                show(ui, &mut f.st, &mut cx);
            },
            Fixture {
                ctrl,
                picker: Arc::new(ScriptedPicker::new(Vec::new())),
                st: State::default(),
                out: Outbox::default(),
            },
        );
        h.set_size(egui::vec2(700.0, 700.0));
        h
    }

    /// Lets the Allow delay pass without sleeping.
    fn skip_delay(h: &mut Harness<'_, Fixture>) {
        h.state_mut().st.backdate_request(ALLOW_DELAY);
        h.run_steps(2);
    }

    #[test]
    fn an_open_window_shows_the_qr_the_manual_details_and_the_warning() {
        let ctrl = Arc::new(DemoController::empty());
        ctrl.open_pairing_window();
        let mut h = harness(ctrl);
        h.run();
        assert!(h.state().st.has_qr(), "texture built from the payload");
        assert!(
            h.query_by_label_contains("open Gander and tap Add a computer")
                .is_some()
        );
        assert!(h.query_by_label("100.101.102.103").is_some());
        assert!(h.query_by_label("42637").is_some());
        assert!(h.query_by_label("studio-pc.tail1234.ts.net").is_some());
        assert!(h.query_by_label("No failed attempts").is_some());
        assert!(
            h.query_by_label_contains("This window closes in 5:00")
                .is_some()
        );
        assert!(h.query_by_label("Only add a device you own.").is_some());
        // The secret is never drawn as text.
        assert!(h.query_by_label_contains("demo-secret").is_none());
    }

    #[test]
    fn a_pending_request_shows_who_the_code_and_allow_deny() {
        let ctrl = Arc::new(DemoController::empty());
        ctrl.open_pairing_window();
        let pending = sample_pending("pair_1", "Pixel 9", Flow::Manual, true);
        let code = pending.sas.to_string();
        ctrl.receive_pairing_request(pending);
        let mut h = harness(Arc::clone(&ctrl));
        h.run_steps(3);
        assert!(h.query_by_label("Pixel 9 wants to pair").is_some());
        assert!(h.query_by_label(&code).is_some(), "the SAS {code} is shown");
        assert!(h.query_by_label("alice@example.com's pixel-9").is_some());
        assert!(
            h.query_by_label("Only approve if the code on your phone matches.")
                .is_some()
        );
        assert!(
            h.query_by_label_contains(
                "If someone is telling you a code over the phone, click Deny."
            )
            .is_some()
        );
        assert!(h.query_by_label("Allow Pixel 9").is_some());
        assert!(h.query_by_label("Deny").is_some());
        // While a request waits the QR code is not offered.
        assert!(h.query_by_label("Only add a device you own.").is_none());
    }

    #[test]
    fn allow_is_inactive_for_the_first_two_seconds() {
        let ctrl = Arc::new(DemoController::empty());
        ctrl.open_pairing_window();
        ctrl.receive_pairing_request(sample_pending("pair_7", "Pixel 9", Flow::Qr, false));
        let mut h = harness(Arc::clone(&ctrl));
        h.run_steps(2);
        h.get_by_label("Allow Pixel 9").click();
        h.run_steps(2);
        assert!(
            !ctrl.calls().iter().any(|c| matches!(c, Call::Approve(_))),
            "a reflex click does nothing"
        );
        skip_delay(&mut h);
        h.get_by_label("Allow Pixel 9").click();
        h.run_steps(2);
        assert_eq!(
            ctrl.calls().last(),
            Some(&Call::Approve(PairingId::from("pair_7")))
        );
    }

    #[test]
    fn allow_approves_that_exact_request() {
        let ctrl = Arc::new(DemoController::empty());
        ctrl.open_pairing_window();
        ctrl.receive_pairing_request(sample_pending("pair_7", "Pixel 9", Flow::Qr, false));
        let mut h = harness(Arc::clone(&ctrl));
        h.run_steps(2);
        assert!(
            h.query_by_label("identity unknown (not a Tailscale peer)")
                .is_some()
        );
        skip_delay(&mut h);
        h.get_by_label("Allow Pixel 9").click();
        h.run_steps(2);
        let calls = ctrl.calls();
        assert_eq!(
            calls.last(),
            Some(&Call::Approve(PairingId::from("pair_7")))
        );
        assert!(!ctrl.pairing().open, "the window closes on success");
        h.run_steps(2);
        assert!(h.query_by_label_contains("Device approved").is_some());
        assert!(
            !h.state().st.has_qr(),
            "the QR image is dropped once the window is closed"
        );
    }

    #[test]
    fn deny_denies_at_once() {
        let ctrl = Arc::new(DemoController::empty());
        ctrl.open_pairing_window();
        ctrl.receive_pairing_request(sample_pending("pair_8", "Pixel 9", Flow::Manual, false));
        let mut h = harness(Arc::clone(&ctrl));
        h.run_steps(2);
        h.get_by_label("Deny").click();
        h.run_steps(2);
        assert_eq!(
            ctrl.calls().last(),
            Some(&Call::Deny(PairingId::from("pair_8")))
        );
        h.run_steps(2);
        assert!(h.query_by_label_contains("Denied.").is_some());
    }

    #[test]
    fn failed_attempts_and_rate_limits_are_counted() {
        let ctrl = Arc::new(DemoController::empty());
        ctrl.open_pairing_window();
        ctrl.record_failed_attempt();
        ctrl.record_failed_attempt();
        let mut h = harness(Arc::clone(&ctrl));
        h.run();
        assert!(h.query_by_label("2 failed attempts").is_some());
    }

    #[test]
    fn an_expired_window_offers_a_new_code_and_drops_the_old_one() {
        let ctrl = Arc::new(DemoController::empty());
        ctrl.open_pairing_window();
        let mut h = harness(Arc::clone(&ctrl));
        h.run();
        assert!(h.state().st.has_qr());
        ctrl.expire_pairing_window();
        h.run();
        assert!(!h.state().st.has_qr());
        assert!(h.query_by_label_contains("The code expired").is_some());
        h.get_by_label("Show a new code").click();
        h.run();
        h.run();
        assert!(ctrl.pairing().open);
        assert!(h.state().st.has_qr());
    }

    #[test]
    fn close_asks_the_application_to_close_the_window() {
        let ctrl = Arc::new(DemoController::empty());
        ctrl.open_pairing_window();
        let mut h = harness(ctrl);
        h.run();
        h.get_by_label("Close").click();
        h.run();
        assert!(h.state().out.close_add_device);
    }
}
