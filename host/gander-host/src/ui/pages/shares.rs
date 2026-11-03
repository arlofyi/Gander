// SPDX-License-Identifier: Apache-2.0
//! The Shares page (mockup 38, `sharesPage`): a master-detail view. On the left the shared
//! folders; on the right the selected share's name, folder, options, the per-device permission
//! grid and the Stream caution. "Share a folder" picks a folder and opens the Share a folder
//! dialog (mockup 39, `hSharesAdd`), where every device starts unticked and Stream is never set.
//!
//! Edits on the page are drafts until Save (see [`crate::ui::share_editor`]); nothing is shared
//! until a folder is added AND a device is granted something (deny by default). The dialog's
//! "Share folder" button commits like Save.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use egui::{Align, Layout, Margin, Sense, Stroke, Vec2};
use gander_core::protocol::DeviceId;
use gander_core::protocol::files::{Permission, StreamDecision};

use super::Cx;
use crate::ui::glyphs::{self, Icon};
use crate::ui::share_editor::{PERMISSIONS, ShareEditor};
use crate::ui::theme::{self, pal};
use crate::ui::widgets::{self, Bar, Btn, Col, Kind};

/// What a folder pick is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickTarget {
    /// A new share (opens the dialog).
    New,
    /// Another folder in the open dialog.
    Dialog,
    /// Another folder for the share at this index.
    Change(usize),
}

struct PendingPick {
    target: PickTarget,
    rx: Receiver<Option<PathBuf>>,
}

/// The Share a folder dialog.
#[derive(Debug, Clone)]
pub struct NewShare {
    /// The folder.
    pub root: PathBuf,
    /// The name devices see.
    pub name: String,
    /// Which devices may list, preview and fetch (all unticked at first).
    pub allowed: Vec<(DeviceId, String, bool)>,
}

/// What the page remembers.
#[derive(Default)]
pub struct State {
    /// The drafts.
    pub editor: ShareEditor,
    /// Whether the editor has been filled from the host yet.
    loaded: bool,
    /// The last error (validation or the host's refusal).
    pub error: Option<String>,
    /// Shown after a successful save.
    pub saved: bool,
    /// The share shown on the right.
    pub selected: usize,
    /// The share whose Stop sharing button was clicked once.
    confirm_remove: Option<usize>,
    /// The hidden-patterns box is open.
    editing_patterns: bool,
    /// The Share a folder dialog, while open.
    pub dialog: Option<NewShare>,
    pick: Option<PendingPick>,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State")
            .field("editor", &self.editor)
            .field("error", &self.error)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

/// Draws the Shares page.
pub fn show(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>) {
    if st.loaded {
        st.editor.sync(cx.data.shares.clone());
    } else {
        st.editor = ShareEditor::new(cx.data.shares.clone());
        st.loaded = true;
    }
    poll_pick(ui.ctx(), st, cx);
    let n = st.editor.drafts().len();
    if n > 0 && st.selected >= n {
        st.selected = n - 1;
    }

    let picking = st.pick.is_some();
    widgets::page_header(
        ui,
        "Shares",
        "Nothing is reachable until you share a folder with a device. Always read-only.",
        |ui| {
            let b = Btn::new("Share a folder")
                .kind(Kind::Primary)
                .icon(Icon::Plus);
            if ui.add_enabled(!picking, b).clicked() {
                start_pick(ui.ctx(), cx, st, PickTarget::New);
            }
        },
    );
    save_bar(ui, st, cx);

    if st.editor.drafts().is_empty() {
        widgets::card(ui, |ui| {
            let p = pal(ui);
            ui.label(theme::semi(
                ui,
                "Nothing is reachable yet",
                theme::BODY_STRONG,
                p.text,
            ));
            ui.label(theme::text(
                "No folders are shared. Click Share a folder to choose one; nobody can use it \
                 until you tick a device.",
                theme::BODY,
                p.text_2,
            ));
        });
    } else {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            ui.allocate_ui_with_layout(
                Vec2::new(220.0, ui.available_height()),
                Layout::top_down(Align::Min),
                |ui| share_list(ui, st, cx),
            );
            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_width(), ui.available_height()),
                Layout::top_down(Align::Min),
                |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| detail(ui, st, cx));
                },
            );
        });
    }
    dialog(ui, st, cx);
}

