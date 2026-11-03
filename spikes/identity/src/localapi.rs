// SPDX-License-Identifier: Apache-2.0
//! Transport to the local tailscaled: the Windows LocalAPI named pipe (HTTP over a pipe) and the
//! `tailscale` CLI. Both are local-only; nothing here talks to a peer.
//!
//! Every call runs on a worker thread with a timeout, because a synchronous pipe read has no
//! timeout of its own.

use crate::http;
use anyhow::Result;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const PIPE_PATH: &str = r"\\.\pipe\ProtectedPrefix\Administrators\Tailscale\tailscaled";
pub const LOCALAPI_HOST: &str = "local-tailscaled.sock";
const TIMEOUT: Duration = Duration::from_secs(10);

/// How many times opening the pipe had to be retried because every instance was busy.
pub static BUSY_RETRIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// How many pipe connections were opened in this run.
pub static PIPE_OPENS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Security quality-of-service requested when opening the pipe (the impersonation level the
/// client allows the server).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qos {
    /// No QoS flags, what `File::open` does; Windows then applies `SecurityImpersonation`.
    Default,
    /// `SECURITY_ANONYMOUS`: what go-winio (and so the `tailscale` CLI) requests.
    Anonymous,
    /// `SECURITY_IDENTIFICATION`: the server may read the client identity but not act as it.
    Identification,
    /// `SECURITY_IMPERSONATION`, requested explicitly.
    Impersonation,
}

impl Qos {
    pub const ALL: [Qos; 4] = [
        Qos::Default,
        Qos::Anonymous,
        Qos::Identification,
        Qos::Impersonation,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Qos::Default => "default_no_flags",
            Qos::Anonymous => "anonymous",
            Qos::Identification => "identification",
            Qos::Impersonation => "impersonation",
        }
    }
}

/// Outcome of one call, transport-neutral.
#[derive(Debug, Default, Clone)]
pub struct Call {
    /// HTTP status (pipe) when a response was received.
    pub http_status: Option<u16>,
    /// Process exit code (CLI) when the process ran.
    pub exit_code: Option<i32>,
    /// Body or stdout parsed as JSON.
    pub json: Option<Value>,
    pub body_len: usize,
    /// Head of a non-JSON body, stderr or error text (to be scrubbed before display).
    pub text_head: Option<String>,
    /// Transport error (could not open, timed out, malformed response).
    pub error: Option<String>,
    /// Win32 error code behind `error`, when there is one.
    pub os_error: Option<i32>,
    pub micros: u128,
}

impl Call {
    pub fn ok(&self) -> bool {
        self.json.is_some() && (self.http_status == Some(200) || self.exit_code == Some(0))
    }
}

/// `GET target` over the LocalAPI pipe.
pub fn pipe_get(
    pipe: &str,
    target: &str,
    host: Option<&str>,
    extra: &[(&str, &str)],
    qos: Qos,
) -> Call {
    let started = Instant::now();
    let request = http::build_get(target, host, extra);
    let pipe = pipe.to_string();
    let result = with_timeout(move || roundtrip(&pipe, &request, qos));
    let mut call = Call::default();
    match result {
        Ok(Ok(resp)) => {
            call.http_status = Some(resp.status);
            call.body_len = resp.body.len();
            match serde_json::from_slice::<Value>(&resp.body) {
                Ok(v) => call.json = Some(v),
                Err(_) => call.text_head = Some(head_text(&resp.body)),
            }
        }
        Ok(Err(e)) => {
            call.os_error = e
                .downcast_ref::<std::io::Error>()
                .and_then(|io| io.raw_os_error());
            call.error = Some(format!("{e:#}"));
        }
        Err(msg) => call.error = Some(msg),
    }
    call.micros = started.elapsed().as_micros();
    call
}

