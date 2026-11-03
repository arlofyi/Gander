// SPDX-License-Identifier: Apache-2.0
//! Launching one `gander-render.exe` job under a job object (ADR-0007, renderer spec "Worker
//! protocol").
//!
//! Sequence for every job:
//!
//! 1. A job object is created with: process memory cap (512 MiB), per-process CPU time cap (20 s),
//!    one active process (no children), below-normal priority class, kill on job close, die on
//!    unhandled exception, and UI restrictions (no clipboard, no global atoms, no desktop or
//!    display changes, no `ExitWindows`).
//! 2. Three anonymous pipes are created. Only the child's ends are inheritable, and the child is
//!    given exactly those three handles through `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`, so it cannot
//!    inherit anything else the host holds.
//! 3. The worker is created **suspended**, below normal priority, without a window, by default
//!    with a copy of the host's token lowered to **low integrity** (`S-1-16-4096`).
//! 4. It is assigned to the job object while still suspended.
//! 5. The file handle is duplicated straight into the worker with read-only access
//!    ([`WORKER_FILE_ACCESS`]: read data, read attributes, synchronize; exactly what the path
//!    resolver opens files with, since a duplicate can never gain rights the source handle lacks);
//!    the host never makes the file handle inheritable. The worker receives
//!    the handle value in the job description on stdin, never a path.
//! 6. The thread is resumed; stdout is drained into a capped buffer, stderr into a short capped
//!    diagnostic string. The host polls the worker every 25 ms: past the wall-clock timeout or
//!    the CPU time cap it terminates the job (exit code 3).
//!
//! Exit codes are interpreted by [`WorkerOutcome::from_exit`]: the job object kills a worker that
//! exceeds its CPU time with an arbitrary status, so that case is recognised from the process's
//! user time and reported as a timeout.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    DuplicateHandle, FILETIME, HANDLE, HANDLE_FLAG_INHERIT, LocalFree, SetHandleInformation,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows_sys::Win32::Security::{
    DuplicateTokenEx, GetLengthSid, PSID, SECURITY_ATTRIBUTES, SID_AND_ATTRIBUTES,
    SecurityAnonymous, SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY,
    TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel, TokenPrimary,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_READ_ATTRIBUTES, FILE_READ_DATA, SYNCHRONIZE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_PRIORITY_CLASS, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    JOB_OBJECT_LIMIT_PROCESS_TIME, JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS,
    JOB_OBJECT_UILIMIT_EXITWINDOWS, JOB_OBJECT_UILIMIT_GLOBALATOMS,
    JOB_OBJECT_UILIMIT_READCLIPBOARD, JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
    JOB_OBJECT_UILIMIT_WRITECLIPBOARD, JOBOBJECT_BASIC_UI_RESTRICTIONS,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicUIRestrictions,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemServices::SE_GROUP_INTEGRITY;
use windows_sys::Win32::System::Threading::{
    BELOW_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessAsUserW,
    CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, GetProcessTimes, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcessToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

use super::job::{
    EXIT_BAD_JOB, EXIT_DECODER_ERROR, EXIT_OK, EXIT_TIMEOUT, EXIT_UNSUPPORTED, RenderJob,
};

/// File name of the worker executable, expected next to the host executable.
pub const WORKER_EXE_NAME: &str = "gander-render.exe";

/// How often the host checks a running worker's CPU time.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Low mandatory integrity level.
const LOW_INTEGRITY_SID: &str = "S-1-16-4096";

/// Resource caps for one worker process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerLimits {
    /// Committed memory cap of the worker process (job object). Spec: 512 MiB.
    pub memory_bytes: usize,
    /// User-mode CPU time cap (job object). Spec: 20 s.
    pub cpu_time: Duration,
    /// Wall-clock cap enforced by the host; the job is terminated with exit code 3 after it.
    pub wall_timeout: Duration,
    /// Largest stdout the host accepts; more is a failure ([`WorkerOutcome::OutputTooLarge`]).
    pub max_output_bytes: usize,
    /// Run the worker with a low-integrity copy of the host token (spec default). Office COM
    /// jobs (WP-16) need medium integrity and set this to `false`; they stay in the job object.
    pub low_integrity: bool,
}

