// SPDX-License-Identifier: Apache-2.0//! | image | WIC (or the `image` crate), resized to the width, never upscaled | dimensions |
//! Preview operations of the worker: one page image, metadata, and the Office PDF export.
//!
//! The host picks the [`RenderKind`]; this module routes it to the renderer:
//!
//! | kind | page | info |
//! | --- | --- | --- |
//! | image | WIC (or the `image` crate), resized to the width, never upscaled | dimensions |
//! | raw | embedded JPEG | dimensions of that JPEG |
//! | pdf | `Windows.Data.Pdf` at the width | page count, page 1 size |
//! | office | PowerPoint COM slide export | slide count and size from the zip |
//! | video | shell poster frame | nothing (the host answers without a job) |

use std::fs::File;

use gander_core::tiles::Pyramid;
use gander_win::render::job::{
    PAGE_OUT_OF_RANGE_MSG, PreviewMeta, RenderJob, RenderKind, RenderOp,
};
use image::RgbaImage;

use crate::{Failure, Rendered, Target, fallback, office, office_com, pdf, raw, shell, wic};

/// Whether the job needs a single-threaded COM apartment (shell and Office automation).
pub fn needs_sta(job: &RenderJob) -> bool {
    job.kind.uses_shell()
        || (job.kind == RenderKind::Office
            && matches!(job.op, RenderOp::Page { .. } | RenderOp::ExportPdf))
}

/// Page `page` (1-based) at `width` pixels.
pub fn page(file: &File, job: &RenderJob, page: u32, width: u32) -> Rendered<RgbaImage> {
    let target = Target::Width(width);
    match job.kind {
        RenderKind::Image => {
            only_first(page)?;
            match wic::decode_file_target(file, target) {
                Err(Failure::Unsupported(why)) => fallback::decode_file_target(file, target)
                    .map_err(|e| match e {
                        Failure::Unsupported(more) => {
                            Failure::Unsupported(format!("{why}; {more}"))
                        }
                        other => other,
                    }),
                other => other,
            }
        }
        RenderKind::Raw => {
            only_first(page)?;
            raw::preview(file, width)
        }
        RenderKind::Pdf => pdf::page(file, page, width),
        RenderKind::Office => office_com::render_page(file, &job.ext, page, width),
        RenderKind::Video => {
            only_first(page)?;
            let poster = shell::thumb(file, width)?;
            let (w, h) = poster.dimensions();
            let (tw, th) = target.fit(w, h);
            Ok(if (tw, th) == (w, h) {
                poster
            } else {
                image::imageops::thumbnail(&poster, tw, th)
            })
        }
        RenderKind::Audio | RenderKind::Shell => {
            Err(Failure::unsupported("no preview for this kind of file"))
        }
    }
}

/// Deep-zoom tile `(level, x, y)` of page `page` (1-based).
pub fn tile(
    file: &File,
    job: &RenderJob,
    page: u32,
    level: u32,
    x: u32,
    y: u32,
) -> Rendered<RgbaImage> {
    match job.kind {
        RenderKind::Image => {
            only_first(page)?;
            match wic::tile_file(file, level, x, y) {
                Err(Failure::Unsupported(why)) => {
                    fallback::tile_file(file, level, x, y).map_err(|e| match e {
                        Failure::Unsupported(more) => {
                            Failure::Unsupported(format!("{why}; {more}"))
                        }
                        other => other,
                    })
                }
                other => other,
            }
        }
        RenderKind::Raw => {
            only_first(page)?;
            raw::tile(file, level, x, y)
        }
        RenderKind::Pdf => pdf::tile(file, page, level, x, y),
        _ => Err(Failure::unsupported("no tiles for this kind of file")),
    }
}

/// The tile size list of a one-page image: its own pixels, when it can be tiled at all.
fn image_tile_pages(w: u32, h: u32) -> Vec<(u32, u32)> {
    Pyramid::new(w, h)
        .map(|p| p.top_size())
        .into_iter()
        .collect()
}

/// Cheap metadata for the preview info endpoint.
pub fn info(file: &File, job: &RenderJob) -> Rendered<PreviewMeta> {
    match job.kind {
        RenderKind::Image => {
            let (w, h) = match wic::frame_size_file(file) {
                Err(Failure::Unsupported(_)) => fallback::frame_size_file(file)?,
                other => other?,
            };
            Ok(PreviewMeta {
                pages: None,
                width: Some(w),
                height: Some(h),
                tile_pages: image_tile_pages(w, h),
            })
        }
        RenderKind::Raw => {
            let (w, h) = raw::frame_size(file)?;
            Ok(PreviewMeta {
                pages: None,
                width: Some(w),
                height: Some(h),
                tile_pages: image_tile_pages(w, h),
            })
        }
        RenderKind::Pdf => pdf::info(file),
        RenderKind::Office => office::info(file, &job.ext),
        RenderKind::Video | RenderKind::Audio | RenderKind::Shell => {
            Err(Failure::unsupported("no metadata for this kind of file"))
        }
    }
}

/// Office documents to PDF (Word, Excel), as the PDF bytes.
pub fn export_pdf(file: &File, job: &RenderJob) -> Rendered<Vec<u8>> {
    if job.kind != RenderKind::Office {
        return Err(Failure::unsupported("only Office documents export to PDF"));
    }
    office_com::export_pdf(file, &job.ext)
}

fn only_first(page: u32) -> Rendered<()> {
    if page == 1 {
        Ok(())
    } else {
        Err(Failure::decode(PAGE_OUT_OF_RANGE_MSG))
    }
}
