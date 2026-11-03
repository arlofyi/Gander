// SPDX-License-Identifier: Apache-2.0
//! PDF thumbnails, preview pages and metadata through `Windows.Data.Pdf` (in the box since
//! Windows 10).
//!
//! The bytes are copied from the inherited handle into an in-memory WinRT stream (the API takes
//! a stream, and the worker has no path). A page is rendered to PNG at the target size by the
//! OS and decoded by WIC. A password-protected document is "unsupported"; anything the parser
//! rejects is a decoder error.

use std::fs::File;

use gander_core::tiles::{MAX_TILE_PAGES, Pyramid, pdf_top_size};
use gander_win::render::job::{PAGE_OUT_OF_RANGE_MSG, PreviewMeta, TILE_OUT_OF_RANGE_MSG};
use image::RgbaImage;
use windows::Data::Pdf::{PdfDocument, PdfPage, PdfPageRenderOptions};
use windows::Foundation::Rect;
use windows::Storage::Streams::{DataReader, DataWriter, InMemoryRandomAccessStream};

use crate::{Failure, Rendered, Target, cap_pixels, fit, read_all, wic};

/// Tallest page image a preview may ask the OS for (pixels).
const MAX_PAGE_HEIGHT: u32 = 8192;

/// Thumbnail of page 1.
pub fn thumb(file: &File, size: u32) -> Rendered<RgbaImage> {
    let bytes = read_all(file)?;
    let (png, _) = render(&bytes, 0, Target::Box(size)).map_err(into_failure)?;
    wic::decode_bytes(&png, size, Some(1))
}

/// Preview page `page` (1-based) rendered `width` pixels wide.
pub fn page(file: &File, page: u32, width: u32) -> Rendered<RgbaImage> {
    let bytes = read_all(file)?;
    page_from_bytes(&bytes, page, width)
}

/// [`page`] for a document already in memory.
pub fn page_from_bytes(bytes: &[u8], page: u32, width: u32) -> Rendered<RgbaImage> {
    let (png, (tw, th)) =
        render(bytes, page.saturating_sub(1), Target::Width(width)).map_err(into_failure)?;
    // The box only needs to be large enough not to scale.
    let img = wic::decode_bytes(&png, 16_384, Some(1))?;
    // The renderer may not honour the size to the pixel (see `render_scale`); a preview is
    // exactly as wide as asked.
    Ok(if img.dimensions() == (tw, th) {
        img
    } else {
        image::imageops::resize(&img, tw, th, image::imageops::FilterType::Lanczos3)
    })
}

/// Page count and page 1 size (Windows.Data.Pdf units, 1/96 inch).
pub fn info(file: &File) -> Rendered<PreviewMeta> {
    let bytes = read_all(file)?;
    let doc = open(&bytes).map_err(into_failure)?;
    let pages = doc.PageCount().map_err(|e| into_failure(e.into()))?;
    let first = doc
        .GetPage(0)
        .and_then(|p| p.Size())
        .map_err(|e| into_failure(e.into()))?;
    Ok(PreviewMeta {
        pages: Some(pages),
        width: Some(first.Width.max(1.0).round() as u32),
        height: Some(first.Height.max(1.0).round() as u32),
        tile_pages: tile_pages(&doc, pages),
    })
}

#[derive(Debug)]
enum PdfError {
    Locked,
    OutOfRange,
    Win(windows::core::Error),
}

impl From<windows::core::Error> for PdfError {
    fn from(e: windows::core::Error) -> Self {
        PdfError::Win(e)
    }
}

fn into_failure(e: PdfError) -> Failure {
    match e {
        PdfError::Locked => Failure::unsupported("password-protected PDF"),
        PdfError::OutOfRange => Failure::decode(PAGE_OUT_OF_RANGE_MSG),
        PdfError::Win(e) => Failure::Decode(format!("PDF: {:?}", e.code())),
    }
}

fn open(bytes: &[u8]) -> Result<PdfDocument, PdfError> {
    if !bytes.starts_with(b"%PDF")
        && !bytes[..bytes.len().min(1024)]
            .windows(4)
            .any(|w| w == b"%PDF")
    {
        return Err(PdfError::Win(windows::core::Error::from_hresult(
            windows::core::HRESULT(0x8000_4005_u32 as i32),
        )));
    }
    let input = InMemoryRandomAccessStream::new()?;
    {
        let writer = DataWriter::CreateDataWriter(&input)?;
        writer.WriteBytes(bytes)?;
        writer.StoreAsync()?.join()?;
        writer.FlushAsync()?.join()?;
        writer.DetachStream()?;
    }
    input.Seek(0)?;
    let doc = PdfDocument::LoadFromStreamAsync(&input)?.join()?;
    if doc.IsPasswordProtected()? {
        return Err(PdfError::Locked);
    }
    if doc.PageCount()? == 0 {
        return Err(PdfError::Win(windows::core::Error::from_hresult(
            windows::core::HRESULT(0x8000_4005_u32 as i32),
        )));
    }
    Ok(doc)
}