impl Default for WorkerLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 512 * 1024 * 1024,
            cpu_time: Duration::from_secs(20),
            wall_timeout: Duration::from_secs(30),
            max_output_bytes: 32 * 1024 * 1024,
            low_integrity: true,
        }
    }
}

/// Where the worker lives and how it is capped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerConfig {
    /// Absolute path of `gander-render.exe`.
    pub exe: PathBuf,
    /// Caps applied to every job.
    pub limits: WorkerLimits,
}

impl WorkerConfig {
    /// `gander-render.exe` next to the running executable, default limits.
    pub fn beside_current_exe() -> io::Result<Self> {
        let exe = std::env::current_exe()?;
        let dir = exe
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "executable has no parent"))?;
        Ok(Self::new(dir.join(WORKER_EXE_NAME)))
    }

    /// A config for the given worker executable with default limits.
    pub fn new(exe: impl Into<PathBuf>) -> Self {
        Self {
            exe: exe.into(),
            limits: WorkerLimits::default(),
        }
    }
}

/// How a worker run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerOutcome {
    /// Exit 0; the bytes are the encoded image.
    Ok(Vec<u8>),
    /// Exit 2: no renderer, no embedded thumbnail, icon only, encrypted document.
    Unsupported,
    /// Exit 3, the host's wall-clock kill, or the job object's CPU time cap.
    TimedOut,
    /// Exit 4: the decoder rejected the file.
    DecoderError,
    /// Exit 1: the worker refused the job description (a host bug).
    BadJob,
    /// Any other exit status: crash, abort, out of memory under the job cap.
    Crashed(u32),
    /// Exit 0 but more output than [`WorkerLimits::max_output_bytes`].
    OutputTooLarge,
}

impl WorkerOutcome {
    /// Maps an exit status (plus whether the host killed it, and the CPU time it used) to an
    /// outcome. `stdout` is only kept for a successful exit.
    pub fn from_exit(
        exit_code: u32,
        killed_by_host: bool,
        cpu_cap_hit: bool,
        stdout: Vec<u8>,
        overflow: bool,
    ) -> Self {
        if killed_by_host {
            return WorkerOutcome::TimedOut;
        }
        match exit_code {
            EXIT_OK if overflow => WorkerOutcome::OutputTooLarge,
            EXIT_OK => WorkerOutcome::Ok(stdout),
            EXIT_UNSUPPORTED => WorkerOutcome::Unsupported,
            EXIT_TIMEOUT => WorkerOutcome::TimedOut,
            EXIT_DECODER_ERROR => WorkerOutcome::DecoderError,
            EXIT_BAD_JOB => WorkerOutcome::BadJob,
            _ if cpu_cap_hit => WorkerOutcome::TimedOut,
            other => WorkerOutcome::Crashed(other),
        }
    }
}

/// Everything known about one finished worker run.
#[derive(Debug, Clone)]
pub struct WorkerReport {
    /// The interpreted result.
    pub outcome: WorkerOutcome,
    /// The raw process exit status.
    pub exit_code: u32,
    /// Peak committed memory of the worker as recorded by the job object.
    pub peak_memory_bytes: usize,
    /// User-mode CPU time the worker consumed.
    pub user_time: Duration,
    /// Wall-clock time from spawn to exit.
    pub elapsed: Duration,
    /// First few KiB of the worker's stderr (diagnostics only; never file contents).
    pub stderr: String,
}

