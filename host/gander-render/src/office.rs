// SPDX-License-Identifier: Apache-2.0
//! Office/ODF thumbnails from the thumbnail the author's application embedded in the zip.
//!
//! OOXML: the package relationship of type `.../metadata/thumbnail` in `_rels/.rels` (usually
//! `docProps/thumbnail.jpeg`, sometimes `.emf` or `.wmf`). ODF: `Thumbnails/thumbnail.png`.
//! JPEG/PNG go through WIC; EMF is played into a bitmap with GDI. WMF and a missing thumbnail are
//! "unsupported" (no page rendering here; page rendering is [`crate::office_com`]).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use gander_win::render::job::PreviewMeta;
use image::RgbaImage;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteEnhMetaFile, DeleteObject, ENHMETAHEADER, GdiFlush, GetEnhMetaFileHeader,
    HGDIOBJ, PatBlt, PlayEnhMetaFile, SelectObject, SetEnhMetaFileBits, WHITENESS,
};

use crate::{Failure, Rendered, fit, wic};

/// Largest embedded thumbnail read (uncompressed).
const MAX_THUMB_BYTES: u64 = 16 * 1024 * 1024;
/// Most bytes of `_rels/.rels` read.
const MAX_RELS_BYTES: u64 = 256 * 1024;
const THUMB_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail";

/// Thumbnail for an Office/ODF container.
pub fn thumb(file: &File, size: u32) -> Rendered<RgbaImage> {
    let mut f = file;
    f.seek(SeekFrom::Start(0)).map_err(Failure::decode)?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| match e {
        zip::result::ZipError::InvalidArchive(_) => Failure::decode(e),
        other => Failure::decode(other),
    })?;
    let (name, bytes) = find_thumbnail(&mut zip)?;
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".emf") {
        render_emf(&bytes, size)
    } else if lower.ends_with(".wmf") {
        Err(Failure::unsupported("WMF thumbnails are not rendered"))
    } else {
        wic::decode_bytes(&bytes, size, None)
    }
}

/// Most bytes of `ppt/presentation.xml` read for the slide size.
const MAX_PRESENTATION_BYTES: u64 = 1024 * 1024;
/// EMU (English Metric Units) per CSS pixel at 96 dpi.
const EMU_PER_PX: u64 = 9525;

/// Cheap metadata of an OOXML container: a presentation's slide count and slide size. Word and
/// Excel containers do not know their page count without layout, so they report nothing.
pub fn info(file: &File, ext: &str) -> Rendered<PreviewMeta> {
    let mut f = file;
    f.seek(SeekFrom::Start(0)).map_err(Failure::decode)?;
    let mut zip = zip::ZipArchive::new(f).map_err(Failure::decode)?;
    if ext != "pptx" {
        return Ok(PreviewMeta::default());
    }
    let slides = zip
        .file_names()
        .filter(|n| n.as_ref().is_ok_and(|n| is_slide_part(n)))
        .count()
        .try_into()
        .ok();
    let size = zip.by_name("ppt/presentation.xml").ok().and_then(|entry| {
        let mut xml = String::new();
        entry
            .take(MAX_PRESENTATION_BYTES)
            .read_to_string(&mut xml)
            .ok()?;
        slide_size(&xml)
    });
    Ok(PreviewMeta {
        pages: slides,
        width: size.map(|s| s.0),
        height: size.map(|s| s.1),
        tile_pages: Vec::new(),
    })
}

