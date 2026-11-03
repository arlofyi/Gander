// SPDX-License-Identifier: Apache-2.0
//! A small CPU rasteriser for egui output: renders a window to an RGBA image with no GPU, no
//! window and no screen capture.
//!
//! This is how the screenshots under `docs/specs/assets/ui/` are made (`gander-host --ui-smoke`):
//! the window's own drawing code runs in a headless `egui::Context`, egui tessellates it into
//! triangle meshes, and this module fills the triangles. It works in a locked session, on a
//! machine without a usable GPU, and can never show anything that is on the real desktop.
//!
//! What it does, like egui's GL painter: per-vertex premultiplied colours interpolated across the
//! triangle, a texture lookup (bilinear, or nearest for textures asked for as such) multiplied
//! in, clipping to each primitive's clip rectangle, and "over" blending in gamma space. What it
//! leaves out: paint callbacks, mesh anti-aliasing beyond egui's own feathered edges, and
//! sub-pixel triangle coverage (pixel centres decide).

use std::collections::HashMap;

use egui::epaint::{ImageData, Primitive, Vertex};
use egui::{Color32, TextureId};

/// A rendered window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// RGBA8, row-major, straight (not premultiplied) alpha; always opaque.
    pub rgba: Vec<u8>,
}

impl Image {
    /// The pixel at (`x`, `y`).
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }

    /// Encodes the image as PNG.
    pub fn to_png(&self) -> Result<Vec<u8>, png::EncodingError> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.width, self.height);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header()?;
            writer.write_image_data(&self.rgba)?;
        }
        Ok(out)
    }
}

struct Texture {
    size: [usize; 2],
    pixels: Vec<Color32>,
    nearest: bool,
}

impl Texture {
    fn texel(&self, x: i64, y: i64) -> [f32; 4] {
        let x = x.clamp(0, self.size[0] as i64 - 1) as usize;
        let y = y.clamp(0, self.size[1] as i64 - 1) as usize;
        let c = self.pixels[y * self.size[0] + x];
        [
            f32::from(c.r()) / 255.0,
            f32::from(c.g()) / 255.0,
            f32::from(c.b()) / 255.0,
            f32::from(c.a()) / 255.0,
        ]
    }

    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let fx = u * self.size[0] as f32;
        let fy = v * self.size[1] as f32;
        if self.nearest {
            return self.texel(fx.floor() as i64, fy.floor() as i64);
        }
        let fx = fx - 0.5;
        let fy = fy - 0.5;
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let a = self.texel(x0, y0);
        let b = self.texel(x0 + 1, y0);
        let c = self.texel(x0, y0 + 1);
        let d = self.texel(x0 + 1, y0 + 1);
        let mut out = [0.0; 4];
        for i in 0..4 {
            let top = a[i] * (1.0 - tx) + b[i] * tx;
            let bottom = c[i] * (1.0 - tx) + d[i] * tx;
            out[i] = top * (1.0 - ty) + bottom * ty;
        }
        out
    }
}