fn save_bar(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>) {
    if let Some(e) = st.error.clone() {
        widgets::infobar(ui, Bar::Bad, Icon::Error, "Not saved", &e);
    }
    if st.editor.is_dirty() {
        widgets::infobar_with(
            ui,
            Bar::Warn,
            Icon::Warn,
            "Unsaved changes",
            "Devices keep the old settings until you save.",
            |ui| {
                if ui.add(Btn::new("Revert")).clicked() {
                    st.editor.revert();
                    st.error = None;
                    st.confirm_remove = None;
                }
                if ui.add(Btn::new("Save").kind(Kind::Primary)).clicked() {
                    save(st, cx);
                }
            },
        );
    } else if st.saved {
        widgets::infobar(
            ui,
            Bar::Ok,
            Icon::OkCircle,
            "",
            "Saved. Devices see the change now.",
        );
    }
}

/// "2 devices · Stream for 1".
fn summary(st: &State, i: usize) -> String {
    let Some(d) = st.editor.drafts().get(i) else {
        return String::new();
    };
    let devices = d.grants.len();
    let stream = st.editor.stream_grants(i);
    let mut s = match devices {
        0 => "No devices".to_owned(),
        1 => "1 device".to_owned(),
        n => format!("{n} devices"),
    };
    if stream > 0 {
        s.push_str(&format!(" \u{b7} Stream for {stream}"));
    }
    s
}

fn share_list(ui: &mut egui::Ui, st: &mut State, _cx: &mut Cx<'_>) {
    let p = pal(ui);
    ui.spacing_mut().item_spacing.y = 4.0;
    for i in 0..st.editor.drafts().len() {
        let sel = st.selected == i;
        let (name, root) = {
            let d = &st.editor.drafts()[i];
            (d.name.clone(), display_path(&d.root))
        };
        let chips = summary(st, i);
        let frame = egui::Frame::new()
            .fill(if sel {
                p.layer
            } else {
                egui::Color32::TRANSPARENT
            })
            .stroke(Stroke::new(
                1.0,
                if sel {
                    p.stroke
                } else {
                    egui::Color32::TRANSPARENT
                },
            ))
            .corner_radius(6)
            .inner_margin(Margin::symmetric(12, 9));
        let r = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                glyphs::icon(ui, Icon::Folder, 17.0, p.text_2);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.add(
                        egui::Label::new(theme::semi(ui, &name, theme::BODY, p.text)).truncate(),
                    );
                    ui.add(egui::Label::new(theme::mono(&root, 11.5, p.text_3)).truncate());
                    ui.label(theme::text(&chips, 11.5, p.text_2));
                });
            });
        });
        let rect = r.response.rect;
        if sel {
            let bar = egui::Rect::from_min_size(
                rect.left_top() + Vec2::new(0.0, 10.0),
                Vec2::new(3.0, rect.height() - 20.0),
            );
            ui.painter().rect_filled(bar, 2, p.accent);
        }
        let resp = ui.interact(rect, ui.id().with(("share-item", i)), Sense::click());
        resp.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                true,
                sel,
                format!("Share {name}"),
            )
        });
        if resp.hovered() && !sel {
            ui.painter()
                .rect_filled(rect, 6, p.text.gamma_multiply(0.04));
        }
        if resp.clicked() {
            st.selected = i;
            st.confirm_remove = None;
            st.editing_patterns = false;
        }
    }
}