/// Runs one job in a fresh worker and waits for it.
///
/// `file` must be open with at least read access; it is duplicated into the worker read-only and
/// the caller's handle is not modified. `job.handle` is overwritten with the worker-side value.
pub fn run_job(config: &WorkerConfig, file: &File, job: &RenderJob) -> io::Result<WorkerReport> {
    let limits = config.limits;
    let started = Instant::now();

    let jobobj = JobObject::new(&limits)?;
    let stdin = Pipe::new(PipeDir::ChildReads)?;
    let stdout = Pipe::new(PipeDir::ChildWrites)?;
    let stderr = Pipe::new(PipeDir::ChildWrites)?;

    let child = spawn_suspended(
        &config.exe,
        [
            stdin.child.as_raw_handle(),
            stdout.child.as_raw_handle(),
            stderr.child.as_raw_handle(),
        ],
        limits.low_integrity,
    )?;
    // From here on any early return drops `jobobj`, which kills the suspended child
    // (KILL_ON_JOB_CLOSE) once it is assigned; until then terminate it explicitly.
    if let Err(e) = jobobj.assign(&child.process) {
        child.terminate();
        return Err(e);
    }
    // The child holds its own copies now.
    drop(stdin.child);
    drop(stdout.child);
    drop(stderr.child);

    let worker_handle = duplicate_into(&child.process, file)?;
    let mut job = job.clone();
    job.handle = worker_handle as u64;
    let body = serde_json::to_vec(&job).map_err(io::Error::other)?;

    let out_cap = limits.max_output_bytes;
    let out_reader = thread::spawn(move || drain(File::from(stdout.host), out_cap));
    let err_reader = thread::spawn(move || drain(File::from(stderr.host), 8 * 1024));

    child.resume()?;
    {
        let mut stdin_host = File::from(stdin.host);
        // A worker that dies before reading its stdin makes this fail; the exit code says why.
        let _ = stdin_host.write_all(&body);
    }

    // Wait in short slices: the wall-clock cap and the CPU cap are both enforced here. The job
    // object's per-process user-time limit stays set as a backstop, but measured on Windows 11
    // 26200 it did not stop a worker that ran 10x over it, so the host polls the process's
    // user time itself.
    let deadline = started + limits.wall_timeout;
    let mut killed_by_host = false;
    let mut cpu_killed = false;
    loop {
        let now = Instant::now();
        if now >= deadline {
            killed_by_host = true;
            break;
        }
        let slice = (deadline - now).min(POLL_INTERVAL);
        let slice_ms = u32::try_from(slice.as_millis()).unwrap_or(50).max(1);
        // SAFETY: `child.process` is a valid process handle owned by `child`.
        match unsafe { WaitForSingleObject(raw(&child.process), slice_ms) } {
            WAIT_OBJECT_0 => break,
            WAIT_TIMEOUT => {
                if process_user_time(&child.process) >= limits.cpu_time {
                    cpu_killed = true;
                    break;
                }
            }
            _ => {
                jobobj.terminate(EXIT_TIMEOUT);
                return Err(io::Error::last_os_error());
            }
        }
    }
    if killed_by_host || cpu_killed {
        jobobj.terminate(EXIT_TIMEOUT);
        // SAFETY: as above; the job was terminated so this returns promptly.
        unsafe { WaitForSingleObject(raw(&child.process), 5_000) };
    }

    let mut exit_code = 0u32;
    // SAFETY: valid process handle, valid out-pointer.
    if unsafe { GetExitCodeProcess(raw(&child.process), &mut exit_code) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let user_time = process_user_time(&child.process);
    let peak_memory_bytes = jobobj.peak_process_memory();
    // Pipes close when the process is gone, so the readers finish.
    let (stdout_bytes, overflow) = out_reader
        .join()
        .map_err(|_| io::Error::other("stdout reader panicked"))?;
    let (stderr_bytes, _) = err_reader
        .join()
        .map_err(|_| io::Error::other("stderr reader panicked"))?;

    let cpu_cap_hit = cpu_killed || user_time + Duration::from_millis(50) >= limits.cpu_time;
    let outcome = WorkerOutcome::from_exit(
        exit_code,
        killed_by_host || cpu_killed,
        cpu_cap_hit,
        stdout_bytes,
        overflow,
    );
    Ok(WorkerReport {
        outcome,
        exit_code,
        peak_memory_bytes,
        user_time,
        elapsed: started.elapsed(),
        stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
    })
}

/// Reads `src` to EOF, keeping at most `cap` bytes; returns the bytes and whether more arrived.
/// Keeps draining past the cap so the worker never blocks on a full pipe.
fn drain(mut src: File, cap: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut overflow = false;
    let mut buf = [0u8; 64 * 1024];
    loop {
        match src.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = cap.saturating_sub(kept.len());
                if n > room {
                    overflow = true;
                }
                kept.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
    (kept, overflow)
}

fn raw(h: &OwnedHandle) -> HANDLE {
    h.as_raw_handle() as HANDLE
}

/// Takes ownership of a raw handle returned by a successful Win32 call.
fn own(h: HANDLE) -> OwnedHandle {
    // SAFETY: callers pass a handle they just received from a successful Win32 call and do not
    // close elsewhere.
    unsafe { OwnedHandle::from_raw_handle(h as RawHandle) }
}

struct JobObject(OwnedHandle);

impl JobObject {
    fn new(limits: &WorkerLimits) -> io::Result<Self> {
        // SAFETY: null attributes and name are allowed.
        let h = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if h.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = JobObject(own(h));

        // SAFETY: all-zero is a valid JOBOBJECT_EXTENDED_LIMIT_INFORMATION.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        let basic = &mut info.BasicLimitInformation;
        basic.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_PROCESS_TIME
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_PRIORITY_CLASS
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        // 100 ns units.
        basic.PerProcessUserTimeLimit =
            i64::try_from(limits.cpu_time.as_nanos() / 100).unwrap_or(i64::MAX);
        basic.ActiveProcessLimit = 1;
        basic.PriorityClass = BELOW_NORMAL_PRIORITY_CLASS;
        info.ProcessMemoryLimit = limits.memory_bytes;
        // SAFETY: `info` is a properly sized, initialised struct of the class's type.
        let ok = unsafe {
            SetInformationJobObject(
                raw(&job.0),
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }

        let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                | JOB_OBJECT_UILIMIT_EXITWINDOWS
                | JOB_OBJECT_UILIMIT_GLOBALATOMS
                | JOB_OBJECT_UILIMIT_READCLIPBOARD
                | JOB_OBJECT_UILIMIT_WRITECLIPBOARD
                | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
        };
        // SAFETY: as above, for the UI restrictions class.
        let ok = unsafe {
            SetInformationJobObject(
                raw(&job.0),
                JobObjectBasicUIRestrictions,
                (&raw const ui).cast(),
                size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    fn assign(&self, process: &OwnedHandle) -> io::Result<()> {
        // SAFETY: both handles are valid and owned.
        if unsafe { AssignProcessToJobObject(raw(&self.0), raw(process)) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn terminate(&self, code: u32) {
        // SAFETY: valid job handle.
        unsafe { TerminateJobObject(raw(&self.0), code) };
    }

    fn peak_process_memory(&self) -> usize {
        // SAFETY: zeroed struct is valid; the out-pointer and size match the class.
        unsafe {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            let ok = QueryInformationJobObject(
                raw(&self.0),
                JobObjectExtendedLimitInformation,
                (&raw mut info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ptr::null_mut(),
            );
            if ok == 0 {
                0
            } else {
                info.PeakProcessMemoryUsed
            }
        }
    }
}

enum PipeDir {
    ChildReads,
    ChildWrites,
}

/// An anonymous pipe whose child end is inheritable and whose host end is not.
struct Pipe {
    host: OwnedHandle,
    child: OwnedHandle,
}

impl Pipe {
    fn new(dir: PipeDir) -> io::Result<Self> {
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: ptr::null_mut(),
            bInheritHandle: 1,
        };
        let mut read: HANDLE = ptr::null_mut();
        let mut write: HANDLE = ptr::null_mut();
        // SAFETY: valid out-pointers and attributes.
        if unsafe { CreatePipe(&mut read, &mut write, &sa, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let (read, write) = (own(read), own(write));
        let (host, child) = match dir {
            PipeDir::ChildReads => (write, read),
            PipeDir::ChildWrites => (read, write),
        };
        // SAFETY: valid handle owned by `host`.
        if unsafe { SetHandleInformation(raw(&host), HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { host, child })
    }
}

struct Child {
    process: OwnedHandle,
    thread: OwnedHandle,
}

impl Child {
    fn resume(&self) -> io::Result<()> {
        // SAFETY: valid thread handle.
        if unsafe { ResumeThread(raw(&self.thread)) } == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn terminate(&self) {
        // SAFETY: valid process handle.
        unsafe {
            windows_sys::Win32::System::Threading::TerminateProcess(
                raw(&self.process),
                EXIT_TIMEOUT,
            )
        };
    }
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Owns a `PROC_THREAD_ATTRIBUTE_LIST` buffer.
struct AttrList {
    buf: Vec<u64>,
}

impl AttrList {
    fn with_handle_list(handles: &[HANDLE]) -> io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: size query with a null list is the documented first call; it "fails" with
        // ERROR_INSUFFICIENT_BUFFER and sets `size`.
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size) };
        let mut list = AttrList {
            buf: vec![0u64; size.div_ceil(8)],
        };
        // SAFETY: `buf` is at least `size` bytes and 8-aligned.
        if unsafe { InitializeProcThreadAttributeList(list.ptr(), 1, 0, &mut size) } == 0 {
            list.buf.clear();
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `handles` outlives the list's use in CreateProcess (the caller keeps it alive
        // until after spawning); size is in bytes.
        let ok = unsafe {
            UpdateProcThreadAttribute(
                list.ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                std::mem::size_of_val(handles),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(list)
    }

    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buf.as_mut_ptr().cast()
    }
}

impl Drop for AttrList {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            // SAFETY: the list was initialised.
            unsafe { DeleteProcThreadAttributeList(self.ptr()) };
        }
    }
}

fn spawn_suspended(exe: &Path, stdio: [RawHandle; 3], low_integrity: bool) -> io::Result<Child> {
    let handles: [HANDLE; 3] = stdio.map(|h| h as HANDLE);
    let mut attrs = AttrList::with_handle_list(&handles)?;

    // SAFETY: all-zero STARTUPINFOEXW is valid before the fields below are set.
    let mut si: STARTUPINFOEXW = unsafe { zeroed() };
    si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = handles[0];
    si.StartupInfo.hStdOutput = handles[1];
    si.StartupInfo.hStdError = handles[2];
    si.lpAttributeList = attrs.ptr();

    let app = wide(exe.as_os_str());
    let mut cmdline: Vec<u16> = {
        let mut s = std::ffi::OsString::from("\"");
        s.push(exe.as_os_str());
        s.push("\"");
        wide(&s)
    };
    let cwd = exe.parent().map(|p| wide(p.as_os_str()));
    let cwd_ptr = cwd.as_ref().map_or(ptr::null(), |c| c.as_ptr());
    let flags = CREATE_SUSPENDED
        | CREATE_NO_WINDOW
        | BELOW_NORMAL_PRIORITY_CLASS
        | EXTENDED_STARTUPINFO_PRESENT;
    // SAFETY: zeroed PROCESS_INFORMATION is a valid out-parameter.
    let mut pi: PROCESS_INFORMATION = unsafe { zeroed() };

    let ok = if low_integrity {
        let token = low_integrity_token()?;
        // SAFETY: every pointer is valid for the call; `si` is a STARTUPINFOEXW as announced by
        // EXTENDED_STARTUPINFO_PRESENT; `handles` and `attrs` are alive.
        unsafe {
            CreateProcessAsUserW(
                raw(&token),
                app.as_ptr(),
                cmdline.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                1,
                flags,
                ptr::null(),
                cwd_ptr,
                (&raw const si).cast::<STARTUPINFOW>(),
                &mut pi,
            )
        }
    } else {
        // SAFETY: as above.
        unsafe {
            CreateProcessW(
                app.as_ptr(),
                cmdline.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                1,
                flags,
                ptr::null(),
                cwd_ptr,
                (&raw const si).cast::<STARTUPINFOW>(),
                &mut pi,
            )
        }
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    drop(attrs);
    Ok(Child {
        process: own(pi.hProcess),
        thread: own(pi.hThread),
    })
}

/// A primary token copied from the host's and lowered to low integrity.
fn low_integrity_token() -> io::Result<OwnedHandle> {
    let mut current: HANDLE = ptr::null_mut();
    // SAFETY: pseudo handle and valid out-pointer.
    let ok = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY,
            &mut current,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let current = own(current);
    let mut dup: HANDLE = ptr::null_mut();
    // SAFETY: valid token handle and out-pointer.
    let ok = unsafe {
        DuplicateTokenEx(
            raw(&current),
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY,
            ptr::null(),
            SecurityAnonymous,
            TokenPrimary,
            &mut dup,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let dup = own(dup);

    let sid_str = wide(OsStr::new(LOW_INTEGRITY_SID));
    let mut sid: PSID = ptr::null_mut();
    // SAFETY: NUL-terminated string, valid out-pointer; `sid` is LocalFree'd below.
    if unsafe { ConvertStringSidToSidW(sid_str.as_ptr(), &mut sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: sid,
            Attributes: SE_GROUP_INTEGRITY as u32,
        },
    };
    // SAFETY: `label` and the SID it points to are valid for the call; the length covers both.
    let ok = unsafe {
        let len = size_of::<TOKEN_MANDATORY_LABEL>() as u32 + GetLengthSid(sid);
        SetTokenInformation(
            raw(&dup),
            TokenIntegrityLevel,
            (&raw const label).cast(),
            len,
        )
    };
    let err = io::Error::last_os_error();
    // SAFETY: `sid` came from ConvertStringSidToSidW.
    unsafe { LocalFree(sid) };
    if ok == 0 {
        return Err(err);
    }
    Ok(dup)
}

/// Duplicates `file` into the (suspended) worker with read-only access; returns the handle value
/// valid inside the worker.
/// Access the worker's copy of the file handle gets. It must be a subset of the access the path
/// resolver opens files with (`gander_win::path`), or `DuplicateHandle` fails with
/// `ERROR_ACCESS_DENIED` (found by the WP-1B e2e suite: `FILE_GENERIC_READ` also asks for
/// `READ_CONTROL` and `FILE_READ_EA`, which resolved handles do not carry).
pub const WORKER_FILE_ACCESS: u32 = FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

fn duplicate_into(process: &OwnedHandle, file: &File) -> io::Result<usize> {
    let mut target: HANDLE = ptr::null_mut();
    // SAFETY: source handle belongs to this process and is valid for the call; the target
    // process handle has PROCESS_DUP_HANDLE (we created it).
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            file.as_raw_handle() as HANDLE,
            raw(process),
            &mut target,
            WORKER_FILE_ACCESS,
            0,
            0,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(target as usize)
}

fn process_user_time(process: &OwnedHandle) -> Duration {
    let mut c = FILETIME::default();
    let mut e = FILETIME::default();
    let mut k = FILETIME::default();
    let mut u = FILETIME::default();
    // SAFETY: valid handle and out-pointers.
    if unsafe { GetProcessTimes(raw(process), &mut c, &mut e, &mut k, &mut u) } == 0 {
        return Duration::ZERO;
    }
    let ticks = (u64::from(u.dwHighDateTime) << 32) | u64::from(u.dwLowDateTime);
    Duration::from_nanos(ticks.saturating_mul(100))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resolver_handle_can_be_duplicated_for_the_worker() {
        use std::io::Read as _;
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcessId, OpenProcess, PROCESS_DUP_HANDLE,
        };

        let path = std::env::temp_dir().join(format!("gander-dup-{}.txt", std::process::id()));
        std::fs::write(&path, b"resolver handle").unwrap();
        // Exactly the resolver's access mask (path/nt.rs `ACCESS`).
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
            .open(&path)
            .unwrap();
        // SAFETY: plain call; the result is checked and owned below.
        let me = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, GetCurrentProcessId()) };
        assert!(!me.is_null());
        let me = own(me);
        let dup = duplicate_into(&me, &file).expect("duplicate a resolver handle");
        // SAFETY: `dup` is a fresh handle in this process that nothing else owns.
        let mut copy = unsafe { File::from_raw_handle(dup as RawHandle) };
        let mut text = String::new();
        copy.read_to_string(&mut text).unwrap();
        assert_eq!(text, "resolver handle");
        drop(copy);
        drop(file);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn exit_codes_map_to_outcomes() {
        let ok = WorkerOutcome::from_exit(0, false, false, vec![1, 2], false);
        assert_eq!(ok, WorkerOutcome::Ok(vec![1, 2]));
        assert_eq!(
            WorkerOutcome::from_exit(0, false, false, vec![], true),
            WorkerOutcome::OutputTooLarge
        );
        assert_eq!(
            WorkerOutcome::from_exit(2, false, false, vec![], false),
            WorkerOutcome::Unsupported
        );
        assert_eq!(
            WorkerOutcome::from_exit(3, false, false, vec![], false),
            WorkerOutcome::TimedOut
        );
        assert_eq!(
            WorkerOutcome::from_exit(4, false, false, vec![], false),
            WorkerOutcome::DecoderError
        );
        assert_eq!(
            WorkerOutcome::from_exit(0xC000_0409, false, true, vec![], false),
            WorkerOutcome::TimedOut
        );
        assert_eq!(
            WorkerOutcome::from_exit(0xC000_0409, false, false, vec![], false),
            WorkerOutcome::Crashed(0xC000_0409)
        );
        assert_eq!(
            WorkerOutcome::from_exit(0, true, false, vec![9], false),
            WorkerOutcome::TimedOut
        );
    }
}
