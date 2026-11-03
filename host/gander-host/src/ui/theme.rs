// SPDX-License-Identifier: Apache-2.0
//! The Gander design system on egui (docs/design/2026-11-02-ui-redesign.md, sections 3, 4 and
//! 9.2): the colour tokens of both themes, the Windows type ramp, the egui `Visuals` and
//! `Spacing` built from them, and the light / dark / system choice.
//!
//! # Mica without blur
//!
//! egui cannot blur what is behind a window, so Mica is approximated with flat layers: the window
//! background and the navigation pane in `mica`, the content page in `layer_2` with a 1 px
//! `stroke` on its top and left edges and an 8 px top-left radius, and cards in `layer`.
//!
//! # Fonts
//!
//! The bundled egui fonts stay as the fallback. On Windows, Segoe UI (Variable when present),
//! Segoe UI Semibold and Cascadia Mono are read from `C:/Windows/Fonts` at start-up (never
//! redistributed). egui cannot pick a weight axis of a variable font, so semibold text uses the
//! static `seguisb.ttf` as its own family ([`semibold`]).

use std::sync::{Arc, OnceLock};

use egui::{Color32, CornerRadius, FontFamily, FontId, RichText, Stroke};

/// The owner's theme setting (Settings, Theme).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ThemeChoice {
    /// Follow the Windows light / dark setting.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

impl ThemeChoice {
    /// Every choice, in the order the dropdown lists them.
    pub const ALL: [ThemeChoice; 3] = [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark];

    /// What the dropdown shows.
    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::System => "Use system setting",
            ThemeChoice::Light => "Light",
            ThemeChoice::Dark => "Dark",
        }
    }

    /// The stored form.
    pub fn as_str(self) -> &'static str {
        match self {
            ThemeChoice::System => "system",
            ThemeChoice::Light => "light",
            ThemeChoice::Dark => "dark",
        }
    }

    /// Parses the stored form; anything unknown is `System`.
    pub fn parse(text: &str) -> Self {
        match text.trim() {
            "light" => ThemeChoice::Light,
            "dark" => ThemeChoice::Dark,
            _ => ThemeChoice::System,
        }
    }

    fn preference(self) -> egui::ThemePreference {
        match self {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Light => egui::ThemePreference::Light,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
        }
    }
}

/// The colour tokens of one theme (design doc 3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Whether this is the dark theme.
    pub dark: bool,
    /// Window background, nav pane, title bar.
    pub mica: Color32,
    /// Cards and setting rows.
    pub layer: Color32,
    /// The content page behind cards.
    pub layer_2: Color32,
    /// Card and layer borders.
    pub stroke: Color32,
    /// Control borders, window border.
    pub stroke_2: Color32,
    /// Tiles, filled chips.
    pub surface_3: Color32,
    /// 1 px dividers.
    pub line: Color32,
    /// Body and titles.
    pub text: Color32,
    /// Secondary text.
    pub text_2: Color32,
    /// Muted text, metadata.
    pub text_3: Color32,
    /// Commands (filled buttons, switches on).
    pub primary: Color32,
    /// Text on `primary`.
    pub on_primary: Color32,
    /// Live dot, selection, focus (non-text only).
    pub accent: Color32,
    /// Text on an orange fill.
    pub on_accent: Color32,
    /// Orange text.
    pub accent_text: Color32,
    /// Selected rows, the Stream column when on.
    pub accent_container: Color32,
    /// Text on `accent_container`.
    pub on_accent_container: Color32,
    /// Online, verified.
    pub ok: Color32,
    /// Relayed, caution.
    pub warn: Color32,
    /// Refused, error text.
    pub bad: Color32,
    /// Fill of destructive buttons.
    pub danger: Color32,
    /// Banner background, ok.
    pub ok_c: Color32,
    /// Banner background, warn.
    pub warn_c: Color32,
    /// Banner background, bad.
    pub bad_c: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// `rgb` at `alpha` (0-255), premultiplied as egui wants it.
const fn rgba(hex: u32, alpha: u8) -> Color32 {
    let r = (hex >> 16) as u8;
    let g = (hex >> 8) as u8;
    let b = hex as u8;
    Color32::from_rgba_premultiplied(premul(r, alpha), premul(g, alpha), premul(b, alpha), alpha)
}

