// SPDX-License-Identifier: Apache-2.0
//! Preview golden tests: the `PreviewService` with the real `gander-render.exe` behind it.
//!
//! Fixtures are generated in `common` (a 3-page PDF, a rotated JPEG, UTF-16 text, decks with and
//! without an embedded thumbnail); outputs are compared by dimensions, sampled pixels and a
//! perceptual hash, never by bytes. Office COM is covered separately in `office_com.rs`.

#![cfg(windows)]

mod common;

use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{assert_similar, encode, temp_dir, test_card, with_exif_orientation, write};
use gander_core::protocol::files::{ImageFormat, PreviewKind};
use gander_win::preview::office::OfficeEnv;
use gander_win::preview::runner::{JobResult, JobRunner, WorkerRunner};
use gander_win::preview::service::{
    InfoRequest, PageRequest, PageResponse, PreviewError, PreviewService, PreviewServiceConfig,
    TextRequest,
};
use gander_win::render::job::{RenderJob, RenderKind, RenderOp};
use gander_win::render::worker::WorkerConfig;
use gander_win::thumbs::cache::ThumbCache;
use image::{DynamicImage, GenericImageView, ImageFormat as ImgFormat};

fn worker() -> WorkerConfig {
    WorkerConfig::new(env!("CARGO_BIN_EXE_gander-render"))
}

fn cache(dir: &Path) -> Arc<ThumbCache> {
    Arc::new(ThumbCache::open_in(&dir.join("cache"), "previews", 256 * 1024 * 1024).unwrap())
}

fn service(dir: &Path, office: OfficeEnv) -> PreviewService<WorkerRunner> {
    PreviewService::with_worker(cache(dir), worker(), office)
}

fn page_req(path: &Path) -> PageRequest {
    PageRequest {
        file: File::open(path).unwrap(),
        ext: path.extension().unwrap().to_string_lossy().into_owned(),
        device: "dev-test".into(),
        page: None,
        width: None,
        format: ImageFormat::Webp,
    }
}

fn page<R: JobRunner>(
    svc: &PreviewService<R>,
    path: &Path,
    page: u32,
    width: u32,
) -> Result<PageResponse, PreviewError> {
    // Poll like a client honouring `retry_after_ms`.
    for _ in 0..600 {
        let mut req = page_req(path);
        req.page = Some(page);
        req.width = Some(width);
        match svc.page(req) {
            Err(PreviewError::Generating { .. }) => std::thread::sleep(Duration::from_millis(50)),
            other => return other,
        }
    }
    panic!("preview never finished");
}

