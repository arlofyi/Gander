// SPDX-License-Identifier: Apache-2.0
//! The line icons of the mockups (`docs/design/mockups/index.html`, the `#i-*` symbols), drawn
//! as egui shapes on a 24-unit grid with 1.8-unit strokes, so no icon font is needed (Segoe Fluent
//! Icons may be missing) and nothing is bundled. The Lens mark is not redrawn: [`lens`] shows the
//! committed app icon.

use egui::epaint::{CubicBezierShape, PathShape, PathStroke};
use egui::{Color32, Painter, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Vec2};

/// One icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    /// A folder.
    Folder,
    /// A phone.
    Phone,
    /// A tablet.
    Tablet,
    /// Activity.
    Pulse,
    /// Network.
    Net,
    /// Settings.
    Gear,
    /// Add.
    Plus,
    /// Kill switch.
    Power,
    /// Preview (an eye).
    Preview,
    /// Fetch (arrow into a tray).
    Fetch,
    /// Stream (screen with a play triangle).
    Stream,
    /// A tick.
    Check,
    /// Chevron down (dropdowns).
    ChevDown,
    /// A warning triangle.
    Warn,
    /// Information.
    Info,
    /// An error.
    Error,
    /// A ticked circle.
    OkCircle,
    /// Refresh.
    Refresh,
    /// A shield with a tick (identity).
    Shield,
    /// A clock.
    Clock,
    /// A padlock.
    Lock,
    /// A window.
    Window,
    /// Pause.
    Pause,
    /// Close.
    Close,
    /// A person.
    User,
    /// Storage.
    Storage,
    /// Search.
    Search,
}

#[derive(Clone, Copy)]
struct Pen<'a> {
    painter: &'a Painter,
    origin: Pos2,
    k: f32,
    stroke: Stroke,
    color: Color32,
}

impl Pen<'_> {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.origin + Vec2::new(x, y) * self.k
    }

    fn line(&self, pts: &[(f32, f32)]) {
        let pts: Vec<Pos2> = pts.iter().map(|(x, y)| self.p(*x, *y)).collect();
        self.painter
            .add(PathShape::line(pts, PathStroke::from(self.stroke)));
    }

    fn closed(&self, pts: &[(f32, f32)]) {
        let pts: Vec<Pos2> = pts.iter().map(|(x, y)| self.p(*x, *y)).collect();
        self.painter
            .add(PathShape::closed_line(pts, PathStroke::from(self.stroke)));
    }

    fn circle(&self, x: f32, y: f32, r: f32) {
        self.painter
            .circle_stroke(self.p(x, y), r * self.k, self.stroke);
    }

    fn dot(&self, x: f32, y: f32, r: f32) {
        self.painter
            .circle_filled(self.p(x, y), r * self.k, self.color);
    }

    fn rrect(&self, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) {
        self.painter.rect_stroke(
            Rect::from_min_max(self.p(x0, y0), self.p(x1, y1)),
            (r * self.k).round() as u8,
            self.stroke,
            StrokeKind::Middle,
        );
    }

    /// An arc of a circle (or ellipse with `ry`), angles in degrees, y down (90 = bottom).
    fn arc(&self, cx: f32, cy: f32, rx: f32, ry: f32, a0: f32, a1: f32) {
        let steps = 24;
        let pts: Vec<(f32, f32)> = (0..=steps)
            .map(|i| {
                let a = (a0 + (a1 - a0) * i as f32 / steps as f32).to_radians();
                (cx + rx * a.cos(), cy + ry * a.sin())
            })
            .collect();
        self.line(&pts);
    }

    fn cubic(&self, pts: [(f32, f32); 4]) {
        self.painter.add(CubicBezierShape::from_points_stroke(
            pts.map(|(x, y)| self.p(x, y)),
            false,
            Color32::TRANSPARENT,
            self.stroke,
        ));
    }
}

/// Paints `icon` into `rect` (square) in `color`.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let side = rect.width().min(rect.height());
    let k = side / 24.0;
    let origin = rect.center() - Vec2::splat(side / 2.0);
    let width = (1.8 * k).max(1.1);
    let pen = Pen {
        painter,
        origin,
        k,
        stroke: Stroke::new(width, color),
        color,
    };
    draw(&pen, icon);
}