const fn premul(c: u8, alpha: u8) -> u8 {
    ((c as u32 * alpha as u32 + 127) / 255) as u8
}

/// The light theme.
pub const LIGHT: Palette = Palette {
    dark: false,
    mica: rgb(0xeef0ef),
    layer: rgb(0xfbfbfa),
    layer_2: rgb(0xf6f7f6),
    stroke: rgba(0x1d2a30, 26),
    stroke_2: rgba(0x1d2a30, 41),
    surface_3: rgb(0xe3e7e5),
    line: rgb(0xd6dbda),
    text: rgb(0x1d2a30),
    text_2: rgb(0x46555c),
    text_3: rgb(0x5f6e75),
    primary: rgb(0x1d2a30),
    on_primary: rgb(0xffffff),
    accent: rgb(0xe2682a),
    on_accent: rgb(0x170d06),
    accent_text: rgb(0xa8460f),
    accent_container: rgb(0xfbe3d4),
    on_accent_container: rgb(0x5a2206),
    ok: rgb(0x2e7d4f),
    warn: rgb(0x8a6100),
    bad: rgb(0xb3261e),
    danger: rgb(0xb3261e),
    ok_c: rgb(0xdcefe2),
    warn_c: rgb(0xf6e7c4),
    bad_c: rgb(0xf9dedc),
};

/// The dark theme.
pub const DARK: Palette = Palette {
    dark: true,
    mica: rgb(0x1b2124),
    layer: rgb(0x232b2e),
    layer_2: rgb(0x1f2629),
    stroke: rgba(0xffffff, 18),
    stroke_2: rgba(0xffffff, 33),
    surface_3: rgb(0x263236),
    line: rgb(0x33424a),
    text: rgb(0xe7ecec),
    text_2: rgb(0xb3c0c4),
    text_3: rgb(0x8d9ba0),
    primary: rgb(0xe7ecec),
    on_primary: rgb(0x1d2a30),
    accent: rgb(0xee7a3c),
    on_accent: rgb(0x170d06),
    accent_text: rgb(0xf59a62),
    accent_container: rgb(0x4a2412),
    on_accent_container: rgb(0xffdccb),
    ok: rgb(0x6fcf97),
    warn: rgb(0xe3b341),
    bad: rgb(0xf2b8b5),
    danger: rgb(0xc4302b),
    ok_c: rgb(0x1f3a2a),
    warn_c: rgb(0x3d3012),
    bad_c: rgb(0x4a1d1a),
};

/// The palette of the theme `ui` is drawn in.
pub fn pal(ui: &egui::Ui) -> &'static Palette {
    if ui.visuals().dark_mode {
        &DARK
    } else {
        &LIGHT
    }
}

/// The palette of the theme `ctx` is drawn in.
pub fn pal_ctx(ctx: &egui::Context) -> &'static Palette {
    if ctx.style().visuals.dark_mode {
        &DARK
    } else {
        &LIGHT
    }
}

/// `a` mixed toward `b` by `t` (0..1), in gamma space like the CSS `color-mix` of the mockups.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (f32::from(x) * (1.0 - t) + f32::from(y) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

// ---- type ramp (design doc 4.2) ----

/// Page titles.
pub const TITLE: f32 = 26.0;
/// Dialog titles, the pairing request title.
pub const SUBTITLE: f32 = 20.0;
/// Section labels, device names.
pub const BODY_STRONG: f32 = 14.0;
/// Everything else.
pub const BODY: f32 = 13.0;
/// Descriptions, table headers.
pub const CAPTION: f32 = 12.0;
/// The pairing code.
pub const CODE: f32 = 52.0;

const SEMIBOLD: &str = "gander-semibold";
const INSTALLED: &str = "gander-fonts-installed";

/// The semibold family where it was installed ([`install`]), else the regular one (tests run
/// on a bare context, and egui panics on an unknown family).
pub fn semibold(ctx: &egui::Context) -> FontFamily {
    let installed = ctx.data(|d| {
        d.get_temp::<bool>(egui::Id::new(INSTALLED))
            .unwrap_or(false)
    });
    if installed {
        FontFamily::Name(SEMIBOLD.into())
    } else {
        FontFamily::Proportional
    }
}

