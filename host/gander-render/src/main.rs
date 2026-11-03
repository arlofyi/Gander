// SPDX-License-Identifier: Apache-2.0
//! Gander render worker: parses untrusted bytes out of process under a job object.
//!
//! Contract (renderer spec, "Worker protocol"; types in `gander_win::render::job`):
//!
//! - stdin: one JSON [`RenderJob`], then EOF. It names a file **handle** that the host
//!   duplicated into this process read-only; the worker never receives a path.
//! - stdout: the encoded image (WebP or JPEG). stderr: one-line diagnostics, never file contents.
//! - exit code: 0 ok, 1 bad job, 2 unsupported, 3 timeout, 4 decoder error.
//!
//! The host runs this executable under a job object (memory, CPU time, no children, below-normal
//! priority) at low integrity and kills it on a wall-clock timeout; the worker additionally runs
//! its own watchdog so a hung third-party shell handler ends with exit code 3.

#[cfg(windows)]
mod dispatch;
#[cfg(windows)]
mod encode;
#[cfg(windows)]
mod fallback;
#[cfg(windows)]
mod office;
#[cfg(windows)]
mod office_com;
#[cfg(windows)]
mod pdf;
#[cfg(windows)]
mod preview;
#[cfg(windows)]
mod raw;
#[cfg(windows)]
mod shell;
#[cfg(windows)]
mod wic;

#[cfg(windows)]
use gander_win::render::job::RenderJob;

/// Largest image (in pixels) any decoder is allowed to materialise before downscaling. A
/// decompression bomb declares far more than this in its header and is refused before decoding.
pub const MAX_SOURCE_PIXELS: u64 = 300_000_000;

/// Largest file the worker reads fully into memory (RAW, PDF, `image` crate fallback).
pub const MAX_READ_BYTES: u64 = 256 * 1024 * 1024;

/// Why a job produced no image; each maps to an exit code.
#[derive(Debug)]
pub enum Failure {
    /// Exit 2: no renderer for these bytes.
    Unsupported(String),
    /// Exit 4: the bytes are malformed or over a limit.
    Decode(String),
}

impl Failure {
    /// `Failure::Decode` from anything printable.
    pub fn decode(e: impl std::fmt::Display) -> Self {
        Failure::Decode(e.to_string())
    }

    /// `Failure::Unsupported` from anything printable.
    pub fn unsupported(e: impl std::fmt::Display) -> Self {
        Failure::Unsupported(e.to_string())
    }
}

/// Result type of every renderer.
pub type Rendered<T> = Result<T, Failure>;

/// Most pixels a preview page may have after scaling (about 16 MP, 64 MiB of RGBA).
pub const MAX_PREVIEW_PIXELS: u64 = 16 * 1024 * 1024;

/// How a renderer sizes its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Longest side at most this many pixels (thumbnails).
    Box(u32),
    /// Exactly this wide unless the source is narrower (preview pages; images never upscale).
    Width(u32),
}

impl Target {
    /// Output dimensions for a source of `(w, h)`.
    pub fn fit(self, w: u32, h: u32) -> (u32, u32) {
        match self {
            Target::Box(size) => fit(w, h, size),
            Target::Width(width) => fit_width(w, h, width),
        }
    }
}

/// Dimensions for a source `(w, h)` scaled to `width` columns (never upscaled, never zero), then
/// reduced further if the result would exceed [`MAX_PREVIEW_PIXELS`] (a very tall page).
pub fn fit_width(w: u32, h: u32, width: u32) -> (u32, u32) {
    let (w, h) = (w.max(1), h.max(1));
    let (tw, th) = if w <= width {
        (w, h)
    } else {
        let nh = (u64::from(h) * u64::from(width) + u64::from(w) / 2) / u64::from(w);
        (width, u32::try_from(nh.max(1)).unwrap_or(u32::MAX))
    };
    cap_pixels(tw, th)
}

