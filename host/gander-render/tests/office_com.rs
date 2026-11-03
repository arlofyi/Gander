// SPDX-License-Identifier: Apache-2.0
//! Office COM automation through the real worker: PowerPoint slide export (the plan's Verify),
//! Word and Excel PDF export, the timeout kill and process hygiene.
//!
//! These tests start a hidden Office instance of their own. They skip with an explicit message
//! when the application is not installed, and also when Office is already running (a user's
//! instance is never touched, so the worker refuses and so do these tests). They run one at a
//! time (`OFFICE_LOCK`) because PowerPoint is single-instance.
//!
//! Every test that may start Office holds an [`OfficeTest`] guard: when the test ends - passed,
//! panicked or timed out - it stops every automation instance (`/automation` or `-Embedding` on
//! the command line) started since the test began, so a failure never leaves a hidden
//! PowerPoint behind to make later runs skip. An instance without the switch (the owner's) and
//! any instance older than the test are never touched.

#![cfg(windows)]

mod common;

use std::fs::File;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{temp_dir, write};
use gander_core::protocol::files::{ImageFormat, PreviewKind};
use gander_win::preview::office::OfficeEnv;
use gander_win::preview::procs;
use gander_win::preview::runner::WorkerRunner;
use gander_win::preview::service::{InfoRequest, PageRequest, PreviewError, PreviewService};
use gander_win::render::job::{RenderJob, RenderKind, RenderOp};
use gander_win::render::worker::{WorkerConfig, WorkerOutcome, WorkerReport, run_job};
use gander_win::thumbs::cache::ThumbCache;

static OFFICE_LOCK: Mutex<()> = Mutex::new(());

/// The worker watchdog these tests give an Office job. Generous on purpose: it is the test
/// harness's budget under a loaded machine (a whole-workspace run), not the product's
/// (`OFFICE_TIMEOUT_MS` in the preview service, unchanged). The plan's 30 s Verify is asserted
/// separately on the measured time.
const TEST_OFFICE_TIMEOUT_MS: u64 = 90_000;

/// How long a test waits for an Office instance that is already running to exit (a previous
/// test's instance still quitting) before it skips.
const SETTLE: Duration = Duration::from_secs(20);

/// Name of the session-wide mutex that serialises Office tests across processes: two test
/// binaries (parallel worktrees, a second `cargo test`) must not drive the single-instance
/// PowerPoint, or sweep each other's instances, at the same time.
const MACHINE_LOCK_NAME: &str = "Local\\GanderOfficeComTests";
/// How long a test waits for another process's Office tests before it skips.
const MACHINE_LOCK_WAIT: Duration = Duration::from_secs(600);

/// The session-wide Office test mutex, owned by the test thread that took it.
struct MachineLock(windows_sys::Win32::Foundation::HANDLE);

