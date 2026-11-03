// SPDX-License-Identifier: Apache-2.0
//! The Activity page (mockup 43, `hActivity`): the tail of the audit log (security.md principle
//! 8: "Viewable in the tray UI"). The audit log never holds tokens or file contents, so everything
//! in it can be shown.

use gander_core::protocol::DeviceId;

use super::Cx;
use crate::audit::{AuditEvent, Outcome};
use crate::ui::format;
use crate::ui::glyphs::{self, Icon};
use crate::ui::theme::{self, Palette, pal};
use crate::ui::widgets::{self, Col};

/// What the window remembers.
#[derive(Debug, Default)]
pub struct State {
    /// Free-text filter over event, device, address and details.
    pub filter: String,
    /// Hide events whose outcome is plain `ok`.
    pub only_problems: bool,
}

/// Whether `event` passes the window's filters. `device_name` resolves a device id.
pub fn matches(
    event: &AuditEvent,
    filter: &str,
    only_problems: bool,
    device_name: &dyn Fn(&DeviceId) -> Option<String>,
) -> bool {
    if only_problems && event.outcome == Outcome::Ok {
        return false;
    }
    let needle = filter.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    let mut hay = format::event_label(&event.event).to_lowercase();
    hay.push(' ');
    hay.push_str(&event.event.to_lowercase());
    if let Some(id) = &event.device_id {
        hay.push(' ');
        hay.push_str(
            &device_name(id)
                .unwrap_or_else(|| id.to_string())
                .to_lowercase(),
        );
    }
    if let Some(a) = event.source_addr {
        hay.push(' ');
        hay.push_str(&a.to_string());
    }
    hay.push(' ');
    hay.push_str(&format::detail_summary(&event.detail).to_lowercase());
    hay.contains(&needle)
}

/// The icon and its colour for an event (design doc: status is never colour alone).
fn event_icon(p: &Palette, e: &AuditEvent) -> (Icon, egui::Color32) {
    match e.outcome {
        Outcome::Error | Outcome::Refused => return (Icon::Error, p.bad),
        Outcome::Warning => return (Icon::Warn, p.warn),
        Outcome::Ok => {}
    }
    let action = e
        .detail
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match e.event.as_str() {
        k if k.starts_with("stream.") => (Icon::Stream, p.accent),
        "pair.approved" | "pair.collected" => (Icon::OkCircle, p.ok),
        "pair.denied" | "pair.expired" | "pair.abandoned" => (Icon::Warn, p.warn),
        k if k.starts_with("pair.") => (Icon::Phone, p.text_2),
        "device.revoked" | "kill_switch" => (Icon::Power, p.bad),
        k if k.starts_with("listener.") => (Icon::Net, p.text_2),
        "access" if action == "fetch" => (Icon::Fetch, p.text_2),
        "access" if action == "preview" || action == "thumbnail" => (Icon::Preview, p.text_2),
        "access" | "shares.changed" => (Icon::Folder, p.text_2),
        "settings.changed" | "cache.purged" => (Icon::Gear, p.text_2),
        _ => (Icon::Pulse, p.text_2),
    }
}

/// Draws the Activity page.
pub fn show(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>) {
    let p = pal(ui);
    widgets::page_header(
        ui,
        "Activity",
        "Who did what on this computer, newest first. Times are in your time zone.",
        |_| {},
    );
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let mut sel = usize::from(st.only_problems);
        if widgets::segmented(ui, &mut sel, &["All events", "Only problems"]) {
            st.only_problems = sel == 1;
        }
        ui.add(
            widgets::text_field(&mut st.filter)
                .hint_text("Filter by device, file or event")
                .desired_width(f32::INFINITY),
        );
    });
    ui.add_space(8.0);

    let name_of = |id: &DeviceId| {
        cx.data
            .devices
            .iter()
            .find(|d| &d.id == id)
            .map(|d| d.name.clone())
    };
    let rows: Vec<&AuditEvent> = cx
        .data
        .activity
        .iter()
        .rev()
        .filter(|e| matches(e, &st.filter, st.only_problems, &name_of))
        .collect();
    let footer = format!(
        "Showing {} of the last {} events. Paths appear only here, on this computer. Tokens, \
         codes and file contents are never logged.",
        rows.len(),
        cx.data.activity.len()
    );

    if rows.is_empty() {
        widgets::card(ui, |ui| {
            ui.label(theme::text(
                if cx.data.activity.is_empty() {
                    "Nothing has happened yet."
                } else {
                    "No events match."
                },
                theme::BODY,
                p.text_2,
            ));
        });
        widgets::caption(ui, &footer);
        return;
    }

    let cols = [
        Col {
            width: 70.0,
            centred: false,
        },
        Col {
            width: 250.0,
            centred: false,
        },
        Col {
            width: 140.0,
            centred: false,
        },
        Col {
            width: 0.0,
            centred: false,
        },
    ];
    let footer_h = 28.0;
    widgets::card_with(ui, egui::Margin::symmetric(10, 2), |ui| {
        widgets::table_header(ui, &cols, &["Time", "Event", "Device", "Detail"]);
        egui::ScrollArea::vertical()
            .max_height((ui.available_height() - footer_h).max(120.0))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (n, e) in rows.iter().enumerate() {
                    ui.push_id(n, |ui| {
                        widgets::table_row(
                            ui,
                            &cols,
                            30.0,
                            |_| None,
                            |c, ui| {
                                event_cell(ui, p, cx, e, c, &name_of);
                            },
                        );
                    });
                }
            });
    });
    ui.add_space(4.0);
    widgets::caption(ui, &footer);
}