/// Scales `(w, h)` down, keeping the aspect ratio, until it has at most [`MAX_PREVIEW_PIXELS`].
pub fn cap_pixels(w: u32, h: u32) -> (u32, u32) {
    let px = u64::from(w) * u64::from(h);
    if px <= MAX_PREVIEW_PIXELS {
        return (w, h);
    }
    let scale = (MAX_PREVIEW_PIXELS as f64 / px as f64).sqrt();
    (
        ((f64::from(w) * scale) as u32).max(1),
        ((f64::from(h) * scale) as u32).max(1),
    )
}

/// Dimensions that fit `(w, h)` inside a `size` x `size` box keeping the aspect ratio, never
/// upscaling, never zero.
pub fn fit(w: u32, h: u32, size: u32) -> (u32, u32) {
    if w <= size && h <= size {
        return (w.max(1), h.max(1));
    }
    let (w64, h64, s) = (u64::from(w), u64::from(h), u64::from(size));
    if w >= h {
        let nh = (h64 * s + w64 / 2) / w64;
        (size, u32::try_from(nh.max(1)).unwrap_or(1))
    } else {
        let nw = (w64 * s + h64 / 2) / h64;
        (u32::try_from(nw.max(1)).unwrap_or(1), size)
    }
}

/// Reads the whole file from the start, refusing files over [`MAX_READ_BYTES`].
pub fn read_all(file: &std::fs::File) -> Rendered<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = file;
    f.seek(SeekFrom::Start(0)).map_err(Failure::decode)?;
    let mut buf = Vec::new();
    f.take(MAX_READ_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(Failure::decode)?;
    if buf.len() as u64 > MAX_READ_BYTES {
        return Err(Failure::unsupported("file too large to read into memory"));
    }
    Ok(buf)
}

#[cfg(windows)]
fn main() {
    std::process::exit(worker::run() as i32);
}

#[cfg(not(windows))]
fn main() {
    // Only the Windows renderers exist in v1.
    eprintln!("gander-render: unsupported platform");
    std::process::exit(2);
}

#[cfg(windows)]
mod worker {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::windows::io::FromRawHandle;
    use std::sync::Mutex;
    use std::time::Duration;

