// SPDX-License-Identifier: Apache-2.0
//! The Fluent-style components of the redesign (design doc section 8, Windows column): buttons,
//! toggle switch, checkbox, status line, cards, setting rows, InfoBars, the page header, a simple
//! table and a segmented control. Every interactive one carries an accessible name, so Narrator and
//! the kittest tests find it by label.

use egui::{
    Align, Color32, CornerRadius, Layout, Margin, Response, RichText, Sense, Stroke, StrokeKind,
    Ui, Vec2, Widget, WidgetInfo, WidgetText, WidgetType,
};

use super::glyphs::{self, Icon};
use super::theme::{self, Palette, pal};

/// What a button looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Layer fill, control border.
    Standard,
    /// Ink fill (the command of a screen).
    Primary,
    /// Red fill (Disconnect everyone in the tray flyout).
    Danger,
    /// Red outline (Revoke, Disconnect everyone in the nav).
    DangerOutline,
    /// No fill, no border.
    Subtle,
    /// White on an orange band (Stop stream).
    OnAccent,
}

/// A button. `Btn::new("Save").kind(Kind::Primary).icon(Icon::Plus)`.
#[derive(Debug, Clone)]
pub struct Btn {
    text: String,
    icon: Option<Icon>,
    kind: Kind,
    min_width: f32,
    height: f32,
    full_width: bool,
    name: Option<String>,
}

impl Btn {
    /// A standard button.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            icon: None,
            kind: Kind::Standard,
            min_width: 0.0,
            height: 30.0,
            full_width: false,
            name: None,
        }
    }

    /// Its look.
    pub fn kind(mut self, kind: Kind) -> Self {
        self.kind = kind;
        self
    }

    /// An icon before the text.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// At least this wide.
    pub fn min_width(mut self, w: f32) -> Self {
        self.min_width = w;
        self
    }

    /// This tall (30 by default).
    pub fn height(mut self, h: f32) -> Self {
        self.height = h;
        self
    }

    /// As wide as the space it is in.
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    /// The accessible name, when it should say more than the text ("Revoke Pixel 9").
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

struct Look {
    fill: Color32,
    stroke: Stroke,
    text: Color32,
}

fn look(p: &Palette, kind: Kind) -> Look {
    match kind {
        Kind::Standard => Look {
            fill: p.layer,
            stroke: Stroke::new(1.0, p.stroke_2),
            text: p.text,
        },
        Kind::Primary => Look {
            fill: p.primary,
            stroke: Stroke::NONE,
            text: p.on_primary,
        },
        Kind::Danger => Look {
            fill: p.danger,
            stroke: Stroke::NONE,
            text: Color32::WHITE,
        },
        Kind::DangerOutline => Look {
            fill: p.layer,
            stroke: Stroke::new(1.0, p.bad.gamma_multiply(0.55)),
            text: p.bad,
        },
        Kind::Subtle => Look {
            fill: Color32::TRANSPARENT,
            stroke: Stroke::NONE,
            text: p.text,
        },
        Kind::OnAccent => Look {
            fill: Color32::from_white_alpha(224),
            stroke: Stroke::NONE,
            text: theme::LIGHT.text,
        },
    }
}

