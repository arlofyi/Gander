// SPDX-License-Identifier: Apache-2.0
//! Running one preview job: the worker for every format, plus what only Office needs.
//!
//! [`JobRunner`] is the seam between the preview service and the process machinery, so the
//! service is tested with fakes and the production [`WorkerRunner`] is tested end to end by the
//! `gander-render` integration tests.
//!
//! For Office automation jobs the runner also:
//!
//! - runs the worker at medium integrity (COM activation requires it; the worker is still in its
//!   job object) with a wall-clock cap that outlasts the worker's own watchdog;
//! - after the job, whatever its outcome, kills the Office instance the worker announced if it
//!   is still alive, and sweeps any automation-launched Office process that started during the
//!   job (a worker killed while an activation was in flight cannot clean up after itself). An
//!   Office the user started has no automation switch and is never touched.

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use super::office::OfficeApp;
use super::procs;
use crate::render::job::{
    OFFICE_PID_PREFIX, PAGE_OUT_OF_RANGE_MSG, RenderJob, RenderKind, RenderOp,
    TILE_OUT_OF_RANGE_MSG,
};
use crate::render::worker::{WorkerConfig, WorkerOutcome, run_job};

/// How a job ended, from the service's point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobResult {
    /// The worker's output (an image, JSON metadata or PDF bytes, depending on the op).
    Output(Vec<u8>),
    /// No renderer for these bytes, an encrypted document, or the backend is unavailable.
    Unsupported,
    /// The worker or converter ran past its limits and was killed.
    TimedOut,
    /// The decoder rejected the file, or the worker crashed on it.
    DecoderError,
    /// A `Page` job named a page past the end of the document.
    PageOutOfRange,
    /// A `Tile` job named a level, column or row the page does not have.
    TileOutOfRange,
    /// The host could not run the job (spawn failure, pipe error).
    HostFailure,
}

/// Something that runs preview jobs.
pub trait JobRunner: Send + Sync + 'static {
    /// Runs one worker job on `file`.
    fn run(&self, file: &File, job: &RenderJob) -> JobResult;

    /// Converts an Office or ODF document to PDF with the LibreOffice at `soffice`.
    fn convert_with_libreoffice(&self, file: &File, ext: &str, soffice: &Path) -> JobResult;

    /// Whether `app` is running on this computer right now (the owner's own instance). A failed
    /// render while this is true is not held against the document.
    fn office_busy(&self, _app: OfficeApp) -> bool {
        false
    }
}

/// Wall-clock allowance beyond the worker's own watchdog for an Office job.
const OFFICE_WALL_MARGIN: Duration = Duration::from_secs(15);
/// Largest PDF an Office export may send back.
const OFFICE_MAX_OUTPUT: usize = 64 * 1024 * 1024;
/// How long LibreOffice may take.
pub const LIBREOFFICE_TIMEOUT: Duration = Duration::from_secs(90);

/// The production runner: one `gander-render.exe` per job under a job object.
#[derive(Debug, Clone)]
pub struct WorkerRunner {
    /// Worker location and limits.
    pub config: WorkerConfig,
}

/// Whether the job automates an Office application (as opposed to reading the zip).
fn is_office_automation(job: &RenderJob) -> bool {
    job.kind == RenderKind::Office && matches!(job.op, RenderOp::Page { .. } | RenderOp::ExportPdf)
}

/// The pid after `gander-render: office-pid=` on any line of the worker's diagnostics.
fn announced_office_pid(stderr: &str) -> Option<u32> {
    stderr
        .lines()
        .find_map(|l| l.strip_prefix(OFFICE_PID_PREFIX))
        .and_then(|p| p.trim().parse().ok())
}

impl JobRunner for WorkerRunner {
    fn run(&self, file: &File, job: &RenderJob) -> JobResult {
        let office = is_office_automation(job);
        let mut config = self.config.clone();
        let since = procs::now_filetime();
        if office {
            config.limits.low_integrity = false;
            config.limits.wall_timeout = Duration::from_millis(job.timeout_ms) + OFFICE_WALL_MARGIN;
            config.limits.max_output_bytes = config.limits.max_output_bytes.max(OFFICE_MAX_OUTPUT);
        }
        let report = run_job(&config, file, job);
        if office {
            if let Ok(report) = &report
                && let Some(pid) = announced_office_pid(&report.stderr)
            {
                for exe in procs::OFFICE_EXES {
                    procs::kill_if(pid, exe, since);
                }
            }
            procs::sweep_stray_office(since);
        }
        match report {
            Err(_) => JobResult::HostFailure,
            Ok(report) => match report.outcome {
                WorkerOutcome::Ok(bytes) if !bytes.is_empty() => JobResult::Output(bytes),
                WorkerOutcome::Ok(_) => JobResult::DecoderError,
                WorkerOutcome::Unsupported => JobResult::Unsupported,
                WorkerOutcome::TimedOut => JobResult::TimedOut,
                WorkerOutcome::DecoderError => {
                    if report.stderr.contains(PAGE_OUT_OF_RANGE_MSG) {
                        JobResult::PageOutOfRange
                    } else if report.stderr.contains(TILE_OUT_OF_RANGE_MSG) {
                        JobResult::TileOutOfRange
                    } else {
                        JobResult::DecoderError
                    }
                }
                WorkerOutcome::BadJob => JobResult::HostFailure,
                WorkerOutcome::Crashed(_) | WorkerOutcome::OutputTooLarge => {
                    JobResult::DecoderError
                }
            },
        }
    }

    fn convert_with_libreoffice(&self, file: &File, ext: &str, soffice: &Path) -> JobResult {
        super::libreoffice::convert_to_pdf(soffice, file, ext, LIBREOFFICE_TIMEOUT)
    }

    fn office_busy(&self, app: OfficeApp) -> bool {
        !procs::pids_named(app.exe()).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn office_pid_is_read_from_the_diagnostics() {
        let stderr =
            "gander-render: office-pid=4242\ngander-render: office-timing work_ms=1 quit_ms=2\n";
        assert_eq!(announced_office_pid(stderr), Some(4242));
        assert_eq!(
            announced_office_pid("gander-render: watchdog fired\n"),
            None
        );
        assert_eq!(
            announced_office_pid("gander-render: office-pid=abc\n"),
            None
        );
    }

    #[test]
    fn only_page_and_export_jobs_automate_office() {
        let page = RenderJob::page(RenderKind::Office, "pptx", 1, 800);
        let export = RenderJob::new(RenderKind::Office, "docx", RenderOp::ExportPdf);
        let info = RenderJob::info(RenderKind::Office, "pptx");
        let thumb = RenderJob::thumb(RenderKind::Office, "pptx", 256);
        let pdf = RenderJob::page(RenderKind::Pdf, "pdf", 1, 800);
        assert!(is_office_automation(&page));
        assert!(is_office_automation(&export));
        assert!(!is_office_automation(&info));
        assert!(!is_office_automation(&thumb));
        assert!(!is_office_automation(&pdf));
    }
}