    use gander_core::tiles::MAX_LEVELS;
    use gander_win::render::job::{
        EXIT_BAD_JOB, EXIT_DECODER_ERROR, EXIT_OK, EXIT_TIMEOUT, EXIT_UNSUPPORTED, JOB_VERSION,
        MAX_JOB_BYTES, RenderKind, RenderOp,
    };
    use image::RgbaImage;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{FILE_TYPE_DISK, GetFileType};
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, COINIT_MULTITHREADED, CoInitializeEx,
    };

    use super::{
        Failure, RenderJob, Rendered, encode, fallback, fit, office, office_com, pdf, preview, raw,
        shell, wic,
    };

    /// Minimum size requested from the shell for video posters (spec: 512 px).
    const VIDEO_POSTER_SIZE: u32 = 512;

    /// Decides who ends the process: the job (with its result) or the watchdog (with
    /// `EXIT_TIMEOUT`). Whoever takes it first wins; the other never exits on its own. Without
    /// it a watchdog that killed Office mid-call let the job's resulting error (`0x800706BE`)
    /// exit with code 4 first, and a timed-out job was reported as a broken document.
    static EXIT: Mutex<bool> = Mutex::new(false);

    /// Takes the exit for the caller. False when the other side already has it.
    fn claim_exit() -> bool {
        let mut taken = EXIT.lock().unwrap_or_else(|p| p.into_inner());
        !std::mem::replace(&mut *taken, true)
    }

    /// Called by the job side before it reports anything: if the watchdog has already fired,
    /// wait for it to finish its cleanup and end the process.
    fn job_may_exit() {
        if !claim_exit() {
            loop {
                std::thread::park();
            }
        }
    }

    pub fn run() -> u32 {
        let job = match read_job() {
            Ok(job) => job,
            Err(msg) => {
                eprintln!("gander-render: bad job: {msg}");
                return EXIT_BAD_JOB;
            }
        };
        start_watchdog(job.timeout_ms);
        let file = match take_handle(job.handle) {
            Ok(f) => f,
            Err(msg) => {
                eprintln!("gander-render: bad handle: {msg}");
                return EXIT_BAD_JOB;
            }
        };
        if !(1..=100).contains(&job.quality) {
            eprintln!("gander-render: bad job: quality out of range");
            return EXIT_BAD_JOB;
        }
        let result = match job.op {
            RenderOp::Thumb { size } => {
                if !(16..=4096).contains(&size) {
                    eprintln!("gander-render: bad job: size out of range");
                    return EXIT_BAD_JOB;
                }
                render(&file, &job, size)
                    .and_then(|img| encode::encode(&img, job.format, job.quality))
            }
            RenderOp::Page { page, width } => {
                if !(1..=65_535).contains(&page) || !(16..=4096).contains(&width) {
                    eprintln!("gander-render: bad job: page or width out of range");
                    return EXIT_BAD_JOB;
                }
                init_com(preview::needs_sta(&job))
                    .and_then(|()| preview::page(&file, &job, page, width))
                    .and_then(|img| encode::encode(&img, job.format, job.quality))
            }
            RenderOp::Tile { page, level, x, y } => {
                if !(1..=65_535).contains(&page) || level >= MAX_LEVELS || x > 65_535 || y > 65_535
                {
                    eprintln!("gander-render: bad job: tile address out of range");
                    return EXIT_BAD_JOB;
                }
                init_com(false)
                    .and_then(|()| preview::tile(&file, &job, page, level, x, y))
                    .and_then(|img| encode::encode(&img, job.format, job.quality))
            }
            RenderOp::Info => init_com(false)
                .and_then(|()| preview::info(&file, &job))
                .and_then(|meta| serde_json::to_vec(&meta).map_err(Failure::decode)),
            RenderOp::ExportPdf => init_com(true).and_then(|()| preview::export_pdf(&file, &job)),
        };
        job_may_exit();
        match result {
            Ok(bytes) => {
                let mut out = std::io::stdout().lock();
                if out.write_all(&bytes).and_then(|()| out.flush()).is_err() {
                    eprintln!("gander-render: stdout closed");
                    return EXIT_DECODER_ERROR;
                }
                EXIT_OK
            }
            Err(Failure::Unsupported(msg)) => {
                eprintln!("gander-render: unsupported: {msg}");
                EXIT_UNSUPPORTED
            }
            Err(Failure::Decode(msg)) => {
                eprintln!("gander-render: decoder error: {msg}");
                EXIT_DECODER_ERROR
            }
        }
    }

    /// First and only COM initialisation of this thread (the process exits after one job).
    /// Office automation and the shell need a single-threaded apartment.
    fn init_com(sta: bool) -> Rendered<()> {
        let apartment = if sta {
            COINIT_APARTMENTTHREADED
        } else {
            COINIT_MULTITHREADED
        };
        // SAFETY: first COM initialisation on this thread; never uninitialised because the
        // process exits after one job.
        unsafe { CoInitializeEx(None, apartment | COINIT_DISABLE_OLE1DDE) }
            .ok()
            .map_err(Failure::decode)
    }

    fn read_job() -> Result<RenderJob, String> {
        let mut buf = Vec::new();
        std::io::stdin()
            .lock()
            .take(MAX_JOB_BYTES as u64 + 1)
            .read_to_end(&mut buf)
            .map_err(|e| e.to_string())?;
        if buf.len() > MAX_JOB_BYTES {
            return Err("job description too large".into());
        }
        let job: RenderJob = serde_json::from_slice(&buf).map_err(|e| e.to_string())?;
        if job.v != JOB_VERSION {
            return Err(format!("unsupported job version {}", job.v));
        }
        Ok(job)
    }

    fn start_watchdog(timeout_ms: u64) {
        let timeout = Duration::from_millis(timeout_ms.clamp(100, 120_000));
        std::thread::spawn(move || {
            std::thread::sleep(timeout);
            if !claim_exit() {
                return; // the job finished first and is reporting its own result
            }
            eprintln!("gander-render: watchdog fired");
            // An Office instance is not a child of this process, so the job object does not
            // contain it: stop it (and remove the scratch copy) before exiting.
            office_com::emergency_cleanup();
            std::process::exit(EXIT_TIMEOUT as i32);
        });
    }

    fn take_handle(value: u64) -> Result<File, String> {
        let raw = usize::try_from(value).map_err(|_| "handle out of range".to_owned())?;
        if raw == 0 {
            return Err("null handle".into());
        }
        let handle = HANDLE(raw as *mut core::ffi::c_void);
        // SAFETY: GetFileType only inspects the handle; an invalid value yields FILE_TYPE_UNKNOWN.
        let kind = unsafe { GetFileType(handle) };
        if kind != FILE_TYPE_DISK {
            return Err("handle is not a disk file".into());
        }
        // SAFETY: the host duplicated this handle into this process for this job; nothing else
        // in the worker owns it, so `File` may take ownership.
        Ok(unsafe { File::from_raw_handle(raw as std::os::windows::io::RawHandle) })
    }

    fn render(file: &File, job: &RenderJob, size: u32) -> Rendered<RgbaImage> {
        init_com(job.kind.uses_shell())?;

        let img = match job.kind {
            RenderKind::Image => match wic::decode_file(file, size) {
                Err(Failure::Unsupported(why)) => {
                    fallback::decode_file(file, size).map_err(|e| match e {
                        Failure::Unsupported(more) => {
                            Failure::Unsupported(format!("{why}; {more}"))
                        }
                        other => other,
                    })
                }
                other => other,
            },
            RenderKind::Raw => raw::thumb(file, size),
            RenderKind::Pdf => pdf::thumb(file, size),
            RenderKind::Office => office::thumb(file, size),
            RenderKind::Video => shell::thumb(file, size.max(VIDEO_POSTER_SIZE)),
            RenderKind::Audio | RenderKind::Shell => shell::thumb(file, size),
        }?;
        Ok(downscale(img, size))
    }

    /// Final fit to the requested box (renderers may return larger images).
    fn downscale(img: RgbaImage, size: u32) -> RgbaImage {
        let (w, h) = img.dimensions();
        let (tw, th) = fit(w, h, size);
        if (tw, th) == (w, h) {
            img
        } else {
            image::imageops::thumbnail(&img, tw, th)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PREVIEW_PIXELS, Target, fit, fit_width};

    #[test]
    fn fit_width_scales_to_width_and_never_upscales() {
        assert_eq!(fit_width(4000, 3000, 1000), (1000, 750));
        assert_eq!(fit_width(3000, 4000, 1000), (1000, 1333));
        assert_eq!(fit_width(600, 400, 1000), (600, 400));
        assert_eq!(Target::Width(100).fit(0, 0), (1, 1));
        assert_eq!(Target::Box(128).fit(400, 200), (128, 64));
        // A very tall page is reduced to the pixel budget, keeping its aspect ratio.
        let (w, h) = fit_width(2000, 400_000, 2000);
        assert!(u64::from(w) * u64::from(h) <= MAX_PREVIEW_PIXELS);
        assert!(h > w * 100);
    }

    #[test]
    fn fit_keeps_aspect_and_never_upscales() {
        assert_eq!(fit(4000, 3000, 256), (256, 192));
        assert_eq!(fit(3000, 4000, 256), (192, 256));
        assert_eq!(fit(100, 50, 256), (100, 50));
        assert_eq!(fit(10_000, 1, 128), (128, 1));
        assert_eq!(fit(0, 0, 128), (1, 1));
    }
}
