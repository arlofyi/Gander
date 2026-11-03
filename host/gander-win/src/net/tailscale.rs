// SPDX-License-Identifier: Apache-2.0
//! Backends for [`TailscaleLocal`]: the LocalAPI named pipe (primary) and the `tailscale` CLI
//! (fallback and debugging aid). Both talk only to the LOCAL tailscaled.
//!
//! Pipe facts measured by the identity spike (`docs/specs/2026-11-01-identity-spike-findings.md`):
//!
//! - The pipe must be opened with `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`; a plain open
//!   gets HTTP 401 ("Unable to impersonate ..."). A 401 is mapped to "LocalAPI refused this
//!   process" so [`Fallback`] moves on to the CLI.
//! - 404 means "no network identity" (loopback, LAN, not a tailnet node).
//! - One connection per request with `Connection: close`; retry on `ERROR_PIPE_BUSY`.
//! - Synchronous pipe reads cannot time out, so every call runs on a worker thread and is
//!   abandoned after [`CALL_TIMEOUT`].
//!
//! The pipe's DACL admits BUILTIN\Users; it is not a security gate for Gander.

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use gander_core::net::localapi::{
    MAX_RESPONSE, PREFS_PATH, STATUS_PATH, build_get, parse_response, whois_path,
};
use gander_core::net::tailscale::{
    Fallback, TailscaleError, TailscaleLocal, TailscaleStatus, WhoIsAnswer, is_not_found_message,
    parse_shields_up, parse_status, parse_whois,
};
use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;

/// The LocalAPI named pipe on Windows.
pub const LOCALAPI_PIPE: &str = r"\\.\pipe\ProtectedPrefix\Administrators\Tailscale\tailscaled";

/// Longest a single LocalAPI or CLI call may take.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(5);

const PIPE_BUSY_RETRIES: u32 = 10;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Runs `f` on a worker thread and waits at most `timeout`.
fn with_timeout<T: Send + 'static>(
    timeout: Duration,
    what: &'static str,
    f: impl FnOnce() -> Result<T, TailscaleError> + Send + 'static,
) -> Result<T, TailscaleError> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name(format!("gander-tailscale-{what}"))
        .spawn(move || {
            let _ = tx.send(f());
        })
        .map_err(|e| TailscaleError::Request(format!("{what}: spawn: {e}")))?;
    rx.recv_timeout(timeout)
        .map_err(|_| TailscaleError::Request(format!("{what}: timed out")))?
}

/// The LocalAPI over the named pipe.
#[derive(Debug, Clone)]
pub struct LocalApiPipe {
    pipe: String,
    timeout: Duration,
}

impl Default for LocalApiPipe {
    fn default() -> Self {
        Self {
            pipe: LOCALAPI_PIPE.to_owned(),
            timeout: CALL_TIMEOUT,
        }
    }
}

/// HTTP status and body of one LocalAPI call.
type Raw = (u16, Vec<u8>);

impl LocalApiPipe {
    /// Uses another pipe path (tests).
    pub fn with_pipe(pipe: impl Into<String>) -> Self {
        Self {
            pipe: pipe.into(),
            timeout: CALL_TIMEOUT,
        }
    }

    fn get(&self, path: String) -> Result<Raw, TailscaleError> {
        let pipe = self.pipe.clone();
        with_timeout(self.timeout, "localapi", move || get_blocking(&pipe, &path))
    }

    fn get_ok(&self, path: String) -> Result<Vec<u8>, TailscaleError> {
        match self.get(path)? {
            (200, body) => Ok(body),
            (status, body) => Err(status_error(status, &body)),
        }
    }
}

fn status_error(status: u16, body: &[u8]) -> TailscaleError {
    match status {
        401 => TailscaleError::Request("LocalAPI refused this process (401)".into()),
        403 => TailscaleError::Request("LocalAPI rejected the request (403)".into()),
        _ => {
            // Body is daemon text; keep it short and never echo it whole.
            let text = String::from_utf8_lossy(&body[..body.len().min(120)]).into_owned();
            TailscaleError::Request(format!("LocalAPI status {status}: {}", text.trim()))
        }
    }
}

