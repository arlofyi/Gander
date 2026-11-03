// SPDX-License-Identifier: Apache-2.0
//! WP-18: the binary's `--ui-smoke` mode, run as a child process.
//!
//! The offscreen pass runs on every `cargo test`: it renders each window with the CPU rasteriser,
//! needs no display and no GPU, and writes PNGs to a temp directory (never to `docs/`). The native
//! pass opens real windows and a tray icon, so it is opt-in:
//! `cargo test -p gander-host --test ui_smoke -- --ignored`.

#![cfg(feature = "ui")]

use std::path::PathBuf;
use std::process::Command;

const EXE: &str = env!("CARGO_BIN_EXE_gander-host");

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("gander-ui-smoke-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const SHOTS: [&str; 8] = [
    "shares",
    "devices",
    "add-device",
    "add-device-pending",
    "activity",
    "network",
    "settings",
    "toasts",
];

#[test]
fn offscreen_smoke_writes_every_screenshot_and_exits_zero() {
    let dir = TempDir::new();
    let out = Command::new(EXE)
        .args(["--ui-smoke", "--ui-no-native", "--ui-out"])
        .arg(&dir.0)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("ui-smoke: OK"), "{stdout}");
    assert!(stdout.contains("native pass skipped"), "{stdout}");
    // It must not have started a server.
    assert!(!stdout.contains("listening on"), "{stdout}");
    for name in SHOTS {
        let path = dir.0.join(format!("{name}.png"));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(&bytes[..4], b"\x89PNG", "{name}");
        assert!(bytes.len() > 2_000, "{name} is {} bytes", bytes.len());
    }
}

#[test]
fn the_screenshot_flags_need_the_smoke_flag() {
    let out = Command::new(EXE)
        .args(["--ui-out", "somewhere"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("only go with --ui-smoke"));
}

/// Real windows and a real tray icon, opened and closed by the application itself; nothing on the
/// screen is captured. Opt-in because it needs a window system (it works in a locked session).
#[test]
#[ignore = "opens real windows and a tray icon; run with --ignored"]
fn native_smoke_opens_every_window_once_and_exits_zero() {
    let dir = TempDir::new();
    let out = Command::new(EXE)
        .args(["--ui-smoke", "--ui-out"])
        .arg(&dir.0)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("ui-smoke: OK"), "{stdout}");
    for window in [
        "page:shares",
        "page:devices",
        "page:activity",
        "page:network",
        "page:settings",
        "add-device",
    ] {
        assert!(
            stdout.contains(&format!("window {window}:")),
            "{window} missing in {stdout}"
        );
    }
}