/// Pixel size to render `page` at for `target`.
fn target_size(page: &PdfPage, target: Target) -> Result<(u32, u32), PdfError> {
    let dims = page.Size()?;
    let (pw, ph) = (dims.Width.max(1.0) as u32, dims.Height.max(1.0) as u32);
    Ok(match target {
        // Thumbnails render at the target box even for small pages (vector content scales up
        // cleanly).
        Target::Box(size) => {
            if pw.max(ph) < size {
                let scale = f64::from(size) / f64::from(pw.max(ph));
                (
                    ((f64::from(pw) * scale) as u32).max(1),
                    ((f64::from(ph) * scale) as u32).max(1),
                )
            } else {
                fit(pw, ph, size)
            }
        }
        // Preview pages render at exactly the requested width (vector content, so enlarging a
        // small page is fine), bounded in height and total pixels.
        Target::Width(width) => {
            // From the exact page size: truncating it to whole units skews the aspect ratio.
            let (exact_w, exact_h) = (
                f64::from(dims.Width.max(1.0)),
                f64::from(dims.Height.max(1.0)),
            );
            let height = (f64::from(width) * exact_h / exact_w).round();
            let height = (height as u32).max(1);
            let (w, h) = cap_pixels(width, height);
            if h > MAX_PAGE_HEIGHT {
                let scale = f64::from(MAX_PAGE_HEIGHT) / f64::from(h);
                (((f64::from(w) * scale) as u32).max(1), MAX_PAGE_HEIGHT)
            } else {
                (w, h)
            }
        }
    })
}

/// Width of the probe render that measures the renderer's scale.
const PROBE_WIDTH: u32 = 64;

/// Renders `page` to PNG bytes with the destination size set to `w` x `h`.
fn render_png(page: &PdfPage, w: u32, h: u32) -> Result<Vec<u8>, PdfError> {
    render_png_rect(page, None, w, h)
}

/// [`render_png`] for the part of the page inside `source` (page units), when given.
fn render_png_rect(
    page: &PdfPage,
    source: Option<Rect>,
    w: u32,
    h: u32,
) -> Result<Vec<u8>, PdfError> {
    let options = PdfPageRenderOptions::new()?;
    if let Some(rect) = source {
        options.SetSourceRect(rect)?;
    }
    options.SetDestinationWidth(w)?;
    options.SetDestinationHeight(h)?;
    let output = InMemoryRandomAccessStream::new()?;
    page.RenderWithOptionsToStreamAsync(&output, &options)?
        .join()?;
    let len = u32::try_from(output.Size()?).unwrap_or(u32::MAX);
    let reader = DataReader::CreateDataReader(&output.GetInputStreamAt(0)?)?;
    reader.LoadAsync(len)?.join()?;
    let mut png = vec![0u8; len as usize];
    reader.ReadBytes(&mut png)?;
    Ok(png)
}

/// Pixel size from a PNG's IHDR chunk.
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    let b = png.get(16..24)?;
    Some((
        u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        u32::from_be_bytes([b[4], b[5], b[6], b[7]]),
    ))
}

/// How many output pixels the OS renderer produces per requested pixel. It is 1.0 in principle,
/// but on the development machine (Windows 11, display scale 140 %) a page asked for at 400 px
/// came back 560 px wide, at every size and whether or not the process declared itself DPI
/// aware. A small probe render measures it so the real render can ask for the right size
/// instead of producing, and then shrinking, an oversized bitmap inside the memory cap.
fn render_scale(page: &PdfPage) -> Result<f64, PdfError> {
    let dims = page.Size()?;
    let (pw, ph) = (dims.Width.max(1.0), dims.Height.max(1.0));
    let h = ((f64::from(PROBE_WIDTH) * f64::from(ph) / f64::from(pw)).round() as u32).max(1);
    let png = render_png(page, PROBE_WIDTH, h)?;
    let scale = png_size(&png).map_or(1.0, |(w, _)| f64::from(w) / f64::from(PROBE_WIDTH));
    // Ignore noise and anything absurd.
    Ok(
        if (scale - 1.0).abs() < 0.02 || !(0.25..=4.0).contains(&scale) {
            1.0
        } else {
            scale
        },
    )
}