fn detail(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>) {
    let i = st.selected;
    let p = pal(ui);
    let picking = st.pick.is_some();
    ui.push_id(
        ("share", st.editor.drafts()[i].id.as_str().to_owned()),
        |ui| {
            widgets::card(ui, |ui| {
                ui.horizontal_top(|ui| {
                    let w = (ui.available_width() - 100.0).max(200.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(w * 0.42, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            widgets::field_label(ui, "Name devices see");
                            if let Some(d) = st.editor.draft_mut(i) {
                                ui.add(
                                    widgets::text_field(&mut d.name).desired_width(f32::INFINITY),
                                );
                            }
                        },
                    );
                    ui.allocate_ui_with_layout(
                        Vec2::new(w * 0.58, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            widgets::field_label(ui, "Folder on this computer");
                            let root = display_path(&st.editor.drafts()[i].root);
                            widgets::value_box(ui, theme::mono(root, 12.0, p.text));
                        },
                    );
                    ui.vertical(|ui| {
                        ui.add_space(17.0);
                        if ui
                            .add_enabled(!picking, Btn::new("Change\u{2026}"))
                            .clicked()
                        {
                            start_pick(ui.ctx(), cx, st, PickTarget::Change(i));
                        }
                    });
                });
            });

            options(ui, st, i);

            widgets::section(ui, "Who can use this share");
            permissions_grid(ui, st, cx, i);

            if st.editor.stream_grants(i) > 0 {
                let names = stream_names(st, cx, i);
                widgets::infobar(
                    ui,
                    Bar::Warn,
                    Icon::Warn,
                    &format!("Stream is on for {names}."),
                    "It opens files in apps here, and an app's own Open and Save dialogs can reach \
                 folders outside this share. Only for devices you trust.",
                );
            } else {
                widgets::caption(
                    ui,
                    "Stream opens files in apps on this computer and shows the app window on the \
                 device. Turn it on only for devices you trust.",
                );
            }

            ui.add_space(8.0);
            if st.confirm_remove == Some(i) {
                widgets::infobar_with(
                    ui,
                    Bar::Bad,
                    Icon::Warn,
                    "Stop sharing this folder?",
                    "Devices lose access when you save.",
                    |ui| {
                        if ui.add(Btn::new("Cancel")).clicked() {
                            st.confirm_remove = None;
                        }
                        if ui
                            .add(Btn::new("Yes, stop sharing").kind(Kind::Danger))
                            .clicked()
                        {
                            st.editor.remove(i);
                            st.confirm_remove = None;
                            st.saved = false;
                        }
                    },
                );
            } else if ui
                .add(Btn::new("Stop sharing this folder").kind(Kind::DangerOutline))
                .clicked()
            {
                st.confirm_remove = Some(i);
            }
        },
    );
}

fn options(ui: &mut egui::Ui, st: &mut State, i: usize) {
    let p = pal(ui);
    widgets::card_with(ui, Margin::symmetric(14, 6), |ui| {
        let row =
            |ui: &mut egui::Ui, title: &str, note: &str, control: &mut dyn FnMut(&mut egui::Ui)| {
                ui.horizontal(|ui| {
                    ui.set_min_height(32.0);
                    ui.label(theme::text(title, theme::BODY, p.text));
                    if !note.is_empty() {
                        ui.label(theme::text(note, theme::CAPTION, p.text_3));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| control(ui));
                });
            };
        if let Some(d) = st.editor.draft_mut(i) {
            row(ui, "Show hidden and system files", "", &mut |ui| {
                widgets::toggle(ui, &mut d.show_hidden, "Show hidden and system files");
            });
            row(
                ui,
                "Tell devices the folder path",
                "\u{b7} otherwise they only see the name",
                &mut |ui| {
                    widgets::toggle(ui, &mut d.show_path, "Tell devices the folder path");
                },
            );
            row(ui, "Stream an app that is not on the list", "", &mut |ui| {
                egui::ComboBox::from_id_salt("stream_default")
                    .selected_text(stream_text(d.stream_default))
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for v in [
                            StreamDecision::Ask,
                            StreamDecision::Allowed,
                            StreamDecision::Refused,
                        ] {
                            ui.selectable_value(&mut d.stream_default, v, stream_text(v));
                        }
                    });
            });
        }
        let patterns = st.editor.drafts()[i].hidden_patterns.join(" \u{b7} ");
        let editing = st.editing_patterns;
        let mut toggle_edit = false;
        row(ui, "Hidden patterns", "", &mut |ui| {
            let label = if editing { "Done" } else { "Edit" };
            if ui
                .add(
                    Btn::new(label)
                        .kind(Kind::Subtle)
                        .height(26.0)
                        .name("Edit hidden patterns"),
                )
                .clicked()
            {
                toggle_edit = true;
            }
            let shown = if patterns.is_empty() {
                "none".to_owned()
            } else {
                patterns.clone()
            };
            ui.add(egui::Label::new(theme::mono(shown, 11.5, p.text_3)).truncate());
        });
        if toggle_edit {
            st.editing_patterns = !st.editing_patterns;
        }
        if st.editing_patterns {
            widgets::caption(
                ui,
                "One pattern per line, matched against the path inside the share.",
            );
            let mut changed = false;
            if let Some(text) = st.editor.pattern_text_mut(i) {
                changed = ui
                    .add(
                        egui::TextEdit::multiline(text)
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .hint_text("**/.git"),
                    )
                    .changed();
            }
            if changed {
                st.editor.commit_patterns(i);
            }
            ui.add_space(4.0);
        }
    });
}

