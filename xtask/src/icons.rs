// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask icons [--check]`: turns the brand SVGs in `brand/` into every raster asset the
//! apps embed, so no build needs a rasteriser (the generated files are committed).
//!
//! | Output | From | Used by |
//! | --- | --- | --- |
//! | `host/gander-host/assets/gander.ico` (16, 20, 24, 32, 40, 48, 64, 256) | `gander-app-icon.svg`, `gander-app-icon-small.svg` for 24 px and below | the exe icon (`build.rs`), the MSI (ARPPRODUCTICON) |
//! | `host/gander-host/assets/gander-window-64.png` | `gander-app-icon.svg` | the egui windows' icon |
//! | `host/gander-host/assets/tray/gander-tray-<state>-<16,20,24,32>.png` | `brand/tray/*.svg` | the tray icon, one set per state |
//! | `brand/png/*.png` | the master, the reversed (dark-background) master, the app icon, the lockup and the reversed lockup (transparent, ink or paper coloured) | README, social previews, store listings, review |
//!
//! The SVGs are plain outlined paths (no text), rasterised with `resvg` (`usvg` + `tiny-skia`, all
//! Apache-2.0, MIT or BSD-3-Clause). The output is deterministic: running the task twice, or on
//! another machine, writes identical bytes. `--check` writes nothing and exits 1 when a committed
//! file differs from what the SVGs produce (the reproducibility gate).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg;

use crate::proc::workspace_root;

/// The sizes inside `gander.ico`.
pub const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];
/// Tray icon sizes: the notification area asks for 16 px at 100 % scale, 20 at 125 %, 24 at 150 %
/// and 32 at 200 %.
pub const TRAY_SIZES: [u32; 4] = [16, 20, 24, 32];
/// The tray states, in the order of `TrayState::ALL` in the host.
pub const TRAY_STATES: [&str; 4] = ["idle", "connected", "streaming", "paused"];
/// Up to this size the app icon uses the heavier small-size drawing (the thin goose of the master
/// blurs away below about 40 px).
const SMALL_UP_TO: u32 = 32;

/// One generated file.
#[derive(Debug)]
pub struct Output {
    /// Path relative to the repository root, with forward slashes.
    pub path: String,
    /// The bytes the file must contain.
    pub bytes: Vec<u8>,
}

fn read_svg(root: &Path, relative: &str) -> Result<usvg::Tree> {
    let path = root.join(relative);
    let data = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    usvg::Tree::from_data(&data, &usvg::Options::default())
        .with_context(|| format!("parsing {relative}"))
}

/// Renders a tree to a square-pixel pixmap of width `size` (the height follows the aspect ratio).
fn render(tree: &usvg::Tree, size: u32) -> Result<Pixmap> {
    let scale = size as f32 / tree.size().width();
    let height = (tree.size().height() * scale).round() as u32;
    let mut pixmap = Pixmap::new(size, height).context("empty pixmap")?;
    resvg::render(
        tree,
        Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap)
}

/// Straight (not premultiplied) RGBA8 of a pixmap.
pub fn rgba(pixmap: &Pixmap) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixmap.pixels().len() * 4);
    for p in pixmap.pixels() {
        let c = p.demultiply();
        out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    out
}

/// A PNG of straight RGBA8 pixels.
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(out)
}

fn png_of(tree: &usvg::Tree, size: u32) -> Result<Vec<u8>> {
    let pixmap = render(tree, size)?;
    encode_png(pixmap.width(), pixmap.height(), &rgba(&pixmap))
}

/// An ICO file holding one PNG frame per entry (Windows Vista and later read PNG frames at any
/// size). `frames` is `(edge in pixels, PNG bytes)`.
pub fn encode_ico(frames: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * frames.len();
    for (size, png) in frames {
        let edge = if *size >= 256 { 0 } else { *size as u8 };
        out.push(edge); // width, 0 means 256
        out.push(edge); // height
        out.push(0); // palette colours
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += png.len();
    }
    for (_, png) in frames {
        out.extend_from_slice(png);
    }
    out
}

/// The frame sizes of an ICO written by [`encode_ico`].
#[cfg(test)]
fn decode_ico_sizes(ico: &[u8]) -> Option<Vec<u32>> {
    let u16_at = |o: usize| ico.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    if u16_at(0)? != 0 || u16_at(2)? != 1 {
        return None;
    }
    let count = u16_at(4)? as usize;
    (0..count)
        .map(|i| {
            let w = *ico.get(6 + 16 * i)?;
            Some(if w == 0 { 256 } else { u32::from(w) })
        })
        .collect()
}

