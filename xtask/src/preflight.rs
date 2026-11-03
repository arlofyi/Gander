// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask preflight`: the automated preflight of master plan section 1b (WP-00).
//!
//! Hard checks decide the exit code. Optional checks are reported but never fail the run.
//! Nothing here installs software, changes machine settings, or contacts another computer:
//! `tailscale status` and `adb devices` only talk to the local daemon.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::proc::{capture, display_path, find_on_path, workspace_root};

const MIN_CARGO: (u32, u32) = (1, 90);
const MIN_FREE_BYTES: u64 = 20 * 1024 * 1024 * 1024;
const MIN_WINDOWS_BUILD: u32 = 22000;
const ANDROID_PLATFORM: &str = "android-36";
const AVD_NAME: &str = "gander";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    Hard,
    Optional,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Pass,
    Fail,
    Skipped,
}

#[derive(Debug)]
struct Check {
    id: &'static str,
    level: Level,
    expected: &'static str,
    outcome: Outcome,
    detail: String,
}

impl Check {
    fn label(&self) -> &'static str {
        match self.outcome {
            Outcome::Pass => "PASS",
            Outcome::Fail if self.level == Level::Hard => "FAIL",
            Outcome::Fail => "WARN",
            Outcome::Skipped => "SKIP",
        }
    }

    fn is_hard_failure(&self) -> bool {
        self.level == Level::Hard && self.outcome == Outcome::Fail
    }
}

fn check(
    id: &'static str,
    level: Level,
    expected: &'static str,
    run: impl FnOnce() -> (Outcome, String),
) -> Check {
    let (outcome, detail) = run();
    Check {
        id,
        level,
        expected,
        outcome,
        detail,
    }
}

/// Entry point for `cargo xtask preflight [--report <path>]`.
pub fn run(args: &[String]) -> Result<ExitCode> {
    let mut report_path: Option<PathBuf> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--report" => {
                let value = iter.next().context("--report needs a path")?;
                report_path = Some(PathBuf::from(value));
            }
            other => bail!("unknown preflight argument `{other}` (supported: --report <path>)"),
        }
    }

    let root = workspace_root();
    let sdk = android_sdk_root();
    let checks = vec![
        check("cargo", Level::Hard, "cargo >= 1.90", check_cargo),
        check(
            "msvc-build",
            Level::Hard,
            "hello-world cargo build links and runs (MSVC)",
            || check_msvc_build(&root),
        ),
        check("java", Level::Hard, "java 17 or 21", check_java),
        check(
            "android-sdk",
            Level::Hard,
            "ANDROID_HOME with platforms/android-36, build-tools/36.*, cmdline-tools/latest",
            || check_android_sdk(sdk.as_deref()),
        ),
        check(
            "tailscale",
            Level::Hard,
            "`tailscale status` reports Running",
            check_tailscale,
        ),
        check(
            "git",
            Level::Hard,
            "git user.name and user.email configured",
            check_git,
        ),
        check("disk", Level::Hard, "free disk >= 20 GB", || {
            check_disk(&root)
        }),
        check(
            "windows-build",
            Level::Hard,
            "Windows build >= 22000",
            check_windows_build,
        ),
        check(
            "msvc-cl",
            Level::Optional,
            "cl.exe locatable through vswhere",
            check_cl_exe,
        ),
        check(
            "emulator-avd",
            Level::Optional,
            "`emulator -list-avds` lists `gander`",
            || check_avd(sdk.as_deref()),
        ),
        check(
            "emulator-accel",
            Level::Optional,
            "`emulator -accel-check` reports usable",
            || check_accel(sdk.as_deref()),
        ),
        check(
            "adb-devices",
            Level::Optional,
            "`adb devices` lists a device",
            || check_adb(sdk.as_deref()),
        ),
        check(
            "office",
            Level::Optional,
            "POWERPNT.EXE exists",
            check_office,
        ),
        check("ffmpeg", Level::Optional, "ffmpeg on PATH", check_ffmpeg),
        check(
            "pwsh",
            Level::Optional,
            "PowerShell 7+ installed",
            check_pwsh,
        ),
        check(
            "cargo-deny",
            Level::Optional,
            "cargo-deny installed",
            check_cargo_deny,
        ),
    ];
    let tools = tool_inventory(sdk.as_deref());

    print_summary(&checks);
    if let Some(path) = report_path {
        let markdown = render_report(&checks, &tools);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&path, markdown).with_context(|| format!("writing {}", path.display()))?;
        println!("report written to {}", display_path(&path));
    }

    let hard_failures = checks.iter().filter(|c| c.is_hard_failure()).count();
    if hard_failures > 0 {
        eprintln!("preflight FAILED: {hard_failures} hard check(s) failed");
        return Ok(ExitCode::from(1));
    }
    println!("preflight passed (all hard checks)");
    Ok(ExitCode::SUCCESS)
}

