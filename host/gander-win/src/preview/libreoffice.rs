// SPDX-License-Identifier: Apache-2.0
//! LibreOffice as the Office renderer when Microsoft Office is absent: `soffice --headless
//! --convert-to pdf`, then the PDF pages go through the normal PDF renderer.
//!
//! The conversion runs here, in the host, because a render worker may not start child
//! processes. It is contained the same way as far as it can be: the document is a private copy
//! in a scratch folder, LibreOffice gets a throw-away user profile (nothing is read from or
//! written to the owner's profile), it runs at below-normal priority without a window inside a
//! job object that kills the whole process tree when the conversion ends or times out, with a
//! memory cap on the job.
//!
//! **Not exercised against a real LibreOffice on the development machine** (none is installed,
//! and none may be); the tests drive this code with a stand-in `soffice` script and cover the
//! command line, the success path, failure, timeout and cleanup.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};
use std::mem::{size_of, zeroed};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{BELOW_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW};

use super::runner::JobResult;

/// Largest document copied for conversion.
const MAX_DOC_BYTES: u64 = 256 * 1024 * 1024;
/// Largest PDF accepted back.
const MAX_PDF_BYTES: u64 = 64 * 1024 * 1024;
/// Memory cap on the whole LibreOffice process tree.
const JOB_MEMORY_BYTES: usize = 2 * 1024 * 1024 * 1024;
/// How long a kill waits for the job's processes to be gone.
const JOB_EXIT_WAIT: Duration = Duration::from_secs(5);

/// A job object that kills everything in it when closed.
struct KillOnClose(HANDLE);