impl MachineLock {
    /// Waits for the mutex. `None` when another process held it for [`MACHINE_LOCK_WAIT`].
    fn acquire() -> Option<Self> {
        use windows_sys::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
        let name: Vec<u16> = MACHINE_LOCK_NAME.encode_utf16().chain(Some(0)).collect();
        // SAFETY: NUL-terminated name, default security, not initially owned.
        let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        assert!(!h.is_null(), "cannot create the Office test mutex");
        let lock = MachineLock(h);
        let ms = u32::try_from(MACHINE_LOCK_WAIT.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: valid mutex handle. An abandoned mutex (a test process killed while holding
        // it) is owned by this thread now, which is what we want.
        match unsafe { WaitForSingleObject(lock.0, ms) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Some(lock),
            _ => {
                // Not owned: only close the handle.
                // SAFETY: valid handle, closed once; `forget` skips the release in `Drop`.
                unsafe { windows_sys::Win32::Foundation::CloseHandle(lock.0) };
                std::mem::forget(lock);
                None
            }
        }
    }
}

impl Drop for MachineLock {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Threading::ReleaseMutex;
        // SAFETY: owned by this thread (taken in `acquire`, dropped on the same test thread);
        // the handle is closed once.
        unsafe {
            ReleaseMutex(self.0);
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Serialises the Office tests (in this process and across processes) and cleans up after
/// each one, however it ends.
struct OfficeTest {
    since: u64,
    // Field order is drop order: clean up, then release the session lock, then the local one.
    _machine: MachineLock,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl OfficeTest {
    /// Takes both locks and notes when the test began. `None` (after printing a `SKIPPED` line)
    /// when another process kept the Office tests for too long.
    fn begin() -> Option<Self> {
        let lock = OFFICE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(machine) = MachineLock::acquire() else {
            eprintln!(
                "SKIPPED: another process has been running Office tests for over {} s",
                MACHINE_LOCK_WAIT.as_secs()
            );
            return None;
        };
        Some(OfficeTest {
            since: procs::now_filetime(),
            _machine: machine,
            _lock: lock,
        })
    }
}

impl Drop for OfficeTest {
    fn drop(&mut self) {
        // Runs on success and while unwinding from a failed assertion or a worker timeout.
        let killed = procs::sweep_stray_office(self.since);
        if killed > 0 {
            eprintln!(
                "office test cleanup: stopped {killed} automation instance(s) the test started"
            );
        }
        for exe in procs::OFFICE_EXES {
            for pid in procs::pids_named(exe) {
                if procs::started_at(pid).is_some_and(|t| t >= self.since) {
                    procs::wait_exit(pid, Duration::from_secs(10));
                }
            }
        }
    }
}

/// Waits up to [`SETTLE`] for every `exe` process to exit. True when none is left.
fn settled(exe: &str) -> bool {
    let deadline = Instant::now() + SETTLE;
    while !procs::pids_named(exe).is_empty() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    true
}

/// Prints the clear skip line for a running `exe`, saying whose instance it is.
fn skip_running(exe: &str) {
    let who: Vec<String> = procs::pids_named(exe)
        .into_iter()
        .map(|pid| {
            let automation =
                procs::command_line(pid).is_some_and(|c| procs::is_automation_command_line(&c));
            format!(
                "pid {pid} ({})",
                if automation {
                    "automation instance, not started by this test"
                } else {
                    "started by the user"
                }
            )
        })
        .collect();
    eprintln!(
        "SKIPPED: {exe} is already running ({}); Gander never touches a running Office",
        who.join(", ")
    );
}

/// Config for an Office job: medium integrity (COM activation needs it), Office-sized timeouts.
fn office_config() -> WorkerConfig {
    let mut cfg = WorkerConfig::new(env!("CARGO_BIN_EXE_gander-render"));
    cfg.limits.low_integrity = false;
    cfg.limits.wall_timeout =
        Duration::from_millis(TEST_OFFICE_TIMEOUT_MS) + Duration::from_secs(30);
    cfg.limits.max_output_bytes = 64 * 1024 * 1024;
    cfg
}

fn office_job(ext: &str, op: RenderOp, timeout_ms: u64) -> RenderJob {
    let mut job = RenderJob::new(RenderKind::Office, ext, op);
    job.timeout_ms = timeout_ms;
    job
}

fn run(path: &std::path::Path, job: &RenderJob) -> WorkerReport {
    let file = File::open(path).unwrap();
    run_job(&office_config(), &file, job).unwrap()
}

/// Whether to run a test for `exe` (an Office application image name). Prints why not.
fn ready(app: &str, installed: bool, exe: &str) -> Option<OfficeTest> {
    if !installed {
        eprintln!("SKIPPED: {app} is not installed on this machine (Office COM test not run)");
        return None;
    }
    let guard = OfficeTest::begin()?;
    if !settled(exe) {
        skip_running(exe);
        return None;
    }
    Some(guard)
}

fn running(exe: &str) -> Vec<u32> {
    procs::pids_named(exe)
}

#[test]
fn powerpoint_slide_export_renders_slide_one() {
    let env = OfficeEnv::detect();
    let Some(_guard) = ready("PowerPoint", env.powerpoint, "POWERPNT.EXE") else {
        return;
    };
    let dir = temp_dir("ppt");
    let path = write(&dir, "deck.pptx", &common::pptx(2, None));
    let started = Instant::now();
    let job = office_job(
        "pptx",
        RenderOp::Page {
            page: 1,
            width: 640,
        },
        TEST_OFFICE_TIMEOUT_MS,
    );
    let report = run(&path, &job);
    let elapsed = started.elapsed();
    eprintln!(
        "PowerPoint COM: slide 1 of a generated deck rendered in {:.2} s (exit {}, stderr: {})",
        elapsed.as_secs_f64(),
        report.exit_code,
        report.stderr.trim()
    );
    let WorkerOutcome::Ok(bytes) = &report.outcome else {
        panic!("{:?} stderr: {}", report.outcome, report.stderr);
    };
    assert!(elapsed < Duration::from_secs(30), "took {elapsed:?}");
    let img = image::load_from_memory(bytes).unwrap().to_rgb8();
    assert_eq!(img.dimensions(), (640, 360), "16:9 slide at width 640");
    let left = img.get_pixel(100, 180).0;
    let right = img.get_pixel(540, 180).0;
    assert!(
        left[0] > 200 && left[1] < 60 && left[2] < 60,
        "left half is red: {left:?}"
    );
    assert!(
        right[0] > 240 && right[1] > 240 && right[2] > 240,
        "right half is white: {right:?}"
    );

    // Slide 2 is the mirror image (blue on the right); slide 3 does not exist.
    let second = run(
        &path,
        &office_job(
            "pptx",
            RenderOp::Page {
                page: 2,
                width: 320,
            },
            TEST_OFFICE_TIMEOUT_MS,
        ),
    );
    let WorkerOutcome::Ok(bytes) = &second.outcome else {
        panic!("{:?} stderr: {}", second.outcome, second.stderr);
    };
    let img = image::load_from_memory(bytes).unwrap().to_rgb8();
    let right = img.get_pixel(270, 90).0;
    assert!(
        right[2] > 200 && right[0] < 60,
        "slide 2 right is blue: {right:?}"
    );
    let third = run(
        &path,
        &office_job(
            "pptx",
            RenderOp::Page {
                page: 3,
                width: 320,
            },
            TEST_OFFICE_TIMEOUT_MS,
        ),
    );
    assert_eq!(third.exit_code, 4);
    assert!(
        third.stderr.contains("page out of range"),
        "{}",
        third.stderr
    );

    assert!(
        running("POWERPNT.EXE").is_empty(),
        "no PowerPoint left running"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn word_and_excel_export_pdf() {
    let env = OfficeEnv::detect();
    for (app, installed, exe, ext, bytes) in [
        ("Word", env.word, "WINWORD.EXE", "docx", common::docx()),
        ("Excel", env.excel, "EXCEL.EXE", "xlsx", common::xlsx()),
    ] {
        let Some(_guard) = ready(app, installed, exe) else {
            continue;
        };
        let dir = temp_dir(ext);
        let path = write(&dir, &format!("doc.{ext}"), &bytes);
        let started = Instant::now();
        let report = run(
            &path,
            &office_job(ext, RenderOp::ExportPdf, TEST_OFFICE_TIMEOUT_MS),
        );
        eprintln!(
            "{app} COM: PDF export in {:.2} s (exit {}, stderr: {})",
            started.elapsed().as_secs_f64(),
            report.exit_code,
            report.stderr.trim()
        );
        let WorkerOutcome::Ok(pdf) = &report.outcome else {
            panic!("{app}: {:?} stderr: {}", report.outcome, report.stderr);
        };
        assert!(pdf.starts_with(b"%PDF"), "{app} produced a PDF");
        assert!(running(exe).is_empty(), "no {exe} left running");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// Names of the scratch folders Office jobs leave in the temp directory.
fn scratch_dirs() -> Vec<String> {
    std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("gander-office-"))
        .collect()
}

#[test]
fn a_timeout_kills_the_office_instance_it_started() {
    let env = OfficeEnv::detect();
    let Some(_guard) = ready("PowerPoint", env.powerpoint, "POWERPNT.EXE") else {
        return;
    };
    let dir = temp_dir("pptkill");
    let path = write(&dir, "deck.pptx", &common::pptx(1, None));
    let scratch_before = scratch_dirs();
    let since = procs::now_filetime();
    // The worker's own watchdog fires before PowerPoint can have started and opened a deck.
    let report = run(
        &path,
        &office_job(
            "pptx",
            RenderOp::Page {
                page: 1,
                width: 320,
            },
            700,
        ),
    );
    eprintln!(
        "timeout test: exit {} after {:.2} s, stderr: {}",
        report.exit_code,
        report.elapsed.as_secs_f64(),
        report.stderr.trim()
    );
    assert_eq!(report.outcome, WorkerOutcome::TimedOut, "{}", report.stderr);
    assert_eq!(report.exit_code, 3);
    // Whatever the watchdog could not reach (an activation still in flight when it fired) is
    // what the host-side sweep after every Office job removes.
    std::thread::sleep(Duration::from_millis(1500));
    procs::sweep_stray_office(since);
    let deadline = Instant::now() + Duration::from_secs(15);
    while !running("POWERPNT.EXE").is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        running("POWERPNT.EXE").is_empty(),
        "the PowerPoint this test started is gone after the timeout"
    );
    let scratch_after = scratch_dirs();
    let leaked: Vec<_> = scratch_after
        .iter()
        .filter(|n| !scratch_before.contains(n))
        .collect();
    assert!(leaked.is_empty(), "scratch folders left behind: {leaked:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A stand-in process whose image name is `POWERPNT.EXE` (a renamed copy of a system tool), so
/// the process rules can be tested without a real Office. Killed on drop.
struct FakeOffice {
    child: std::process::Child,
    dir: std::path::PathBuf,
}

impl FakeOffice {
    fn start(name: &str, args: &[&str]) -> Self {
        let dir = temp_dir(name);
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let exe = dir.join("POWERPNT.EXE");
        std::fs::copy(format!(r"{system}\System32\cmd.exe"), &exe).unwrap();
        let child = std::process::Command::new(&exe).args(args).spawn().unwrap();
        let fake = FakeOffice { child, dir };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !running("POWERPNT.EXE").contains(&fake.child.id()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        fake
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for FakeOffice {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_running_powerpoint_is_refused_and_left_alone() {
    let Some(guard) = OfficeTest::begin() else {
        return;
    };
    if !settled("POWERPNT.EXE") {
        skip_running("POWERPNT.EXE");
        return;
    }
    // "The user's PowerPoint": no automation switch on its command line.
    let mut users = FakeOffice::start("users-ppt", &["/c", "ping -n 40 127.0.0.1 >nul"]);
    assert!(running("POWERPNT.EXE").contains(&users.child.id()));

    let dir = temp_dir("refuse");
    let path = write(&dir, "deck.pptx", &common::pptx(1, None));
    let started = Instant::now();
    let report = run(
        &path,
        &office_job(
            "pptx",
            RenderOp::Page {
                page: 1,
                width: 320,
            },
            20_000,
        ),
    );
    assert_eq!(
        report.outcome,
        WorkerOutcome::Unsupported,
        "{}",
        report.stderr
    );
    assert!(
        report.stderr.contains("already running"),
        "{}",
        report.stderr
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "refused before any COM work"
    );
    // The refusal touched nothing, and the post-job sweep (which the service runs) spares an
    // instance the user started: it has no automation switch.
    assert!(
        !report.stderr.contains("office-pid"),
        "no instance was claimed"
    );
    // The user's instance started after this test began, so only the missing switch spares it.
    procs::sweep_automation("POWERPNT.EXE", guard.since);
    assert!(users.alive(), "the user's PowerPoint is still running");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_sweep_kills_only_automation_instances_started_after_the_job_began() {
    let Some(_guard) = OfficeTest::begin() else {
        return;
    };
    if !settled("POWERPNT.EXE") {
        skip_running("POWERPNT.EXE");
        return;
    }
    let job_began = procs::now_filetime();
    let mut orphan = FakeOffice::start(
        "orphan-ppt",
        &["/c", "ping -n 40 127.0.0.1 >nul & rem /Automation"],
    );
    // Started after "the job began": swept. (Only POWERPNT.EXE and only since this test's
    // start: a sweep here must never reach an instance this test did not create.)
    assert_eq!(procs::sweep_automation("POWERPNT.EXE", job_began), 1);
    let deadline = Instant::now() + Duration::from_secs(5);
    while orphan.alive() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !orphan.alive(),
        "the automation-launched stand-in was killed"
    );

    // Started *before* the job began: spared even with the switch.
    let mut earlier = FakeOffice::start(
        "earlier-ppt",
        &["/c", "ping -n 40 127.0.0.1 >nul & rem /Automation"],
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(procs::sweep_stray_office(procs::now_filetime()), 0);
    assert!(earlier.alive(), "an older instance is never swept");
}

// ---------------------------------------------------------------------------------------------
// The whole stack: `PreviewService` -> `WorkerRunner` -> worker -> a real Office instance.

fn real_service(dir: &std::path::Path, env: &OfficeEnv) -> PreviewService<WorkerRunner> {
    let cache = Arc::new(ThumbCache::open_in(&dir.join("cache"), "previews", 256 << 20).unwrap());
    PreviewService::with_worker(
        cache,
        WorkerConfig::new(env!("CARGO_BIN_EXE_gander-render")),
        env.clone(),
    )
}

fn poll_page(
    svc: &PreviewService<WorkerRunner>,
    path: &std::path::Path,
    page: u32,
    width: u32,
) -> Result<gander_win::preview::service::PageResponse, PreviewError> {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let req = PageRequest {
            file: File::open(path).unwrap(),
            ext: path.extension().unwrap().to_string_lossy().into_owned(),
            device: "dev-office".into(),
            page: Some(page),
            width: Some(width),
            format: ImageFormat::Webp,
        };
        match svc.page(req) {
            Err(PreviewError::Generating { .. }) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            other => return other,
        }
    }
}

fn service_info(
    svc: &PreviewService<WorkerRunner>,
    path: &std::path::Path,
) -> gander_core::protocol::files::PreviewInfo {
    let req = InfoRequest {
        file: File::open(path).unwrap(),
        ext: path.extension().unwrap().to_string_lossy().into_owned(),
        device: "dev-office".into(),
    };
    svc.info(req).unwrap()
}

#[test]
fn the_service_renders_a_deck_a_document_and_a_workbook_through_real_office() {
    let env = OfficeEnv::detect();
    let dir = temp_dir("svc-office");
    let svc = real_service(&dir, &env);

    // PowerPoint: slide export per page.
    if let Some(_guard) = ready("PowerPoint", env.powerpoint, "POWERPNT.EXE") {
        let path = write(&dir, "deck.pptx", &common::pptx(2, None));
        let info = service_info(&svc, &path);
        assert_eq!(info.kind, PreviewKind::Office);
        assert_eq!(info.renderer, "office_com_powerpoint");
        assert_eq!(
            (info.pages, info.width, info.height),
            (Some(2), Some(960), Some(540))
        );
        assert!(info.estimated_ms > 0);
        let started = Instant::now();
        let r = poll_page(&svc, &path, 1, 640).unwrap();
        eprintln!(
            "service: pptx slide 1 in {:.2} s (estimated_ms was {})",
            started.elapsed().as_secs_f64(),
            info.estimated_ms
        );
        assert!(!r.degraded);
        assert_eq!((r.renderer, r.pages), ("office_com_powerpoint", Some(2)));
        let img = image::load_from_memory(&r.bytes).unwrap().to_rgb8();
        assert_eq!(img.dimensions(), (640, 360));
        assert!(img.get_pixel(100, 180).0[0] > 200 && img.get_pixel(100, 180).0[1] < 60);
        let r2 = poll_page(&svc, &path, 2, 640).unwrap();
        let img2 = image::load_from_memory(&r2.bytes).unwrap().to_rgb8();
        assert!(img2.get_pixel(540, 180).0[2] > 200 && img2.get_pixel(540, 180).0[0] < 60);
        // Slide 3 does not exist; the known slide count rejects it without starting PowerPoint.
        assert_eq!(
            poll_page(&svc, &path, 3, 640).unwrap_err(),
            PreviewError::BadRequest("page out of range")
        );
        assert!(running("POWERPNT.EXE").is_empty());
    }

    // Word: exported to PDF once, pages come from the PDF.
    if let Some(_guard) = ready("Word", env.word, "WINWORD.EXE") {
        let path = write(&dir, "report.docx", &common::docx());
        assert_eq!(
            service_info(&svc, &path).pages,
            None,
            "unknown before the first render"
        );
        let started = Instant::now();
        let r = poll_page(&svc, &path, 1, 600).unwrap();
        eprintln!(
            "service: docx page 1 in {:.2} s",
            started.elapsed().as_secs_f64()
        );
        assert_eq!((r.renderer, r.degraded), ("office_com_word", false));
        assert_eq!(image::load_from_memory(&r.bytes).unwrap().width(), 600);
        let after = service_info(&svc, &path);
        assert_eq!(
            after.pages,
            Some(2),
            "the page count is known once the PDF exists"
        );
        let started = Instant::now();
        let r2 = poll_page(&svc, &path, 2, 600).unwrap();
        eprintln!(
            "service: docx page 2 (from the cached PDF) in {:.2} s",
            started.elapsed().as_secs_f64()
        );
        assert_ne!(r.etag, r2.etag);
        assert_eq!(
            poll_page(&svc, &path, 3, 600).unwrap_err(),
            PreviewError::BadRequest("page out of range")
        );
        assert!(running("WINWORD.EXE").is_empty());
    }

    // Excel.
    if let Some(_guard) = ready("Excel", env.excel, "EXCEL.EXE") {
        let path = write(&dir, "sheet.xlsx", &common::xlsx());
        let r = poll_page(&svc, &path, 1, 500).unwrap();
        assert_eq!((r.renderer, r.degraded), ("office_com_excel", false));
        assert_eq!(image::load_from_memory(&r.bytes).unwrap().width(), 500);
        assert_eq!(service_info(&svc, &path).pages, Some(1));
        assert!(running("EXCEL.EXE").is_empty());
    }
    drop(svc);
    std::fs::remove_dir_all(&dir).unwrap();
}
