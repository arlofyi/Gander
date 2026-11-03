// SPDX-License-Identifier: Apache-2.0
//! Office rendering through COM automation (renderer spec, "office (render)").
//!
//! - **PowerPoint** (`.pptx`): `Presentation.Slides(n).Export` to PNG at the requested width.
//! - **Word** (`.docx`) and **Excel** (`.xlsx`): `ExportAsFixedFormat` to PDF; the host caches
//!   the PDF and renders its pages through the PDF renderer ([`RenderOp::ExportPdf`]).
//!
//! Rules this module keeps (and the tests check):
//!
//! 1. **Own, hidden instance only.** The application is created with `CoCreateInstance`, never
//!    `GetActiveObject`. PowerPoint is single-instance: if any `POWERPNT.EXE` is running the job
//!    is refused before any COM call. For Word and Excel the process list is compared before and
//!    after creation, and if no new process appeared the worker attached to the user's instance:
//!    it lets go without calling anything, and above all without `Quit`.
//! 2. **The document is a private copy.** The bytes are copied from the inherited handle into a
//!    scratch folder and Office opens the copy: no path from the phone reaches Office, the
//!    share's folder never gets an Office lock file, and a file swapped after the resolver
//!    checked it cannot be what Office parses. Macros are force-disabled, alerts are off, the
//!    document opens read-only without a window, and link updates are off where the object
//!    model offers it.
//! 3. **Always cleaned up.** [`Session`] quits the application on drop, waits for the process to
//!    go, kills it if it will not, and deletes the scratch folder. The worker's watchdog calls
//!    [`emergency_cleanup`] first, because an Office process is not a child of the worker and the
//!    job object does not contain it. The pid is also printed on stderr so the host can kill it
//!    after a worker that was itself killed.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gander_win::preview::procs;
use gander_win::render::job::{OFFICE_PID_PREFIX, PAGE_OUT_OF_RANGE_MSG};
use image::RgbaImage;

use crate::dispatch::{DispError, Dispatch, Var};
use crate::{Failure, Rendered, Target, wic};

/// Largest document copied into the scratch folder.
const MAX_DOC_BYTES: u64 = 256 * 1024 * 1024;
/// Largest PDF the export may produce.
const MAX_PDF_BYTES: u64 = 64 * 1024 * 1024;
/// Tallest slide image requested (pixels).
const MAX_SLIDE_HEIGHT: u32 = 8192;
/// How long a politely quit application gets to exit before it is killed.
const QUIT_GRACE: Duration = Duration::from_secs(8);
/// How long [`emergency_cleanup`] keeps sweeping for an instance whose activation was still in
/// flight when the watchdog fired (DCOM may finish launching it after the first sweep).
const ACTIVATION_SWEEP: Duration = Duration::from_secs(2);
/// Pause before the one retry of an activation that failed at the RPC layer.
const ACTIVATION_RETRY_DELAY: Duration = Duration::from_secs(1);

/// `RPC_S_SERVER_UNAVAILABLE`, `RPC_S_CALL_FAILED`, `RPC_E_DISCONNECTED`,
/// `CO_E_SERVER_EXEC_FAILURE`: the activation reached a server that was exiting (a previous
/// automation instance that had just quit or been killed can still be registered with DCOM for
/// a moment). Worth one retry once that instance is gone; anything else is final.
const TRANSIENT_ACTIVATION_ERRORS: [u32; 4] = [0x8007_06BA, 0x8007_06BE, 0x8001_0108, 0x8008_0005];