fn info<R: JobRunner>(
    svc: &PreviewService<R>,
    path: &Path,
) -> Result<gander_core::protocol::files::PreviewInfo, PreviewError> {
    for _ in 0..600 {
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

fn decode(r: &PageResponse) -> DynamicImage {
    image::load_from_memory(&r.bytes).unwrap()
}

fn luma(img: &DynamicImage, x: u32, y: u32) -> u8 {
    img.to_luma8().get_pixel(x, y).0[0]
}

// ---------------------------------------------------------------------------------------------
// PDF

#[test]
fn three_page_pdf_renders_each_page_at_the_requested_width() {
    let dir = temp_dir("pdf3");
    let path = write(&dir, "three.pdf", &common::pdf_pages(3));
    let svc = service(&dir, OfficeEnv::none());

    let meta = info(&svc, &path).unwrap();
    assert_eq!(meta.kind, PreviewKind::Pdf);
    assert_eq!(meta.pages, Some(3));
    // 400 x 200 points at 96 dpi.
    assert_eq!((meta.width, meta.height), (Some(533), Some(267)));
    assert_eq!(meta.renderer, "windows_pdf");
    assert_eq!(meta.text_bytes, None);

    let p1 = page(&svc, &path, 1, 400).unwrap();
    let p2 = page(&svc, &path, 2, 400).unwrap();
    let p3 = page(&svc, &path, 3, 400).unwrap();
    for (n, r) in [(1, &p1), (2, &p2), (3, &p3)] {
        let img = decode(r);
        assert_eq!(img.dimensions(), (400, 200), "page {n}");
        assert_eq!(r.content_type, "image/webp");
        assert_eq!(
            (r.page, r.pages, r.renderer, r.degraded),
            (n, Some(3), "windows_pdf", false)
        );
    }
    let (i1, i2, i3) = (decode(&p1), decode(&p2), decode(&p3));
    // Page 1: black left half. Page 2: black top half. Page 3: a black square in the middle.
    assert!(
        luma(&i1, 50, 100) < 60 && luma(&i1, 350, 100) > 200,
        "page 1"
    );
    assert!(
        luma(&i2, 200, 50) < 60 && luma(&i2, 200, 150) > 200,
        "page 2"
    );
    assert!(
        luma(&i3, 200, 100) < 60 && luma(&i3, 30, 30) > 200 && luma(&i3, 370, 170) > 200,
        "page 3"
    );
    assert_ne!(p1.etag, p2.etag);

    // A different width is a different render; a width over the maximum is clamped to 2048.
    let wide = page(&svc, &path, 1, 9000).unwrap();
    assert_eq!(decode(&wide).dimensions(), (2048, 1024));
    let narrow = page(&svc, &path, 1, 200).unwrap();
    assert_eq!(decode(&narrow).dimensions(), (200, 100));

    // The document ends after page 3.
    assert_eq!(
        page(&svc, &path, 4, 400).unwrap_err(),
        PreviewError::BadRequest("page out of range")
    );
    assert_eq!(page(&svc, &path, 4, 400).unwrap_err().http_status(), 400);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_page_past_the_end_is_a_client_error_even_without_prior_info() {
    // No `info` call first: the worker itself says "page out of range".
    let dir = temp_dir("pdfoor");
    let path = write(&dir, "three.pdf", &common::pdf_pages(3));
    let svc = service(&dir, OfficeEnv::none());
    let e = page(&svc, &path, 9, 300).unwrap_err();
    assert_eq!(e, PreviewError::BadRequest("page out of range"));
    // And the failure is remembered per page, not for the document.
    assert!(page(&svc, &path, 2, 300).is_ok());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_broken_pdf_has_no_preview_and_does_not_hurt_the_host() {
    let dir = temp_dir("pdfbad");
    let mut bytes = b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n".to_vec();
    bytes.extend((0..4096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8));
    bytes.extend(b"\nstartxref\n999999999\n%%EOF");
    let path = write(&dir, "broken.pdf", &bytes);
    let svc = service(&dir, OfficeEnv::none());
    assert_eq!(
        page(&svc, &path, 1, 300).unwrap_err(),
        PreviewError::Unsupported
    );
    assert_eq!(info(&svc, &path).unwrap_err(), PreviewError::Unsupported);
    // The service is still healthy afterwards.
    let good = write(&dir, "good.pdf", &common::pdf_pages(1));
    assert!(page(&svc, &good, 1, 300).is_ok());
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------------------------
// Images

#[test]
fn rotated_jpeg_preview_applies_orientation_and_never_upscales() {
    let dir = temp_dir("jpeg");
    let card = DynamicImage::ImageRgb8(test_card());
    let jpeg = with_exif_orientation(&encode(&card, ImgFormat::Jpeg), 6);
    let path = write(&dir, "rotated.jpg", &jpeg);
    let svc = service(&dir, OfficeEnv::none());

    let meta = info(&svc, &path).unwrap();
    // Stored 400 x 200; orientation 6 displays it 200 x 400.
    assert_eq!(meta.kind, PreviewKind::Image);
    assert_eq!(
        (meta.width, meta.height, meta.pages),
        (Some(200), Some(400), None)
    );
    assert_eq!(meta.renderer, "wic");

    let r = page(&svc, &path, 1, 150).unwrap();
    let img = decode(&r);
    // The width is the displayed width: 150 wide, 300 tall.
    assert_eq!(img.dimensions(), (150, 300));
    assert_similar(&img, &card.rotate90());
    let wrong = card.resize_exact(150, 300, image::imageops::FilterType::Triangle);
    assert!(
        common::distance(&img, &wrong) > 6,
        "not the unrotated card squeezed"
    );

    // Wider than the source: the native (displayed) size, never upscaled.
    let native = page(&svc, &path, 1, 1000).unwrap();
    assert_eq!(decode(&native).dimensions(), (200, 400));

    // JPEG output on request, at its own cache entry.
    let mut req = page_req(&path);
    req.page = Some(1);
    req.width = Some(150);
    req.format = ImageFormat::Jpeg;
    let mut jpeg_resp = svc.page(req);
    for _ in 0..200 {
        if !matches!(jpeg_resp, Err(PreviewError::Generating { .. })) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        let mut req = page_req(&path);
        req.page = Some(1);
        req.width = Some(150);
        req.format = ImageFormat::Jpeg;
        jpeg_resp = svc.page(req);
    }
    let jpeg_resp = jpeg_resp.unwrap();
    assert_eq!(jpeg_resp.content_type, "image/jpeg");
    assert_eq!(
        image::guess_format(&jpeg_resp.bytes).unwrap(),
        ImgFormat::Jpeg
    );
    assert_ne!(jpeg_resp.etag, r.etag);

    // An image has one page.
    assert_eq!(
        page(&svc, &path, 2, 150).unwrap_err(),
        PreviewError::BadRequest("page out of range")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------------------------
// Text

#[test]
fn utf16_text_is_decoded_golden() {
    let dir = temp_dir("utf16");
    let text = "Gr\u{fc}\u{df}e aus K\u{f6}ln\r\n\u{4e16}\u{754c} \u{1f980}\r\nlast line";
    let le = write(&dir, "le.txt", &common::utf16(text, true));
    let be = write(&dir, "be.txt", &common::utf16(text, false));
    let svc = service(&dir, OfficeEnv::none());
    for (path, label) in [(&le, "utf-16le"), (&be, "utf-16be")] {
        let r = svc
            .text(TextRequest {
                file: File::open(path).unwrap(),
                offset: 0,
                limit: None,
            })
            .unwrap();
        assert_eq!(r.encoding, label);
        assert_eq!(r.content_type, "text/plain; charset=utf-8");
        assert_eq!(std::str::from_utf8(&r.bytes).unwrap(), text);
        assert_eq!(r.total, text.len() as u64);
        assert!(!r.has_more);
        let meta = info(&svc, path).unwrap();
        assert_eq!(
            (meta.kind, meta.text_bytes),
            (PreviewKind::Text, Some(text.len() as u64))
        );
    }
    // A slice from the middle starts on a character boundary and reports where it ended.
    let r = svc
        .text(TextRequest {
            file: File::open(&le).unwrap(),
            offset: 7,
            limit: Some(10),
        })
        .unwrap();
    assert!(std::str::from_utf8(&r.bytes).is_ok());
    assert_eq!(r.next_offset, r.offset + r.bytes.len() as u64);
    assert!(r.has_more);
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------------------------
// Office documents with and without an embedded thumbnail

/// The real worker for everything except Office automation, which always fails: a stand-in for
/// "PowerPoint cannot render this" that does not depend on Office being installed.
struct OfficeFails(WorkerRunner);

impl JobRunner for OfficeFails {
    fn run(&self, file: &File, job: &RenderJob) -> JobResult {
        let automation = job.kind == RenderKind::Office
            && matches!(job.op, RenderOp::Page { .. } | RenderOp::ExportPdf);
        if automation {
            JobResult::Unsupported
        } else {
            self.0.run(file, job)
        }
    }

    fn convert_with_libreoffice(&self, file: &File, ext: &str, soffice: &Path) -> JobResult {
        self.0.convert_with_libreoffice(file, ext, soffice)
    }
}

fn powerpoint_only() -> OfficeEnv {
    OfficeEnv {
        powerpoint: true,
        ..OfficeEnv::none()
    }
}

#[test]
fn pptx_with_an_embedded_thumbnail_degrades_to_it_and_without_one_has_no_preview() {
    let dir = temp_dir("pptx-thumb");
    let thumb_src = DynamicImage::ImageRgb8(test_card()).resize_exact(
        256,
        192,
        image::imageops::FilterType::Triangle,
    );
    let thumb_jpeg = encode(&thumb_src, ImgFormat::Jpeg);
    let with = write(&dir, "with.pptx", &common::pptx(1, Some(&thumb_jpeg)));
    let without = write(&dir, "without.pptx", &common::pptx(1, None));

    // PowerPoint is "installed" but cannot render: page 1 falls back to the embedded thumbnail.
    let svc = PreviewService::new(
        cache(&dir),
        OfficeFails(WorkerRunner { config: worker() }),
        powerpoint_only(),
        PreviewServiceConfig::default(),
    );
    let meta = info(&svc, &with).unwrap();
    assert_eq!(meta.kind, PreviewKind::Office);
    assert_eq!(meta.pages, Some(1), "slide count comes from the package");
    assert_eq!((meta.width, meta.height), (Some(960), Some(540)));
    assert_eq!(meta.renderer, "office_com_powerpoint");

    let r = page(&svc, &with, 1, 128).unwrap();
    assert!(r.degraded);
    assert_eq!(r.renderer, "office_thumbnail");
    let img = decode(&r);
    assert_eq!(
        img.dimensions(),
        (128, 96),
        "the embedded thumbnail, fitted to the width"
    );
    assert_similar(&img, &thumb_src);

    let e = page(&svc, &without, 1, 128).unwrap_err();
    assert_eq!(e, PreviewError::Unsupported, "no thumbnail to fall back to");
    assert_eq!(e.http_status(), 415);

    // With no Office renderer at all the deck is `none` in listings and info, and /preview is
    // 415 even when a thumbnail is embedded (the thumbnail still serves the list).
    std::fs::create_dir_all(dir.join("bare")).unwrap();
    let bare = service(&dir.join("bare"), OfficeEnv::none());
    assert_eq!(bare.predict("pptx"), PreviewKind::None);
    assert_eq!(info(&bare, &with).unwrap().kind, PreviewKind::None);
    assert_eq!(
        page(&bare, &with, 1, 128).unwrap_err(),
        PreviewError::Unsupported
    );
    assert_eq!(
        bare.preview_kinds().office,
        gander_core::protocol::host::OfficePreview::Thumbnail
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn libreoffice_is_reported_absent_and_office_falls_to_none_without_com() {
    let env = OfficeEnv::detect();
    eprintln!(
        "detected: powerpoint={} word={} excel={} libreoffice={:?}",
        env.powerpoint, env.word, env.excel, env.libreoffice
    );
    if env.libreoffice.is_some() {
        eprintln!("SKIPPED: this machine has a LibreOffice");
        return;
    }
    // Without Office either, an ODF document has no renderer at all.
    let none_env = OfficeEnv {
        libreoffice: env.libreoffice.clone(),
        ..OfficeEnv::none()
    };
    let dir = temp_dir("lo-none");
    let odp = write(&dir, "slides.odp", b"PK");
    let svc = service(&dir, none_env);
    assert_eq!(svc.predict("odp"), PreviewKind::None);
    assert_eq!(
        page(&svc, &odp, 1, 300).unwrap_err(),
        PreviewError::Unsupported
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
