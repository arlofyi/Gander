// SPDX-License-Identifier: Apache-2.0
//! The Settings page (mockup 45, `hSettings`): Fluent setting rows for autostart and the theme,
//! the listener port, the stream clipboard setting, and the thumbnail and preview cache.
//!
//! The port and the cache cap are read when Gander starts, so changing them saves the value and
//! says "after restart". The autostart switch goes through the [`crate::ui::autostart`] trait. The
//! theme and the clipboard setting apply at once (a running stream reads the clipboard setting on
//! its next request).

use gander_core::clipboard::ClipboardMode;
use gander_core::config::MIN_CACHE_CAP_MIB;

use super::Cx;
use crate::ui::controller::Applied;
use crate::ui::format;
use crate::ui::glyphs::Icon;
use crate::ui::theme::{self, ThemeChoice, Tone, pal};
use crate::ui::widgets::{self, Bar, Btn, Kind};

/// What the page remembers.
#[derive(Debug, Default)]
pub struct State {
    loaded: bool,
    /// The port box.
    pub port_text: String,
    /// The cache cap box, MiB.
    pub cap_mib: u32,
    /// The last result line.
    pub notice: Option<(Tone, String)>,
    /// Empty cache was clicked once.
    confirm_purge: bool,
}

/// The port in the box, if it is a valid listener port.
pub fn parse_port(text: &str) -> Option<u16> {
    text.trim().parse::<u16>().ok().filter(|p| *p != 0)
}

/// Draws the Settings page.
pub fn show(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>) {
    let s = &cx.data.settings;
    if !st.loaded {
        st.port_text = s.port.to_string();
        st.cap_mib = s.cache_cap_mib;
        st.loaded = true;
    }
    widgets::page_header(ui, "Settings", "", |_| {});
    if let Some(problem) = &s.previews_problem {
        widgets::infobar(
            ui,
            Bar::Bad,
            Icon::Error,
            "Previews and thumbnails are off",
            problem,
        );
    }
    if let Some((tone, text)) = &st.notice {
        let (bar, icon) = match tone {
            Tone::Ok => (Bar::Ok, Icon::OkCircle),
            Tone::Bad => (Bar::Bad, Icon::Error),
            Tone::Warn => (Bar::Warn, Icon::Warn),
            Tone::Info => (Bar::Neutral, Icon::Info),
        };
        widgets::infobar(ui, bar, icon, "", text);
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            general(ui, st, cx);
            listener(ui, st, cx);
            streaming(ui, st, cx);
            storage(ui, st, cx);
            ui.add_space(8.0);
            widgets::caption(ui, &format!("Gander {}", cx.data.settings.version));
        });
}

fn general(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>) {
    let s = &cx.data.settings;
    widgets::section(ui, "General");
    let mut desc = "Nothing is reachable after a restart until you sign in.".to_owned();
    if let Some(note) = &s.autostart.note {
        desc.push(' ');
        desc.push_str(note);
    }
    widgets::setting_row(
        ui,
        Some(Icon::Power),
        "Start Gander when I sign in",
        &desc,
        |ui| {
            let mut on = s.autostart.enabled;
            if widgets::toggle(ui, &mut on, "Start Gander when I sign in").changed() {
                match cx.ctrl.set_autostart(on) {
                    Ok(()) => st.notice = None,
                    Err(e) => st.notice = Some((Tone::Bad, e.to_string())),
                }
            }
        },
    );
    widgets::setting_row(
        ui,
        Some(Icon::Window),
        "Theme",
        "Light, dark, or the same as Windows.",
        |ui| {
            let mut choice = s.theme;
            egui::ComboBox::from_id_salt("theme")
                .selected_text(choice.label())
                .width(170.0)
                .show_ui(ui, |ui| {
                    for c in ThemeChoice::ALL {
                        ui.selectable_value(&mut choice, c, c.label());
                    }
                });
            if choice != s.theme {
                match cx.ctrl.set_theme(choice) {
                    Ok(()) => cx.out.theme = Some(choice),
                    Err(e) => st.notice = Some((Tone::Bad, e.to_string())),
                }
            }
        },
    );
}

fn listener(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>) {
    let s = &cx.data.settings;
    widgets::section(ui, "Listener");
    let parsed = parse_port(&st.port_text);
    let desc = if parsed.is_none() {
        "Enter a port from 1 to 65535"
    } else {
        "Takes effect after Gander restarts. Paired devices remember the port they paired with."
    };
    widgets::setting_row(ui, Some(Icon::Net), "Port", desc, |ui| {
        let can = parsed.is_some_and(|p| p != s.port);
        if ui.add_enabled(can, Btn::new("Save port")).clicked()
            && let Some(p) = parsed
        {
            report(st, cx.ctrl.set_port(p), "Port saved");
        }
        ui.add(
            widgets::text_field(&mut st.port_text)
                .desired_width(70.0)
                .font(egui::TextStyle::Monospace),
        );
    });
}

