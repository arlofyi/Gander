// SPDX-License-Identifier: Apache-2.0
//! Deep-zoom tile golden tests (WP-67): the `PreviewService` with the real `gander-render.exe`
//! behind it. A 6000 x 4500 photo, a rotated JPEG and a large-format PDF (an A1 sheet next to an
//! A4 page) are built from flat coloured cells; each tile is checked for its size (the pyramid
//! geometry of `gander_core::tiles`) and for its placement (the centre of every cell that falls
//! inside the tile must have that cell's colour), at several levels and on the right and bottom
//! edges where tiles are smaller.

#![cfg(windows)]

mod common;

use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{cell_color, grid_rgb, jpeg_bytes, pdf_grid_content, pdf_with_pages, png_bytes};
use common::{temp_dir, with_exif_orientation, write};
use gander_core::protocol::files::{PreviewInfo, PreviewKind};
use gander_core::tiles::{Pyramid, TILE_SIZE};
use gander_win::preview::office::OfficeEnv;
use gander_win::preview::runner::WorkerRunner;
use gander_win::preview::service::{
    InfoRequest, PreviewError, PreviewService, TileRequest, TileResponse,
};
use gander_win::render::worker::WorkerConfig;
use gander_win::thumbs::cache::ThumbCache;
use image::RgbImage;

fn service(dir: &Path) -> PreviewService<WorkerRunner> {
    service_with_cache(dir, "cache")
}

/// A service with its own cache folder (a second one over the same files has no memory of the
/// first: no info, no tiles).
fn service_with_cache(dir: &Path, cache_dir: &str) -> PreviewService<WorkerRunner> {
    let cache = Arc::new(ThumbCache::open_in(&dir.join(cache_dir), "previews", 256 << 20).unwrap());
    PreviewService::with_worker(
        cache,
        WorkerConfig::new(env!("CARGO_BIN_EXE_gander-render")),
        OfficeEnv::none(),
    )
}

fn info(svc: &PreviewService<WorkerRunner>, path: &Path) -> Result<PreviewInfo, PreviewError> {
    for _ in 0..1200 {
        let req = InfoRequest {
            file: File::open(path).unwrap(),
            ext: path.extension().unwrap().to_string_lossy().into_owned(),
            device: "dev-test".into(),
        };
        match svc.info(req) {
            Err(PreviewError::Generating { .. }) => std::thread::sleep(Duration::from_millis(50)),
            other => return other,
        }
    }
    panic!("info never finished");
}

fn tile(
    svc: &PreviewService<WorkerRunner>,
    path: &Path,
    page: u32,
    level: u32,
    x: u32,
    y: u32,
) -> Result<TileResponse, PreviewError> {
    for _ in 0..1200 {
        let req = TileRequest {
            file: File::open(path).unwrap(),
            ext: path.extension().unwrap().to_string_lossy().into_owned(),
            device: "dev-test".into(),
            page: Some(page),
            level,
            x,
            y,
        };
        match svc.tile(req) {
            Err(PreviewError::Generating { .. }) => std::thread::sleep(Duration::from_millis(50)),
            other => return other,
        }
    }
    panic!("tile never finished");
}

fn decode(r: &TileResponse) -> RgbImage {
    assert_eq!(r.content_type, "image/webp");
    assert_eq!(
        image::guess_format(&r.bytes).unwrap(),
        image::ImageFormat::WebP
    );
    image::load_from_memory(&r.bytes).unwrap().to_rgb8()
}

fn near(a: [u8; 3], b: [u8; 3], tolerance: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tolerance)
}

/// What the cells are: `cell_w` x `cell_h` top-level pixels each, `cols` x `rows` of them (the
/// area beyond is white), coloured by [`cell_color`].
struct Cells {
    cell_w: f64,
    cell_h: f64,
    cols: u32,
    rows: u32,
}