/// `msoAutomationSecurityForceDisable`: no macros run when the automation opens a file.
const AUTOMATION_SECURITY_FORCE_DISABLE: i32 = 3;
/// `MsoTriState.msoTrue` / `msoFalse`.
const MSO_TRUE: i32 = -1;
const MSO_FALSE: i32 = 0;
/// `ppAlertsNone`.
const PP_ALERTS_NONE: i32 = 1;
/// `wdAlertsNone`.
const WD_ALERTS_NONE: i32 = 0;
/// `wdDoNotSaveChanges`.
const WD_DO_NOT_SAVE_CHANGES: i32 = 0;
/// `wdExportFormatPDF`.
const WD_EXPORT_FORMAT_PDF: i32 = 17;
/// `xlTypePDF`.
const XL_TYPE_PDF: i32 = 0;

/// The Office application that handles a file type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum App {
    PowerPoint,
    Word,
    Excel,
}

impl App {
    /// The application for an extension (lower case, no dot). Macro-enabled and legacy formats
    /// are deliberately absent.
    pub fn from_ext(ext: &str) -> Option<App> {
        match ext {
            "pptx" => Some(App::PowerPoint),
            "docx" => Some(App::Word),
            "xlsx" => Some(App::Excel),
            _ => None,
        }
    }

    fn prog_id(self) -> &'static str {
        match self {
            App::PowerPoint => "PowerPoint.Application",
            App::Word => "Word.Application",
            App::Excel => "Excel.Application",
        }
    }

    fn exe(self) -> &'static str {
        match self {
            App::PowerPoint => "POWERPNT.EXE",
            App::Word => "WINWORD.EXE",
            App::Excel => "EXCEL.EXE",
        }
    }

    fn label(self) -> &'static str {
        match self {
            App::PowerPoint => "PowerPoint",
            App::Word => "Word",
            App::Excel => "Excel",
        }
    }
}