/// What the clipboard row says under its label for each mode.
fn clipboard_description(mode: ClipboardMode) -> &'static str {
    match mode {
        ClipboardMode::Off => {
            "Off. Text is never copied between the phone and this computer during a stream."
        }
        ClipboardMode::ToPhone => {
            "The phone can copy text from this computer. You are notified each time."
        }
        ClipboardMode::FromPhone => {
            "The phone can put text on this computer's clipboard. You are notified each time."
        }
        ClipboardMode::Both => "Text can go both ways during a stream. You are notified each time.",
    }
}

fn streaming(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>) {
    let s = &cx.data.settings;
    widgets::section(ui, "Streaming");
    widgets::setting_row(
        ui,
        Some(Icon::Stream),
        "Clipboard text",
        &format!(
            "{} Text only, up to 64 KiB, and only while the stream is active and the computer is unlocked.",
            clipboard_description(s.clipboard)
        ),
        |ui| {
            let mut mode = s.clipboard;
            egui::ComboBox::from_id_salt("clipboard")
                .selected_text(mode.label())
                .width(170.0)
                .show_ui(ui, |ui| {
                    for m in ClipboardMode::ALL {
                        ui.selectable_value(&mut mode, m, m.label());
                    }
                });
            if mode != s.clipboard {
                report(
                    st,
                    cx.ctrl.set_clipboard_mode(mode),
                    "Clipboard setting saved",
                );
            }
        },
    );
}

fn storage(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>) {
    let s = &cx.data.settings;
    let p = pal(ui);
    widgets::section(ui, "Storage");
    let used = match s.cache_used {
        Some(used) => format!(
            "Using {} of the {} MiB limit.",
            format::bytes(used),
            s.cache_cap_mib
        ),
        None => "The cache is not available.".to_owned(),
    };
    widgets::setting_row(
        ui,
        Some(Icon::Storage),
        "Thumbnail and preview cache",
        &used,
        |ui| {
            if st.confirm_purge {
                if ui.add(Btn::new("Cancel")).clicked() {
                    st.confirm_purge = false;
                }
                if ui
                    .add(Btn::new("Yes, empty it").kind(Kind::Danger))
                    .clicked()
                {
                    match cx.ctrl.purge_cache() {
                        Ok(freed) => {
                            st.notice = Some((
                                Tone::Ok,
                                format!("Cache emptied, {} freed.", format::bytes(freed)),
                            ));
                        }
                        Err(e) => st.notice = Some((Tone::Bad, e.to_string())),
                    }
                    st.confirm_purge = false;
                }
                ui.label(theme::text(
                    "Delete every cached thumbnail and preview?",
                    theme::CAPTION,
                    p.text_2,
                ));
            } else if ui
                .add_enabled(s.cache_used.is_some(), Btn::new("Empty cache"))
                .clicked()
            {
                st.confirm_purge = true;
            }
        },
    );
    widgets::setting_row(
        ui,
        None,
        "Cache limit",
        "Applies after Gander restarts. Old entries are dropped first.",
        |ui| {
            let can = st.cap_mib != s.cache_cap_mib;
            if ui.add_enabled(can, Btn::new("Save limit")).clicked() {
                report(st, cx.ctrl.set_cache_cap_mib(st.cap_mib), "Limit saved");
            }
            ui.add(
                egui::DragValue::new(&mut st.cap_mib)
                    .range(MIN_CACHE_CAP_MIB..=1_048_576)
                    .speed(32.0)
                    .suffix(" MiB"),
            );
        },
    );
}