impl KillOnClose {
    fn new() -> io::Result<Self> {
        // SAFETY: null attributes and name are allowed.
        let h = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if h.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = KillOnClose(h);
        // SAFETY: all-zero is a valid JOBOBJECT_EXTENDED_LIMIT_INFORMATION; the call gets a
        // correctly sized struct of the class's type.
        unsafe {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            info.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_JOB_MEMORY;
            info.JobMemoryLimit = JOB_MEMORY_BYTES;
            if SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(job)
    }

    fn assign(&self, child: &std::process::Child) -> io::Result<()> {
        // SAFETY: both handles are valid while `child` is alive.
        if unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Terminates every process in the job and waits (bounded) until they have exited.
    /// `TerminateJobObject` only starts the termination: a process that is still dying keeps
    /// its working directory and open files in the scratch folder, and removing the folder
    /// right after would fail (measured 2026-11-02 under parallel test load: the folder leaked).
    fn kill(&self) {
        // SAFETY: valid job handle.
        unsafe { TerminateJobObject(self.0, 1) };
        let deadline = Instant::now() + JOB_EXIT_WAIT;
        while self.active_processes() > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Processes still running in the job (0 when it cannot be queried).
    fn active_processes(&self) -> u32 {
        // SAFETY: all-zero is a valid accounting struct; the call gets its exact size.
        unsafe {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = zeroed();
            if QueryInformationJobObject(
                self.0,
                JobObjectBasicAccountingInformation,
                (&raw mut info).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                ptr::null_mut(),
            ) == 0
            {
                return 0;
            }
            info.ActiveProcesses
        }
    }
}

impl Drop for KillOnClose {
    fn drop(&mut self) {
        // Every exit path (early return, `?`) ends here: kill and wait so the scratch folder,
        // dropped after this, is no longer in use.
        self.kill();
        // SAFETY: the handle came from CreateJobObjectW and is closed once; closing it kills any
        // process still in the job.
        unsafe { CloseHandle(self.0) };
    }
}

/// A scratch folder deleted on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> io::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("gander-lo-{}-{nanos:x}", std::process::id()));
        std::fs::create_dir(&dir)?;
        Ok(Scratch(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A file can stay briefly in use after its process exited (antivirus or indexer
        // scanning the new PDF); retry for a moment rather than leak the folder.
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.0).is_ok() || !self.0.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// `file:///C:/dir/with%20space` for LibreOffice's `-env:UserInstallation`.
fn file_url(path: &Path) -> String {
    let forward = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from("file:///");
    for c in forward.chars() {
        match c {
            ' ' => url.push_str("%20"),
            '%' => url.push_str("%25"),
            '#' => url.push_str("%23"),
            other => url.push(other),
        }
    }
    url
}

/// The `soffice` command line (everything after the executable).
fn soffice_args(profile: &Path, outdir: &Path, input: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--headless",
        "--norestore",
        "--nologo",
        "--nodefault",
        "--nolockcheck",
        "--nofirststartwizard",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    args.push(format!("-env:UserInstallation={}", file_url(profile)).into());
    args.push("--convert-to".into());
    args.push("pdf".into());
    args.push("--outdir".into());
    args.push(outdir.into());
    args.push(input.into());
    args
}

/// Converts the document behind `file` (extension `ext`) to PDF with `soffice`.
pub fn convert_to_pdf(soffice: &Path, file: &File, ext: &str, timeout: Duration) -> JobResult {
    match convert(soffice, file, ext, timeout) {
        Ok(result) => result,
        Err(_) => JobResult::HostFailure,
    }
}

fn convert(soffice: &Path, file: &File, ext: &str, timeout: Duration) -> io::Result<JobResult> {
    use std::io::{Seek, SeekFrom};
    // The extension came from the resolved file name; keep only plain characters.
    if ext.is_empty() || !ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Ok(JobResult::Unsupported);
    }
    let scratch = Scratch::new()?;
    let (indir, outdir, profile) = (
        scratch.0.join("in"),
        scratch.0.join("out"),
        scratch.0.join("profile"),
    );
    for d in [&indir, &outdir, &profile] {
        std::fs::create_dir(d)?;
    }
    let input = indir.join(format!("doc.{ext}"));
    {
        let mut src = file;
        src.seek(SeekFrom::Start(0))?;
        let mut dst = File::create(&input)?;
        let copied = io::copy(&mut src.take(MAX_DOC_BYTES + 1), &mut dst)?;
        if copied > MAX_DOC_BYTES {
            return Ok(JobResult::Unsupported);
        }
    }

    let job = KillOnClose::new()?;
    let mut child = Command::new(soffice)
        .args(soffice_args(&profile, &outdir, &input))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS)
        .spawn()?;
    if let Err(e) = job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            job.kill();
            let _ = child.wait();
            return Ok(JobResult::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // soffice.exe may hand the work to soffice.bin and return early on some builds; the PDF is
    // the arbiter. Kill whatever is left in the job once we have what we need.
    let pdf = outdir.join("doc.pdf");
    let grace = Instant::now() + Duration::from_secs(5);
    while !pdf.is_file() && status.success() && Instant::now() < grace {
        std::thread::sleep(Duration::from_millis(50));
    }
    job.kill();
    let Ok(meta) = std::fs::metadata(&pdf) else {
        return Ok(JobResult::DecoderError);
    };
    if meta.len() == 0 || meta.len() > MAX_PDF_BYTES {
        return Ok(JobResult::DecoderError);
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    File::open(&pdf)?.read_to_end(&mut bytes)?;
    Ok(JobResult::Output(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn command_line_is_headless_with_a_private_profile() {
        let args = soffice_args(
            Path::new(r"C:\Temp\my scratch\profile"),
            Path::new(r"C:\Temp\my scratch\out"),
            Path::new(r"C:\Temp\my scratch\in\doc.docx"),
        );
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], "--headless");
        assert!(args.contains(&"--norestore".to_owned()));
        assert!(args.contains(&"--nolockcheck".to_owned()));
        assert!(
            args.contains(&"-env:UserInstallation=file:///C:/Temp/my%20scratch/profile".to_owned()),
            "{args:?}"
        );
        let n = args.len();
        assert_eq!(
            &args[n - 5..n - 1],
            ["--convert-to", "pdf", "--outdir", r"C:\Temp\my scratch\out"]
        );
        assert_eq!(args[n - 1], r"C:\Temp\my scratch\in\doc.docx");
    }

    #[test]
    fn file_urls_escape_what_must_be_escaped() {
        assert_eq!(
            file_url(Path::new(r"C:\a b\c#d%e")),
            "file:///C:/a%20b/c%23d%25e"
        );
    }

    /// A stand-in `soffice`: a batch file that finds `--outdir <dir>` and copies `fixture.pdf`
    /// (next to it) to `<dir>\doc.pdf`, like `--convert-to pdf` of `doc.<ext>` would.
    fn fake_soffice(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("soffice.bat");
        let mut f = File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    /// The stand-in script. `shift` also shifts %0, so the script folder is captured first.
    fn copying_script() -> String {
        [
            "@echo off",
            "set \"HERE=%~dp0\"",
            ":loop",
            "if \"%~1\"==\"\" exit /b 2",
            "if \"%~1\"==\"--outdir\" goto found",
            "shift",
            "goto loop",
            ":found",
            "copy /y \"%HERE%fixture.pdf\" \"%~2\\doc.pdf\" >nul",
            "exit /b 0",
            "",
        ]
        .join("\r\n")
    }

    /// The tests look for leftover scratch folders in the shared temp directory, so they must
    /// not overlap each other.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn work_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gander-lotest-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn all_scratch_dirs() -> Vec<String> {
        std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&format!("gander-lo-{}-", std::process::id())))
            .collect()
    }

    /// Scratch folders of this process that did not exist when the test binary started. The
    /// process id alone is not enough: a folder left by an earlier process whose id Windows has
    /// since recycled carries the same prefix (seen in looped runs).
    fn scratch_dirs() -> Vec<String> {
        static AT_START: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        let at_start = AT_START.get_or_init(all_scratch_dirs);
        all_scratch_dirs()
            .into_iter()
            .filter(|n| !at_start.contains(n))
            .collect()
    }

    #[test]
    fn conversion_returns_the_pdf_and_cleans_up() {
        let _serial = serial();
        assert!(scratch_dirs().is_empty());
        let dir = work_dir("ok");
        std::fs::write(dir.join("fixture.pdf"), b"%PDF-1.4 stand-in").unwrap();
        let soffice = fake_soffice(&dir, &copying_script());
        let doc = dir.join("in.docx");
        std::fs::write(&doc, b"not really a document").unwrap();
        let result = convert_to_pdf(
            &soffice,
            &File::open(&doc).unwrap(),
            "docx",
            Duration::from_secs(30),
        );
        assert_eq!(result, JobResult::Output(b"%PDF-1.4 stand-in".to_vec()));
        assert!(
            scratch_dirs().is_empty(),
            "scratch removed: {:?}",
            scratch_dirs()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failing_converter_is_a_decoder_error() {
        let _serial = serial();
        assert!(scratch_dirs().is_empty());
        let dir = work_dir("fail");
        let soffice = fake_soffice(&dir, "@echo off\r\nexit /b 1\r\n");
        let doc = dir.join("in.docx");
        std::fs::write(&doc, b"x").unwrap();
        let result = convert_to_pdf(
            &soffice,
            &File::open(&doc).unwrap(),
            "docx",
            Duration::from_secs(30),
        );
        assert_eq!(result, JobResult::DecoderError);
        // A missing executable is a host failure, not a verdict on the document.
        let result = convert_to_pdf(
            &dir.join("no-such-soffice.exe"),
            &File::open(&doc).unwrap(),
            "docx",
            Duration::from_secs(30),
        );
        assert_eq!(result, JobResult::HostFailure);
        // An odd extension is refused before anything is copied or run.
        let result = convert_to_pdf(
            &soffice,
            &File::open(&doc).unwrap(),
            "d/../x",
            Duration::from_secs(1),
        );
        assert_eq!(result, JobResult::Unsupported);
        assert!(scratch_dirs().is_empty(), "{:?}", scratch_dirs());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_hung_converter_is_killed_at_the_timeout() {
        let _serial = serial();
        assert!(scratch_dirs().is_empty());
        let dir = work_dir("hang");
        let soffice = fake_soffice(&dir, "@echo off\r\nping -n 60 127.0.0.1 >nul\r\n");
        let doc = dir.join("in.docx");
        std::fs::write(&doc, b"x").unwrap();
        let started = Instant::now();
        let result = convert_to_pdf(
            &soffice,
            &File::open(&doc).unwrap(),
            "docx",
            Duration::from_millis(700),
        );
        assert_eq!(result, JobResult::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert!(scratch_dirs().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