/// What [`emergency_cleanup`] must undo.
#[derive(Default)]
struct Cleanup {
    /// The Office process this worker owns: pid, image name, our start time.
    office: Option<(u32, &'static str, u64)>,
    /// An activation in flight (image name, when it began): the process may already exist
    /// while its pid is not known here yet.
    activating: Option<(&'static str, u64)>,
    scratch: Option<PathBuf>,
}

static CLEANUP: Mutex<Cleanup> = Mutex::new(Cleanup {
    office: None,
    activating: None,
    scratch: None,
});

fn with_cleanup<T>(f: impl FnOnce(&mut Cleanup) -> T) -> T {
    f(&mut CLEANUP.lock().unwrap_or_else(|p| p.into_inner()))
}

/// Stops the Office process this worker started and deletes its scratch folder. Called by the
/// watchdog thread just before the worker exits, and safe to call at any time.
pub fn emergency_cleanup() {
    let (office, activating, scratch) =
        with_cleanup(|c| (c.office.take(), c.activating.take(), c.scratch.take()));
    if let Some((pid, exe, since)) = office {
        procs::kill_if(pid, exe, since);
        procs::wait_exit(pid, Duration::from_secs(3));
    } else if let Some((exe, since)) = activating {
        // The watchdog fired inside `CoCreateInstance`: the instance DCOM is launching for us
        // has no recorded pid yet. Sweep automation instances of that application started since
        // the activation began, for a moment, so one that appears only now is caught too.
        let until = Instant::now() + ACTIVATION_SWEEP;
        loop {
            procs::sweep_automation(exe, since);
            if Instant::now() >= until {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    if let Some(dir) = scratch {
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn fail(app: App, e: &DispError) -> Failure {
    Failure::Decode(format!("{}: {e}", app.label()))
}

/// A private folder under the temp directory holding the document copy and the output.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new() -> io::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir =
            std::env::temp_dir().join(format!("gander-office-{}-{nanos:x}", std::process::id()));
        std::fs::create_dir(&dir)?;
        with_cleanup(|c| c.scratch = Some(dir.clone()));
        Ok(Scratch { dir })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        with_cleanup(|c| c.scratch = None);
    }
}

/// Copies the open file (from the start) into `dest`, refusing more than [`MAX_DOC_BYTES`].
fn copy_in(file: &File, dest: &Path) -> Rendered<()> {
    use std::io::{Seek, SeekFrom};
    let mut src = file;
    src.seek(SeekFrom::Start(0)).map_err(Failure::decode)?;
    let mut out = File::create(dest).map_err(Failure::decode)?;
    let copied = io::copy(&mut src.take(MAX_DOC_BYTES + 1), &mut out).map_err(Failure::decode)?;
    if copied > MAX_DOC_BYTES {
        return Err(Failure::unsupported("document too large to render"));
    }
    Ok(())
}

/// One automation session: our own hidden application instance plus the scratch copy of the
/// document. Dropping it quits and, if necessary, kills the instance and removes the scratch.
struct Session {
    app_kind: App,
    app: Option<Dispatch>,
    doc: Option<Dispatch>,
    pid: u32,
    started: u64,
    began: Instant,
    scratch: Scratch,
    copy: PathBuf,
}

impl Session {
    fn start(file: &File, ext: &str) -> Rendered<Session> {
        let app_kind = App::from_ext(ext)
            .ok_or_else(|| Failure::unsupported("no Office renderer for this type"))?;
        let scratch = Scratch::new().map_err(Failure::decode)?;
        let copy = scratch.path(&format!("doc.{ext}"));
        copy_in(file, &copy)?;

        // PowerPoint hands every activation to the running instance, so a user's PowerPoint
        // would be driven (and quit). Refuse before touching COM.
        if app_kind == App::PowerPoint && !procs::pids_named(app_kind.exe()).is_empty() {
            return Err(Failure::unsupported(
                "PowerPoint is already running; leaving it alone",
            ));
        }
        let before = procs::pids_named(app_kind.exe());
        let started = procs::now_filetime();
        let app = activate(app_kind, &before, started)?;
        // Which process is ours? A new one that carries the automation switch.
        let mine = new_automation_pids(app_kind, &before);
        let Some(&pid) = mine.first() else {
            // Attached to an instance that was already there: do not call it, do not quit it.
            drop(app);
            with_cleanup(|c| c.activating = None);
            return Err(Failure::unsupported(
                "the application handed back a running instance; leaving it alone",
            ));
        };
        with_cleanup(|c| {
            c.office = Some((pid, app_kind.exe(), started));
            c.activating = None;
        });
        eprintln!("{OFFICE_PID_PREFIX}{pid}");
        procs::set_below_normal(pid);
        Ok(Session {
            app_kind,
            app: Some(app),
            doc: None,
            pid,
            started,
            began: Instant::now(),
            scratch,
            copy,
        })
    }

    fn app(&self) -> &Dispatch {
        self.app.as_ref().expect("application is alive until drop")
    }

    fn copy_str(&self) -> String {
        self.copy.to_string_lossy().into_owned()
    }

    fn harden(&self) {
        let app = self.app();
        // Best effort: not every property exists on every version.
        let _ = app.put(
            "AutomationSecurity",
            Var::i4(AUTOMATION_SECURITY_FORCE_DISABLE),
        );
        match self.app_kind {
            App::PowerPoint => {
                let _ = app.put("DisplayAlerts", Var::i4(PP_ALERTS_NONE));
            }
            App::Word => {
                let _ = app.put("DisplayAlerts", Var::i4(WD_ALERTS_NONE));
                let _ = app.put("Visible", Var::boolean(false));
                if let Ok(options) = app.child("Options") {
                    let _ = options.put("UpdateLinksAtOpen", Var::boolean(false));
                }
            }
            App::Excel => {
                let _ = app.put("DisplayAlerts", Var::boolean(false));
                let _ = app.put("Visible", Var::boolean(false));
                let _ = app.put("EnableEvents", Var::boolean(false));
            }
        }
    }
}

/// Processes of `app` that were not in `before` and carry the automation switch (or whose
/// command line cannot be read).
fn new_automation_pids(app: App, before: &[u32]) -> Vec<u32> {
    let mut mine = procs::pids_named(app.exe());
    mine.retain(|pid| !before.contains(pid));
    mine.retain(|pid| {
        procs::command_line(*pid).is_none_or(|c| procs::is_automation_command_line(&c))
    });
    mine
}

/// Stops what a failed activation of `app` may have launched: instances new since `before`,
/// carrying the automation switch, started at or after `started`. Never a pre-existing or
/// user-started instance.
fn kill_our_strays(app: App, before: &[u32], started: u64) {
    for pid in new_automation_pids(app, before) {
        let automation =
            procs::command_line(pid).is_some_and(|c| procs::is_automation_command_line(&c));
        if automation && procs::kill_if(pid, app.exe(), started) {
            procs::wait_exit(pid, Duration::from_secs(3));
        }
    }
}

/// `CoCreateInstance` for `app`, with the in-flight activation recorded for the watchdog. One
/// retry after a transient RPC failure (see [`TRANSIENT_ACTIVATION_ERRORS`]); whatever a failed
/// attempt launched is stopped before the error is returned.
fn activate(app: App, before: &[u32], started: u64) -> Rendered<Dispatch> {
    with_cleanup(|c| c.activating = Some((app.exe(), started)));
    let mut attempt = 0;
    loop {
        attempt += 1;
        match Dispatch::create(app.prog_id()) {
            Ok(dispatch) => return Ok(dispatch),
            Err(e) => {
                kill_our_strays(app, before, started);
                let transient = TRANSIENT_ACTIVATION_ERRORS.contains(&(e.code.0 as u32));
                // PowerPoint is single-instance: never retry next to an instance that appeared
                // meanwhile (it could be the user's).
                let clear = app != App::PowerPoint || procs::pids_named(app.exe()).is_empty();
                if attempt == 1 && transient && clear {
                    eprintln!(
                        "gander-render: {} activation failed ({e}); retrying once",
                        app.label()
                    );
                    std::thread::sleep(ACTIVATION_RETRY_DELAY);
                    continue;
                }
                with_cleanup(|c| c.activating = None);
                return Err(Failure::Unsupported(format!(
                    "cannot start {}: {e}",
                    app.label()
                )));
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let worked = self.began.elapsed();
        // The deadline starts BEFORE the polite calls: `Close` and `Quit` are cross-process COM
        // calls that can block for a minute when the application is slow to shut down (measured
        // 2026-11-02 under a loaded test run: work 0.5 s, `Quit` 60 s, so the worker's watchdog
        // fired on a finished render). Killing the process makes a blocked call return
        // (`RPC_E_DISCONNECTED` or `RPC_S_CALL_FAILED`, ignored here).
        let (pid, exe, started) = (self.pid, self.app_kind.exe(), self.started);
        let deadline = std::thread::spawn(move || {
            if !procs::wait_exit(pid, QUIT_GRACE) {
                procs::kill_if(pid, exe, started);
                procs::wait_exit(pid, Duration::from_secs(3));
            }
        });
        if let Some(doc) = self.doc.take() {
            let close = match self.app_kind {
                App::PowerPoint => doc.call("Close", &[]),
                App::Word | App::Excel => {
                    doc.call_named("Close", &[], &[("SaveChanges", Var::boolean(false))])
                }
            };
            let _ = close;
        }
        if let Some(app) = self.app.take() {
            let _ = match self.app_kind {
                // Without SaveChanges Word may wait to ask about Normal.dotm until it is killed.
                App::Word => app.call_named(
                    "Quit",
                    &[],
                    &[("SaveChanges", Var::i4(WD_DO_NOT_SAVE_CHANGES))],
                ),
                App::PowerPoint | App::Excel => app.call("Quit", &[]),
            };
        }
        if deadline.join().is_err() {
            // The deadline thread cannot panic in practice; never leave the instance behind.
            procs::kill_if(pid, exe, started);
            procs::wait_exit(pid, Duration::from_secs(3));
        }
        with_cleanup(|c| c.office = None);
        eprintln!(
            "gander-render: office-timing work_ms={} quit_ms={}",
            worked.as_millis(),
            self.began.elapsed().saturating_sub(worked).as_millis()
        );
        // `scratch` is dropped after this, once nothing holds the files open.
        let _ = &self.scratch;
    }
}

/// Renders slide `page` of a `.pptx` to an image `width` pixels wide.
pub fn render_page(file: &File, ext: &str, page: u32, width: u32) -> Rendered<RgbaImage> {
    if App::from_ext(ext) != Some(App::PowerPoint) {
        return Err(Failure::unsupported(
            "page rendering through Office is for presentations; documents export to PDF",
        ));
    }
    let mut s = Session::start(file, ext)?;
    s.harden();
    let png_path = s.scratch.path("slide.png");
    let err = |e: DispError| fail(App::PowerPoint, &e);

    let presentations = s.app().child("Presentations").map_err(err)?;
    let pres = presentations
        .call_named(
            "Open",
            &[],
            &[
                ("FileName", Var::bstr(&s.copy_str())),
                ("ReadOnly", Var::i4(MSO_TRUE)),
                ("Untitled", Var::i4(MSO_FALSE)),
                ("WithWindow", Var::i4(MSO_FALSE)),
            ],
        )
        .map_err(err)?
        .into_dispatch()
        .ok_or_else(|| Failure::decode("PowerPoint returned no presentation"))?;
    s.doc = Some(pres.clone());

    let slides = pres.child("Slides").map_err(err)?;
    let count = slides.get("Count", &[]).map_err(err)?.to_i32().unwrap_or(0);
    if count < 1 {
        return Err(Failure::decode("presentation has no slides"));
    }
    if i64::from(page) > i64::from(count) {
        return Err(Failure::decode(PAGE_OUT_OF_RANGE_MSG));
    }
    let setup = pres.child("PageSetup").map_err(err)?;
    let sw = setup
        .get("SlideWidth", &[])
        .map_err(err)?
        .to_f64()
        .unwrap_or(0.0);
    let sh = setup
        .get("SlideHeight", &[])
        .map_err(err)?
        .to_f64()
        .unwrap_or(0.0);
    if sw < 1.0 || sh < 1.0 {
        return Err(Failure::decode("presentation has no slide size"));
    }
    let (w, h) = slide_pixels(sw, sh, width);

    let slide = slides
        .call("Item", &[Var::i4(i32::try_from(page).unwrap_or(1))])
        .map_err(err)?
        .into_dispatch()
        .ok_or_else(|| Failure::decode("PowerPoint returned no slide"))?;
    slide
        .call(
            "Export",
            &[
                Var::bstr(&png_path.to_string_lossy()),
                Var::bstr("PNG"),
                Var::i4(i32::try_from(w).unwrap_or(1080)),
                Var::i4(i32::try_from(h).unwrap_or(608)),
            ],
        )
        .map_err(err)?;
    drop(slide);
    drop(slides);
    drop(setup);
    drop(pres);
    drop(presentations);

    let png = std::fs::read(&png_path).map_err(Failure::decode)?;
    drop(s);
    // The export is already the requested size; the box only has to be big enough not to scale.
    wic::decode_bytes_target(&png, Target::Box(16_384), Some(1))
}

/// Pixel size for a slide of `sw` x `sh` points exported `width` pixels wide.
fn slide_pixels(sw: f64, sh: f64, width: u32) -> (u32, u32) {
    let height = (f64::from(width) * sh / sw).round().max(1.0);
    let height = (height as u32).min(MAX_SLIDE_HEIGHT);
    (width, height)
}

/// Exports a `.docx` or `.xlsx` to PDF and returns the PDF bytes.
pub fn export_pdf(file: &File, ext: &str) -> Rendered<Vec<u8>> {
    let kind = App::from_ext(ext)
        .filter(|a| *a != App::PowerPoint)
        .ok_or_else(|| Failure::unsupported("PDF export is for Word and Excel documents"))?;
    let mut s = Session::start(file, ext)?;
    s.harden();
    let pdf_path = s.scratch.path("out.pdf");
    let pdf_str = pdf_path.to_string_lossy().into_owned();
    let err = |e: DispError| fail(kind, &e);

    match kind {
        App::Word => {
            let docs = s.app().child("Documents").map_err(err)?;
            let doc = docs
                .call_named(
                    "Open",
                    &[],
                    &[
                        ("FileName", Var::bstr(&s.copy_str())),
                        ("ConfirmConversions", Var::boolean(false)),
                        ("ReadOnly", Var::boolean(true)),
                        ("AddToRecentFiles", Var::boolean(false)),
                        ("Visible", Var::boolean(false)),
                        ("OpenAndRepair", Var::boolean(false)),
                        ("NoEncodingDialog", Var::boolean(true)),
                    ],
                )
                .map_err(err)?
                .into_dispatch()
                .ok_or_else(|| Failure::decode("Word returned no document"))?;
            s.doc = Some(doc.clone());
            doc.call_named(
                "ExportAsFixedFormat",
                &[],
                &[
                    ("OutputFileName", Var::bstr(&pdf_str)),
                    ("ExportFormat", Var::i4(WD_EXPORT_FORMAT_PDF)),
                    ("OpenAfterExport", Var::boolean(false)),
                ],
            )
            .map_err(err)?;
        }
        App::Excel => {
            let books = s.app().child("Workbooks").map_err(err)?;
            let book = books
                .call_named(
                    "Open",
                    &[],
                    &[
                        ("Filename", Var::bstr(&s.copy_str())),
                        ("UpdateLinks", Var::i4(0)),
                        ("ReadOnly", Var::boolean(true)),
                        ("AddToMru", Var::boolean(false)),
                    ],
                )
                .map_err(err)?
                .into_dispatch()
                .ok_or_else(|| Failure::decode("Excel returned no workbook"))?;
            s.doc = Some(book.clone());
            book.call_named(
                "ExportAsFixedFormat",
                &[],
                &[
                    ("Type", Var::i4(XL_TYPE_PDF)),
                    ("Filename", Var::bstr(&pdf_str)),
                    ("OpenAfterPublish", Var::boolean(false)),
                ],
            )
            .map_err(err)?;
        }
        App::PowerPoint => unreachable!("filtered above"),
    }

    let len = std::fs::metadata(&pdf_path)
        .map_err(|_| Failure::decode("the export produced no PDF"))?
        .len();
    if len > MAX_PDF_BYTES {
        return Err(Failure::decode("the exported PDF is too large"));
    }
    let mut bytes = Vec::with_capacity(len as usize);
    File::open(&pdf_path)
        .and_then(|mut f| f.read_to_end(&mut bytes))
        .map_err(Failure::decode)?;
    drop(s);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_ooxml_documents_are_automated() {
        assert_eq!(App::from_ext("pptx"), Some(App::PowerPoint));
        assert_eq!(App::from_ext("docx"), Some(App::Word));
        assert_eq!(App::from_ext("xlsx"), Some(App::Excel));
        // Macro-enabled, legacy and ODF formats never reach Office.
        for ext in [
            "pptm", "docm", "xlsm", "ppt", "doc", "xls", "odp", "vsdx", "",
        ] {
            assert_eq!(App::from_ext(ext), None, "{ext}");
        }
    }

    #[test]
    fn slide_pixels_keep_the_aspect_ratio_and_cap_the_height() {
        assert_eq!(slide_pixels(960.0, 540.0, 1080), (1080, 608));
        assert_eq!(slide_pixels(720.0, 540.0, 800), (800, 600));
        assert_eq!(slide_pixels(100.0, 100_000.0, 1000).1, MAX_SLIDE_HEIGHT);
    }
}