fn report(st: &mut State, result: crate::ui::controller::ControlResult<Applied>, ok: &str) {
    st.notice = Some(match result {
        Ok(Applied::Now) => (Tone::Ok, format!("{ok}.")),
        Ok(Applied::AfterRestart) => (
            Tone::Ok,
            format!("{ok}. It takes effect after Gander restarts."),
        ),
        Err(e) => (Tone::Bad, e.to_string()),
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    use super::*;
    use crate::ui::controller::{FolderPicker, HostController as _};
    use crate::ui::demo::{Call, DemoController};
    use crate::ui::model::UiData;
    use crate::ui::pages::Outbox;
    use crate::ui::picker::ScriptedPicker;

    type Fx = (Arc<DemoController>, State, Outbox);

    fn harness(ctrl: Arc<DemoController>) -> Harness<'static, Fx> {
        let picker: Arc<dyn FolderPicker> = Arc::new(ScriptedPicker::new(Vec::new()));
        let mut h = Harness::new_ui_state(
            move |ui, (ctrl, st, out): &mut Fx| {
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
        h.set_size(egui::vec2(700.0, 800.0));
        h
    }

    #[test]
    fn ports_parse_strictly() {
        assert_eq!(parse_port("42637"), Some(42637));
        assert_eq!(parse_port(" 8080 "), Some(8080));
        assert_eq!(parse_port("0"), None);
        assert_eq!(parse_port("65536"), None);
        assert_eq!(parse_port("-1"), None);
        assert_eq!(parse_port("http"), None);
        assert_eq!(parse_port(""), None);
    }

    #[test]
    fn shows_cache_use_and_the_autostart_caveat() {
        let mut h = harness(Arc::new(DemoController::sample()));
        h.run();
        assert!(
            h.query_by_label("Using 412 MiB of the 2048 MiB limit.")
                .is_some()
        );
        assert!(
            h.query_by_label_contains("This switch is not saved.")
                .is_some()
        );
        assert!(h.query_by_label("Gander 0.0.0").is_some());
    }

    #[test]
    fn a_changed_port_is_saved_and_reported_as_needing_a_restart() {
        let ctrl = Arc::new(DemoController::empty());
        let mut h = harness(Arc::clone(&ctrl));
        h.run();
        assert!(h.query_by_label("Save port").is_some());
        h.state_mut().1.port_text = "50000".into();
        h.run();
        h.get_by_label("Save port").click();
        h.run();
        assert_eq!(ctrl.calls(), vec![Call::SetPort(50000)]);
        assert!(
            h.query_by_label_contains("takes effect after Gander restarts")
                .is_some()
        );
    }

    #[test]
    fn a_bad_port_is_refused_before_the_controller_hears_of_it() {
        let ctrl = Arc::new(DemoController::empty());
        let mut h = harness(Arc::clone(&ctrl));
        h.run();
        h.state_mut().1.port_text = "99999".into();
        h.run();
        assert!(h.query_by_label("Enter a port from 1 to 65535").is_some());
        assert!(ctrl.calls().is_empty());
    }

    #[test]
    fn purge_asks_first_then_reports_what_it_freed() {
        let ctrl = Arc::new(DemoController::sample());
        let mut h = harness(Arc::clone(&ctrl));
        h.run();
        h.get_by_label("Empty cache").click();
        h.run();
        assert!(ctrl.calls().is_empty(), "the first click only asks");
        h.get_by_label("Yes, empty it").click();
        h.run();
        assert_eq!(ctrl.calls(), vec![Call::PurgeCache]);
        assert!(
            h.query_by_label_contains("Cache emptied, 412 MiB freed.")
                .is_some()
        );
        assert_eq!(ctrl.settings().cache_used, Some(0));
    }

    #[test]
    fn the_autostart_switch_goes_through_the_controller() {
        let ctrl = Arc::new(DemoController::empty());
        let mut h = harness(Arc::clone(&ctrl));
        h.run();
        h.get_by_role_and_label(
            egui::accesskit::Role::CheckBox,
            "Start Gander when I sign in",
        )
        .click();
        h.run();
        assert_eq!(ctrl.calls(), vec![Call::SetAutostart(true)]);
        assert!(ctrl.settings().autostart.enabled);
    }

    #[test]
    fn the_theme_choice_is_saved_and_applied() {
        let ctrl = Arc::new(DemoController::empty());
        let mut h = harness(Arc::clone(&ctrl));
        h.run();
        // A combo box shows its choice as its value; there are two on the page.
        h.get_by_value("Use system setting").click();
        h.run();
        h.get_by_label("Dark").click();
        h.run();
        assert_eq!(ctrl.calls(), vec![Call::SetTheme(ThemeChoice::Dark)]);
        assert_eq!(h.state().2.theme, Some(ThemeChoice::Dark));
    }

    #[test]
    fn clipboard_sharing_is_off_until_the_owner_picks_a_direction() {
        let ctrl = Arc::new(DemoController::empty());
        let mut h = harness(Arc::clone(&ctrl));
        h.run();
        assert!(h.query_by_label("Streaming").is_some());
        assert!(h.query_by_label("Clipboard text").is_some());
        assert!(
            h.query_by_label_contains("Text is never copied between the phone")
                .is_some()
        );
        assert_eq!(ctrl.settings().clipboard, ClipboardMode::Off);
        assert!(ctrl.calls().is_empty(), "showing the page changes nothing");
    }

    #[test]
    fn the_clipboard_choice_is_saved_and_applied_at_once() {
        let ctrl = Arc::new(DemoController::empty());
        let mut h = harness(Arc::clone(&ctrl));
        h.run();
        h.get_by_value("Off").click();
        h.run();
        h.get_by_label("Computer to phone").click();
        h.run();
        assert_eq!(
            ctrl.calls(),
            vec![Call::SetClipboard(ClipboardMode::ToPhone)]
        );
        assert_eq!(ctrl.settings().clipboard, ClipboardMode::ToPhone);
        // "Now", not "after restart".
        assert!(
            h.query_by_label_contains("Clipboard setting saved.")
                .is_some()
        );
        assert!(
            h.query_by_label_contains("It takes effect after Gander restarts")
                .is_none()
        );
        // The row says what the new mode allows.
        assert!(
            h.query_by_label_contains("The phone can copy text from this computer")
                .is_some()
        );
    }

    #[test]
    fn a_changed_cache_limit_is_saved() {
        let ctrl = Arc::new(DemoController::empty());
        let mut h = harness(Arc::clone(&ctrl));
        h.run();
        h.state_mut().1.cap_mib = 4096;
        h.run();
        h.get_by_label("Save limit").click();
        h.run();
        assert_eq!(ctrl.calls(), vec![Call::SetCacheCap(4096)]);
    }
}
