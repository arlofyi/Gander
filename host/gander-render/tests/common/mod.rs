// SPDX-License-Identifier: Apache-2.0
//! Fixture builders shared by the preview integration tests. Everything is generated here;
//! nothing binary is committed.

#![allow(dead_code)]

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageFormat, Rgb, RgbImage};

pub fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("gander-preview-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 64-bit average hash of an image (8x8 grayscale, bit set where brighter than the mean).
pub fn ahash(img: &DynamicImage) -> u64 {
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

pub fn distance(a: &DynamicImage, b: &DynamicImage) -> u32 {
    (ahash(a) ^ ahash(b)).count_ones()
}

pub fn assert_similar(actual: &DynamicImage, expected: &DynamicImage) {
    let d = distance(actual, expected);
    assert!(d <= 6, "perceptual hash distance {d} > 6");
}

/// 400x200 test card: four coloured quadrants, so any rotation or flip changes the hash.
pub fn test_card() -> RgbImage {
    RgbImage::from_fn(400, 200, |x, y| match (x < 200, y < 100) {
        (true, true) => Rgb([230, 20, 20]),
        (false, true) => Rgb([20, 200, 40]),
        (true, false) => Rgb([20, 30, 220]),
        (false, false) => Rgb([250, 250, 250]),
    })
}

pub fn encode(img: &DynamicImage, format: ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, format).unwrap();
    out.into_inner()
}

/// Inserts an EXIF APP1 segment with the given orientation right after the JPEG SOI.
pub fn with_exif_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
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

/// A PDF with `pages` pages of 400 x 200 points. Page `n` (1-based) has a black bar whose
/// position differs per page: page 1 left half, page 2 top half, page 3 a centred square.
pub fn pdf_pages(pages: usize) -> Vec<u8> {
    let content = |i: usize| match i {
        0 => "0 0 0 rg 0 0 200 200 re f".to_owned(),
        1 => "0 0 0 rg 0 100 400 100 re f".to_owned(),
        _ => "0 0 0 rg 150 50 100 100 re f".to_owned(),
    };
    // Objects: 1 catalog, 2 pages, then (page, content) pairs.
    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        String::new(),
    ];
    let mut kids = String::new();
    for i in 0..pages {
        let page_id = 3 + i * 2;
        let content_id = page_id + 1;
        kids.push_str(&format!("{page_id} 0 R "));
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 200] /Contents {content_id} 0 R >>"
        ));
        let c = content(i);
        objects.push(format!("<< /Length {} >>\nstream\n{c}\nendstream", c.len()));
    }
    objects[1] = format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>");
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

fn zip_of(parts: &[(&str, String)], extra: &[(&str, &[u8])]) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default();
    for (name, body) in parts {
        w.start_file(*name, opts).unwrap();
        w.write_all(body.as_bytes()).unwrap();
    }
    for (name, body) in extra {
        w.start_file(*name, opts).unwrap();
        w.write_all(body).unwrap();
    }
    w.finish().unwrap().into_inner()
}

const NS_REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const NS_CT: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const NS_R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const NS_A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const NS_P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
const XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;