/// Alpha-composites straight RGBA `src` over an opaque `background` colour, returning RGBA.
fn over(background: [u8; 3], src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    for p in src.chunks_exact(4) {
        let a = u32::from(p[3]);
        for c in 0..3 {
            let v = (u32::from(p[c]) * a + u32::from(background[c]) * (255 - a) + 127) / 255;
            out.push(v as u8);
        }
        out.push(255);
    }
    out
}

const PAPER: [u8; 3] = [0xf4, 0xf2, 0xec];
const SLATE: [u8; 3] = [0x1d, 0x2a, 0x30];

/// A review sheet: one row per `(drawing, background)`; each row shows the drawing at every size
/// in `sizes`, real size at the top left of its cell and enlarged by `zoom` (nearest neighbour,
/// so the pixels are visible) beside it. It is how a person checks legibility at 16, 24 and 32 px.
fn review_sheet(
    path: &str,
    rows: &[(&usvg::Tree, [u8; 3])],
    sizes: &[u32],
    zoom: u32,
) -> Result<Output> {
    let pad = 6u32;
    let biggest = sizes.iter().copied().max().unwrap_or(0);
    let width: u32 = sizes.iter().map(|s| s * (zoom + 1) + pad * 2).sum();
    let row_h = biggest * zoom + pad * 2;
    let height = row_h * rows.len() as u32;
    let mut sheet = vec![0u8; (width * height * 4) as usize];
    for (row, (tree, bg)) in rows.iter().enumerate() {
        let top = row as u32 * row_h;
        for y in 0..row_h {
            for x in 0..width {
                let i = (((top + y) * width + x) * 4) as usize;
                sheet[i..i + 4].copy_from_slice(&[bg[0], bg[1], bg[2], 255]);
            }
        }
        let mut x0 = pad;
        for &size in sizes {
            let pixmap = render(tree, size)?;
            let flat = over(*bg, &rgba(&pixmap));
            let y0 = top + pad;
            for y in 0..size {
                for x in 0..size {
                    let s = ((y * size + x) * 4) as usize;
                    let d = (((y0 + y) * width + x0 + x) * 4) as usize;
                    sheet[d..d + 4].copy_from_slice(&flat[s..s + 4]);
                }
            }
            let zx = x0 + size + 2;
            for y in 0..size * zoom {
                for x in 0..size * zoom {
                    let s = (((y / zoom) * size + x / zoom) * 4) as usize;
                    let d = (((y0 + y) * width + zx + x) * 4) as usize;
                    sheet[d..d + 4].copy_from_slice(&flat[s..s + 4]);
                }
            }
            x0 += size * (zoom + 1) + pad * 2;
        }
    }
    Ok(Output {
        path: path.to_owned(),
        bytes: encode_png(width, height, &sheet)?,
    })
}

/// The brand mark and the app icon at 16, 24 and 32 px, on paper and on slate.
fn legibility_sheet(root: &Path) -> Result<Output> {
    let master = read_svg(root, "brand/gander-lens.svg")?;
    let small = read_svg(root, "brand/gander-lens-small.svg")?;
    let reversed = read_svg(root, "brand/gander-lens-reversed.svg")?;
    let app = read_svg(root, "brand/gander-app-icon.svg")?;
    let app_small = read_svg(root, "brand/gander-app-icon-small.svg")?;
    review_sheet(
        "brand/png/legibility.png",
        &[
            (&master, PAPER),
            (&small, PAPER),
            (&reversed, SLATE),
            (&app, PAPER),
            (&app_small, PAPER),
            (&app_small, SLATE),
        ],
        &[16, 24, 32],
        8,
    )
}

/// The four tray states at 16, 20, 24 and 32 px on a light and a dark taskbar colour.
fn tray_sheet(root: &Path) -> Result<Output> {
    let trees: Vec<usvg::Tree> = TRAY_STATES
        .iter()
        .map(|s| read_svg(root, &format!("brand/tray/gander-tray-{s}.svg")))
        .collect::<Result<_>>()?;
    let rows: Vec<(&usvg::Tree, [u8; 3])> = trees
        .iter()
        .flat_map(|t| [(t, [0xf3, 0xf3, 0xf3]), (t, [0x20, 0x20, 0x20])])
        .collect();
    review_sheet("brand/png/tray-states.png", &rows, &TRAY_SIZES, 6)
}