fn active_devices(cx: &Cx<'_>) -> Vec<(DeviceId, String, Icon)> {
    cx.data
        .devices
        .iter()
        .filter(|d| d.revoked_at.is_none())
        .map(|d| (d.id.clone(), d.name.clone(), device_icon(&d.name, &d.model)))
        .collect()
}

/// A tablet icon for tablets, a phone icon otherwise (from the name and model).
pub fn device_icon(name: &str, model: &str) -> Icon {
    let s = format!("{name} {model}").to_lowercase();
    if s.contains("tab") || s.contains("pad") || s.contains("sm-x") {
        Icon::Tablet
    } else {
        Icon::Phone
    }
}

fn stream_names(st: &State, cx: &Cx<'_>, i: usize) -> String {
    let names: Vec<String> = active_devices(cx)
        .into_iter()
        .filter(|(id, _, _)| st.editor.has(i, id, Permission::Stream))
        .map(|(_, n, _)| n)
        .collect();
    if names.is_empty() {
        "a device".to_owned()
    } else {
        names.join(", ")
    }
}

fn permissions_grid(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>, i: usize) {
    let p = pal(ui);
    let devices = active_devices(cx);
    if devices.is_empty() {
        widgets::infobar(
            ui,
            Bar::Neutral,
            Icon::Info,
            "",
            "No devices are paired yet. Add a device first, then allow it here.",
        );
        return;
    }
    widgets::card_with(ui, Margin::symmetric(12, 4), |ui| {
        let cols = [
            Col {
                width: (ui.available_width() - 4.0 * 78.0).max(160.0),
                centred: false,
            },
            Col {
                width: 78.0,
                centred: true,
            },
            Col {
                width: 78.0,
                centred: true,
            },
            Col {
                width: 78.0,
                centred: true,
            },
            Col {
                width: 78.0,
                centred: true,
            },
        ];
        widgets::table_header(ui, &cols, &["Device", "List", "Preview", "Fetch", "Stream"]);
        for (id, name, icon) in &devices {
            let stream_on = st.editor.has(i, id, Permission::Stream);
            ui.push_id(id.as_str().to_owned(), |ui| {
                widgets::table_row(
                    ui,
                    &cols,
                    34.0,
                    |c| (c == 4 && stream_on).then_some(p.accent_container),
                    |c, ui| {
                        if c == 0 {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            glyphs::icon(ui, *icon, 14.0, p.text_2);
                            ui.add(egui::Label::new(theme::text(name, 12.5, p.text)).truncate());
                            return;
                        }
                        let perm = PERMISSIONS[c - 1];
                        let mut on = st.editor.has(i, id, perm);
                        let label = format!("{} for {}", permission_name(perm), name);
                        if widgets::check(ui, &mut on, &label)
                            .on_hover_text(permission_help(perm))
                            .changed()
                        {
                            st.editor.set_permission(i, id, perm, on);
                            st.saved = false;
                        }
                    },
                );
            });
        }
    });
}

fn permission_name(p: Permission) -> &'static str {
    match p {
        Permission::List => "List",
        Permission::Preview => "Preview",
        Permission::Fetch => "Fetch",
        Permission::Stream => "Stream",
    }
}

fn permission_help(p: Permission) -> &'static str {
    match p {
        Permission::List => "Browse folders and see file names",
        Permission::Preview => "See thumbnails and previews drawn by this computer",
        Permission::Fetch => "Download the file to the device",
        Permission::Stream => "Open the file in an app on this computer and watch it on the device",
    }
}

fn stream_text(v: StreamDecision) -> &'static str {
    match v {
        StreamDecision::Ask => "Ask me",
        StreamDecision::Allowed => "Allow",
        StreamDecision::Refused => "Refuse",
    }
}