fn print_summary(checks: &[Check]) {
    for c in checks {
        println!("[{}] {:<14} {}", c.label(), c.id, c.detail);
    }
}

// ---------------------------------------------------------------------------------------------
// Individual checks
// ---------------------------------------------------------------------------------------------

fn check_cargo() -> (Outcome, String) {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    match capture(&cargo, &["--version"]) {
        Ok(out) if out.success() => {
            let line = out.first_line();
            match parse_cargo_version(&line) {
                Some((major, minor, _)) if (major, minor) >= MIN_CARGO => (Outcome::Pass, line),
                Some(_) => (Outcome::Fail, format!("{line} is older than 1.90")),
                None => (Outcome::Fail, format!("could not parse `{line}`")),
            }
        }
        Ok(out) => (
            Outcome::Fail,
            format!("cargo --version failed: {}", out.first_line()),
        ),
        Err(e) => (Outcome::Fail, format!("{e:#}")),
    }
}

fn check_msvc_build(root: &Path) -> (Outcome, String) {
    if !cfg!(windows) {
        return (Outcome::Skipped, "not a Windows host".into());
    }
    match hello_world_build(root) {
        Ok(detail) => (Outcome::Pass, detail),
        Err(e) => (Outcome::Fail, format!("{e:#}")),
    }
}

/// Build and run a throwaway hello-world crate under `target/xtask-preflight`.
fn hello_world_build(root: &Path) -> Result<String> {
    const MARKER: &str = "gander-preflight-ok";
    let dir = root.join("target").join("xtask-preflight").join("hello");
    std::fs::create_dir_all(dir.join("src")).context("creating hello-world directory")?;
    // An empty [workspace] table keeps cargo from attaching this crate to the repository workspace.
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"hello\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )?;
    std::fs::write(
        dir.join("src").join("main.rs"),
        format!("fn main() {{ println!(\"{MARKER}\"); }}\n"),
    )?;
    let target_dir = root.join("target").join("xtask-preflight").join("target");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let manifest = display_path(&dir.join("Cargo.toml"));
    let target = display_path(&target_dir);
    let build = capture(
        &cargo,
        &[
            "build",
            "--quiet",
            "--offline",
            "--manifest-path",
            &manifest,
            "--target-dir",
            &target,
        ],
    )?;
    if !build.success() {
        bail!("hello-world build failed: {}", tail(&build.combined(), 6));
    }
    let exe = target_dir.join("debug").join("hello.exe");
    let run = capture(&exe, &[])?;
    if !run.success() || !run.stdout.contains(MARKER) {
        bail!(
            "hello-world binary did not run correctly: {}",
            run.first_line()
        );
    }
    let rustc = capture("rustc", &["-vV"])
        .map(|o| o.stdout)
        .unwrap_or_default();
    let host = rustc
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .unwrap_or("unknown host");
    if !host.contains("msvc") {
        bail!("rustc host `{host}` is not an MSVC target");
    }
    Ok(format!("built and ran hello-world, rustc host {host}"))
}