/// Semibold text of `size` in `color`.
pub fn semi(ui: &egui::Ui, text: impl Into<String>, size: f32, color: Color32) -> RichText {
    RichText::new(text)
        .font(FontId::new(size, semibold(ui.ctx())))
        .color(color)
}

/// Regular text of `size` in `color`.
pub fn text(text: impl Into<String>, size: f32, color: Color32) -> RichText {
    RichText::new(text).size(size).color(color)
}

/// Monospace text (Cascadia Mono when present).
pub fn mono(text: impl Into<String>, size: f32, color: Color32) -> RichText {
    RichText::new(text)
        .font(FontId::new(size, FontFamily::Monospace))
        .color(color)
}

struct SystemFonts {
    regular: Option<Arc<egui::FontData>>,
    semibold: Option<Arc<egui::FontData>>,
    mono: Option<Arc<egui::FontData>>,
}

fn read_font(names: &[&str]) -> Option<Arc<egui::FontData>> {
    let dir = std::env::var_os("WINDIR")
        .map_or_else(|| std::path::PathBuf::from("C:/Windows"), Into::into)
        .join("Fonts");
    names.iter().find_map(|n| {
        std::fs::read(dir.join(n))
            .ok()
            .map(|bytes| Arc::new(egui::FontData::from_owned(bytes)))
    })
}

fn system_fonts() -> &'static SystemFonts {
    static FONTS: OnceLock<SystemFonts> = OnceLock::new();
    FONTS.get_or_init(|| {
        if cfg!(windows) {
            SystemFonts {
                regular: read_font(&["segoeui.ttf"]),
                semibold: read_font(&["seguisb.ttf"]),
                mono: read_font(&["CascadiaMono.ttf", "consola.ttf"]),
            }
        } else {
            SystemFonts {
                regular: None,
                semibold: None,
                mono: None,
            }
        }
    })
}

fn font_definitions() -> egui::FontDefinitions {
    let mut defs = egui::FontDefinitions::default();
    let sys = system_fonts();
    let fallback_prop = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut semi_family = Vec::new();
    if let Some(f) = &sys.regular {
        defs.font_data.insert("segoe-ui".into(), Arc::clone(f));
        if let Some(fam) = defs.families.get_mut(&FontFamily::Proportional) {
            fam.insert(0, "segoe-ui".into());
        }
    }
    if let Some(f) = &sys.semibold {
        defs.font_data
            .insert("segoe-ui-semibold".into(), Arc::clone(f));
        semi_family.push("segoe-ui-semibold".to_owned());
    } else if sys.regular.is_some() {
        semi_family.push("segoe-ui".to_owned());
    }
    semi_family.extend(fallback_prop);
    defs.families
        .insert(FontFamily::Name(SEMIBOLD.into()), semi_family);
    if let Some(f) = &sys.mono {
        defs.font_data.insert("cascadia-mono".into(), Arc::clone(f));
        if let Some(fam) = defs.families.get_mut(&FontFamily::Monospace) {
            fam.insert(0, "cascadia-mono".into());
        }
    }
    defs
}

/// Fonts and both themes' styles; the theme choice stays as it is. Call once per context.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(INSTALLED), true));
    ctx.style_mut_of(egui::Theme::Light, |s| style(s, &LIGHT));
    ctx.style_mut_of(egui::Theme::Dark, |s| style(s, &DARK));
}

/// [`install`] and follow the system theme (the default before the setting is read).
pub fn apply(ctx: &egui::Context) {
    install(ctx);
    set_choice(ctx, ThemeChoice::System);
}

/// Applies the owner's theme choice.
pub fn set_choice(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.set_theme(choice.preference());
}