/// A path as the owner reads it (forward slashes, as in the mockups).
fn display_path(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// Whether `root` is a whole drive or the owner's profile folder (the dialog warns).
pub fn is_broad(root: &Path) -> bool {
    if root.parent().is_none() || root.components().count() <= 1 {
        return true;
    }
    std::env::var_os("USERPROFILE").is_some_and(|home| Path::new(&home) == root)
}

fn dialog(ui: &mut egui::Ui, st: &mut State, cx: &mut Cx<'_>) {
    if st.dialog.is_none() {
        return;
    }
    let p = pal(ui);
    let picking = st.pick.is_some();
    let mut close = false;
    let mut commit = false;
    let mut browse = false;
    let frame = egui::Frame::new()
        .fill(p.layer)
        .stroke(Stroke::new(1.0, p.stroke_2))
        .corner_radius(8)
        .shadow(ui.visuals().window_shadow);
    let modal = egui::Modal::new(egui::Id::new("share-a-folder"))
        .frame(frame)
        .backdrop_color(egui::Color32::from_black_alpha(72))
        .show(ui.ctx(), |ui| {
            ui.set_width(500.0);
            let Some(d) = st.dialog.as_mut() else {
                return;
            };
            egui::Frame::new()
                .inner_margin(Margin {
                    left: 24,
                    right: 24,
                    top: 22,
                    bottom: 18,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(theme::semi(ui, "Share a folder", theme::SUBTITLE, p.text));
                    ui.add_space(8.0);
                    widgets::field_label(ui, "Folder");
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add_enabled(!picking, Btn::new("Browse\u{2026}"))
                                .clicked()
                            {
                                browse = true;
                            }
                            widgets::value_box(
                                ui,
                                theme::mono(display_path(&d.root), 12.0, p.text),
                            );
                        });
                    });
                    if is_broad(&d.root) {
                        ui.add_space(6.0);
                        widgets::infobar(
                            ui,
                            Bar::Warn,
                            Icon::Warn,
                            "This shares a whole drive or your profile folder.",
                            "Devices you tick can browse everything in it. A smaller folder is \
                             safer.",
                        );
                    }
                    ui.add_space(8.0);
                    widgets::field_label(ui, "Name devices see");
                    ui.add(widgets::text_field(&mut d.name).desired_width(f32::INFINITY));
                    ui.add_space(10.0);
                    widgets::field_label(ui, "Who can use it");
                    if d.allowed.is_empty() {
                        widgets::caption(ui, "No devices are paired yet.");
                    }
                    for (_, name, on) in &mut d.allowed {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            let label = format!("Allow {name}");
                            widgets::check(ui, on, &label);
                            ui.label(theme::text(name.as_str(), theme::BODY, p.text));
                            ui.label(theme::text(
                                "\u{b7} list, preview, fetch",
                                theme::BODY,
                                p.text_3,
                            ));
                        });
                    }
                    ui.add_space(10.0);
                    widgets::infobar(
                        ui,
                        Bar::Neutral,
                        Icon::Lock,
                        "",
                        "Nobody can use this folder until you tick a device. Access is \
                         read-only. Stream stays off; turn it on per device afterwards.",
                    );
                });
            egui::Frame::new()
                .fill(p.mica)
                .inner_margin(Margin::symmetric(24, 16))
                .corner_radius(egui::CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 8,
                    se: 8,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.columns(2, |cols| {
                        if cols[0]
                            .add(Btn::new("Share folder").kind(Kind::Primary).full_width())
                            .clicked()
                        {
                            commit = true;
                        }
                        if cols[1].add(Btn::new("Cancel").full_width()).clicked() {
                            close = true;
                        }
                    });
                });
        });
    if modal.should_close() {
        close = true;
    }
    if browse {
        start_pick(ui.ctx(), cx, st, PickTarget::Dialog);
    }
    if commit && let Some(d) = st.dialog.take() {
        let i = st.editor.add_share(d.root);
        if let Some(draft) = st.editor.draft_mut(i)
            && !d.name.trim().is_empty()
        {
            draft.name = d.name.trim().to_owned();
        }
        for (id, _, on) in &d.allowed {
            if *on {
                for perm in [Permission::List, Permission::Preview, Permission::Fetch] {
                    st.editor.set_permission(i, id, perm, true);
                }
            }
        }
        st.selected = i;
        save(st, cx);
    } else if close {
        st.dialog = None;
    }
}