impl Widget for Btn {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = pal(ui);
        let lk = look(p, self.kind);
        let galley = WidgetText::from(RichText::new(&self.text).size(theme::BODY)).into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            egui::TextStyle::Button,
        );
        let icon_w = if self.icon.is_some() { 15.0 + 6.0 } else { 0.0 };
        let pad = if self.kind == Kind::OnAccent {
            10.0
        } else {
            12.0
        };
        let mut width = (galley.size().x + icon_w + pad * 2.0).max(self.min_width);
        if self.full_width {
            width = width.max(ui.available_width());
        }
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(width, self.height), Sense::click());
        let enabled = ui.is_enabled();
        let label = self.name.clone().unwrap_or_else(|| self.text.clone());
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, &label));
        if ui.is_rect_visible(rect) {
            let mut fill = lk.fill;
            if enabled && (response.is_pointer_button_down_on() || response.hovered()) {
                let toward = if self.kind == Kind::Primary || self.kind == Kind::Danger {
                    p.layer
                } else {
                    p.text
                };
                let t = if response.is_pointer_button_down_on() {
                    0.12
                } else {
                    0.06
                };
                fill = if fill == Color32::TRANSPARENT {
                    p.text.gamma_multiply(t)
                } else {
                    theme::mix(fill, toward, t)
                };
            }
            let alpha = if enabled { 1.0 } else { 0.45 };
            let radius = if self.kind == Kind::OnAccent {
                CornerRadius::same((self.height / 2.0) as u8)
            } else {
                CornerRadius::same(4)
            };
            let painter = ui.painter();
            painter.rect(
                rect,
                radius,
                fill.gamma_multiply(alpha),
                lk.stroke,
                StrokeKind::Inside,
            );
            if response.has_focus() {
                painter.rect_stroke(
                    rect.expand(2.0),
                    CornerRadius::same(6),
                    Stroke::new(2.0, p.accent),
                    StrokeKind::Outside,
                );
            }
            let text_color = lk.text.gamma_multiply(alpha);
            let content_w = galley.size().x + icon_w;
            let mut x = rect.center().x - content_w / 2.0;
            if let Some(icon) = self.icon {
                let r = egui::Rect::from_center_size(
                    egui::pos2(x + 7.5, rect.center().y),
                    Vec2::splat(15.0),
                );
                glyphs::paint(painter, r, icon, text_color);
                x += icon_w;
            }
            let pos = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
            painter.galley(pos, galley, text_color);
        }
        response
    }
}

/// A toggle switch (40 x 20). `label` is its accessible name (the setting's title).
pub fn toggle(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(Vec2::new(40.0, 20.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let enabled = ui.is_enabled();
    let value = *on;
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, value, label));
    if ui.is_rect_visible(rect) {
        let p = pal(ui);
        let painter = ui.painter();
        let radius = CornerRadius::same(10);
        if value {
            painter.rect_filled(rect, radius, p.primary);
            painter.circle_filled(
                egui::pos2(rect.right() - 10.0, rect.center().y),
                6.0,
                p.on_primary,
            );
        } else {
            painter.rect_stroke(rect, radius, Stroke::new(1.0, p.text_2), StrokeKind::Inside);
            painter.circle_filled(
                egui::pos2(rect.left() + 9.0, rect.center().y),
                6.0,
                p.text_2,
            );
        }
        if response.hovered() {
            painter.rect_stroke(
                rect,
                radius,
                Stroke::new(1.0, p.text.gamma_multiply(0.5)),
                StrokeKind::Outside,
            );
        }
    }
    response
}

/// A checkbox (18 x 18, ink when ticked). `label` is its accessible name.
pub fn check(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let enabled = ui.is_enabled();
    let value = *on;
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, value, label));
    if ui.is_rect_visible(rect) {
        let p = pal(ui);
        let painter = ui.painter();
        let b = egui::Rect::from_center_size(rect.center(), Vec2::splat(18.0));
        let alpha = if enabled { 1.0 } else { 0.35 };
        if value {
            painter.rect_filled(b, CornerRadius::same(4), p.primary.gamma_multiply(alpha));
            glyphs::paint(
                painter,
                b.shrink(2.5),
                Icon::Check,
                p.on_primary.gamma_multiply(alpha),
            );
        } else {
            let fill = if response.hovered() {
                theme::mix(p.layer, p.text, 0.06)
            } else {
                p.layer
            };
            painter.rect(
                b,
                CornerRadius::same(4),
                fill,
                Stroke::new(1.0, p.text_2.gamma_multiply(alpha)),
                StrokeKind::Inside,
            );
        }
    }
    response
}