/// Every generated file, in a stable order.
pub fn outputs(root: &Path) -> Result<Vec<Output>> {
    let mut out = Vec::new();

    // Brand PNGs.
    for (svg, name, size) in [
        ("brand/gander-lens.svg", "gander-lens-512", 512),
        (
            "brand/gander-lens-reversed.svg",
            "gander-lens-reversed-512",
            512,
        ),
        ("brand/gander-app-icon.svg", "gander-app-icon-512", 512),
        ("brand/gander-lockup.svg", "gander-lockup-1440", 1440),
        (
            "brand/gander-lockup-reversed.svg",
            "gander-lockup-reversed-1440",
            1440,
        ),
    ] {
        out.push(Output {
            path: format!("brand/png/{name}.png"),
            bytes: png_of(&read_svg(root, svg)?, size)?,
        });
    }
    out.push(legibility_sheet(root)?);
    out.push(tray_sheet(root)?);

    // The Windows icon and the window icon.
    let app = read_svg(root, "brand/gander-app-icon.svg")?;
    let app_small = read_svg(root, "brand/gander-app-icon-small.svg")?;
    let mut frames = Vec::new();
    for size in ICO_SIZES {
        let tree = if size <= SMALL_UP_TO {
            &app_small
        } else {
            &app
        };
        frames.push((size, png_of(tree, size)?));
    }
    out.push(Output {
        path: "host/gander-host/assets/gander.ico".to_owned(),
        bytes: encode_ico(&frames),
    });
    out.push(Output {
        path: "host/gander-host/assets/gander-window-64.png".to_owned(),
        bytes: png_of(&app, 64)?,
    });

    // Tray icons: four states at four sizes.
    for state in TRAY_STATES {
        let tree = read_svg(root, &format!("brand/tray/gander-tray-{state}.svg"))?;
        for size in TRAY_SIZES {
            out.push(Output {
                path: format!("host/gander-host/assets/tray/gander-tray-{state}-{size}.png"),
                bytes: png_of(&tree, size)?,
            });
        }
    }
    Ok(out)
}

/// Entry point for `cargo xtask icons [--check]`.
pub fn run(args: &[String]) -> Result<ExitCode> {
    let mut check = false;
    for a in args {
        match a.as_str() {
            "--check" => check = true,
            other => bail!("unknown argument {other}; usage: cargo xtask icons [--check]"),
        }
    }
    let root = workspace_root();
    let generated = outputs(&root)?;
    let mut stale = Vec::new();
    let mut written = 0usize;
    for o in &generated {
        let path: PathBuf = root.join(&o.path);
        let current = std::fs::read(&path).ok();
        if current.as_deref() == Some(o.bytes.as_slice()) {
            continue;
        }
        if check {
            stale.push(o.path.clone());
        } else {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, &o.bytes).with_context(|| format!("writing {}", o.path))?;
            written += 1;
        }
    }
    if check {
        if stale.is_empty() {
            println!("icons: {} generated file(s) are current", generated.len());
            return Ok(ExitCode::SUCCESS);
        }
        for s in &stale {
            eprintln!("icons: {s} differs from what the SVGs produce; run `cargo xtask icons`");
        }
        return Ok(ExitCode::from(1));
    }
    println!(
        "icons: {} generated file(s), {written} written, {} unchanged",
        generated.len(),
        generated.len() - written
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ico_header_lists_every_size() {
        let frames: Vec<(u32, Vec<u8>)> = ICO_SIZES.iter().map(|s| (*s, vec![0u8; 4])).collect();
        let ico = encode_ico(&frames);
        assert_eq!(
            decode_ico_sizes(&ico).as_deref(),
            Some(ICO_SIZES.as_slice())
        );
        assert_eq!(decode_ico_sizes(b"not an icon"), None);
    }

    #[test]
    fn rendering_twice_gives_the_same_bytes() {
        let root = workspace_root();
        let tree = read_svg(&root, "brand/gander-lens.svg").unwrap();
        assert_eq!(png_of(&tree, 48).unwrap(), png_of(&tree, 48).unwrap());
    }

    #[test]
    fn the_committed_assets_are_what_the_svgs_produce() {
        let root = workspace_root();
        for o in outputs(&root).unwrap() {
            let on_disk = std::fs::read(root.join(&o.path))
                .unwrap_or_else(|e| panic!("{}: {e}; run `cargo xtask icons`", o.path));
            assert!(
                on_disk == o.bytes,
                "{} is stale; run `cargo xtask icons`",
                o.path
            );
        }
    }

    #[test]
    fn the_committed_ico_has_every_promised_size() {
        let ico =
            std::fs::read(workspace_root().join("host/gander-host/assets/gander.ico")).unwrap();
        assert_eq!(
            decode_ico_sizes(&ico).as_deref(),
            Some(ICO_SIZES.as_slice())
        );
    }

    #[test]
    fn the_tray_states_match_the_host_order() {
        assert_eq!(TRAY_STATES, ["idle", "connected", "streaming", "paused"]);
    }
}
