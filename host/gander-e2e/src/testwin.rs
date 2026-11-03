// SPDX-License-Identifier: Apache-2.0
//! The `gander-testwin` control pipe from the harness side (WP-57): find the test windows that
//! are running (each serves `\\.\pipe\gander-testwin-<pid>`), and ask one a question.
//!
//! Only `gander-testwin` is ever talked to; the pipe is restricted to the current user by the
//! test window itself.

use std::collections::BTreeSet;
use std::io::{BufRead as _, BufReader, Write as _};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};

const PREFIX: &str = "gander-testwin-";

/// Pids of every running test window (from the pipe namespace).
pub fn pids() -> BTreeSet<u32> {
    let Ok(dir) = std::fs::read_dir(r"\\.\pipe\") else {
        return BTreeSet::new();
    };
    dir.filter_map(Result::ok)
        .filter_map(|e| parse_pipe_name(&e.file_name().to_string_lossy()))
        .collect()
}

/// `gander-testwin-<pid>` -> pid (the `-reuse-<key>` pipes are not windows).
pub fn parse_pipe_name(name: &str) -> Option<u32> {
    name.strip_prefix(PREFIX)?.parse().ok()
}

/// Waits up to `within` for a test window that is not in `before`.
pub fn wait_new(before: &BTreeSet<u32>, within: Duration) -> Option<u32> {
    let deadline = Instant::now() + within;
    loop {
        if let Some(p) = pids().difference(before).next() {
            return Some(*p);
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Waits up to `within` until the window `pid` has gone (its pipe is closed).
pub fn wait_gone(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if !pids().contains(&pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !pids().contains(&pid)
}

/// One request, one reply line (`ok ...` or `err ...`).
pub fn request(pid: u32, line: &str) -> Result<String> {
    let name = format!(r"\\.\pipe\{PREFIX}{pid}");
    let deadline = Instant::now() + Duration::from_secs(3);
    let f = loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
        {
            Ok(f) => break f,
            // ERROR_PIPE_BUSY: another client is being served.
            Err(e) if e.raw_os_error() == Some(231) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e).with_context(|| format!("open {name}")),
        }
    };
    let mut w = &f;
    w.write_all(format!("{line}\n").as_bytes())?;
    let mut reply = String::new();
    BufReader::new(&f).read_line(&mut reply)?;
    let reply = reply.trim_end().to_owned();
    if !reply.starts_with("ok") {
        bail!("testwin {pid}: `{line}` -> {reply}");
    }
    Ok(reply)
}

/// The value of `key=` in a reply.
pub fn field<'a>(reply: &'a str, key: &str) -> Option<&'a str> {
    reply
        .split_whitespace()
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

/// `colour?`: (RRGGBB, palette index, hits).
pub fn colour(pid: u32) -> Result<(String, u32, u32)> {
    let r = request(pid, "colour?")?;
    let get = |k: &str| field(&r, k).with_context(|| format!("no {k} in {r}"));
    Ok((
        get("colour")?.to_owned(),
        get("index")?.parse()?,
        get("hits")?.parse()?,
    ))
}

/// The main window's handle (`windows?`, main first).
pub fn main_hwnd(pid: u32) -> Result<u64> {
    let r = request(pid, "windows?")?;
    let first = field(&r, "hwnd")
        .and_then(|h| h.split(',').next())
        .and_then(|h| h.strip_prefix("0x"))
        .with_context(|| format!("no hwnd in {r}"))?;
    Ok(u64::from_str_radix(first, 16)?)
}

/// Asks the window to quit; best effort.
pub fn quit(pid: u32) {
    let _ = request(pid, "quit");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_names_and_fields() {
        assert_eq!(parse_pipe_name("gander-testwin-1234"), Some(1234));
        assert_eq!(parse_pipe_name("gander-testwin-reuse-abc"), None);
        assert_eq!(parse_pipe_name("other-1"), None);
        let r = "ok colour=FF8000 index=0 hits=2 clicks=3";
        assert_eq!(field(r, "colour"), Some("FF8000"));
        assert_eq!(field(r, "hits"), Some("2"));
        assert_eq!(field(r, "hit"), None);
    }
}