fn check_java() -> (Outcome, String) {
    let java = java_executable();
    match capture(&java, &["-version"]) {
        Ok(out) if out.success() => {
            let combined = out.combined();
            let line = combined
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("");
            match parse_java_major(&combined) {
                Some(17 | 21) => (Outcome::Pass, format!("{line} ({})", display_path(&java))),
                Some(other) => (Outcome::Fail, format!("java {other} found, need 17 or 21")),
                None => (Outcome::Fail, format!("could not parse `{line}`")),
            }
        }
        Ok(out) => (
            Outcome::Fail,
            format!("java -version failed: {}", out.first_line()),
        ),
        Err(e) => (Outcome::Fail, format!("{e:#}")),
    }
}

fn java_executable() -> PathBuf {
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let exe =
            Path::new(&home)
                .join("bin")
                .join(if cfg!(windows) { "java.exe" } else { "java" });
        if exe.is_file() {
            return exe;
        }
    }
    find_on_path("java").unwrap_or_else(|| PathBuf::from("java"))
}

pub(crate) fn android_sdk_root() -> Option<PathBuf> {
    ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .find(|p| p.is_dir())
}

fn check_android_sdk(sdk: Option<&Path>) -> (Outcome, String) {
    let Some(sdk) = sdk else {
        return (
            Outcome::Fail,
            "ANDROID_HOME (or ANDROID_SDK_ROOT) is unset or not a directory".into(),
        );
    };
    let platform = sdk.join("platforms").join(ANDROID_PLATFORM);
    let cmdline = sdk.join("cmdline-tools").join("latest");
    let build_tools = list_dirs(&sdk.join("build-tools"))
        .into_iter()
        .filter(|name| name.starts_with("36."))
        .collect::<Vec<_>>();
    let mut missing = Vec::new();
    if !platform.is_dir() {
        missing.push("platforms/android-36");
    }
    if build_tools.is_empty() {
        missing.push("build-tools/36.*");
    }
    if !cmdline.is_dir() {
        missing.push("cmdline-tools/latest");
    }
    if missing.is_empty() {
        (
            Outcome::Pass,
            format!(
                "{} with platforms/android-36, build-tools {}, cmdline-tools/latest",
                display_path(sdk),
                build_tools.join(", ")
            ),
        )
    } else {
        (
            Outcome::Fail,
            format!("{} is missing {}", display_path(sdk), missing.join(", ")),
        )
    }
}