#[allow(clippy::too_many_lines)] // one arm per icon, each a transcription of the SVG
fn draw(g: &Pen<'_>, icon: Icon) {
    match icon {
        Icon::Folder => g.closed(&[
            (3.0, 7.0),
            (3.6, 5.6),
            (5.0, 5.0),
            (9.0, 5.0),
            (11.0, 7.0),
            (19.0, 7.0),
            (20.4, 7.6),
            (21.0, 9.0),
            (21.0, 17.0),
            (20.4, 18.4),
            (19.0, 19.0),
            (5.0, 19.0),
            (3.6, 18.4),
            (3.0, 17.0),
        ]),
        Icon::Phone => {
            g.rrect(7.0, 2.5, 17.0, 21.5, 2.0);
            g.line(&[(11.0, 18.5), (13.0, 18.5)]);
        }
        Icon::Tablet => {
            g.rrect(3.0, 5.0, 21.0, 19.0, 2.0);
            g.line(&[(18.0, 11.0), (18.0, 13.0)]);
        }
        Icon::Pulse => g.line(&[
            (3.0, 12.0),
            (7.0, 12.0),
            (10.0, 5.0),
            (14.0, 19.0),
            (17.0, 12.0),
            (21.0, 12.0),
        ]),
        Icon::Net => {
            g.circle(5.0, 12.0, 2.2);
            g.circle(19.0, 5.5, 2.2);
            g.circle(19.0, 18.5, 2.2);
            g.line(&[(7.0, 11.0), (17.0, 6.5)]);
            g.line(&[(7.0, 13.0), (17.0, 17.5)]);
        }
        Icon::Gear => {
            g.circle(12.0, 12.0, 3.0);
            for (a, b) in [
                ((12.0, 2.5), (12.0, 5.5)),
                ((12.0, 18.5), (12.0, 21.5)),
                ((2.5, 12.0), (5.5, 12.0)),
                ((18.5, 12.0), (21.5, 12.0)),
                ((5.3, 5.3), (7.4, 7.4)),
                ((16.6, 16.6), (18.7, 18.7)),
                ((5.3, 18.7), (7.4, 16.6)),
                ((16.6, 7.4), (18.7, 5.3)),
            ] {
                g.line(&[a, b]);
            }
        }
        Icon::Plus => {
            g.line(&[(12.0, 5.0), (12.0, 19.0)]);
            g.line(&[(5.0, 12.0), (19.0, 12.0)]);
        }
        Icon::Power => {
            g.line(&[(12.0, 3.0), (12.0, 11.0)]);
            g.arc(12.0, 13.1, 8.0, 8.0, 224.5, -44.5);
        }
        Icon::Preview => {
            g.cubic([(2.0, 12.0), (4.5, 7.5), (8.0, 5.0), (12.0, 5.0)]);
            g.cubic([(12.0, 5.0), (16.0, 5.0), (19.5, 7.5), (22.0, 12.0)]);
            g.cubic([(2.0, 12.0), (4.5, 16.5), (8.0, 19.0), (12.0, 19.0)]);
            g.cubic([(12.0, 19.0), (16.0, 19.0), (19.5, 16.5), (22.0, 12.0)]);
            g.circle(12.0, 12.0, 3.0);
        }
        Icon::Fetch => {
            g.line(&[(12.0, 4.0), (12.0, 15.0)]);
            g.line(&[(7.0, 10.0), (12.0, 15.0), (17.0, 10.0)]);
            g.line(&[(5.0, 20.0), (19.0, 20.0)]);
        }
        Icon::Stream => {
            g.rrect(3.0, 4.0, 21.0, 17.0, 1.5);
            g.line(&[(9.0, 21.0), (15.0, 21.0)]);
            g.closed(&[(10.5, 8.2), (10.5, 13.8), (15.1, 11.0)]);
        }
        Icon::Check => g.line(&[(5.0, 12.5), (9.5, 17.0), (19.0, 7.0)]),
        Icon::ChevDown => g.line(&[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)]),
        Icon::Warn => {
            g.closed(&[
                (12.0, 3.4),
                (21.8, 19.6),
                (21.0, 21.0),
                (3.0, 21.0),
                (2.2, 19.6),
            ]);
            g.line(&[(12.0, 10.0), (12.0, 14.0)]);
            g.dot(12.0, 17.5, 1.1);
        }
        Icon::Info => {
            g.circle(12.0, 12.0, 9.0);
            g.line(&[(12.0, 11.0), (12.0, 17.0)]);
            g.dot(12.0, 7.5, 1.1);
        }
        Icon::Error => {
            g.circle(12.0, 12.0, 9.0);
            g.line(&[(12.0, 7.0), (12.0, 13.0)]);
            g.dot(12.0, 16.5, 1.1);
        }
        Icon::OkCircle => {
            g.circle(12.0, 12.0, 9.0);
            g.line(&[(8.0, 12.5), (11.0, 15.5), (16.0, 9.5)]);
        }
        Icon::Refresh => {
            g.arc(12.0, 11.0, 8.0, 8.0, 0.0, -315.0);
            g.line(&[(20.0, 4.0), (20.0, 11.0), (13.0, 11.0)]);
        }
        Icon::Shield => {
            g.closed(&[
                (12.0, 3.0),
                (19.0, 6.0),
                (19.0, 12.0),
                (17.5, 16.5),
                (12.0, 21.0),
                (6.5, 16.5),
                (5.0, 12.0),
                (5.0, 6.0),
            ]);
            g.line(&[(9.0, 12.0), (11.0, 14.0), (15.0, 10.0)]);
        }
        Icon::Clock => {
            g.circle(12.0, 12.0, 9.0);
            g.line(&[(12.0, 7.0), (12.0, 12.0), (15.0, 14.0)]);
        }
        Icon::Lock => {
            g.rrect(5.0, 11.0, 19.0, 21.0, 2.0);
            g.line(&[(8.0, 11.0), (8.0, 8.0)]);
            g.arc(12.0, 8.0, 4.0, 4.0, 180.0, 360.0);
            g.line(&[(16.0, 8.0), (16.0, 11.0)]);
        }
        Icon::Window => {
            g.rrect(3.0, 4.0, 21.0, 20.0, 2.0);
            g.line(&[(3.0, 8.5), (21.0, 8.5)]);
        }
        Icon::Pause => {
            let thick = Pen {
                stroke: Stroke::new(g.stroke.width * 1.6, g.color),
                ..*g
            };
            thick.line(&[(8.0, 5.0), (8.0, 19.0)]);
            thick.line(&[(16.0, 5.0), (16.0, 19.0)]);
        }
        Icon::Close => {
            g.line(&[(6.0, 6.0), (18.0, 18.0)]);
            g.line(&[(18.0, 6.0), (6.0, 18.0)]);
        }
        Icon::User => {
            g.circle(12.0, 8.0, 4.0);
            g.arc(12.0, 21.0, 8.0, 8.0, 180.0, 360.0);
        }
        Icon::Storage => {
            g.arc(12.0, 6.0, 8.0, 3.0, 0.0, 360.0);
            g.line(&[(4.0, 6.0), (4.0, 18.0)]);
            g.line(&[(20.0, 6.0), (20.0, 18.0)]);
            g.arc(12.0, 18.0, 8.0, 3.0, 0.0, 180.0);
            g.arc(12.0, 12.0, 8.0, 3.0, 0.0, 180.0);
        }
        Icon::Search => {
            g.circle(11.0, 11.0, 6.5);
            g.line(&[(20.0, 20.0), (15.8, 15.8)]);
        }
    }
}

/// Allocates `size` points and paints `icon` there. Decorative: no accessible name.
pub fn icon(ui: &mut egui::Ui, icon: Icon, size: f32, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if ui.is_rect_visible(rect) {
        paint(ui.painter(), rect, icon, color);
    }
    response
}

/// The Lens app icon (the committed `assets/gander-window-64.png`, see [`super::icons`]) at
/// `size` points. The texture is made once per context.
pub fn lens(ui: &mut egui::Ui, size: f32) -> Response {
    let id = egui::Id::new("gander-lens-texture");
    let tex = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(id));
    let tex = tex.unwrap_or_else(|| {
        let px = super::icons::window_icon();
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [px.width as usize, px.height as usize],
            &px.rgba,
        );
        let t = ui
            .ctx()
            .load_texture("gander-lens", image, egui::TextureOptions::LINEAR);
        ui.ctx().data_mut(|d| d.insert_temp(id, t.clone()));
        t
    });
    ui.add(egui::Image::new(&tex).fit_to_exact_size(Vec2::splat(size)))
}