/// Renders page `index` (0-based) to PNG bytes; also returns the pixel size the caller asked
/// for (the PNG itself can differ for preview pages, see [`render_scale`]).
fn render(bytes: &[u8], index: u32, target: Target) -> Result<(Vec<u8>, (u32, u32)), PdfError> {
    let doc = open(bytes)?;
    if index >= doc.PageCount()? {
        return Err(PdfError::OutOfRange);
    }
    let page = doc.GetPage(index)?;
    let (tw, th) = target_size(&page, target)?;
    let (rw, rh) = match target {
        Target::Box(_) => (tw, th),
        Target::Width(_) => {
            let scale = render_scale(&page)?;
            (
                ((f64::from(tw) / scale).round() as u32).max(1),
                ((f64::from(th) / scale).round() as u32).max(1),
            )
        }
    };
    Ok((render_png(&page, rw, rh)?, (tw, th)))
}

/// Top-level tile size of each page, as many leading pages as can be measured (at most
/// [`MAX_TILE_PAGES`]). The list stops at the first page that cannot be measured so that an
/// index always means the same page.
fn tile_pages(doc: &PdfDocument, pages: u32) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for index in 0..pages.min(MAX_TILE_PAGES as u32) {
        let Ok(size) = doc.GetPage(index).and_then(|p| p.Size()) else {
            break;
        };
        match pdf_top_size(f64::from(size.Width), f64::from(size.Height)) {
            Some(top) => out.push(top),
            None => break,
        }
    }
    out
}

/// Deep-zoom tile `(level, x, y)` of page `page` (1-based) of the PDF behind `file`.
pub fn tile(file: &File, page: u32, level: u32, x: u32, y: u32) -> Rendered<RgbaImage> {
    let bytes = read_all(file)?;
    tile_from_bytes(&bytes, page, level, x, y)
}

/// [`tile`] for a document already in memory. Only the tile's area of the page is rendered:
/// `PdfPageRenderOptions.SourceRect` (in the page's 1/96 inch units) selects it and the
/// destination size is the tile size.
pub fn tile_from_bytes(bytes: &[u8], page: u32, level: u32, x: u32, y: u32) -> Rendered<RgbaImage> {
    let doc = open(bytes).map_err(into_failure)?;
    let count = doc.PageCount().map_err(|e| into_failure(e.into()))?;
    if page == 0 || page > count {
        return Err(Failure::decode(PAGE_OUT_OF_RANGE_MSG));
    }
    let pdf_page = doc.GetPage(page - 1).map_err(|e| into_failure(e.into()))?;
    let dims = pdf_page.Size().map_err(|e| into_failure(e.into()))?;
    let (dip_w, dip_h) = (f64::from(dims.Width), f64::from(dims.Height));
    let (top_w, top_h) = pdf_top_size(dip_w, dip_h)
        .ok_or_else(|| Failure::unsupported("page has no usable size"))?;
    let pyramid =
        Pyramid::new(top_w, top_h).ok_or_else(|| Failure::unsupported("page too large to tile"))?;
    let tile = pyramid
        .tile(level, x, y)
        .ok_or_else(|| Failure::decode(TILE_OUT_OF_RANGE_MSG))?;
    // Top-level pixels to the page's own units.
    let (kx, ky) = (dip_w / f64::from(top_w), dip_h / f64::from(top_h));
    let rect = Rect {
        X: (f64::from(tile.src_x) * kx) as f32,
        Y: (f64::from(tile.src_y) * ky) as f32,
        Width: (f64::from(tile.src_width) * kx) as f32,
        Height: (f64::from(tile.src_height) * ky) as f32,
    };
    render_tile(&pdf_page, rect, tile.width, tile.height)
}

/// Renders `rect` of `page` as a `tw` x `th` image. The OS renderer does not always honour the
/// destination size to the pixel (see [`render_scale`]): an oversized result is scaled down, a
/// too small one is rendered again asking for proportionally more.
fn render_tile(page: &PdfPage, rect: Rect, tw: u32, th: u32) -> Rendered<RgbaImage> {
    let (mut rw, mut rh) = (tw, th);
    for attempt in 0..2 {
        let png = render_png_rect(page, Some(rect), rw, rh).map_err(into_failure)?;
        let got = png_size(&png).unwrap_or((rw, rh));
        // Close enough: a pixel either way is the renderer rounding.
        let fits = got.0.abs_diff(tw) <= 1 && got.1.abs_diff(th) <= 1;
        if !fits && attempt == 0 && (got.0 < tw || got.1 < th) && got.0 > 0 && got.1 > 0 {
            let scale = f64::min(
                f64::from(got.0) / f64::from(rw),
                f64::from(got.1) / f64::from(rh),
            );
            rw = ((f64::from(tw) / scale).round() as u32).clamp(1, 4096);
            rh = ((f64::from(th) / scale).round() as u32).clamp(1, 4096);
            continue;
        }
        let img = wic::decode_bytes(&png, 16_384, Some(1))?;
        return Ok(if img.dimensions() == (tw, th) {
            img
        } else {
            image::imageops::resize(&img, tw, th, image::imageops::FilterType::Lanczos3)
        });
    }
    Err(Failure::decode("PDF tile render did not settle on a size"))
}