/// `ppt/slides/slide<N>.xml` (not the `_rels` folder).
fn is_slide_part(name: &str) -> bool {
    name.strip_prefix("ppt/slides/slide")
        .and_then(|rest| rest.strip_suffix(".xml"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Slide size in pixels (96 dpi) from the `p:sldSz` element of `presentation.xml`.
fn slide_size(xml: &str) -> Option<(u32, u32)> {
    let element = xml.split('<').find(|e| e.starts_with("p:sldSz"))?;
    let attr = |name: &str| -> Option<u64> {
        let key = format!("{name}=\"");
        let start = element.find(&key)? + key.len();
        let end = start + element[start..].find('"')?;
        element[start..end].parse().ok()
    };
    let (cx, cy) = (attr("cx")?, attr("cy")?);
    Some((
        u32::try_from(cx / EMU_PER_PX).ok()?.max(1),
        u32::try_from(cy / EMU_PER_PX).ok()?.max(1),
    ))
}

/// Name and bytes of the embedded thumbnail, or "unsupported" when there is none.
pub fn find_thumbnail<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> Rendered<(String, Vec<u8>)> {
    let mut candidates = Vec::new();
    if let Some(target) = rels_thumbnail_target(zip) {
        candidates.push(target);
    }
    for c in [
        "docProps/thumbnail.jpeg",
        "docProps/thumbnail.jpg",
        "docProps/thumbnail.png",
        "docProps/thumbnail.emf",
        "docProps/thumbnail.wmf",
        "Thumbnails/thumbnail.png",
    ] {
        candidates.push(c.to_owned());
    }
    for name in candidates {
        let Ok(entry) = zip.by_name(&name) else {
            continue;
        };
        if entry.size() > MAX_THUMB_BYTES {
            return Err(Failure::decode("embedded thumbnail too large"));
        }
        let mut bytes = Vec::new();
        entry
            .take(MAX_THUMB_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(Failure::decode)?;
        if bytes.len() as u64 > MAX_THUMB_BYTES {
            return Err(Failure::decode("embedded thumbnail too large"));
        }
        return Ok((name, bytes));
    }
    Err(Failure::unsupported("no embedded thumbnail"))
}

/// The thumbnail target named in `_rels/.rels`, normalised to a zip entry name.
fn rels_thumbnail_target<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> Option<String> {
    let entry = zip.by_name("_rels/.rels").ok()?;
    let mut xml = String::new();
    entry.take(MAX_RELS_BYTES).read_to_string(&mut xml).ok()?;
    // A tiny scan rather than an XML parser: find the Relationship element with the thumbnail
    // type and read its Target attribute.
    for element in xml.split('<').filter(|e| e.starts_with("Relationship ")) {
        if !element.contains(THUMB_REL_TYPE) {
            continue;
        }
        let start = element.find("Target=\"")? + "Target=\"".len();
        let end = start + element[start..].find('"')?;
        let target = element[start..end].trim_start_matches('/');
        if target.contains("..") || target.contains(':') {
            return None;
        }
        return Some(target.to_owned());
    }
    None
}

/// Plays an EMF into a white bitmap that fits `size`.
fn render_emf(bytes: &[u8], size: u32) -> Rendered<RgbaImage> {
    // SAFETY: GDI calls on objects created here and released on every path; the DIB section's
    // bits pointer is valid while the bitmap exists and is read only after GdiFlush.
    unsafe {
        let emf = SetEnhMetaFileBits(bytes);
        if emf.is_invalid() {
            return Err(Failure::decode("invalid EMF"));
        }
        let mut header = ENHMETAHEADER::default();
        let got = GetEnhMetaFileHeader(
            emf,
            std::mem::size_of::<ENHMETAHEADER>() as u32,
            Some(&mut header),
        );
        let bounds = header.rclBounds;
        let (bw, bh) = (
            (bounds.right - bounds.left + 1).max(1),
            (bounds.bottom - bounds.top + 1).max(1),
        );
        if got == 0 || bw > 20_000 || bh > 20_000 {
            let _ = DeleteEnhMetaFile(Some(emf));
            return Err(Failure::decode("invalid EMF header"));
        }
        let (w, h) = fit(bw as u32, bh as u32, size.max(256));
        let dc = CreateCompatibleDC(None);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bitmap = match CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(b) if !bits.is_null() => b,
            _ => {
                let _ = DeleteDC(dc);
                let _ = DeleteEnhMetaFile(Some(emf));
                return Err(Failure::decode("cannot allocate bitmap"));
            }
        };
        let old = SelectObject(dc, HGDIOBJ(bitmap.0));
        let _ = PatBlt(dc, 0, 0, w as i32, h as i32, WHITENESS);
        let rect = RECT {
            left: 0,
            top: 0,
            right: w as i32,
            bottom: h as i32,
        };
        let played = PlayEnhMetaFile(dc, emf, &rect).as_bool();
        let _ = GdiFlush();
        let len = w as usize * h as usize * 4;
        let bgra = std::slice::from_raw_parts(bits.cast::<u8>(), len).to_vec();
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(dc);
        let _ = DeleteEnhMetaFile(Some(emf));
        if !played {
            return Err(Failure::decode("EMF playback failed"));
        }
        let mut rgba = bgra;
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        RgbaImage::from_raw(w, h, rgba).ok_or_else(|| Failure::decode("buffer size"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slide_parts_and_size() {
        assert!(is_slide_part("ppt/slides/slide1.xml"));
        assert!(is_slide_part("ppt/slides/slide12.xml"));
        assert!(!is_slide_part("ppt/slides/_rels/slide1.xml.rels"));
        assert!(!is_slide_part("ppt/slideLayouts/slideLayout1.xml"));
        assert!(!is_slide_part("ppt/slides/slide.xml"));
        let xml = r#"<p:presentation><p:sldSz cx="12192000" cy="6858000"/></p:presentation>"#;
        assert_eq!(slide_size(xml), Some((1280, 720)));
        assert_eq!(slide_size("<p:presentation/>"), None);
    }
}
