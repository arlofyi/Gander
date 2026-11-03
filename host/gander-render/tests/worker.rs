// SPDX-License-Identifier: Apache-2.0
//! End-to-end tests of `gander-render.exe` through the host's spawner
//! (`gander_win::render::worker::run_job`): job object, low integrity, handle duplication,
//! exit codes, and golden checks (dimensions + perceptual hash, never bytes).
//!
//! Every fixture is generated here; nothing binary is committed.

#![cfg(windows)]

use std::fs::File;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gander_win::render::job::{OutputFormat, RenderJob, RenderKind};
use gander_win::render::worker::{
    WorkerConfig, WorkerLimits, WorkerOutcome, WorkerReport, run_job,
};
use image::{DynamicImage, GenericImageView, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};

// ---------------------------------------------------------------------------------------------
// Helpers

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("gander-render-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn config() -> WorkerConfig {
    WorkerConfig::new(env!("CARGO_BIN_EXE_gander-render"))
}

fn run(path: &Path, kind: RenderKind, size: u32) -> WorkerReport {
    run_with(path, kind, size, config(), |_| {})
}

fn run_with(
    path: &Path,
    kind: RenderKind,
    size: u32,
    config: WorkerConfig,
    tweak: impl FnOnce(&mut RenderJob),
) -> WorkerReport {
    let file = File::open(path).unwrap();
    let ext = path.extension().unwrap().to_string_lossy().into_owned();
    let mut job = RenderJob::thumb(kind, &ext, size);
    tweak(&mut job);
    run_job(&config, &file, &job).unwrap()
}

/// Decodes a worker's output (WebP or JPEG) for inspection.
fn decode_output(report: &WorkerReport) -> DynamicImage {
    match &report.outcome {
        WorkerOutcome::Ok(bytes) => image::load_from_memory(bytes).unwrap(),
        other => panic!(
            "expected an image, got {other:?} (exit {:#x}, stderr: {})",
            report.exit_code, report.stderr
        ),
    }
}

/// 64-bit average hash of an image (8x8 grayscale, bit set where brighter than the mean).
fn ahash(img: &DynamicImage) -> u64 {
    let small = img
        .resize_exact(8, 8, image::imageops::FilterType::Triangle)
        .to_luma8();
    let mean = small.pixels().map(|p| u32::from(p.0[0])).sum::<u32>() / 64;
    small.pixels().enumerate().fold(0u64, |h, (i, p)| {
        if u32::from(p.0[0]) > mean {
            h | 1 << i
        } else {
            h
        }
    })
}

fn assert_similar(actual: &DynamicImage, expected: &DynamicImage) {
    let d = (ahash(actual) ^ ahash(expected)).count_ones();
    assert!(d <= 6, "perceptual hash distance {d} > 6");
}

/// 400x200 test card: four coloured quadrants, so any rotation or flip changes the hash.
fn test_card() -> RgbImage {
    RgbImage::from_fn(400, 200, |x, y| match (x < 200, y < 100) {
        (true, true) => Rgb([230, 20, 20]),
        (false, true) => Rgb([20, 200, 40]),
        (true, false) => Rgb([20, 30, 220]),
        (false, false) => Rgb([250, 250, 250]),
    })
}

fn encode(img: &DynamicImage, format: ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, format).unwrap();
    out.into_inner()
}

/// Inserts an EXIF APP1 segment with the given orientation right after the JPEG SOI.
fn with_exif_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
    let mut tiff = b"II*\0".to_vec();
    tiff.extend(8u32.to_le_bytes());
    tiff.extend(1u16.to_le_bytes());
    tiff.extend(0x0112u16.to_le_bytes());
    tiff.extend(3u16.to_le_bytes());
    tiff.extend(1u32.to_le_bytes());
    tiff.extend(orientation.to_le_bytes());
    tiff.extend(0u16.to_le_bytes());
    tiff.extend(0u32.to_le_bytes());
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend(tiff);
    let mut out = jpeg[..2].to_vec();
    out.extend([0xFF, 0xE1]);
    out.extend(((payload.len() + 2) as u16).to_be_bytes());
    out.extend(payload);
    out.extend_from_slice(&jpeg[2..]);
    out
}