/// Samples the centre of every cell inside the tile and returns how many it checked. A tile
/// pixel is sampled where the cell centre falls inside the area the tile shows.
fn check_cells(img: &RgbImage, p: &Pyramid, level: u32, x: u32, y: u32, cells: &Cells) -> usize {
    let t = p.tile(level, x, y).unwrap();
    assert_eq!(
        img.dimensions(),
        (t.width, t.height),
        "tile {level}/{x}/{y} size"
    );
    let mut checked = 0;
    for cy in 0..cells.rows {
        for cx in 0..cells.cols {
            let (mx, my) = (
                (f64::from(cx) + 0.5) * cells.cell_w,
                (f64::from(cy) + 0.5) * cells.cell_h,
            );
            let inside = mx >= f64::from(t.src_x)
                && mx < f64::from(t.src_x + t.src_width)
                && my >= f64::from(t.src_y)
                && my < f64::from(t.src_y + t.src_height);
            if !inside {
                continue;
            }
            let px =
                ((mx - f64::from(t.src_x)) * f64::from(t.width) / f64::from(t.src_width)) as u32;
            let py =
                ((my - f64::from(t.src_y)) * f64::from(t.height) / f64::from(t.src_height)) as u32;
            let got = img.get_pixel(px.min(t.width - 1), py.min(t.height - 1)).0;
            let want = cell_color(cx, cy);
            assert!(
                near(got, want, 30),
                "tile {level}/{x}/{y}: cell ({cx},{cy}) at ({px},{py}) is {got:?}, want {want:?}"
            );
            checked += 1;
        }
    }
    checked
}

// ---------------------------------------------------------------------------------------------
// Images