/// The state a status line shows (design doc 3.4: colour plus shape plus word).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dot {
    /// Filled green dot.
    Ok,
    /// Hollow ring, muted.
    Off,
    /// Filled amber dot.
    Warn,
    /// Filled red dot.
    Bad,
    /// Orange dot with a halo: something is looking.
    Live,
}

/// A status line: dot and words. `size` is the text size.
pub fn status(ui: &mut Ui, dot: Dot, text: &str, size: f32) -> Response {
    let p = pal(ui);
    let color = match dot {
        Dot::Ok => p.ok,
        Dot::Off => p.text_3,
        Dot::Warn => p.warn,
        Dot::Bad => p.bad,
        Dot::Live => p.accent_text,
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(10.0, size + 4.0), Sense::hover());
        let c = rect.center();
        let painter = ui.painter();
        match dot {
            Dot::Off => {
                painter.circle_stroke(c, 3.0, Stroke::new(2.0, color));
            }
            Dot::Live => {
                painter.circle_filled(c, 6.0, p.accent.gamma_multiply(0.3));
                painter.circle_filled(c, 4.0, p.accent);
            }
            _ => {
                painter.circle_filled(c, 4.0, color);
            }
        }
        let rt = if dot == Dot::Live {
            theme::semi(ui, text, size, color)
        } else {
            theme::text(text, size, color)
        };
        ui.label(rt);
    })
    .response
}

/// A card: layer fill, 1 px stroke, radius 6, full width.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    card_with(ui, Margin::symmetric(14, 12), add)
}

/// A card with a chosen margin.
pub fn card_with<R>(ui: &mut Ui, margin: Margin, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.layer)
        .stroke(Stroke::new(1.0, p.stroke))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A section label above a group of rows ("General", "Who can use this share").
pub fn section(ui: &mut Ui, text: &str) {
    ui.add_space(10.0);
    let p = pal(ui);
    ui.label(theme::semi(ui, text, theme::BODY, p.text));
    ui.add_space(2.0);
}

/// A Fluent settings card: icon, title, description, and a control on the right.
pub fn setting_row(
    ui: &mut Ui,
    icon: Option<Icon>,
    title: &str,
    desc: &str,
    control: impl FnOnce(&mut Ui),
) {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.layer)
        .stroke(Stroke::new(1.0, p.stroke))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(36.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                if let Some(icon) = icon {
                    glyphs::icon(ui, icon, 20.0, p.text_2);
                }
                trailing(
                    ui,
                    true,
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(theme::text(title, 13.5, p.text));
                        if !desc.is_empty() {
                            ui.label(theme::text(desc, theme::CAPTION, p.text_2));
                        }
                    },
                    control,
                );
            });
        });
    ui.add_space(4.0);
}

/// A block of text on the left that wraps, and widgets on the right, side by side in the space
/// left. The right side's width is remembered from the previous frame (egui lays out in one pass),
/// so the left side wraps where the right side begins. `centre`: the right side is centred on the
/// left block's height, else top aligned.
pub fn trailing(
    ui: &mut Ui,
    centre: bool,
    left: impl FnOnce(&mut Ui),
    right: impl FnOnce(&mut Ui),
) {
    let id = ui.id().with(("gander-trailing", ui.next_auto_id()));
    let right_w: f32 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(0.0);
    let total = ui.available_width();
    let gap = if right_w > 0.0 { 12.0 } else { 0.0 };
    let left_w = (total - right_w - gap).max(60.0);
    let row = ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        let left_rect = ui
            .allocate_ui_with_layout(Vec2::new(left_w, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(left_w);
                left(ui);
            })
            .response
            .rect;
        let align = if centre { Align::Center } else { Align::Min };
        // The whole row, so the right side is measured at its natural width (it is laid out
        // right to left, so it only covers the left block when the row is too narrow).
        let room = egui::Rect::from_min_max(
            left_rect.left_top(),
            egui::pos2(
                left_rect.left() + total,
                left_rect.bottom().max(left_rect.top() + 30.0),
            ),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(room)
                .layout(Layout::right_to_left(align)),
        );
        right(&mut child);
        let used = child.min_rect();
        ui.advance_cursor_after_rect(used);
        used.width()
    });
    let measured = if row.inner > 0.5 { row.inner } else { 0.0 };
    if (measured - right_w).abs() > 0.5 {
        ui.ctx().data_mut(|d| d.insert_temp(id, measured));
        ui.ctx().request_discard("trailing width changed");
    }
}