fn list_dirs(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn tailscale_executable() -> Option<PathBuf> {
    find_on_path("tailscale").or_else(|| {
        let program_files = std::env::var_os("ProgramFiles")?;
        let exe = Path::new(&program_files)
            .join("Tailscale")
            .join("tailscale.exe");
        exe.is_file().then_some(exe)
    })
}

fn check_tailscale() -> (Outcome, String) {
    let Some(exe) = tailscale_executable() else {
        return (Outcome::Fail, "tailscale executable not found".into());
    };
    // Local daemon only: --peers=false avoids even listing (let alone contacting) other nodes.
    match capture(&exe, &["status", "--json", "--peers=false"]) {
        Ok(out) => match parse_tailscale_state(&out.stdout) {
            Some((state, version)) if state == "Running" => (
                Outcome::Pass,
                format!("BackendState Running, tailscale {version}"),
            ),
            Some((state, _)) => (
                Outcome::Fail,
                format!("BackendState is `{state}`, need Running"),
            ),
            None => (
                Outcome::Fail,
                format!("unreadable `tailscale status`: {}", out.first_line()),
            ),
        },
        Err(e) => (Outcome::Fail, format!("{e:#}")),
    }
}

fn git_config(key: &str) -> Option<String> {
    let out = capture("git", &["config", "--get", key]).ok()?;
    let value = out.stdout.trim().to_string();
    (out.success() && !value.is_empty()).then_some(value)
}

fn check_git() -> (Outcome, String) {
    let missing: Vec<&str> = ["user.name", "user.email"]
        .into_iter()
        .filter(|key| git_config(key).is_none())
        .collect();
    if missing.is_empty() {
        (Outcome::Pass, "user.name and user.email are set".into())
    } else {
        (
            Outcome::Fail,
            format!("missing git config: {}", missing.join(", ")),
        )
    }
}

fn check_disk(root: &Path) -> (Outcome, String) {
    match platform::free_disk_bytes(root) {
        Some(bytes) if bytes >= MIN_FREE_BYTES => {
            (Outcome::Pass, format!("{:.1} GiB free", gib(bytes)))
        }
        Some(bytes) => (
            Outcome::Fail,
            format!("{:.1} GiB free, need 20", gib(bytes)),
        ),
        None => (
            Outcome::Skipped,
            "free disk is only measured on Windows".into(),
        ),
    }
}

#[allow(clippy::cast_precision_loss)]
fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

fn check_windows_build() -> (Outcome, String) {
    match platform::windows_version() {
        Some((major, minor, build)) if build >= MIN_WINDOWS_BUILD => (
            Outcome::Pass,
            format!("Windows {major}.{minor} build {build}"),
        ),
        Some((major, minor, build)) => (
            Outcome::Fail,
            format!("Windows {major}.{minor} build {build}, need >= 22000"),
        ),
        None => (Outcome::Skipped, "not a Windows host".into()),
    }
}

fn check_cl_exe() -> (Outcome, String) {
    match find_cl_exe() {
        Some(path) => (Outcome::Pass, display_path(&path)),
        None => (
            Outcome::Fail,
            "cl.exe not found via vswhere (needed by crates that compile C, e.g. ring)".into(),
        ),
    }
}

fn find_cl_exe() -> Option<PathBuf> {
    let program_files = std::env::var_os("ProgramFiles(x86)")?;
    let vswhere = Path::new(&program_files)
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");
    let out = capture(
        &vswhere,
        &[
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ],
    )
    .ok()?;
    let install = PathBuf::from(out.stdout.lines().map(str::trim).find(|l| !l.is_empty())?);
    let msvc = install.join("VC").join("Tools").join("MSVC");
    let newest = list_dirs(&msvc).into_iter().next_back()?;
    let cl = msvc
        .join(newest)
        .join("bin")
        .join("Hostx64")
        .join("x64")
        .join("cl.exe");
    cl.is_file().then_some(cl)
}

/// The Android AVD store. A session launcher may override HOME/USERPROFILE, so the default
/// `~/.android/avd` of this session is not where the owner's AVDs live. Resolve in order:
/// `ANDROID_AVD_HOME`, `ANDROID_USER_HOME/avd`, `<home>/.android/avd`, and the real user profile
/// derived from `LOCALAPPDATA`. The first directory that holds `*.ini` files wins.
pub fn avd_home() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("ANDROID_AVD_HOME") {
        candidates.push(PathBuf::from(p));
    }
    if let Some(p) = std::env::var_os("ANDROID_USER_HOME") {
        candidates.push(Path::new(&p).join("avd"));
    }
    for var in ["USERPROFILE", "HOME"] {
        if let Some(p) = std::env::var_os(var) {
            candidates.push(Path::new(&p).join(".android").join("avd"));
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA")
        && let Some(profile) = Path::new(&local).parent().and_then(Path::parent)
    {
        candidates.push(profile.join(".android").join("avd"));
    }
    candidates.into_iter().find(|dir| {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.path().extension().is_some_and(|x| x == "ini"))
    })
}

fn sdk_tool(sdk: &Path, relative: &[&str], windows_name: &str, unix_name: &str) -> PathBuf {
    let mut path = sdk.to_path_buf();
    for part in relative {
        path.push(part);
    }
    path.push(if cfg!(windows) {
        windows_name
    } else {
        unix_name
    });
    path
}

pub(crate) fn emulator_path(sdk: &Path) -> PathBuf {
    sdk_tool(sdk, &["emulator"], "emulator.exe", "emulator")
}

pub(crate) fn adb_path(sdk: &Path) -> PathBuf {
    sdk_tool(sdk, &["platform-tools"], "adb.exe", "adb")
}

fn sdkmanager_path(sdk: &Path) -> PathBuf {
    sdk_tool(
        sdk,
        &["cmdline-tools", "latest", "bin"],
        "sdkmanager.bat",
        "sdkmanager",
    )
}

fn avdmanager_path(sdk: &Path) -> PathBuf {
    sdk_tool(
        sdk,
        &["cmdline-tools", "latest", "bin"],
        "avdmanager.bat",
        "avdmanager",
    )
}

fn check_avd(sdk: Option<&Path>) -> (Outcome, String) {
    let Some(sdk) = sdk else {
        return (Outcome::Skipped, "no Android SDK".into());
    };
    let emulator = emulator_path(sdk);
    if !emulator.is_file() {
        return (Outcome::Fail, "emulator is not installed".into());
    }
    let home = avd_home();
    let mut command = std::process::Command::new(&emulator);
    command.arg("-list-avds");
    if let Some(home) = &home {
        command.env("ANDROID_AVD_HOME", home);
    }
    match command.output() {
        Ok(out) => {
            let avds = parse_avd_list(&String::from_utf8_lossy(&out.stdout));
            let location = home
                .as_deref()
                .map_or_else(|| "default AVD home".to_string(), display_path);
            if avds.iter().any(|a| a == AVD_NAME) {
                (
                    Outcome::Pass,
                    format!("AVDs: {} (AVD home {location})", avds.join(", ")),
                )
            } else {
                (
                    Outcome::Fail,
                    format!("no `{AVD_NAME}` AVD (found: {avds:?}, AVD home {location})"),
                )
            }
        }
        Err(e) => (Outcome::Fail, format!("could not run emulator: {e}")),
    }
}

fn check_accel(sdk: Option<&Path>) -> (Outcome, String) {
    let Some(sdk) = sdk else {
        return (Outcome::Skipped, "no Android SDK".into());
    };
    let emulator = emulator_path(sdk);
    if !emulator.is_file() {
        return (Outcome::Skipped, "emulator is not installed".into());
    }
    match capture(&emulator, &["-accel-check"]) {
        Ok(out) => {
            let combined = out.combined();
            let verdict = combined
                .lines()
                .map(str::trim)
                .find(|l| l.contains("usable") || l.contains("not") || l.contains("fail"))
                .unwrap_or("no verdict printed")
                .to_string();
            if out.success() && combined.contains("is installed and usable") {
                (Outcome::Pass, verdict)
            } else {
                (
                    Outcome::Fail,
                    format!("{verdict} (human action: enable Windows Hypervisor Platform)"),
                )
            }
        }
        Err(e) => (Outcome::Fail, format!("{e:#}")),
    }
}

fn check_adb(sdk: Option<&Path>) -> (Outcome, String) {
    let Some(sdk) = sdk else {
        return (Outcome::Skipped, "no Android SDK".into());
    };
    let adb = adb_path(sdk);
    if !adb.is_file() {
        return (Outcome::Fail, "adb is not installed".into());
    }
    // `adb devices` only lists what the local adb server already knows. Never `adb connect`.
    match capture(&adb, &["devices"]) {
        Ok(out) => {
            let devices = parse_adb_devices(&out.stdout);
            if devices.is_empty() {
                (
                    Outcome::Fail,
                    "no devices attached (emulator only for Night 1)".into(),
                )
            } else {
                let list: Vec<String> = devices
                    .iter()
                    .map(|(serial, state)| format!("{serial} ({state})"))
                    .collect();
                (Outcome::Pass, list.join(", "))
            }
        }
        Err(e) => (Outcome::Fail, format!("{e:#}")),
    }
}

fn office_candidates() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(p) = std::env::var_os(var) {
            roots.push(PathBuf::from(p));
        }
    }
    let mut out = Vec::new();
    for root in roots {
        for rel in [
            "Microsoft Office/root/Office16",
            "Microsoft Office/root/Office15",
            "Microsoft Office/Office16",
            "Microsoft Office/Office15",
        ] {
            out.push(root.join(rel).join("POWERPNT.EXE"));
        }
    }
    out
}