/// Renders what `ui` draws into an image of `size` pixels at `pixels_per_point`.
///
/// `ui` runs several passes, as a real frame loop would, so layouts that need a second pass (auto
/// sized areas, wrapped text) settle. The theme is light unless `dark`.
pub fn render(
    size: (u32, u32),
    pixels_per_point: f32,
    dark: bool,
    mut ui: impl FnMut(&egui::Context),
) -> Image {
    let ctx = egui::Context::default();
    crate::ui::theme::install(&ctx);
    ctx.set_theme(if dark {
        egui::ThemePreference::Dark
    } else {
        egui::ThemePreference::Light
    });
    ctx.set_pixels_per_point(pixels_per_point);

    let points = egui::vec2(size.0 as f32, size.1 as f32) / pixels_per_point;
    let mut textures: HashMap<TextureId, Texture> = HashMap::new();
    let mut last = None;
    for pass in 0..4 {
        let mut raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, points)),
            time: Some(f64::from(pass) * 0.2),
            ..Default::default()
        };
        raw.viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(pixels_per_point);
        let out = ctx.run(raw, |c| ui(c));
        for (id, delta) in &out.textures_delta.set {
            apply_delta(&mut textures, *id, delta);
        }
        for id in &out.textures_delta.free {
            textures.remove(id);
        }
        last = Some((out.shapes, out.pixels_per_point));
    }
    let (shapes, ppp) = last.unwrap_or_default();
    let primitives = ctx.tessellate(shapes, ppp);

    let clear = ctx.style().visuals.panel_fill;
    let (w, h) = (size.0 as usize, size.1 as usize);
    let mut buf = vec![[0.0f32; 4]; w * h];
    for px in &mut buf {
        *px = [
            f32::from(clear.r()) / 255.0,
            f32::from(clear.g()) / 255.0,
            f32::from(clear.b()) / 255.0,
            1.0,
        ];
    }
    for prim in &primitives {
        let Primitive::Mesh(mesh) = &prim.primitive else {
            continue;
        };
        let Some(tex) = textures.get(&mesh.texture_id) else {
            continue;
        };
        let clip = [
            ((prim.clip_rect.min.x * ppp).round().max(0.0)) as i32,
            ((prim.clip_rect.min.y * ppp).round().max(0.0)) as i32,
            ((prim.clip_rect.max.x * ppp).round().min(w as f32)) as i32,
            ((prim.clip_rect.max.y * ppp).round().min(h as f32)) as i32,
        ];
        for tri in mesh.indices.chunks_exact(3) {
            let v = |i: u32| &mesh.vertices[i as usize];
            fill_triangle(
                &mut buf,
                w,
                h,
                clip,
                tex,
                ppp,
                [v(tri[0]), v(tri[1]), v(tri[2])],
            );
        }
    }

    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in &buf {
        // Over an opaque clear colour the result is opaque; premultiplied equals straight.
        for c in &px[..3] {
            rgba.push((c.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
        rgba.push(255);
    }
    Image {
        width: size.0,
        height: size.1,
        rgba,
    }
}

fn apply_delta(
    textures: &mut HashMap<TextureId, Texture>,
    id: TextureId,
    delta: &egui::epaint::ImageDelta,
) {
    let ImageData::Color(image) = &delta.image;
    let nearest = delta.options.magnification == egui::TextureFilter::Nearest;
    match delta.pos {
        None => {
            textures.insert(
                id,
                Texture {
                    size: image.size,
                    pixels: image.pixels.clone(),
                    nearest,
                },
            );
        }
        Some([px, py]) => {
            let Some(tex) = textures.get_mut(&id) else {
                return;
            };
            for y in 0..image.size[1] {
                for x in 0..image.size[0] {
                    let (tx, ty) = (px + x, py + y);
                    if tx < tex.size[0] && ty < tex.size[1] {
                        tex.pixels[ty * tex.size[0] + tx] = image.pixels[y * image.size[0] + x];
                    }
                }
            }
        }
    }
}

fn fill_triangle(
    buf: &mut [[f32; 4]],
    width: usize,
    height: usize,
    clip: [i32; 4],
    tex: &Texture,
    ppp: f32,
    v: [&Vertex; 3],
) {
    let p: Vec<[f32; 2]> = v.iter().map(|v| [v.pos.x * ppp, v.pos.y * ppp]).collect();
    let area = edge(p[0], p[1], p[2]);
    if area.abs() < 1e-6 {
        return;
    }
    let min_x = (p.iter().map(|q| q[0]).fold(f32::MAX, f32::min).floor() as i32)
        .max(clip[0])
        .max(0);
    let min_y = (p.iter().map(|q| q[1]).fold(f32::MAX, f32::min).floor() as i32)
        .max(clip[1])
        .max(0);
    let max_x = (p.iter().map(|q| q[0]).fold(f32::MIN, f32::max).ceil() as i32)
        .min(clip[2])
        .min(width as i32);
    let max_y = (p.iter().map(|q| q[1]).fold(f32::MIN, f32::max).ceil() as i32)
        .min(clip[3])
        .min(height as i32);
    let colors: Vec<[f32; 4]> = v
        .iter()
        .map(|v| {
            [
                f32::from(v.color.r()) / 255.0,
                f32::from(v.color.g()) / 255.0,
                f32::from(v.color.b()) / 255.0,
                f32::from(v.color.a()) / 255.0,
            ]
        })
        .collect();
    let inv_area = 1.0 / area;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let q = [x as f32 + 0.5, y as f32 + 0.5];
            let w0 = edge(p[1], p[2], q) * inv_area;
            let w1 = edge(p[2], p[0], q) * inv_area;
            let w2 = edge(p[0], p[1], q) * inv_area;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let u = w0 * v[0].uv.x + w1 * v[1].uv.x + w2 * v[2].uv.x;
            let t = w0 * v[0].uv.y + w1 * v[1].uv.y + w2 * v[2].uv.y;
            let texel = tex.sample(u, t);
            let mut src = [0.0f32; 4];
            for i in 0..4 {
                let vertex = w0 * colors[0][i] + w1 * colors[1][i] + w2 * colors[2][i];
                src[i] = vertex * texel[i];
            }
            let dst = &mut buf[y as usize * width + x as usize];
            let inv = 1.0 - src[3];
            for i in 0..3 {
                dst[i] = src[i] + dst[i] * inv;
            }
        }
    }
}