fn event_cell(
    ui: &mut egui::Ui,
    p: &Palette,
    cx: &Cx<'_>,
    e: &AuditEvent,
    column: usize,
    name_of: &dyn Fn(&DeviceId) -> Option<String>,
) {
    match column {
        0 => {
            ui.label(theme::text(
                format::clock(e.ts, cx.data.offset),
                12.5,
                p.text_2,
            ));
        }
        1 => {
            let (icon, color) = event_icon(p, e);
            ui.spacing_mut().item_spacing.x = 6.0;
            glyphs::icon(ui, icon, 14.0, color);
            ui.add(
                egui::Label::new(theme::text(format::event_label(&e.event), 12.5, p.text))
                    .truncate(),
            );
        }
        2 => {
            let who = e
                .device_id
                .as_ref()
                .map(|id| name_of(id).unwrap_or_else(|| id.to_string()));
            match who {
                Some(w) => {
                    ui.add(egui::Label::new(theme::text(w, 12.5, p.text)).truncate());
                }
                None => {
                    ui.label(theme::text("unknown", 12.5, p.text_3));
                }
            }
        }
        _ => {
            let mut detail = format::detail_summary(&e.detail);
            if let Some(a) = e.source_addr {
                if !detail.is_empty() {
                    detail.push_str(" \u{b7} ");
                }
                detail.push_str(&format!("from {a}"));
            }
            ui.add(egui::Label::new(theme::text(detail, 12.5, p.text_2)).truncate());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    use super::*;
    use crate::audit::kind;
    use crate::ui::controller::FolderPicker;
    use crate::ui::demo::{DemoController, PIXEL};
    use crate::ui::model::UiData;
    use crate::ui::pages::Outbox;
    use crate::ui::picker::ScriptedPicker;

    fn no_names(_: &DeviceId) -> Option<String> {
        None
    }

    #[test]
    fn filters_by_text_device_name_and_outcome() {
        let pixel = DeviceId::from(PIXEL);
        let ok = AuditEvent::new(kind::ACCESS)
            .device(&pixel)
            .detail("path", "/Quarterly review.pptx");
        let refused = AuditEvent::new(kind::AUTH_REFUSED).outcome(Outcome::Refused);
        let by_name = |id: &DeviceId| (id == &pixel).then(|| "Pixel 9".to_owned());

        assert!(matches(&ok, "", false, &no_names));
        assert!(matches(&ok, "quarterly", false, &no_names), "detail text");
        assert!(matches(&ok, "pixel 9", false, &by_name), "device name");
        assert!(!matches(&ok, "pixel 9", false, &no_names), "unknown name");
        assert!(matches(&ok, "file access", false, &no_names), "event label");
        assert!(
            matches(&ok, "ACCESS", false, &no_names),
            "case-insensitive kind"
        );
        assert!(!matches(&ok, "nothing like this", false, &no_names));
        assert!(
            !matches(&ok, "", true, &no_names),
            "ok hidden by only-problems"
        );
        assert!(matches(&refused, "", true, &no_names));
    }

    #[test]
    fn the_window_lists_events_newest_first_with_device_names() {
        let ctrl = Arc::new(DemoController::sample());
        let picker: Arc<dyn FolderPicker> = Arc::new(ScriptedPicker::new(Vec::new()));
        let mut h = Harness::new_ui_state(
            |ui, (ctrl, st, out): &mut (Arc<DemoController>, State, Outbox)| {
                let data = UiData::fetch(&**ctrl);
                let mut cx = Cx {
                    data: &data,
                    ctrl: &**ctrl,
                    picker: &picker,
                    out,
                };
                show(ui, st, &mut cx);
            },
            (ctrl, State::default(), Outbox::default()),
        );
        h.set_size(egui::vec2(1000.0, 700.0));
        h.run();
        assert!(h.query_by_label("Device approved").is_some());
        assert!(h.query_by_label("Kill switch used").is_none());
        assert!(
            h.query_by_label("Request refused (no valid token)")
                .is_some()
        );
        assert!(
            h.query_all_by_label("Pixel 9").count() > 0,
            "device id resolved to its name"
        );
        assert!(
            h.query_by_label_contains("Showing 6 of the last 6 events.")
                .is_some()
        );

        h.state_mut().1.only_problems = true;
        h.run();
        assert!(h.query_by_label("Device approved").is_none());
        assert!(
            h.query_by_label("Request refused (no valid token)")
                .is_some()
        );
        assert!(
            h.query_by_label_contains("Showing 2 of the last 6 events.")
                .is_some()
        );
    }

    #[test]
    fn an_empty_log_says_so() {
        let ctrl = Arc::new(DemoController::empty());
        let picker: Arc<dyn FolderPicker> = Arc::new(ScriptedPicker::new(Vec::new()));
        let mut h = Harness::new_ui_state(
            |ui, (ctrl, st, out): &mut (Arc<DemoController>, State, Outbox)| {
                let data = UiData::fetch(&**ctrl);
                let mut cx = Cx {
                    data: &data,
                    ctrl: &**ctrl,
                    picker: &picker,
                    out,
                };
                show(ui, st, &mut cx);
            },
            (ctrl, State::default(), Outbox::default()),
        );
        h.run();
        assert!(h.query_by_label("Nothing has happened yet.").is_some());
    }
}
