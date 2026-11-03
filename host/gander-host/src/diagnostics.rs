// SPDX-License-Identifier: Apache-2.0
//! Where the host's console output goes when the release build has no console.
//!
//! The release build of `gander-host` is a Windows GUI-subsystem program (no console window opens
//! when it starts from Task Scheduler, the Start menu or Explorer). Its `println!`/`eprintln!`
//! output, panic messages and `Error:` exit line would then go nowhere. [`init`] runs first thing
//! in `main` and picks, per standard handle:
//!
//! 1. **Already usable** (a pipe, a file or a console: the process was started with redirected
//!    output, as the e2e harnesses and the UI smoke test do, or it is a console-subsystem debug
//!    build): left alone.
//! 2. **Launched from a terminal** (the parent process has a console): attach to it and write
//!    there, so `gander-host --ui-smoke` printed from a prompt shows its report.
//! 3. **No console at all** (autostart, Start menu): append to `host.log` in the log folder
//!    (`%LOCALAPPDATA%\Gander\logs`), rotated to `host.log.1` once it passes
//!    [`MAX_LOG_BYTES`], so a failure to start is never silent.
//!
//! Nothing here adds logging; it only makes the existing output reachable. The no-secrets rule
//! for logs is unchanged (the host never prints tokens or pairing secrets).

use std::path::{Path, PathBuf};

/// File name of the log in the log folder.
pub const LOG_FILE: &str = "host.log";

/// The log is rotated at start when it is larger than this (2 MiB).
pub const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

/// Where standard output and error ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// The process already had usable handles (redirected, or a console build).
    Inherited,
    /// Attached to the parent process's console.
    ParentConsole,
    /// Appending to this log file.
    LogFile(PathBuf),
    /// Nothing could be set up; output is discarded.
    Discarded,
}

/// The log folder for a data directory (`<data>\logs`).
pub fn log_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

/// Renames `dir/host.log` to `dir/host.log.1` (replacing an older one) when it is larger than
/// `max_bytes`. Returns whether it rotated.
pub fn rotate_log(dir: &Path, max_bytes: u64) -> std::io::Result<bool> {
    let log = dir.join(LOG_FILE);
    match std::fs::metadata(&log) {
        Ok(meta) if meta.len() > max_bytes => {
            let old = dir.join(format!("{LOG_FILE}.1"));
            match std::fs::remove_file(&old) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            std::fs::rename(&log, &old)?;
            Ok(true)
        }
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Opens `dir/host.log` for appending (creating `dir`), rotating it first when it is large.
pub fn open_log(dir: &Path) -> std::io::Result<std::fs::File> {
    std::fs::create_dir_all(dir)?;
    // A rotation failure (the old log is open elsewhere) must not stop the host logging.
    let _ = rotate_log(dir, MAX_LOG_BYTES);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LOG_FILE))
}

/// Sets up standard output and error as described in the module docs. `log_dir` is only used when
/// there is no console. Call once, first thing in `main`.
#[cfg(windows)]
pub fn init(log_dir: &Path) -> Output {
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        SetStdHandle,
    };

    fn missing(which: u32) -> bool {
        // SAFETY: GetStdHandle only reads the process's standard handle table.
        let h: HANDLE = unsafe { GetStdHandle(which) };
        h.is_null() || h == INVALID_HANDLE_VALUE
    }

    let need_out = missing(STD_OUTPUT_HANDLE);
    let need_err = missing(STD_ERROR_HANDLE);
    if !need_out && !need_err {
        return Output::Inherited;
    }

    // SAFETY: no pointers; fails (returns 0) when the parent has no console or this process
    // already has one.
    let attached = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } != 0;
    let handle = if attached {
        // "CONOUT$" is the attached console's screen buffer; one handle serves both streams.
        std::fs::OpenOptions::new().write(true).open("CONOUT$").ok()
    } else {
        open_log(log_dir).ok()
    };
    let Some(file) = handle else {
        return Output::Discarded;
    };
    let raw = file.as_raw_handle() as HANDLE;
    for (which, needed) in [(STD_OUTPUT_HANDLE, need_out), (STD_ERROR_HANDLE, need_err)] {
        if needed {
            // SAFETY: `raw` is a valid open handle; it is leaked below so it stays valid for
            // the life of the process (Rust's stdio reads the handle table on every write).
            unsafe { SetStdHandle(which, raw) };
        }
    }
    std::mem::forget(file);
    if attached {
        Output::ParentConsole
    } else {
        Output::LogFile(log_dir.join(LOG_FILE))
    }
}

/// Other platforms keep their console.
#[cfg(not(windows))]
pub fn init(_log_dir: &Path) -> Output {
    Output::Inherited
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("gander-diag-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_log_folder_is_under_the_data_directory() {
        assert_eq!(
            log_dir(Path::new("D:/data/Gander")),
            PathBuf::from("D:/data/Gander/logs")
        );
    }

    #[test]
    fn a_missing_or_small_log_is_not_rotated() {
        let s = Scratch::new("small");
        assert!(!rotate_log(&s.0, 10).unwrap());
        std::fs::write(s.0.join(LOG_FILE), b"0123456789").unwrap();
        assert!(!rotate_log(&s.0, 10).unwrap(), "exactly at the limit stays");
        assert!(s.0.join(LOG_FILE).exists());
    }

    #[test]
    fn a_large_log_moves_aside_and_replaces_the_previous_one() {
        let s = Scratch::new("large");
        std::fs::write(s.0.join(format!("{LOG_FILE}.1")), b"ancient").unwrap();
        std::fs::write(s.0.join(LOG_FILE), b"0123456789A").unwrap();
        assert!(rotate_log(&s.0, 10).unwrap());
        assert!(!s.0.join(LOG_FILE).exists());
        assert_eq!(
            std::fs::read(s.0.join(format!("{LOG_FILE}.1"))).unwrap(),
            b"0123456789A"
        );
    }

    #[test]
    fn opening_the_log_creates_the_folder_and_appends() {
        use std::io::Write as _;
        let s = Scratch::new("open");
        let dir = s.0.join("logs");
        open_log(&dir).unwrap().write_all(b"one\n").unwrap();
        open_log(&dir).unwrap().write_all(b"two\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(LOG_FILE)).unwrap(),
            "one\ntwo\n"
        );
    }
}