fn edge(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dominant(img: &Image) -> [u8; 4] {
        img.pixel(2, 2)
    }

    #[test]
    fn an_empty_window_is_the_panel_colour() {
        let img = render((120, 80), 1.0, false, |ctx| {
            egui::CentralPanel::default().show(ctx, |_| {});
        });
        assert_eq!((img.width, img.height), (120, 80));
        assert_eq!(img.rgba.len(), 120 * 80 * 4);
        let bg = dominant(&img);
        assert_eq!(bg[3], 255);
        assert!(
            img.rgba.chunks_exact(4).all(|p| p == bg),
            "nothing but background"
        );
    }

    #[test]
    fn a_filled_rect_lands_where_it_should_with_its_colour() {
        let img = render((200, 100), 1.0, false, |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| {
                    ui.painter().rect_filled(
                        egui::Rect::from_min_max(egui::pos2(20.0, 10.0), egui::pos2(120.0, 60.0)),
                        0.0,
                        Color32::from_rgb(200, 30, 40),
                    );
                });
        });
        assert_eq!(img.pixel(70, 35), [200, 30, 40, 255], "inside");
        assert_ne!(img.pixel(10, 35), [200, 30, 40, 255], "left of it");
        assert_ne!(img.pixel(70, 80), [200, 30, 40, 255], "below it");
    }

    #[test]
    fn text_is_drawn_and_a_larger_scale_makes_a_larger_image() {
        let draw = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("Allow");
            });
        };
        let small = render((160, 60), 1.0, false, draw);
        let bg = dominant(&small);
        let ink = small.rgba.chunks_exact(4).filter(|p| *p != bg).count();
        assert!(ink > 40, "glyphs left {ink} non-background pixels");
        let big = render((320, 120), 2.0, false, draw);
        let ink2 = big
            .rgba
            .chunks_exact(4)
            .filter(|p| **p != dominant(&big))
            .count();
        assert!(ink2 > ink * 2, "{ink2} vs {ink}");
    }

    #[test]
    fn dark_and_light_themes_differ() {
        let draw = |ctx: &egui::Context| {
            egui::CentralPanel::default().show(ctx, |_| {});
        };
        let light = render((10, 10), 1.0, false, draw);
        let dark = render((10, 10), 1.0, true, draw);
        assert_ne!(light.pixel(1, 1), dark.pixel(1, 1));
    }

    #[test]
    fn png_encoding_has_the_signature_and_the_size() {
        let img = render((30, 20), 1.0, false, |ctx| {
            egui::CentralPanel::default().show(ctx, |_| {});
        });
        let png = img.to_png().unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let decoder = png::Decoder::new(std::io::Cursor::new(png));
        let reader = decoder.read_info().unwrap();
        assert_eq!((reader.info().width, reader.info().height), (30, 20));
    }
}