/// Run the `tailscale` CLI and parse stdout as JSON if possible.
pub fn cli_run(exe: &Path, args: &[&str]) -> Call {
    let started = Instant::now();
    let exe = exe.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let result = with_timeout(move || {
        Command::new(&exe)
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .map_err(anyhow::Error::from)
    });
    let mut call = Call::default();
    match result {
        Ok(Ok(out)) => {
            call.exit_code = out.status.code();
            call.body_len = out.stdout.len();
            match serde_json::from_slice::<Value>(&out.stdout) {
                Ok(v) => call.json = Some(v),
                Err(_) => {
                    let text = if out.stdout.is_empty() {
                        &out.stderr
                    } else {
                        &out.stdout
                    };
                    call.text_head = Some(head_text(text));
                }
            }
            if call.json.is_some() && !out.stderr.is_empty() {
                call.text_head = Some(head_text(&out.stderr));
            }
        }
        Ok(Err(e)) => {
            call.os_error = e
                .downcast_ref::<std::io::Error>()
                .and_then(|io| io.raw_os_error());
            call.error = Some(format!("{e:#}"));
        }
        Err(msg) => call.error = Some(msg),
    }
    call.micros = started.elapsed().as_micros();
    call
}

/// Does `tailscale` run? Returns the program to invoke (PATH first, then Program Files).
pub fn cli_locate() -> Option<std::path::PathBuf> {
    let mut candidates = vec![std::path::PathBuf::from("tailscale")];
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        candidates.push(Path::new(&pf).join("Tailscale").join("tailscale.exe"));
    }
    candidates.into_iter().find(|c| {
        Command::new(c)
            .arg("version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

fn head_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.trim().chars().take(200).collect()
}

fn with_timeout<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<Result<T>, String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(TIMEOUT)
        .map_err(|_| format!("timed out after {} s", TIMEOUT.as_secs()))
}

fn roundtrip(pipe: &str, request: &[u8], qos: Qos) -> Result<http::Response> {
    let mut file = open_pipe(pipe, qos)?;
    file.write_all(request)?;
    http::read_response(&mut file)
}

/// Open the pipe, retrying while every server instance is busy.
#[cfg(windows)]
fn open_pipe(pipe: &str, qos: Qos) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const ERROR_PIPE_BUSY: i32 = 231;
    const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;
    const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
    const SECURITY_IMPERSONATION: u32 = 0x0002_0000;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true);
    match qos {
        Qos::Default => {}
        Qos::Anonymous => {
            opts.security_qos_flags(SECURITY_SQOS_PRESENT);
        }
        Qos::Identification => {
            opts.security_qos_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION);
        }
        Qos::Impersonation => {
            opts.security_qos_flags(SECURITY_SQOS_PRESENT | SECURITY_IMPERSONATION);
        }
    }
    let mut attempts = 0;
    loop {
        match opts.open(pipe) {
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 20 => {
                attempts += 1;
                BUSY_RETRIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(50));
            }
            other => {
                PIPE_OPENS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return other;
            }
        }
    }
}

#[cfg(not(windows))]
fn open_pipe(_pipe: &str, _qos: Qos) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the LocalAPI named pipe transport is implemented for Windows only",
    ))
}

/// SDDL of the pipe's security descriptor (owner and DACL), read from an open handle.
#[cfg(windows)]
pub fn pipe_sddl(pipe: &str) -> Result<String> {
    use anyhow::{Context, bail};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
        SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };

    let file = open_pipe(pipe, Qos::Default).context("open pipe")?;
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the handle is valid for the lifetime of `file`; the out-pointer for the security
    // descriptor is a valid local; unused out-parameters are null as the API permits.
    let rc = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if rc != 0 {
        bail!("GetSecurityInfo failed with Win32 error {rc}");
    }
    let mut wide: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sd` was returned by GetSecurityInfo and is still allocated; `wide` is a valid
    // out-pointer; the length out-parameter is optional and null.
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut wide,
            std::ptr::null_mut(),
        )
    };
    let result = if ok == 0 || wide.is_null() {
        Err(anyhow::anyhow!(
            "ConvertSecurityDescriptorToStringSecurityDescriptorW failed"
        ))
    } else {
        // SAFETY: on success `wide` points to a NUL-terminated UTF-16 string.
        let len = (0..).take_while(|&i| unsafe { *wide.add(i) } != 0).count();
        // SAFETY: `len` code units starting at `wide` were just read.
        let slice = unsafe { std::slice::from_raw_parts(wide, len) };
        Ok(String::from_utf16_lossy(slice))
    };
    // SAFETY: both pointers came from LocalAlloc-backed APIs and are freed exactly once.
    unsafe {
        if !wide.is_null() {
            LocalFree(wide.cast());
        }
        LocalFree(sd);
    }
    result
}

#[cfg(not(windows))]
pub fn pipe_sddl(_pipe: &str) -> Result<String> {
    anyhow::bail!("Windows only")
}