fn check_office() -> (Outcome, String) {
    match office_candidates().into_iter().find(|p| p.is_file()) {
        Some(path) => (Outcome::Pass, display_path(&path)),
        None => (
            Outcome::Fail,
            "POWERPNT.EXE not found (Office COM render tests will skip)".into(),
        ),
    }
}

fn check_ffmpeg() -> (Outcome, String) {
    match find_on_path("ffmpeg") {
        Some(path) => {
            let line = capture(&path, &["-version"])
                .map(|o| o.first_line())
                .unwrap_or_default();
            let version = line.split(" Copyright").next().unwrap_or_default();
            (
                Outcome::Pass,
                format!("{version} ({})", display_path(&path)),
            )
        }
        None => (
            Outcome::Fail,
            "ffmpeg not on PATH (WP-42 decode validation will skip)".into(),
        ),
    }
}

fn pwsh_executable() -> Option<PathBuf> {
    find_on_path("pwsh").or_else(|| {
        let local = std::env::var_os("LOCALAPPDATA")?;
        let exe = Path::new(&local)
            .join("Microsoft")
            .join("WindowsApps")
            .join("pwsh.exe");
        exe.is_file().then_some(exe)
    })
}

fn check_pwsh() -> (Outcome, String) {
    let Some(exe) = pwsh_executable() else {
        return (
            Outcome::Fail,
            "pwsh not found (winget install Microsoft.PowerShell)".into(),
        );
    };
    match capture(&exe, &["-NoProfile", "-Version"]) {
        Ok(out) if out.success() => (
            Outcome::Pass,
            format!("{} ({})", out.first_line(), display_path(&exe)),
        ),
        Ok(out) => (Outcome::Fail, format!("pwsh failed: {}", out.first_line())),
        Err(e) => (Outcome::Fail, format!("{e:#}")),
    }
}