fn get_blocking(pipe: &str, path: &str) -> Result<Raw, TailscaleError> {
    let mut attempt = 0;
    let mut file = loop {
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            // std adds SECURITY_SQOS_PRESENT when QoS flags are given.
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(pipe);
        match opened {
            Ok(f) => break f,
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                attempt += 1;
                if attempt > PIPE_BUSY_RETRIES {
                    return Err(TailscaleError::Request("LocalAPI pipe busy".into()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(TailscaleError::Unavailable(
                    "LocalAPI pipe not found (tailscaled not running?)".into(),
                ));
            }
            Err(e) => return Err(TailscaleError::Request(format!("LocalAPI pipe: {e}"))),
        }
    };
    file.write_all(&build_get(path))
        .map_err(|e| TailscaleError::Request(format!("LocalAPI write: {e}")))?;
    let mut raw = Vec::new();
    (&mut file)
        .take(MAX_RESPONSE as u64 + 64 * 1024)
        .read_to_end(&mut raw)
        .map_err(|e| TailscaleError::Request(format!("LocalAPI read: {e}")))?;
    let resp = parse_response(&raw)?;
    Ok((resp.status, resp.body))
}

impl TailscaleLocal for LocalApiPipe {
    fn whois(&self, addr: SocketAddr) -> Result<WhoIsAnswer, TailscaleError> {
        match self.get(whois_path(addr))? {
            (200, body) => parse_whois(&body),
            (404, _) => Ok(WhoIsAnswer::NotFound),
            (status, body) => Err(status_error(status, &body)),
        }
    }

    fn status(&self) -> Result<TailscaleStatus, TailscaleError> {
        parse_status(&self.get_ok(STATUS_PATH.to_owned())?)
    }

    fn shields_up(&self) -> Result<bool, TailscaleError> {
        parse_shields_up(&self.get_ok(PREFS_PATH.to_owned())?)
    }
}

/// The `tailscale` CLI (v1.56+): `whois --json`, `status --json`, `debug prefs`.
#[derive(Debug, Clone)]
pub struct TailscaleCli {
    exe: PathBuf,
    timeout: Duration,
}

impl TailscaleCli {
    /// Finds `tailscale.exe` on `PATH` or in `%ProgramFiles%\Tailscale`.
    pub fn locate() -> Option<Self> {
        let on_path = std::env::var_os("PATH").and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("tailscale.exe"))
                .find(|c| c.is_file())
        });
        let program_files = std::env::var_os("ProgramFiles")
            .map(|p| PathBuf::from(p).join("Tailscale").join("tailscale.exe"))
            .filter(|c| c.is_file());
        on_path.or(program_files).map(Self::with_exe)
    }

    /// Uses a specific executable.
    pub fn with_exe(exe: PathBuf) -> Self {
        Self {
            exe,
            timeout: CALL_TIMEOUT,
        }
    }

    fn run(&self, args: &[String]) -> Result<(bool, Vec<u8>, String), TailscaleError> {
        let exe = self.exe.clone();
        let args = args.to_vec();
        with_timeout(self.timeout, "cli", move || {
            let out = Command::new(&exe)
                .args(&args)
                .stdin(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .map_err(|e| TailscaleError::Unavailable(format!("tailscale CLI: {e}")))?;
            let stderr = String::from_utf8_lossy(&out.stderr[..out.stderr.len().min(200)])
                .trim()
                .to_owned();
            Ok((out.status.success(), out.stdout, stderr))
        })
    }

    fn run_ok(&self, args: &[&str]) -> Result<Vec<u8>, TailscaleError> {
        let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        match self.run(&args)? {
            (true, out, _) => Ok(out),
            (false, _, err) => Err(TailscaleError::Request(format!("tailscale CLI: {err}"))),
        }
    }
}

impl TailscaleLocal for TailscaleCli {
    fn whois(&self, addr: SocketAddr) -> Result<WhoIsAnswer, TailscaleError> {
        let args = vec!["whois".to_owned(), "--json".to_owned(), addr.to_string()];
        match self.run(&args)? {
            (true, out, _) => parse_whois(&out),
            (false, _, err) if is_not_found_message(&err) => Ok(WhoIsAnswer::NotFound),
            (false, _, err) => Err(TailscaleError::Request(format!("tailscale whois: {err}"))),
        }
    }