#[test]
fn a_6000x4500_photo_has_golden_tile_sizes_and_placement_at_every_level() {
    let dir = temp_dir("tile-photo");
    let (w, h, cell) = (6000, 4500, 250);
    let built = Instant::now();
    let path = write(&dir, "photo.png", &png_bytes(&grid_rgb(w, h, cell)));
    eprintln!("built the {w}x{h} fixture in {:?}", built.elapsed());
    let svc = service(&dir);

    let meta = info(&svc, &path).unwrap();
    assert_eq!(meta.kind, PreviewKind::Image);
    assert_eq!((meta.width, meta.height), (Some(w), Some(h)));
    assert_eq!((meta.tile_size, meta.tile_levels), (Some(512), Some(5)));
    let pages = meta.tile_pages.unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(
        (pages[0].width, pages[0].height, pages[0].levels),
        (w, h, 5)
    );

    let p = Pyramid::new(w, h).unwrap();
    let cells = Cells {
        cell_w: f64::from(cell),
        cell_h: f64::from(cell),
        cols: w / cell,
        rows: h / cell,
    };
    let mut checked = 0;
    // The whole photo in one tile, a middle tile, and the right/bottom corner at three levels.
    for (level, x, y) in [
        (0, 0, 0),
        (1, 1, 0),
        (2, 1, 1),
        (2, 2, 2),
        (3, 5, 4),
        (4, 0, 0),
        (4, 6, 4),
        (4, 11, 8),
    ] {
        let started = Instant::now();
        let r = tile(&svc, &path, 1, level, x, y).unwrap();
        eprintln!("tile {level}/{x}/{y} of {w}x{h}: {:?}", started.elapsed());
        checked += check_cells(&decode(&r), &p, level, x, y, &cells);
    }
    assert!(checked >= 40, "only {checked} cells were checked");

    // The edge tiles are smaller than 512, never padded.
    let corner = decode(&tile(&svc, &path, 1, 4, 11, 8).unwrap());
    assert_eq!(corner.dimensions(), (368, 404));
    let mid = decode(&tile(&svc, &path, 1, 3, 5, 4).unwrap());
    assert_eq!(mid.dimensions(), (440, 202));
    let top = decode(&tile(&svc, &path, 1, 4, 0, 0).unwrap());
    assert_eq!(top.dimensions(), (TILE_SIZE, TILE_SIZE));

    // A tile that does not exist is a client error, with the worker deciding when there is no
    // info yet for the file version (a fresh service has none).
    let fresh = service_with_cache(&dir, "fresh");
    assert_eq!(
        tile(&fresh, &path, 1, 4, 12, 0).unwrap_err(),
        PreviewError::BadRequest("tile out of range")
    );
    assert_eq!(
        tile(&fresh, &path, 1, 5, 0, 0).unwrap_err(),
        PreviewError::BadRequest("tile out of range")
    );
    assert_eq!(
        tile(&fresh, &path, 2, 0, 0, 0).unwrap_err(),
        PreviewError::BadRequest("tile out of range")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_large_jpeg_is_cut_from_its_region_and_rotated_jpegs_are_placed_by_orientation() {
    let dir = temp_dir("tile-jpeg");
    // Displayed size 1600 x 1200 in 100 px cells: three levels, 4 x 3 tiles at the top.
    let (dw, dh, cell) = (1600u32, 1200u32, 100u32);
    let shown = grid_rgb(dw, dh, cell);
    let svc = service(&dir);
    let p = Pyramid::new(dw, dh).unwrap();
    let cells = Cells {
        cell_w: f64::from(cell),
        cell_h: f64::from(cell),
        cols: dw / cell,
        rows: dh / cell,
    };
    for orientation in 1..=8u16 {
        // The stored picture that EXIF orientation `orientation` displays as `shown`.
        let swapped = (5..=8).contains(&orientation);
        let (sw, sh) = if swapped { (dh, dw) } else { (dw, dh) };
        let stored = RgbImage::from_fn(sw, sh, |sx, sy| {
            let (x, y) = match orientation {
                2 => (sw - 1 - sx, sy),
                3 => (sw - 1 - sx, sh - 1 - sy),
                4 => (sx, sh - 1 - sy),
                5 => (sy, sx),
                6 => (sh - 1 - sy, sx),
                7 => (sh - 1 - sy, sw - 1 - sx),
                8 => (sy, sw - 1 - sx),
                _ => (sx, sy),
            };
            *shown.get_pixel(x, y)
        });
        let jpeg = with_exif_orientation(&jpeg_bytes(&stored), orientation);
        let path = write(&dir, &format!("o{orientation}.jpg"), &jpeg);
        let meta = info(&svc, &path).unwrap();
        assert_eq!(
            (meta.width, meta.height),
            (Some(dw), Some(dh)),
            "orientation {orientation}"
        );
        assert_eq!(meta.tile_levels, Some(3));
        let mut checked = 0;
        for (level, x, y) in [(0, 0, 0), (1, 1, 0), (2, 0, 0), (2, 3, 2), (2, 2, 1)] {
            let r = tile(&svc, &path, 1, level, x, y).unwrap();
            checked += check_cells(&decode(&r), &p, level, x, y, &cells);
        }
        assert!(checked >= 20, "orientation {orientation}: {checked} cells");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_tile_does_not_decode_the_whole_jpeg() {
    // 8192 x 6144 (50 MP) JPEG: the top-level corner tile (the codec has to read only the
    // blocks of that corner) against the whole picture at level 0 (a reduced-size decode).
    let dir = temp_dir("tile-region");
    let (w, h, cell) = (8192u32, 6144u32, 256u32);
    let path = write(&dir, "huge.jpg", &jpeg_bytes(&grid_rgb(w, h, cell)));
    let svc = service(&dir);
    let p = Pyramid::new(w, h).unwrap();
    let cells = Cells {
        cell_w: f64::from(cell),
        cell_h: f64::from(cell),
        cols: w / cell,
        rows: h / cell,
    };
    let top = p.levels() - 1;
    let started = Instant::now();
    let corner = tile(&svc, &path, 1, top, 0, 0).unwrap();
    let corner_time = started.elapsed();
    check_cells(&decode(&corner), &p, top, 0, 0, &cells);
    let started = Instant::now();
    let whole = tile(&svc, &path, 1, 0, 0, 0).unwrap();
    let whole_time = started.elapsed();
    check_cells(&decode(&whole), &p, 0, 0, 0, &cells);
    let last = p.grid(top).unwrap();
    let started = Instant::now();
    let far = tile(&svc, &path, 1, top, last.0 - 1, last.1 - 1).unwrap();
    let far_time = started.elapsed();
    check_cells(&decode(&far), &p, top, last.0 - 1, last.1 - 1, &cells);
    eprintln!(
        "50 MP JPEG: top-left tile {corner_time:?}, whole picture at level 0 {whole_time:?}, \
         bottom-right tile {far_time:?}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------------------------
// PDF

/// A1 sheet (1684 x 2384 points) of 100 pt cells, then an A4 page (595 x 842) of 100 pt cells.
fn large_format_pdf() -> Vec<u8> {
    pdf_with_pages(&[
        (1684, 2384, pdf_grid_content(2384, 100, 16, 23)),
        (595, 842, pdf_grid_content(842, 100, 5, 8)),
    ])
}

#[test]
fn a_large_format_pdf_has_golden_tile_sizes_and_placement_per_page() {
    let dir = temp_dir("tile-pdf");
    let path = write(&dir, "sheets.pdf", &large_format_pdf());
    let svc = service(&dir);

    let meta = info(&svc, &path).unwrap();
    assert_eq!((meta.kind, meta.pages), (PreviewKind::Pdf, Some(2)));
    assert_eq!(meta.tile_size, Some(512));
    let pages = meta.tile_pages.clone().unwrap();
    assert_eq!(pages.len(), 2);
    // 300 dpi: 1684 pt = 7016.7 px, 2384 pt = 9933.3 px; 595 pt = 2479.2, 842 pt = 3508.3.
    let near_px = |got: u32, want: u32| got.abs_diff(want) <= 1;
    assert!(
        near_px(pages[0].width, 7017) && near_px(pages[0].height, 9933),
        "{pages:?}"
    );
    assert!(
        near_px(pages[1].width, 2479) && near_px(pages[1].height, 3508),
        "{pages:?}"
    );
    assert_eq!((pages[0].levels, pages[1].levels), (6, 4));
    assert_eq!(meta.tile_levels, Some(6));

    for (index, (pt_w, pt_h, cols, rows)) in [(1684.0, 2384.0, 16, 23), (595.0, 842.0, 5, 8)]
        .into_iter()
        .enumerate()
    {
        let page = index as u32 + 1;
        let tp = pages[index];
        let p = Pyramid::new(tp.width, tp.height).unwrap();
        let cells = Cells {
            cell_w: 100.0 * f64::from(tp.width) / pt_w,
            cell_h: 100.0 * f64::from(tp.height) / pt_h,
            cols,
            rows,
        };
        let top = p.levels() - 1;
        let (gx, gy) = p.grid(top).unwrap();
        let mut wanted = vec![
            (0, 0, 0),
            (top, 0, 0),
            (top, gx - 1, gy - 1),
            (top, gx / 2, gy / 2),
        ];
        if top >= 2 {
            wanted.push((top - 1, 1, 1));
            wanted.push((top - 2, p.grid(top - 2).unwrap().0 - 1, 0));
        }
        let mut checked = 0;
        for (level, x, y) in wanted {
            let started = Instant::now();
            let r = tile(&svc, &path, page, level, x, y).unwrap();
            eprintln!(
                "PDF page {page} ({}x{} px) tile {level}/{x}/{y}: {:?}",
                tp.width,
                tp.height,
                started.elapsed()
            );
            checked += check_cells(&decode(&r), &p, level, x, y, &cells);
        }
        assert!(
            checked >= 12,
            "page {page}: only {checked} cells were checked"
        );
    }

    // The bottom-right tile of the A1 sheet is smaller than 512 on both axes.
    let p = Pyramid::new(pages[0].width, pages[0].height).unwrap();
    let (gx, gy) = p.grid(5).unwrap();
    let t = p.tile(5, gx - 1, gy - 1).unwrap();
    assert!(t.width < TILE_SIZE && t.height < TILE_SIZE);
    let img = decode(&tile(&svc, &path, 1, 5, gx - 1, gy - 1).unwrap());
    assert_eq!(img.dimensions(), (t.width, t.height));

    // Pages and tiles that do not exist.
    assert_eq!(
        tile(&svc, &path, 3, 0, 0, 0).unwrap_err(),
        PreviewError::BadRequest("tile out of range")
    );
    let fresh = service_with_cache(&dir, "fresh");
    assert_eq!(
        tile(&fresh, &path, 1, 5, gx, 0).unwrap_err(),
        PreviewError::BadRequest("tile out of range")
    );
    assert_eq!(
        tile(&fresh, &path, 2, 4, 0, 0).unwrap_err(),
        PreviewError::BadRequest("tile out of range")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_hairline_is_resolved_in_the_top_level_tile() {
    // The point of deep zoom: a 2048 px wide page of the A1 sheet shows a 100 pt cell edge over
    // about 120 px; the top-level tile has 417 px per cell, so a thin line drawn across the
    // sheet is resolved by the tile and smeared or lost by the page.
    let line = "0 0 0 rg 0 1190 1684 0.5 re f\n"; // a hairline, half a point thick
    let path_pdf = pdf_with_pages(&[(1684, 2384, line.to_owned())]);
    let dir = temp_dir("tile-sharp");
    let path = write(&dir, "hairline.pdf", &path_pdf);
    let svc = service(&dir);
    let meta = info(&svc, &path).unwrap();
    let top = meta.tile_pages.unwrap()[0];
    let p = Pyramid::new(top.width, top.height).unwrap();
    // The line sits at pt y = 2384 - 1190.5 = 1193.5 from the top: top-level row ~ 4973.
    let row = (1193.5 * f64::from(top.height) / 2384.0) as u32;
    let (level, ty) = (p.levels() - 1, row / TILE_SIZE);
    let r = tile(&svc, &path, 1, level, 3, ty).unwrap();
    let img = decode(&r);
    let local = row - ty * TILE_SIZE;
    // Darkest pixel in the rows around the line, at full tile resolution.
    let darkest = (local.saturating_sub(4)..(local + 5).min(img.height()))
        .map(|y| img.get_pixel(200, y).0[0])
        .min()
        .unwrap();
    assert!(
        darkest < 90,
        "the hairline is resolved in the tile: {darkest}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_thousand_page_pdf_lists_tile_sizes_for_its_first_512_pages_within_the_info_wait() {
    let pages: Vec<(u32, u32, String)> = (0..1000)
        .map(|i| {
            // Every 100th page is a different size, so the list is not trivially uniform.
            let (w, h) = if i % 100 == 99 {
                (1684, 2384)
            } else {
                (595, 842)
            };
            (w, h, "0 0 0 rg 10 10 50 50 re f".to_owned())
        })
        .collect();
    let dir = temp_dir("tile-1000");
    let path = write(&dir, "thousand.pdf", &pdf_with_pages(&pages));
    let svc = service(&dir);
    let started = Instant::now();
    let meta = info(&svc, &path).unwrap();
    eprintln!("info of a 1000 page PDF: {:?}", started.elapsed());
    assert_eq!(meta.pages, Some(1000));
    let listed = meta.tile_pages.unwrap();
    assert_eq!(listed.len(), 512, "the list is bounded");
    assert_eq!(listed[0].levels, 4);
    assert_eq!(listed[99].levels, 6, "page 100 is the large sheet");
    // A page past the list still has tiles when asked for: the worker is authoritative.
    let r = tile(&svc, &path, 700, 0, 0, 0).unwrap();
    assert!(decode(&r).width() <= TILE_SIZE);
    std::fs::remove_dir_all(&dir).unwrap();
}