/// `cargo deny` is found by cargo in `$CARGO_HOME/bin` even when that is not on `PATH`.
fn cargo_deny_executable() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "cargo-deny.exe"
    } else {
        "cargo-deny"
    };
    find_on_path("cargo-deny").or_else(|| {
        let home = std::env::var_os("CARGO_HOME")?;
        let exe = Path::new(&home).join("bin").join(name);
        exe.is_file().then_some(exe)
    })
}

fn check_cargo_deny() -> (Outcome, String) {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    match capture(&cargo, &["deny", "--version"]) {
        Ok(out) if out.success() => (Outcome::Pass, out.first_line()),
        _ => (
            Outcome::Fail,
            "cargo-deny missing (cargo install cargo-deny --locked)".into(),
        ),
    }
}

// ---------------------------------------------------------------------------------------------
// Tool inventory (paths and versions recorded for later workers)
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
struct Tool {
    name: &'static str,
    path: Option<PathBuf>,
    version: String,
}

fn tool(name: &'static str, path: Option<PathBuf>, version_args: &[&str]) -> Tool {
    let version = path
        .as_ref()
        .and_then(|p| capture(p, version_args).ok())
        .map(|o| o.first_line())
        .unwrap_or_default();
    Tool {
        name,
        path,
        version,
    }
}

fn tool_inventory(sdk: Option<&Path>) -> Vec<Tool> {
    let existing = |p: PathBuf| p.is_file().then_some(p);
    let mut tools = vec![
        tool("cargo", find_on_path("cargo"), &["--version"]),
        tool("rustc", find_on_path("rustc"), &["--version"]),
        tool(
            "java",
            Some(java_executable()).and_then(existing),
            &["-version"],
        ),
        tool("git", find_on_path("git"), &["--version"]),
        tool("tailscale", tailscale_executable(), &["version"]),
        tool("ffmpeg", find_on_path("ffmpeg"), &["-version"]),
        tool("pwsh", pwsh_executable(), &["-NoProfile", "-Version"]),
        tool("cargo-deny", cargo_deny_executable(), &["--version"]),
    ];
    if let Some(sdk) = sdk {
        tools.push(tool(
            "sdkmanager",
            existing(sdkmanager_path(sdk)),
            &["--version"],
        ));
        tools.push(Tool {
            name: "avdmanager",
            path: existing(avdmanager_path(sdk)),
            version: String::new(),
        });
        tools.push(tool(
            "emulator",
            existing(emulator_path(sdk)),
            &["-version"],
        ));
        tools.push(tool("adb", existing(adb_path(sdk)), &["--version"]));
    }
    tools
}

// ---------------------------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------------------------