fn style(s: &mut egui::Style, p: &Palette) {
    use egui::{FontFamily as F, TextStyle as T};
    s.text_styles = [
        (T::Small, FontId::new(CAPTION, F::Proportional)),
        (T::Body, FontId::new(BODY, F::Proportional)),
        (T::Button, FontId::new(BODY, F::Proportional)),
        (T::Heading, FontId::new(TITLE, F::Name(SEMIBOLD.into()))),
        (T::Monospace, FontId::new(12.5, F::Monospace)),
    ]
    .into();
    let sp = &mut s.spacing;
    sp.item_spacing = egui::vec2(8.0, 6.0);
    sp.button_padding = egui::vec2(12.0, 5.0);
    sp.interact_size.y = 30.0;
    sp.menu_margin = egui::Margin::same(4);
    sp.window_margin = egui::Margin::same(14);
    sp.combo_width = 160.0;

    let v = &mut s.visuals;
    *v = if p.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.dark_mode = p.dark;
    v.override_text_color = None;
    v.weak_text_color = Some(p.text_2);
    v.panel_fill = p.mica;
    v.window_fill = p.layer;
    v.faint_bg_color = p.layer_2;
    v.extreme_bg_color = p.layer;
    v.text_edit_bg_color = Some(p.layer);
    v.code_bg_color = p.layer_2;
    v.hyperlink_color = p.accent_text;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.bad;
    v.window_stroke = Stroke::new(1.0, p.stroke_2);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(8);
    let shadow = egui::Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(71),
    };
    v.window_shadow = shadow;
    v.popup_shadow = shadow;
    v.striped = false;
    v.button_frame = true;
    v.selection.bg_fill = p.accent_container;
    v.selection.stroke = Stroke::new(1.0, p.accent);

    let hovered = mix(p.layer, p.text, 0.04);
    let active = mix(p.layer, p.text, 0.08);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.layer;
    w.noninteractive.weak_bg_fill = p.layer;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.stroke);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = CornerRadius::same(6);
    for (st, fill) in [
        (&mut w.inactive, p.layer),
        (&mut w.hovered, hovered),
        (&mut w.active, active),
        (&mut w.open, hovered),
    ] {
        st.bg_fill = fill;
        st.weak_bg_fill = fill;
        st.bg_stroke = Stroke::new(1.0, p.stroke_2);
        st.fg_stroke = Stroke::new(1.0, p.text);
        st.corner_radius = CornerRadius::same(4);
        st.expansion = 0.0;
    }
    w.hovered.bg_stroke = Stroke::new(1.0, mix(p.stroke_2, p.text, 0.2));
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
}

// ---- tones, used by the older helpers and by format-level code ----

/// How a notice is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Informational (ink, not blue).
    Info,
    /// All good.
    Ok,
    /// Needs attention, nothing is broken.
    Warn,
    /// Something is wrong or a security-relevant action needs care.
    Bad,
}

/// Text colour of a tone.
pub fn color(dark_mode: bool, tone: Tone) -> Color32 {
    let p = if dark_mode { &DARK } else { &LIGHT };
    match tone {
        Tone::Info => p.text,
        Tone::Ok => p.ok,
        Tone::Warn => p.warn,
        Tone::Bad => p.bad,
    }
}

/// Text in a tone's colour.
pub fn toned(ui: &egui::Ui, tone: Tone, text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(color(ui.visuals().dark_mode, tone))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_choices_round_trip() {
        for c in ThemeChoice::ALL {
            assert_eq!(ThemeChoice::parse(c.as_str()), c);
        }
        assert_eq!(ThemeChoice::parse("purple"), ThemeChoice::System);
    }

    #[test]
    fn install_sets_both_themes_from_the_tokens() {
        let ctx = egui::Context::default();
        install(&ctx);
        set_choice(&ctx, ThemeChoice::Dark);
        assert_eq!(ctx.style().visuals.panel_fill, DARK.mica);
        set_choice(&ctx, ThemeChoice::Light);
        assert_eq!(ctx.style().visuals.panel_fill, LIGHT.mica);
        assert_eq!(semibold(&ctx), FontFamily::Name(SEMIBOLD.into()));
        assert_eq!(
            semibold(&egui::Context::default()),
            FontFamily::Proportional
        );
    }

    #[test]
    fn mix_moves_toward_the_second_colour() {
        let m = mix(
            Color32::from_rgb(0, 0, 0),
            Color32::from_rgb(200, 100, 0),
            0.5,
        );
        assert_eq!(m, Color32::from_rgb(100, 50, 0));
    }
}