/// The colour family of an InfoBar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bar {
    /// Layer fill, ink icon.
    Neutral,
    /// Green.
    Ok,
    /// Amber.
    Warn,
    /// Red.
    Bad,
}

/// A Fluent InfoBar: tinted fill, 1 px stroke, an icon in the tone colour, a semibold title and
/// body text in `text`. Either text may be empty.
pub fn infobar(ui: &mut Ui, bar: Bar, icon: Icon, title: &str, body: &str) {
    infobar_with(ui, bar, icon, title, body, |_| {});
}

/// [`infobar`] with buttons on the right.
pub fn infobar_with(
    ui: &mut Ui,
    bar: Bar,
    icon: Icon,
    title: &str,
    body: &str,
    buttons: impl FnOnce(&mut Ui),
) {
    let p = pal(ui);
    let (fill, tint) = match bar {
        Bar::Neutral => (p.layer, p.text_2),
        Bar::Ok => (p.ok_c, p.ok),
        Bar::Warn => (p.warn_c, p.warn),
        Bar::Bad => (p.bad_c, p.bad),
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, p.stroke))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                glyphs::icon(ui, icon, 18.0, tint);
                trailing(
                    ui,
                    true,
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        if !title.is_empty() {
                            ui.label(theme::semi(ui, title, 12.5, p.text));
                        }
                        if !body.is_empty() {
                            ui.label(theme::text(body, 12.5, p.text));
                        }
                    },
                    buttons,
                );
            });
        });
    ui.add_space(6.0);
}

/// The page header: a 26 px title, a one-line explanation, and buttons on the right.
pub fn page_header(ui: &mut Ui, title: &str, sub: &str, right: impl FnOnce(&mut Ui)) {
    let p = pal(ui);
    trailing(
        ui,
        false,
        |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(theme::semi(ui, title, theme::TITLE, p.text));
            if !sub.is_empty() {
                ui.label(theme::text(sub, theme::BODY, p.text_2));
            }
        },
        right,
    );
    ui.add_space(12.0);
}

/// A device's tile: a rounded square in `surface_3` with the icon.
pub fn tile(ui: &mut Ui, icon: Icon, size: f32) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same((size / 4.3) as u8), p.surface_3);
    glyphs::paint(
        ui.painter(),
        egui::Rect::from_center_size(rect.center(), Vec2::splat(size * 0.46)),
        icon,
        p.text_2,
    );
}

/// One column of a [`table_row`].
#[derive(Debug, Clone, Copy)]
pub struct Col {
    /// Width in points; the last column takes what is left.
    pub width: f32,
    /// Centred (checkbox columns) rather than left aligned.
    pub centred: bool,
}