fn rels(items: &[(&str, &str, &str)]) -> String {
    let body: String = items
        .iter()
        .map(|(id, ty, target)| {
            format!(r#"<Relationship Id="{id}" Type="{ty}" Target="{target}"/>"#)
        })
        .collect();
    format!(r#"{XML}<Relationships xmlns="{NS_REL}">{body}</Relationships>"#)
}

fn theme() -> String {
    let fill = r#"<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>"#;
    let line = format!(r#"<a:ln w="9525">{fill}</a:ln>"#);
    let effect = "<a:effectStyle><a:effectLst/></a:effectStyle>";
    let clr = |name: &str, hex: &str| format!(r#"<a:{name}><a:srgbClr val="{hex}"/></a:{name}>"#);
    let scheme = [
        ("dk1", "000000"),
        ("lt1", "FFFFFF"),
        ("dk2", "44546A"),
        ("lt2", "E7E6E6"),
        ("accent1", "4472C4"),
        ("accent2", "ED7D31"),
        ("accent3", "A5A5A5"),
        ("accent4", "FFC000"),
        ("accent5", "5B9BD5"),
        ("accent6", "70AD47"),
        ("hlink", "0563C1"),
        ("folHlink", "954F72"),
    ]
    .iter()
    .map(|(n, h)| clr(n, h))
    .collect::<String>();
    let font = r#"<a:latin typeface="Calibri"/><a:ea typeface=""/><a:cs typeface=""/>"#;
    format!(
        r#"{XML}<a:theme xmlns:a="{NS_A}" name="Gander"><a:themeElements><a:clrScheme name="Gander">{scheme}</a:clrScheme><a:fontScheme name="Gander"><a:majorFont>{font}</a:majorFont><a:minorFont>{font}</a:minorFont></a:fontScheme><a:fmtScheme name="Gander"><a:fillStyleLst>{fill}{fill}{fill}</a:fillStyleLst><a:lnStyleLst>{line}{line}{line}</a:lnStyleLst><a:effectStyleLst>{effect}{effect}{effect}</a:effectStyleLst><a:bgFillStyleLst>{fill}{fill}{fill}</a:bgFillStyleLst></a:fmtScheme></a:themeElements></a:theme>"#
    )
}

/// A minimal but valid deck of `slides` slides (16:9, 9144000 x 5143500 EMU). Slide 1 has a red
/// rectangle over its left half, slide 2 a blue one over its right half, later slides green.
/// `thumbnail` is embedded as `docProps/thumbnail.jpeg` (with its relationship) when given.
pub fn pptx(slides: usize, thumbnail: Option<&[u8]>) -> Vec<u8> {
    let mut parts: Vec<(&str, String)> = Vec::new();
    let mut names: Vec<(String, String)> = Vec::new();

    let mut content_types = format!(
        r#"{XML}<Types xmlns="{NS_CT}"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="jpeg" ContentType="image/jpeg"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>"#
    );
    for i in 1..=slides {
        content_types.push_str(&format!(
            r#"<Override PartName="/ppt/slides/slide{i}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>"#
        ));
    }
    content_types.push_str("</Types>");

    let mut root_rels = vec![(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
        "ppt/presentation.xml",
    )];
    if thumbnail.is_some() {
        root_rels.push((
            "rId2",
            "http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail",
            "docProps/thumbnail.jpeg",
        ));
    }

    let sld_ids: String = (1..=slides)
        .map(|i| format!(r#"<p:sldId id="{}" r:id="rId{}"/>"#, 255 + i, i + 1))
        .collect();
    let presentation = format!(
        r#"{XML}<p:presentation xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}"><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:sldIdLst>{sld_ids}</p:sldIdLst><p:sldSz cx="9144000" cy="5143500"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>"#
    );
    let mut pres_rels: Vec<(String, &str, String)> = vec![(
        "rId1".into(),
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster",
        "slideMasters/slideMaster1.xml".into(),
    )];
    for i in 1..=slides {
        pres_rels.push((
            format!("rId{}", i + 1),
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide",
            format!("slides/slide{i}.xml"),
        ));
    }
    pres_rels.push((
        format!("rId{}", slides + 2),
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme",
        "theme/theme1.xml".into(),
    ));
    let pres_rels_xml = rels(
        &pres_rels
            .iter()
            .map(|(a, b, c)| (a.as_str(), *b, c.as_str()))
            .collect::<Vec<_>>(),
    );

    let group = r#"<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>"#;
    let master = format!(
        r#"{XML}<p:sldMaster xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}"><p:cSld><p:bg><p:bgPr><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill><a:effectLst/></p:bgPr></p:bg><p:spTree>{group}</p:spTree></p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/><p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst></p:sldMaster>"#
    );
    let master_rels = rels(&[
        (
            "rId1",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout",
            "../slideLayouts/slideLayout1.xml",
        ),
        (
            "rId2",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme",
            "../theme/theme1.xml",
        ),
    ]);
    let layout = format!(
        r#"{XML}<p:sldLayout xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}" type="blank"><p:cSld name="Blank"><p:spTree>{group}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#
    );
    let layout_rels = rels(&[(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster",
        "../slideMasters/slideMaster1.xml",
    )]);
    let slide_rels = rels(&[(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout",
        "../slideLayouts/slideLayout1.xml",
    )]);

    for i in 1..=slides {
        let (x, color) = match i {
            1 => (0, "E61414"),
            2 => (4_572_000, "1414E6"),
            _ => (0, "14B428"),
        };
        let slide = format!(
            r#"{XML}<p:sld xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}"><p:cSld><p:spTree>{group}<p:sp><p:nvSpPr><p:cNvPr id="2" name="Bar"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="{x}" y="0"/><a:ext cx="4572000" cy="5143500"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="{color}"/></a:solidFill><a:ln><a:noFill/></a:ln></p:spPr></p:sp></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"#
        );
        names.push((format!("ppt/slides/slide{i}.xml"), slide));
        names.push((
            format!("ppt/slides/_rels/slide{i}.xml.rels"),
            slide_rels.clone(),
        ));
    }

    parts.push(("[Content_Types].xml", content_types));
    parts.push(("_rels/.rels", rels(&root_rels)));
    parts.push(("ppt/presentation.xml", presentation));
    parts.push(("ppt/_rels/presentation.xml.rels", pres_rels_xml));
    parts.push(("ppt/slideMasters/slideMaster1.xml", master));
    parts.push(("ppt/slideMasters/_rels/slideMaster1.xml.rels", master_rels));
    parts.push(("ppt/slideLayouts/slideLayout1.xml", layout));
    parts.push(("ppt/slideLayouts/_rels/slideLayout1.xml.rels", layout_rels));
    parts.push(("ppt/theme/theme1.xml", theme()));
    for (name, body) in &names {
        parts.push((name.as_str(), body.clone()));
    }
    let extra: Vec<(&str, &[u8])> = thumbnail
        .map(|t| vec![("docProps/thumbnail.jpeg", t)])
        .unwrap_or_default();
    zip_of(&parts, &extra)
}

/// A two-page Word document (a page break between the paragraphs).
pub fn docx() -> Vec<u8> {
    let ct = format!(
        r#"{XML}<Types xmlns="{NS_CT}"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#
    );
    let root = rels(&[(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
        "word/document.xml",
    )]);
    let doc = format!(
        r#"{XML}<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>First page</w:t></w:r></w:p><w:p><w:r><w:br w:type="page"/></w:r></w:p><w:p><w:r><w:t>Second page</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    );
    zip_of(
        &[
            ("[Content_Types].xml", ct),
            ("_rels/.rels", root),
            ("word/document.xml", doc),
        ],
        &[],
    )
}

/// A one-sheet workbook with a few cells.
pub fn xlsx() -> Vec<u8> {
    let ct = format!(
        r#"{XML}<Types xmlns="{NS_CT}"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#
    );
    let root = rels(&[(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
        "xl/workbook.xml",
    )]);
    let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    let workbook = format!(
        r#"{XML}<workbook xmlns="{ns}" xmlns:r="{NS_R}"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    let wb_rels = rels(&[(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet",
        "worksheets/sheet1.xml",
    )]);
    let sheet = format!(
        r#"{XML}<worksheet xmlns="{ns}"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Gander</t></is></c><c r="B1"><v>42</v></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>Sheet</t></is></c><c r="B2"><v>7</v></c></row></sheetData></worksheet>"#
    );
    zip_of(
        &[
            ("[Content_Types].xml", ct),
            ("_rels/.rels", root),
            ("xl/workbook.xml", workbook),
            ("xl/_rels/workbook.xml.rels", wb_rels),
            ("xl/worksheets/sheet1.xml", sheet),
        ],
        &[],
    )
}

/// UTF-16 text with a byte order mark.
pub fn utf16(text: &str, little_endian: bool) -> Vec<u8> {
    let mut out = if little_endian {
        vec![0xFF, 0xFE]
    } else {
        vec![0xFE, 0xFF]
    };
    for unit in text.encode_utf16() {
        out.extend(if little_endian {
            unit.to_le_bytes()
        } else {
            unit.to_be_bytes()
        });
    }
    out
}

pub fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

// ---------------------------------------------------------------------------------------------
// Deep-zoom fixtures (WP-67): flat coloured cells, so a tile's placement can be checked by
// sampling the centre of a cell and comparing it with the colour that cell must have.

/// The colour of cell (`cx`, `cy`): saturated, never white, and different from every neighbour
/// in all three channels, so a shift by one cell is always noticed.
pub fn cell_color(cx: u32, cy: u32) -> [u8; 3] {
    let ch = |a: u32, b: u32| (20 + ((cx * a + cy * b) % 12) * 18) as u8;
    [ch(7, 3), ch(5, 11), ch(13, 5)]
}

/// A `w` x `h` image of `cell` x `cell` pixel squares coloured by [`cell_color`].
pub fn grid_rgb(w: u32, h: u32, cell: u32) -> RgbImage {
    let mut buf = Vec::with_capacity(w as usize * h as usize * 3);
    for cy in 0..h.div_ceil(cell) {
        let colours: Vec<[u8; 3]> = (0..w.div_ceil(cell)).map(|cx| cell_color(cx, cy)).collect();
        let mut row = Vec::with_capacity(w as usize * 3);
        for x in 0..w {
            row.extend(colours[(x / cell) as usize]);
        }
        for _ in 0..cell.min(h - cy * cell) {
            buf.extend_from_slice(&row);
        }
    }
    RgbImage::from_raw(w, h, buf).unwrap()
}

/// PNG bytes of `img`, written with the fast compression level (a 27 MP image in a debug build).
pub fn png_bytes(img: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width(), img.height());
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().unwrap();
        writer.write_image_data(img.as_raw()).unwrap();
    }
    out
}

/// JPEG bytes of `img` at quality 92.
pub fn jpeg_bytes(img: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92)
        .encode_image(img)
        .unwrap();
    out
}

/// Content stream of `cols` x `rows` flat squares of `cell` points, row 0 at the top of a page
/// `page_h` points high, coloured by [`cell_color`] (what `grid_rgb` draws, as vectors).
pub fn pdf_grid_content(page_h: u32, cell: u32, cols: u32, rows: u32) -> String {
    let mut c = String::new();
    for r in 0..rows {
        for col in 0..cols {
            let [red, green, blue] = cell_color(col, r);
            let y = page_h - (r + 1) * cell;
            c.push_str(&format!(
                "{:.3} {:.3} {:.3} rg {} {} {cell} {cell} re f\n",
                f64::from(red) / 255.0,
                f64::from(green) / 255.0,
                f64::from(blue) / 255.0,
                col * cell,
                y
            ));
        }
    }
    c
}

/// A PDF whose pages are `(width pt, height pt, content stream)`.
pub fn pdf_with_pages(pages: &[(u32, u32, String)]) -> Vec<u8> {
    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        String::new(),
    ];
    let mut kids = String::new();
    for (i, (w, h, content)) in pages.iter().enumerate() {
        let page_id = 3 + i * 2;
        kids.push_str(&format!("{page_id} 0 R "));
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents {} 0 R >>",
            page_id + 1
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
    objects[1] = format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", pages.len());
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