fn escape_cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

fn render_report(checks: &[Check], tools: &[Tool]) -> String {
    let hard_total = checks.iter().filter(|c| c.level == Level::Hard).count();
    let hard_failed = checks.iter().filter(|c| c.is_hard_failure()).count();
    let mut out = String::new();
    let _ = writeln!(out, "# Preflight report\n");
    let _ = writeln!(out, "- Generated (UTC): {}", utc_timestamp());
    let _ = writeln!(out, "- Command: `cargo xtask preflight --report`");
    let _ = writeln!(
        out,
        "- Result: **{}** ({} of {} hard checks passed)\n",
        if hard_failed == 0 { "PASS" } else { "FAIL" },
        hard_total - hard_failed,
        hard_total
    );
    let _ = writeln!(out, "## Checks (master plan section 1b)\n");
    let _ = writeln!(out, "| Check | Level | Expected | Result | Detail |");
    let _ = writeln!(out, "| --- | --- | --- | --- | --- |");
    for c in checks {
        let level = if c.level == Level::Hard {
            "hard"
        } else {
            "optional"
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            c.id,
            level,
            escape_cell(c.expected),
            c.label(),
            escape_cell(&c.detail)
        );
    }
    let _ = writeln!(out, "\n## Tool paths and versions\n");
    let _ = writeln!(out, "| Tool | Path | Version |");
    let _ = writeln!(out, "| --- | --- | --- |");
    for t in tools {
        let path = t
            .path
            .as_deref()
            .map_or_else(|| "(not found)".to_string(), display_path);
        let _ = writeln!(
            out,
            "| {} | `{}` | {} |",
            t.name,
            escape_cell(&path),
            escape_cell(&t.version)
        );
    }
    out
}

/// `YYYY-MM-DDTHH:MM:SSZ` from the system clock, without a date-time dependency.
fn utc_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format_utc(secs)
}

fn format_utc(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join(" / ")
}

// ---------------------------------------------------------------------------------------------
// Parsers (unit tested)
// ---------------------------------------------------------------------------------------------