/// A table row: `cell(i, ui)` draws column `i`; a hairline goes under the row. `tint` fills a
/// column's cell (the Stream column when on).
pub fn table_row(
    ui: &mut Ui,
    cols: &[Col],
    height: f32,
    tint: impl Fn(usize) -> Option<Color32>,
    mut cell: impl FnMut(usize, &mut Ui),
) {
    let p = pal(ui);
    let top = ui.cursor().top();
    let full = ui.available_width();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let mut used = 0.0;
        for (i, c) in cols.iter().enumerate() {
            let w = if i + 1 == cols.len() {
                (full - used).max(20.0)
            } else {
                c.width
            };
            used += w;
            let layout = if c.centred {
                Layout::centered_and_justified(egui::Direction::LeftToRight)
            } else {
                Layout::left_to_right(Align::Center)
            };
            let (rect, _) = ui.allocate_exact_size(Vec2::new(w, height), Sense::hover());
            if let Some(fill) = tint(i) {
                ui.painter().rect_filled(rect, 0, fill);
            }
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect.shrink2(Vec2::new(8.0, 0.0)))
                    .layout(layout),
            );
            child.set_clip_rect(rect.intersect(ui.clip_rect()));
            cell(i, &mut child);
        }
    });
    let y = top + height;
    let x = ui.min_rect().left();
    ui.painter()
        .hline(x..=x + full, y, Stroke::new(1.0, p.stroke));
}

/// A table header row: caption text in `text_2`, semibold, with a stronger rule.
pub fn table_header(ui: &mut Ui, cols: &[Col], titles: &[&str]) {
    let p = pal(ui);
    let top = ui.cursor().top();
    let full = ui.available_width();
    table_row(
        ui,
        cols,
        30.0,
        |_| None,
        |i, ui| {
            if let Some(t) = titles.get(i) {
                ui.label(theme::semi(ui, *t, theme::CAPTION, p.text_2));
            }
        },
    );
    let x = ui.min_rect().left();
    ui.painter()
        .hline(x..=x + full, top + 30.0, Stroke::new(1.0, p.stroke_2));
}

/// A segmented control; returns whether the choice changed.
pub fn segmented(ui: &mut Ui, selected: &mut usize, options: &[&str]) -> bool {
    let p = pal(ui);
    let mut changed = false;
    let frame = egui::Frame::new()
        .stroke(Stroke::new(1.0, p.outline()))
        .corner_radius(CornerRadius::same(4))
        .fill(p.layer);
    frame.show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.horizontal(|ui| {
            for (i, o) in options.iter().enumerate() {
                let on = *selected == i;
                let galley = WidgetText::from(RichText::new(*o).size(12.5)).into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::TextStyle::Button,
                );
                let size = Vec2::new(galley.size().x + 24.0, 28.0);
                let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, on, *o));
                if resp.clicked() && !on {
                    *selected = i;
                    changed = true;
                }
                let painter = ui.painter();
                if on {
                    painter.rect_filled(rect, CornerRadius::same(3), p.accent_container);
                } else if resp.hovered() {
                    painter.rect_filled(rect, CornerRadius::same(3), p.text.gamma_multiply(0.05));
                }
                if i > 0 {
                    painter.vline(rect.left(), rect.y_range(), Stroke::new(1.0, p.outline()));
                }
                let color = if on { p.on_accent_container } else { p.text_2 };
                painter.galley(rect.center() - galley.size() / 2.0, galley, color);
            }
        });
    });
    changed
}

impl Palette {
    /// Control edges (fields, segmented controls).
    pub fn outline(&self) -> Color32 {
        if self.dark {
            Color32::from_rgb(0x6f, 0x7d, 0x83)
        } else {
            Color32::from_rgb(0x7e, 0x8b, 0x91)
        }
    }
}

/// A small muted caption.
pub fn caption(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    ui.label(theme::text(text, theme::CAPTION, p.text_3));
}

/// A field label above an input ("Name devices see").
pub fn field_label(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    ui.label(theme::text(text, 11.5, p.text_2));
}

/// A text box in the Fluent style (30 px, layer fill).
pub fn text_field<'t>(text: &'t mut dyn egui::TextBuffer) -> egui::TextEdit<'t> {
    egui::TextEdit::singleline(text).margin(Margin::symmetric(8, 6))
}

/// A read-only value in a field-shaped box (the shared folder's path).
pub fn value_box(ui: &mut Ui, text: RichText) {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.layer_2)
        .stroke(Stroke::new(1.0, p.stroke_2))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(text).truncate());
        });
}