fn save(st: &mut State, cx: &mut Cx<'_>) {
    st.saved = false;
    if let Err(e) = st.editor.validate() {
        st.error = Some(e);
        return;
    }
    match cx.ctrl.apply_shares(st.editor.to_save()) {
        Ok(()) => {
            st.editor.mark_saved();
            st.error = None;
            st.saved = true;
        }
        Err(e) => st.error = Some(e.to_string()),
    }
}

fn start_pick(ctx: &egui::Context, cx: &Cx<'_>, st: &mut State, target: PickTarget) {
    let (tx, rx) = mpsc::channel();
    let picker = std::sync::Arc::clone(cx.picker);
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(picker.pick_folder());
        ctx.request_repaint();
    });
    st.pick = Some(PendingPick { target, rx });
}

/// Opens the dialog for `root` with every device unticked.
pub fn open_dialog(st: &mut State, cx: &Cx<'_>, root: PathBuf) {
    st.dialog = Some(NewShare {
        name: crate::ui::share_editor::default_name(&root),
        root,
        allowed: active_devices(cx)
            .into_iter()
            .map(|(id, name, _)| (id, name, false))
            .collect(),
    });
}

fn poll_pick(ctx: &egui::Context, st: &mut State, cx: &Cx<'_>) {
    let Some(p) = &st.pick else {
        return;
    };
    match p.rx.try_recv() {
        Ok(answer) => {
            let target = p.target;
            st.pick = None;
            if let Some(path) = answer {
                st.saved = false;
                match target {
                    PickTarget::New => open_dialog(st, cx, path),
                    PickTarget::Dialog => {
                        if let Some(d) = st.dialog.as_mut() {
                            d.name = crate::ui::share_editor::default_name(&path);
                            d.root = path;
                        }
                    }
                    PickTarget::Change(i) => st.editor.set_root(i, path),
                }
            }
        }
        Err(TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(150)),
        Err(TryRecvError::Disconnected) => st.pick = None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use egui::accesskit::Role;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    use super::*;
    use crate::ui::controller::{FolderPicker, HostController as _};
    use crate::ui::demo::{Call, DemoController, PIXEL};
    use crate::ui::model::UiData;
    use crate::ui::pages::Outbox;
    use crate::ui::picker::ScriptedPicker;

    struct Fixture {
        ctrl: Arc<DemoController>,
        picker: Arc<dyn FolderPicker>,
        st: State,
        out: Outbox,
    }

    fn harness(
        ctrl: Arc<DemoController>,
        answers: Vec<Option<PathBuf>>,
    ) -> Harness<'static, Fixture> {
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
                picker: Arc::new(ScriptedPicker::new(answers)),
                st: State::default(),
                out: Outbox::default(),
            },
        );
        h.set_size(egui::vec2(1000.0, 1100.0));
        h
    }

    fn folder() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from("E:\\Media\\Music")
        } else {
            PathBuf::from("/media/music")
        }
    }

    /// Runs frames until the folder dialog thread has answered.
    fn settle(h: &mut Harness<'_, Fixture>) {
        for _ in 0..40 {
            // The page keeps asking for repaints while the dialog thread works.
            h.run_steps(2);
            if h.state().st.pick.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        h.run_steps(2);
    }

    #[test]
    fn shows_the_share_list_the_grid_and_the_stream_warning() {
        let mut h = harness(Arc::new(DemoController::sample()), Vec::new());
        h.run();
        for name in ["Projects", "Documents", "Photos"] {
            assert!(
                h.query_by_label(&format!("Share {name}")).is_some(),
                "{name}"
            );
        }
        assert!(h.query_by_label("Fetch for Pixel 9").is_some());
        assert!(h.query_by_label("Stream for Galaxy Tab S9").is_some());
        // Documents grants Stream to the Pixel.
        h.get_by_label("Share Documents").click();
        h.run();
        assert!(
            h.query_by_label_contains("Stream is on for Pixel 9.")
                .is_some()
        );
    }

    #[test]
    fn share_a_folder_opens_the_dialog_with_nobody_allowed_and_commits() {
        let ctrl = Arc::new(DemoController::empty());
        let mut h = harness(Arc::clone(&ctrl), vec![Some(folder())]);
        h.run();
        assert!(h.query_by_label("Nothing is reachable yet").is_some());
        h.get_by_label("Share a folder").click();
        settle(&mut h);
        let d = h.state().st.dialog.clone().expect("the dialog is open");
        assert_eq!(d.name, "Music");
        assert!(d.allowed.iter().all(|(_, _, on)| !on));
        h.get_by_label("Share folder").click();
        h.run();
        assert_eq!(ctrl.calls(), vec![Call::ApplyShares(1)]);
        assert_eq!(ctrl.shares().len(), 1);
        assert!(ctrl.shares()[0].grants.is_empty(), "deny by default");
        assert!(h.state().st.saved);
        assert!(h.state().st.dialog.is_none());
    }

    #[test]
    fn ticked_devices_get_list_preview_and_fetch_but_never_stream() {
        let ctrl = Arc::new(DemoController::sample());
        let mut h = harness(Arc::clone(&ctrl), vec![Some(folder())]);
        h.run();
        h.get_by_label("Share a folder").click();
        settle(&mut h);
        h.get_by_label("Allow Pixel 9").click();
        h.run();
        h.get_by_label("Share folder").click();
        h.run();
        let shares = ctrl.shares();
        let new = shares.last().unwrap();
        let pixel = DeviceId::from(PIXEL);
        assert_eq!(
            new.permissions_for(&pixel),
            &[Permission::List, Permission::Preview, Permission::Fetch]
        );
    }

    #[test]
    fn a_cancelled_pick_changes_nothing() {
        let ctrl = Arc::new(DemoController::empty());
        let mut h = harness(ctrl, vec![None]);
        h.run();
        h.get_by_label("Share a folder").click();
        settle(&mut h);
        assert!(h.state().st.dialog.is_none());
        assert!(h.state().st.editor.drafts().is_empty());
        assert!(!h.state().st.editor.is_dirty());
    }

    #[test]
    fn a_refused_save_keeps_the_drafts_and_shows_why() {
        let ctrl = Arc::new(DemoController::empty());
        ctrl.fail_next_apply("config.toml could not be written");
        let mut h = harness(Arc::clone(&ctrl), vec![Some(folder())]);
        h.run();
        h.get_by_label("Share a folder").click();
        settle(&mut h);
        h.get_by_label("Share folder").click();
        h.run();
        assert!(h.state().st.editor.is_dirty(), "drafts survive");
        assert!(h.query_by_label_contains("could not be written").is_some());
        assert!(ctrl.shares().is_empty());
    }

    #[test]
    fn validation_errors_never_reach_the_controller() {
        let ctrl = Arc::new(DemoController::sample());
        let mut h = harness(Arc::clone(&ctrl), Vec::new());
        h.run();
        h.state_mut().st.editor.draft_mut(0).unwrap().name = "  ".into();
        h.run();
        h.get_by_label("Save").click();
        h.run();
        assert!(ctrl.calls().is_empty());
        assert!(h.query_by_label_contains("empty name").is_some());
    }

    #[test]
    fn ticking_a_permission_edits_the_draft_for_that_device() {
        let ctrl = Arc::new(DemoController::sample());
        let mut h = harness(ctrl, Vec::new());
        h.run();
        h.get_by_label("Share Photos").click();
        h.run();
        assert!(h.state().st.editor.drafts()[2].grants.is_empty());
        h.get_by_role_and_label(Role::CheckBox, "List for Pixel 9")
            .click();
        h.run();
        assert!(
            h.state()
                .st
                .editor
                .has(2, &DeviceId::from(PIXEL), Permission::List)
        );
        assert!(h.state().st.editor.is_dirty());
        assert!(h.query_by_label("Unsaved changes").is_some());
    }

    #[test]
    fn stop_sharing_asks_first() {
        let ctrl = Arc::new(DemoController::sample());
        let mut h = harness(ctrl, Vec::new());
        h.run();
        h.get_by_label("Stop sharing this folder").click();
        h.run();
        assert_eq!(h.state().st.editor.drafts().len(), 3, "not yet");
        h.get_by_label("Yes, stop sharing").click();
        h.run();
        assert_eq!(h.state().st.editor.drafts().len(), 2);
        assert!(h.state().st.editor.is_dirty());
    }

    #[test]
    fn whole_drives_are_broad() {
        if cfg!(windows) {
            assert!(is_broad(Path::new("D:\\")));
            assert!(!is_broad(Path::new("D:\\Projects")));
        } else {
            assert!(is_broad(Path::new("/")));
        }
    }
}