/// Parse `cargo 1.96.1 (356927216 2026-06-26)` into `(1, 96, 1)`.
fn parse_cargo_version(line: &str) -> Option<(u32, u32, u32)> {
    let version = line.split_whitespace().nth(1)?;
    let mut parts = version.split(['.', '-']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

/// Major version from the output of `java -version` (`"17.0.21"`, `"21"`, legacy `"1.8.0_402"`).
fn parse_java_major(output: &str) -> Option<u32> {
    let start = output.find("version \"")? + "version \"".len();
    let version = output[start..].split('"').next()?;
    let mut parts = version.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

/// `(BackendState, Version)` from `tailscale status --json`.
fn parse_tailscale_state(json: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let state = value.get("BackendState")?.as_str()?.to_string();
    let version = value
        .get("Version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    Some((state, version))
}

/// `(serial, state)` pairs from `adb devices`.
pub(crate) fn parse_adb_devices(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .skip_while(|l| !l.starts_with("List of devices"))
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some((parts.next()?.to_string(), parts.next()?.to_string()))
        })
        .collect()
}

/// AVD names from `emulator -list-avds`, ignoring `INFO |` style log lines.
pub(crate) fn parse_avd_list(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains(char::is_whitespace))
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Platform specifics
// ---------------------------------------------------------------------------------------------

#[cfg(windows)]
mod platform {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

    /// `(major, minor, build)` of the running Windows, from `RtlGetVersion` (not shimmed).
    pub fn windows_version() -> Option<(u32, u32, u32)> {
        // SAFETY: OSVERSIONINFOW is plain old data, so the all-zero pattern is valid.
        let mut info: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
        info.dwOSVersionInfoSize = u32::try_from(size_of::<OSVERSIONINFOW>()).ok()?;
        // SAFETY: `info` is a valid, writable OSVERSIONINFOW with its size field set.
        let status = unsafe { RtlGetVersion(&raw mut info) };
        (status == 0).then_some((info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber))
    }

    /// Bytes available to the current user on the volume that holds `path`.
    pub fn free_disk_bytes(path: &Path) -> Option<u64> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut available: u64 = 0;
        // SAFETY: `wide` is NUL-terminated and outlives the call; the two optional out
        // parameters may be null and `available` is a valid writable u64.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &raw mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(available)
    }
}

#[cfg(not(windows))]
mod platform {
    use std::path::Path;

    pub fn windows_version() -> Option<(u32, u32, u32)> {
        None
    }

    pub fn free_disk_bytes(_path: &Path) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_version_parses() {
        assert_eq!(
            parse_cargo_version("cargo 1.96.1 (356927216 2026-06-26)"),
            Some((1, 96, 1))
        );
        assert_eq!(
            parse_cargo_version("cargo 1.90.0-nightly (abc 2025-01-01)"),
            Some((1, 90, 0))
        );
        assert_eq!(parse_cargo_version("garbage"), None);
    }

    #[test]
    fn cargo_minimum_is_enforced() {
        assert!((1, 89) < MIN_CARGO);
        assert!((1, 90) >= MIN_CARGO);
    }

    #[test]
    fn java_major_parses_modern_and_legacy() {
        assert_eq!(
            parse_java_major("openjdk version \"17.0.21.0.2\" 2026-10-22 LTS"),
            Some(17)
        );
        assert_eq!(
            parse_java_major("openjdk version \"21\" 2023-09-19"),
            Some(21)
        );
        assert_eq!(parse_java_major("java version \"1.8.0_402\""), Some(8));
        assert_eq!(
            parse_java_major("openjdk version \"25-ea\" 2025-09-16"),
            Some(25)
        );
        assert_eq!(parse_java_major("no version here"), None);
    }

    #[test]
    fn tailscale_state_parses() {
        let json = r#"{"Version":"1.104.4","BackendState":"Running","Self":{}}"#;
        assert_eq!(
            parse_tailscale_state(json),
            Some(("Running".into(), "1.104.4".into()))
        );
        assert_eq!(
            parse_tailscale_state(r#"{"BackendState":"Stopped"}"#),
            Some(("Stopped".into(), "unknown".into()))
        );
        assert_eq!(parse_tailscale_state("Tailscale is stopped."), None);
    }

    #[test]
    fn adb_devices_parse() {
        let out = "List of devices attached\nemulator-5554\tdevice\nR58M123\tunauthorized\n\n";
        assert_eq!(
            parse_adb_devices(out),
            vec![
                ("emulator-5554".to_string(), "device".to_string()),
                ("R58M123".to_string(), "unauthorized".to_string())
            ]
        );
        assert!(parse_adb_devices("List of devices attached\n\n").is_empty());
        assert!(parse_adb_devices("* daemon not running\n").is_empty());
    }

    #[test]
    fn avd_list_ignores_log_lines() {
        let out = "INFO    | Storing crashdata in: C:/tmp\ngander\nPixel_7_API_35\n";
        assert_eq!(
            parse_avd_list(out),
            vec!["gander".to_string(), "Pixel_7_API_35".to_string()]
        );
    }

    #[test]
    fn utc_formatting_matches_known_instants() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(format_utc(1_782_864_000), "2026-07-01T00:00:00Z");
    }

    #[test]
    fn report_table_cells_are_escaped() {
        assert_eq!(escape_cell("a | b\nc"), "a \\| b c");
    }

    #[test]
    fn hard_failure_only_counts_hard_checks() {
        let hard = Check {
            id: "x",
            level: Level::Hard,
            expected: "",
            outcome: Outcome::Fail,
            detail: String::new(),
        };
        let soft = Check {
            id: "y",
            level: Level::Optional,
            expected: "",
            outcome: Outcome::Fail,
            detail: String::new(),
        };
        let skipped = Check {
            id: "z",
            level: Level::Hard,
            expected: "",
            outcome: Outcome::Skipped,
            detail: String::new(),
        };
        assert!(hard.is_hard_failure());
        assert!(!soft.is_hard_failure());
        assert!(!skipped.is_hard_failure());
        assert_eq!(soft.label(), "WARN");
    }
}