    fn status(&self) -> Result<TailscaleStatus, TailscaleError> {
        parse_status(&self.run_ok(&["status", "--json"])?)
    }

    fn shields_up(&self) -> Result<bool, TailscaleError> {
        // `debug prefs` is a debug command; the pipe's `/prefs` is the primary source.
        parse_shields_up(&self.run_ok(&["debug", "prefs"])?)
    }
}

/// A backend that is never available (no CLI found).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTailscale;

impl TailscaleLocal for NoTailscale {
    fn whois(&self, _: SocketAddr) -> Result<WhoIsAnswer, TailscaleError> {
        Err(TailscaleError::Unavailable(
            "tailscale not installed".into(),
        ))
    }
    fn status(&self) -> Result<TailscaleStatus, TailscaleError> {
        Err(TailscaleError::Unavailable(
            "tailscale not installed".into(),
        ))
    }
    fn shields_up(&self) -> Result<bool, TailscaleError> {
        Err(TailscaleError::Unavailable(
            "tailscale not installed".into(),
        ))
    }
}

/// CLI backend if found, else [`NoTailscale`].
#[derive(Debug)]
pub enum CliOrNone {
    /// The CLI.
    Cli(TailscaleCli),
    /// Not installed.
    None(NoTailscale),
}

impl TailscaleLocal for CliOrNone {
    fn whois(&self, addr: SocketAddr) -> Result<WhoIsAnswer, TailscaleError> {
        match self {
            CliOrNone::Cli(c) => c.whois(addr),
            CliOrNone::None(n) => n.whois(addr),
        }
    }
    fn status(&self) -> Result<TailscaleStatus, TailscaleError> {
        match self {
            CliOrNone::Cli(c) => c.status(),
            CliOrNone::None(n) => n.status(),
        }
    }
    fn shields_up(&self) -> Result<bool, TailscaleError> {
        match self {
            CliOrNone::Cli(c) => c.shields_up(),
            CliOrNone::None(n) => n.shields_up(),
        }
    }
}

/// The production backend: LocalAPI pipe first, CLI on failure.
pub fn system_tailscale() -> Fallback<LocalApiPipe, CliOrNone> {
    let cli = TailscaleCli::locate().map_or(CliOrNone::None(NoTailscale), CliOrNone::Cli);
    Fallback::new(LocalApiPipe::default(), cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_pipe_is_unavailable() {
        let p = LocalApiPipe::with_pipe(r"\\.\pipe\gander-test-no-such-pipe-7f3a");
        let err = p.status().unwrap_err();
        assert!(matches!(err, TailscaleError::Unavailable(_)), "{err}");
    }

    #[test]
    fn status_error_mapping() {
        assert!(
            status_error(401, b"")
                .to_string()
                .contains("refused this process")
        );
        assert!(status_error(500, &[b'x'; 1000]).to_string().len() < 200);
    }

    /// Live check against the local tailscaled (no peer is contacted). Run with
    /// `cargo test -p gander-win -- --ignored live_`.
    #[test]
    #[ignore = "needs a running local tailscaled"]
    fn live_pipe_status_and_loopback_whois() {
        let pipe = LocalApiPipe::default();
        let status = pipe.status().expect("status over the pipe");
        assert_eq!(status.backend_state, "Running");
        assert_eq!(
            pipe.whois("127.0.0.1:1".parse().unwrap()).unwrap(),
            WhoIsAnswer::NotFound
        );
        let me = status.self_addresses[0];
        assert!(matches!(
            pipe.whois(SocketAddr::new(me, 1)).unwrap(),
            WhoIsAnswer::Found(_)
        ));
        pipe.shields_up().expect("prefs over the pipe");
        let cli = TailscaleCli::locate().expect("tailscale CLI");
        assert_eq!(
            cli.whois("127.0.0.1:1".parse().unwrap()).unwrap(),
            WhoIsAnswer::NotFound
        );
        assert!(matches!(
            cli.whois(SocketAddr::new(me, 1)).unwrap(),
            WhoIsAnswer::Found(_)
        ));
    }
}