// ---------------------------------------------------------------------------------------------
// Hand-built PNG "bomb": all-zero pixels, deflated with fixed-Huffman 258-byte back-references
// (13 bits per 258 output bytes), so a 1+ GiB image is a few MiB on disk.

fn crc32(chunks: &[&[u8]]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *t = c;
    }
    let mut crc = !0u32;
    for chunk in chunks {
        for &b in *chunk {
            crc = table[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8);
        }
    }
    !crc
}

struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn put(&mut self, value: u32, bits: u32) {
        self.acc |= u64::from(value) << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Huffman codes are sent most significant bit first.
    fn code(&mut self, code: u32, bits: u32) {
        let rev = code.reverse_bits() >> (32 - bits);
        self.put(rev, bits);
    }
}

fn zero_png(width: u32, height: u32) -> Vec<u8> {
    let total = u64::from(height) * (1 + u64::from(width) * 4);
    let mut bits = Bits {
        out: vec![0x78, 0x01],
        acc: 0,
        n: 0,
    };
    bits.put(1, 1); // BFINAL
    bits.put(1, 2); // fixed Huffman
    bits.code(0x30, 8); // literal 0
    let mut left = total - 1;
    while left >= 258 {
        bits.code(0b1100_0101, 8); // length symbol 285 = 258
        bits.code(0, 5); // distance 1
        left -= 258;
    }
    for _ in 0..left {
        bits.code(0x30, 8);
    }
    bits.code(0, 7); // end of block
    if bits.n > 0 {
        bits.put(0, 8 - bits.n);
    }
    let mut zlib = bits.out;
    let adler = ((total % 65521) << 16) as u32 | 1;
    zlib.extend(adler.to_be_bytes());

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |ty: &[u8], data: &[u8]| {
        png.extend((data.len() as u32).to_be_bytes());
        png.extend_from_slice(ty);
        png.extend_from_slice(data);
        png.extend(crc32(&[ty, data]).to_be_bytes());
    };
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]); // 8-bit RGBA
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &zlib);
    chunk(b"IEND", &[]);
    png
}

/// A one-page PDF (page `w` x `h` points, left half filled black), with a correct xref table.
fn simple_pdf(w: u32, h: u32) -> Vec<u8> {
    let content = format!("0 0 0 rg 0 0 {} {} re f", w / 2, h);
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents 4 0 R >>"),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n{obj}\nendobj\n", i + 1).into_bytes());
    }
    let xref = pdf.len();
    pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).into_bytes());
    for off in offsets {
        pdf.extend(format!("{off:010} 00000 n \n").into_bytes());
    }
    pdf.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .into_bytes(),
    );
    pdf
}

fn office_zip(thumbnail: Option<&[u8]>) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default();
    w.start_file("[Content_Types].xml", opts).unwrap();
    w.write_all(br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#).unwrap();
    w.start_file("_rels/.rels", opts).unwrap();
    let rels = if thumbnail.is_some() {
        r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail" Target="docProps/thumbnail.jpeg"/></Relationships>"#
    } else {
        r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#
    };
    w.write_all(rels.as_bytes()).unwrap();
    w.start_file("ppt/presentation.xml", opts).unwrap();
    w.write_all(b"<p:presentation/>").unwrap();
    if let Some(t) = thumbnail {
        w.start_file("docProps/thumbnail.jpeg", opts).unwrap();
        w.write_all(t).unwrap();
    }
    w.finish().unwrap().into_inner()
}

// ---------------------------------------------------------------------------------------------
// Golden tests

#[test]
fn jpeg_exif_orientation_is_applied() {
    let dir = temp_dir("exif");
    let card = DynamicImage::ImageRgb8(test_card());
    let jpeg = with_exif_orientation(&encode(&card, ImageFormat::Jpeg), 6);
    let path = dir.join("rotated.jpg");
    std::fs::write(&path, jpeg).unwrap();

    let report = run(&path, RenderKind::Image, 256);
    let out = decode_output(&report);
    // Orientation 6 = rotate 90 degrees clockwise: 400x200 becomes portrait 128x256.
    assert_eq!(out.dimensions(), (128, 256));
    assert_similar(&out, &card.rotate90());
    // And it is *not* the unrotated card squeezed into the same box.
    let wrong = card.resize_exact(128, 256, image::imageops::FilterType::Triangle);
    assert!((ahash(&out) ^ ahash(&wrong)).count_ones() > 6);
    assert!(report.peak_memory_bytes <= WorkerLimits::default().memory_bytes);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn png_alpha_is_preserved_and_sizes_are_honoured() {
    let dir = temp_dir("alpha");
    let img = RgbaImage::from_fn(300, 150, |x, _| {
        if x < 150 {
            Rgba([0, 0, 0, 0])
        } else {
            Rgba([220, 10, 10, 255])
        }
    });
    let path = dir.join("alpha.png");
    std::fs::write(
        &path,
        encode(&DynamicImage::ImageRgba8(img), ImageFormat::Png),
    )
    .unwrap();

    for (size, expected) in [(128, (128, 64)), (256, (256, 128)), (512, (300, 150))] {
        let out = decode_output(&run(&path, RenderKind::Image, size));
        assert_eq!(out.dimensions(), expected, "size {size}");
        let rgba = out.to_rgba8();
        let (w, h) = rgba.dimensions();
        assert!(
            rgba.get_pixel(w / 10, h / 2).0[3] < 16,
            "left stays transparent"
        );
        let right = rgba.get_pixel(w * 9 / 10, h / 2).0;
        assert!(
            right[3] > 240 && right[0] > 180 && right[1] < 60,
            "right stays red: {right:?}"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn jpeg_output_format_is_supported() {
    let dir = temp_dir("jpegout");
    let card = DynamicImage::ImageRgb8(test_card());
    let path = dir.join("card.png");
    std::fs::write(&path, encode(&card, ImageFormat::Png)).unwrap();
    let report = run_with(&path, RenderKind::Image, 128, config(), |j| {
        j.format = OutputFormat::Jpeg;
        j.quality = 88;
    });
    let WorkerOutcome::Ok(bytes) = &report.outcome else {
        panic!("{report:?}")
    };
    assert_eq!(image::guess_format(bytes).unwrap(), ImageFormat::Jpeg);
    let out = decode_output(&report);
    assert_eq!(out.dimensions(), (128, 64));
    assert_similar(&out, &card);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn raw_embedded_preview_with_container_orientation() {
    let dir = temp_dir("raw");
    let card = DynamicImage::ImageRgb8(test_card());
    let jpeg = encode(&card, ImageFormat::Jpeg);
    // Minimal little-endian TIFF ("DNG"): IFD0 with Orientation = 8 and a JPEG preview.
    let mut b = b"II*\0".to_vec();
    b.extend(8u32.to_le_bytes());
    b.extend(3u16.to_le_bytes());
    let data_off = 8 + 2 + 3 * 12 + 4;
    for (tag, ty, value) in [
        (0x0112u16, 3u16, 8u32),
        (0x0201, 4, data_off as u32),
        (0x0202, 4, jpeg.len() as u32),
    ] {
        b.extend(tag.to_le_bytes());
        b.extend(ty.to_le_bytes());
        b.extend(1u32.to_le_bytes());
        b.extend(value.to_le_bytes());
    }
    b.extend(0u32.to_le_bytes());
    b.extend(&jpeg);
    let path = dir.join("camera.dng");
    std::fs::write(&path, b).unwrap();

    let out = decode_output(&run(&path, RenderKind::Raw, 256));
    // Orientation 8 = rotate 270 degrees clockwise.
    assert_eq!(out.dimensions(), (128, 256));
    assert_similar(&out, &card.rotate270());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn office_embedded_thumbnail_and_its_absence() {
    let dir = temp_dir("office");
    let thumb = DynamicImage::ImageRgb8(test_card()).resize_exact(
        256,
        192,
        image::imageops::FilterType::Triangle,
    );
    let with = dir.join("deck.pptx");
    std::fs::write(&with, office_zip(Some(&encode(&thumb, ImageFormat::Jpeg)))).unwrap();
    let out = decode_output(&run(&with, RenderKind::Office, 128));
    assert_eq!(out.dimensions(), (128, 96));
    assert_similar(&out, &thumb);

    let without = dir.join("plain.pptx");
    std::fs::write(&without, office_zip(None)).unwrap();
    let report = run(&without, RenderKind::Office, 128);
    assert_eq!(
        report.outcome,
        WorkerOutcome::Unsupported,
        "{}",
        report.stderr
    );
    assert_eq!(report.exit_code, 2);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn pdf_page_one() {
    let dir = temp_dir("pdf");
    let path = dir.join("doc.pdf");
    std::fs::write(&path, simple_pdf(400, 200)).unwrap();
    let out = decode_output(&run(&path, RenderKind::Pdf, 256)).to_luma8();
    assert_eq!(out.dimensions(), (256, 128));
    assert!(out.get_pixel(30, 64).0[0] < 60, "left half is black");
    assert!(out.get_pixel(226, 64).0[0] > 200, "right half is white");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn shell_fallback_renders_and_rejects_icon_only() {
    let dir = temp_dir("shell");
    let card = DynamicImage::ImageRgb8(test_card());
    let png = dir.join("card.png");
    std::fs::write(&png, encode(&card, ImageFormat::Png)).unwrap();
    let report = run(&png, RenderKind::Shell, 256);
    let out = decode_output(&report);
    assert_eq!(out.width().max(out.height()), 256);
    assert_similar(&out, &card);

    // A plain text file has no thumbnail provider: only an icon exists, which is refused.
    let txt = dir.join("notes.txt");
    std::fs::write(&txt, b"hello").unwrap();
    let report = run(&txt, RenderKind::Shell, 256);
    assert_eq!(
        report.outcome,
        WorkerOutcome::Unsupported,
        "{}",
        report.stderr
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------------------------
// Hostile inputs: contained by the worker, never the host process.

#[test]
fn malformed_pdf_is_a_decoder_error_inside_caps() {
    let dir = temp_dir("badpdf");
    let path = dir.join("broken.pdf");
    let mut bytes = b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n".to_vec();
    bytes.extend((0..4096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8));
    bytes.extend(b"\nstartxref\n999999999\n%%EOF");
    std::fs::write(&path, bytes).unwrap();
    let limits = WorkerLimits::default();
    let report = run(&path, RenderKind::Pdf, 256);
    assert!(
        matches!(report.exit_code, 3 | 4),
        "exit {:#x} {:?} {}",
        report.exit_code,
        report.outcome,
        report.stderr
    );
    assert!(report.elapsed < limits.wall_timeout + Duration::from_secs(5));
    assert!(report.peak_memory_bytes <= limits.memory_bytes);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn png_bomb_over_pixel_limit_exits_4() {
    let dir = temp_dir("bomb");
    let path = dir.join("bomb.png");
    // 20000 x 20000 RGBA = 1.6 GB of pixels from a ~10 MB file.
    std::fs::write(&path, zero_png(20_000, 20_000)).unwrap();
    let limits = WorkerLimits::default();
    let report = run(&path, RenderKind::Image, 256);
    assert_eq!(
        report.exit_code, 4,
        "{:?} {}",
        report.outcome, report.stderr
    );
    assert_eq!(report.outcome, WorkerOutcome::DecoderError);
    assert!(report.peak_memory_bytes <= limits.memory_bytes);
    assert!(report.elapsed < limits.wall_timeout);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn png_bomb_under_pixel_limit_is_held_by_the_job_object() {
    let dir = temp_dir("bomb2");
    let path = dir.join("bomb.png");
    // 16000 x 16000 RGBA = 1 GiB decoded, under the pixel guard: only the job caps stand
    // between it and the machine.
    std::fs::write(&path, zero_png(16_000, 16_000)).unwrap();
    let limits = WorkerLimits::default();
    let report = run(&path, RenderKind::Image, 256);
    assert!(
        report.peak_memory_bytes <= limits.memory_bytes,
        "peak {} over cap",
        report.peak_memory_bytes
    );
    match &report.outcome {
        // WIC may stream the rows through the scaler and succeed within the cap.
        WorkerOutcome::Ok(_) => assert_eq!(decode_output(&report).dimensions(), (256, 256)),
        WorkerOutcome::DecoderError | WorkerOutcome::TimedOut | WorkerOutcome::Crashed(_) => {}
        other => panic!("unexpected {other:?}"),
    }

    // The same file under a 100 ms CPU cap is killed by the job object and reported as a timeout.
    let mut tight = config();
    tight.limits.cpu_time = Duration::from_millis(100);
    let report = run_with(&path, RenderKind::Image, 256, tight, |_| {});
    assert_eq!(report.outcome, WorkerOutcome::TimedOut, "{}", report.stderr);

    // And the worker's own watchdog ends it with exit code 3.
    let report = run_with(&path, RenderKind::Image, 256, config(), |j| {
        j.timeout_ms = 100
    });
    assert_eq!(
        report.exit_code, 3,
        "{:?} {}",
        report.outcome, report.stderr
    );
    assert_eq!(report.outcome, WorkerOutcome::TimedOut);

    // A tiny memory cap holds too.
    let mut small = config();
    small.limits.memory_bytes = 48 * 1024 * 1024;
    let report = run_with(&path, RenderKind::Image, 256, small, |_| {});
    assert!(report.peak_memory_bytes <= 48 * 1024 * 1024);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn host_wall_clock_kill_reports_timeout() {
    let dir = temp_dir("wall");
    let path = dir.join("bomb.png");
    std::fs::write(&path, zero_png(16_000, 16_000)).unwrap();
    let mut cfg = config();
    cfg.limits.wall_timeout = Duration::from_millis(150);
    let report = run_with(&path, RenderKind::Image, 256, cfg, |_| {});
    assert_eq!(report.outcome, WorkerOutcome::TimedOut, "{}", report.stderr);
    assert_eq!(report.exit_code, 3);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn garbage_image_is_a_decoder_error() {
    let dir = temp_dir("garbage");
    let path = dir.join("fake.jpg");
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
    bytes.extend(std::iter::repeat_n(0xAB, 2000));
    std::fs::write(&path, bytes).unwrap();
    let report = run(&path, RenderKind::Image, 128);
    assert!(
        matches!(
            report.outcome,
            WorkerOutcome::DecoderError | WorkerOutcome::Unsupported
        ),
        "{:?} {}",
        report.outcome,
        report.stderr
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------------------------------
// The `/thumb` service with the real worker behind it.

#[test]
fn thumb_service_end_to_end() {
    use gander_win::thumbs::cache::ThumbCache;
    use gander_win::thumbs::queue::Priority;
    use gander_win::thumbs::service::{ThumbError, ThumbRequest, ThumbService};
    use std::sync::Arc;

    let dir = temp_dir("service");
    let card = DynamicImage::ImageRgb8(test_card());
    let path = dir.join("card.png");
    std::fs::write(&path, encode(&card, ImageFormat::Png)).unwrap();
    let cache = Arc::new(ThumbCache::open(&dir.join("cache"), 64 * 1024 * 1024).unwrap());
    let svc = ThumbService::with_worker(Arc::clone(&cache), config());
    let ask = || {
        svc.thumb(ThumbRequest {
            file: File::open(&path).unwrap(),
            ext: "png".into(),
            size: Some(128),
            priority: Priority::Visible,
        })
    };
    let mut answer = ask();
    for _ in 0..200 {
        match answer {
            Err(ThumbError::Generating { .. }) => {
                std::thread::sleep(Duration::from_millis(25));
                answer = ask();
            }
            _ => break,
        }
    }
    let ok = answer.expect("thumbnail");
    assert_eq!(ok.content_type, "image/webp");
    let img = image::load_from_memory(&ok.bytes).unwrap();
    assert_eq!(img.dimensions(), (128, 64));
    assert_similar(&img, &card);
    assert_eq!(cache.len(), 1);
    drop(svc);
    drop(cache);
    std::fs::remove_dir_all(&dir).unwrap();
}
