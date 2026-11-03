// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask e2e-host` (WP-1B): the host integration harness.
//!
//! 1. Builds `gander-host` with `--features test-hooks`, `gander-render` and the suite
//!    (`gander-e2e`), and copies the host and the render worker to `target/e2e/bin/` (the host
//!    finds the worker beside its own executable).
//! 2. Generates a fixture tree in a temp directory (images, a 3-page PDF, UTF-16 text, nested
//!    and unicode names, hidden files, a 24 MiB file, a directory with a deny ACE) and a
//!    `config.toml` with three shares: `shr_e2e` (full), `shr_limited` (list only) and
//!    `shr_nogrant` (no grant).
//! 3. Starts the host on 127.0.0.1 port 0 with `--test-approve`, `--test-grant` and
//!    `--test-stream-app <gander-testwin>` (Stream mode launches only the test window), waits for
//!    its ready lines (`listening on`, `host id`, `TEST HOOK ENABLED`).
//! 4. Runs the suite with the host id as the TLS pin; the suite's stdout is the host's stdin
//!    (the test host's `revoke <device>` command). Host output goes to `target/e2e/host.log`.
//! 5. Kills the host (always, also on failure; on Windows a kill-on-close job object also covers
//!    a crash of this process), removes the temp tree and checks `target/e2e/report.json`.
//!
//! The fixture names below must match `host/gander-e2e/src/suite.rs`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};

use crate::proc::{display_path, workspace_root};

/// How long the host may take to print its ready lines.
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the whole suite may run.
const SUITE_TIMEOUT: Duration = Duration::from_secs(600);

const UTF16_TEXT: &str = "Gander UTF-16 \u{e9}t\u{e9} \u{65e5}\u{672c} line 1\r\nline 2\r\n";
const UNICODE_DIR: &str = "\u{dc}n\u{ef}c\u{f6}d\u{e9} \u{65e5}\u{672c}\u{8a9e}";
const EMOJI_FILE: &str = "emoji \u{1f600} file.txt";
const BIG_LEN: usize = 24 * 1024 * 1024;

pub fn run(args: &[String]) -> Result<ExitCode> {
    let mut skip_build = false;
    for a in args {
        match a.as_str() {
            "--no-build" => skip_build = true,
            other => bail!("unknown option {other} (e2e-host [--no-build])"),
        }
    }
    let root = workspace_root();
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|t| if t.is_absolute() { t } else { root.join(t) })
        .unwrap_or_else(|| root.join("target"));
    let out = target.join("e2e");
    std::fs::create_dir_all(&out)?;
    let report = out.join("report.json");
    let _ = std::fs::remove_file(&report);

    if !skip_build {
        build(&root)?;
    }
    let bin = install(&target.join("debug"), &out.join("bin"))?;

    let work = TempTree::new()?;
    let fixture = Fixture::create(work.path())?;
    let config_dir = work.path().join("config");
    let data_dir = work.path().join("data");
    std::fs::create_dir_all(&config_dir)?;
    std::fs::create_dir_all(&data_dir)?;
    std::fs::write(config_dir.join("config.toml"), fixture.config_toml())?;

    let job = job::Job::new();
    let log_path = out.join("host.log");
    let mut host = HostProcess::start(
        &bin.host,
        &config_dir,
        &data_dir,
        &log_path,
        &job,
        &["shr_e2e=list,preview,fetch,stream", "shr_limited=list"],
        Some(&bin.testwin),
    )?;
    let ready = host.wait_ready(READY_TIMEOUT);
    let (addr, host_id) = match ready {
        Ok(r) => r,
        Err(e) => {
            host.kill();
            bail!("{e:#} (host log: {})", display_path(&log_path));
        }
    };
    eprintln!("host ready on {addr}, pinned host id {host_id}");

    let mut cmd = Command::new(&bin.suite);
    cmd.arg("--addr")
        .arg(&addr)
        .arg("--host-id")
        .arg(&host_id)
        .arg("--share-root")
        .arg(&fixture.share)
        .arg("--openapi")
        .arg(root.join("protocol").join("openapi.yaml"))
        .arg("--report")
        .arg(&report)
        .arg("--host-commands")
        .stdin(Stdio::null())
        .stderr(Stdio::inherit());
    if fixture.locked {
        cmd.arg("--locked");
    }
    match host.child.stdin.take() {
        Some(stdin) => {
            cmd.stdout(Stdio::from(stdin));
        }
        None => {
            cmd.stdout(Stdio::null());
        }
    }
    let suite_status = cmd
        .spawn()
        .context("start gander-e2e")
        .and_then(|mut child| {
            job.assign(&child);
            wait_timeout(&mut child, SUITE_TIMEOUT)
        });
    host.kill();
    drop(fixture);
    drop(work);

    let suite_code = suite_status?;
    let text = std::fs::read_to_string(&report)
        .with_context(|| format!("the suite wrote no report at {}", display_path(&report)))?;
    let value: serde_json::Value = serde_json::from_str(&text).context("report.json")?;
    let summary = &value["summary"];
    println!("report: {}", display_path(&report));
    println!(
        "cases: {} passed, {} failed, {} not_reachable (of {})",
        summary["passed"], summary["failed"], summary["not_reachable"], summary["total"]
    );
    println!(
        "error codes: {} covered, {} not_reachable, missing {}",
        summary["error_codes"]["covered"],
        summary["error_codes"]["not_reachable"],
        summary["error_codes"]["missing"]
    );
    if suite_code == Some(0) && summary["ok"] == true {
        println!("e2e-host: OK");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("e2e-host: FAILED (suite exit {suite_code:?})");
        Ok(ExitCode::from(1))
    }
}

/// `cargo build` of the three binaries.
pub(crate) fn build(root: &Path) -> Result<()> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(root)
        .args([
            "build",
            "-p",
            "gander-host",
            "--features",
            "gander-host/test-hooks",
            "-p",
            "gander-render",
            "-p",
            "gander-e2e",
            "-p",
            "gander-testwin",
        ])
        .status()
        .context("run cargo build")?;
    if !status.success() {
        bail!("cargo build failed");
    }
    Ok(())
}

pub(crate) struct Bins {
    pub(crate) host: PathBuf,
    /// `gander-e2e` (the suite, and `gander-e2e pair`).
    pub(crate) suite: PathBuf,
    /// `gander-testwin`, the only app the test host may launch for Stream mode.
    pub(crate) testwin: PathBuf,
}

/// Copies the host and its render worker into `bin` (a test-hook host never runs from
/// `target/debug`, where a later normal build replaces it).
pub(crate) fn install(debug: &Path, bin: &Path) -> Result<Bins> {
    let exe = std::env::consts::EXE_SUFFIX;
    std::fs::create_dir_all(bin)?;
    for name in ["gander-host", "gander-render", "gander-testwin"] {
        let from = debug.join(format!("{name}{exe}"));
        let to = bin.join(format!("{name}{exe}"));
        std::fs::copy(&from, &to)
            .with_context(|| format!("copy {} (build first?)", display_path(&from)))?;
    }
    Ok(Bins {
        host: bin.join(format!("gander-host{exe}")),
        suite: debug.join(format!("gander-e2e{exe}")),
        testwin: bin.join(format!("gander-testwin{exe}")),
    })
}

pub(crate) fn wait_timeout(child: &mut Child, limit: Duration) -> Result<Option<i32>> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code());
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            bail!("the suite did not finish within {} s", limit.as_secs());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------------- host

pub(crate) struct HostProcess {
    pub(crate) child: Child,
    lines: mpsc::Receiver<String>,
}

impl HostProcess {
    /// Starts the test host on 127.0.0.1, port 0, with one `--test-grant` per entry of `grants`.
    pub(crate) fn start(
        exe: &Path,
        config_dir: &Path,
        data_dir: &Path,
        log: &Path,
        job: &job::Job,
        grants: &[&str],
        stream_app: Option<&Path>,
    ) -> Result<Self> {
        let mut cmd = Command::new(exe);
        cmd.args(["--test-approve", "--port", "0"])
            .arg("--config-dir")
            .arg(config_dir)
            .arg("--data-dir")
            .arg(data_dir)
            .args(grants.iter().flat_map(|g| ["--test-grant", g]));
        if let Some(app) = stream_app {
            cmd.arg("--test-stream-app").arg(app);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("start {}", display_path(exe)))?;
        job.assign(&child);
        let log = std::sync::Arc::new(std::sync::Mutex::new(
            std::fs::File::create(log).context("create host.log")?,
        ));
        let (tx, lines) = mpsc::channel();
        if let Some(out) = child.stdout.take() {
            let log = std::sync::Arc::clone(&log);
            std::thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if let Ok(mut f) = log.lock() {
                        let _ = writeln!(f, "{line}");
                    }
                    let _ = tx.send(line);
                }
            });
        }
        if let Some(err) = child.stderr.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    if let Ok(mut f) = log.lock() {
                        let _ = writeln!(f, "stderr: {line}");
                    }
                }
            });
        }
        Ok(Self { child, lines })
    }

    /// `(addr, host id)` from the ready lines.
    pub(crate) fn wait_ready(&mut self, limit: Duration) -> Result<(String, String)> {
        let deadline = Instant::now() + limit;
        let mut addr = None;
        let mut host_id = None;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(l) => l,
                Err(mpsc::RecvTimeoutError::Timeout) => bail!("the host printed no ready lines"),
                Err(mpsc::RecvTimeoutError::Disconnected) => bail!("the host exited early"),
            };
            if let Some(a) = line.strip_prefix("listening on ") {
                if !a.starts_with("127.0.0.1:") {
                    bail!("the test host bound {a}, expected 127.0.0.1 only");
                }
                addr.get_or_insert_with(|| a.trim().to_owned());
            } else if let Some(id) = line.strip_prefix("host id ") {
                host_id = Some(id.trim().to_owned());
            } else if line.starts_with("TEST HOOK ENABLED") {
                match (addr, host_id) {
                    (Some(a), Some(h)) => return Ok((a, h)),
                    _ => bail!("TEST HOOK ENABLED before the address and host id"),
                }
            }
        }
    }

    /// Waits for a stdout line starting with `prefix` (earlier lines are dropped).
    pub(crate) fn wait_line(&mut self, prefix: &str, limit: Duration) -> Result<String> {
        let deadline = Instant::now() + limit;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(l) if l.starts_with(prefix) => return Ok(l),
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    bail!("the host printed no `{prefix}` line")
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => bail!("the host exited"),
            }
        }
    }

    /// Writes one command line (`revoke <device>`) to the test host's stdin.
    pub(crate) fn command(&mut self, line: &str) -> Result<()> {
        let stdin = self
            .child
            .stdin
            .as_mut()
            .context("the host's stdin is gone")?;
        writeln!(stdin, "{line}")?;
        stdin.flush()?;
        Ok(())
    }

    pub(crate) fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

// ---------------------------------------------------------------------------------- fixture

/// A temp directory removed on drop.
pub(crate) struct TempTree(PathBuf);

impl TempTree {
    pub(crate) fn new() -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("gander-e2e-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p)?;
        Ok(Self(p))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    share: PathBuf,
    limited: PathBuf,
    nogrant: PathBuf,
    /// `locked/` carries a deny ACE.
    locked: bool,
}

impl Fixture {
    fn create(base: &Path) -> Result<Self> {
        let share = base.join("share");
        let limited = base.join("limited");
        let nogrant = base.join("nogrant");
        let w = |rel: &str, bytes: &[u8]| -> Result<()> {
            let p = share.join(rel);
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(&p, bytes).with_context(|| format!("write fixture {rel}"))
        };
        w("images/photo.bmp", &bmp(64, 48))?;
        // Large enough for a three-level tile pyramid (WP-67): 1400 x 1000 -> 350 x 250, 700 x
        // 500, 1400 x 1000 px.
        w("images/large.bmp", &bmp(1400, 1000))?;
        w("images/pixel.png", &png(8, 8))?;
        w("docs/three-pages.pdf", &pdf(3))?;
        let mut utf16 = vec![0xFF, 0xFE];
        for u in UTF16_TEXT.encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        w("docs/utf16.txt", &utf16)?;
        w("docs/notes.txt", b"Gander e2e notes: plain UTF-8 text.\n")?;
        let binary: Vec<u8> = (0..4096u32).map(|i| (i % 7) as u8).collect();
        w("docs/data.bin", &binary)?;
        w("docs/tool.exe", b"MZ not really a program")?;
        w("nested/a/b/c/deep.txt", b"deep")?;
        w(
            &format!("{UNICODE_DIR}/{EMOJI_FILE}"),
            "emoji \u{1f600}".as_bytes(),
        )?;
        w(&format!("{UNICODE_DIR}/e\u{301}.txt"), b"decomposed")?;
        w("private/secret.txt", b"hidden by pattern")?;
        w("watch/existing.txt", b"watched")?;
        // Stream mode: the test host opens `.gtw` with gander-testwin (it never reads the file).
        w("stream/e2e.gtw", b"gander test window fixture")?;
        w(
            "stream/e2e.gtx",
            b"gander test window fixture, extension not approved",
        )?;
        let resume: Vec<u8> = (0..256 * 1024u32)
            .map(|i| (i.wrapping_mul(31) % 251) as u8)
            .collect();
        w("change/resume.bin", &resume)?;
        let big: Vec<u8> = (0..BIG_LEN)
            .map(|i| (i.wrapping_mul(131) % 253) as u8)
            .collect();
        w("big.bin", &big)?;
        write_hidden(&share.join(".hidden.txt"), b"hidden attribute")?;
        w("locked/inside.txt", b"locked")?;
        let locked = deny_listing(&share.join("locked"));
        if !locked {
            eprintln!("note: could not set a deny ACE on locked/; forbidden will be not_reachable");
        }
        std::fs::create_dir_all(&limited)?;
        std::fs::write(limited.join("readme.txt"), b"limited share")?;
        std::fs::create_dir_all(&nogrant)?;
        std::fs::write(nogrant.join("a.txt"), b"no grant")?;
        Ok(Self {
            share,
            limited,
            nogrant,
            locked,
        })
    }

    fn config_toml(&self) -> String {
        let share = |id: &str, name: &str, root: &Path, extra: &str| {
            format!(
                "[[share]]\nid = \"{id}\"\nname = \"{name}\"\nroot = '{}'\n{extra}\n",
                root.display()
            )
        };
        let mut s = String::from("# Generated by cargo xtask e2e-host.\nversion = 1\n\n");
        // A zip limit the 24 MiB big.bin crosses, so the suite can see `413 too_large` for the
        // share root while a small folder still downloads (WP-68).
        s += "[zip]\nmax_mib = 16\nmax_entries = 20000\n\n";
        s += &share(
            "shr_e2e",
            "E2E",
            &self.share,
            "hidden_patterns = [\"**/private\"]\nstream_default = \"allowed\"\n",
        );
        s += &share("shr_limited", "Limited", &self.limited, "");
        s += &share("shr_nogrant", "No grant", &self.nogrant, "");
        s
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.locked {
            allow_listing(&self.share.join("locked"));
        }
    }
}

#[cfg(windows)]
fn write_hidden(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt as _;
    // FILE_ATTRIBUTE_HIDDEN
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .attributes(0x2)
        .open(path)?;
    f.write_all(bytes)?;
    Ok(())
}

#[cfg(not(windows))]
fn write_hidden(path: &Path, bytes: &[u8]) -> Result<()> {
    Ok(std::fs::write(path, bytes)?)
}

/// Denies "list folder" on `dir` to Everyone (icacls ships with Windows).
#[cfg(windows)]
fn deny_listing(dir: &Path) -> bool {
    Command::new("icacls")
        .arg(dir)
        .args(["/deny", "*S-1-1-0:(RD)"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(windows)]
fn allow_listing(dir: &Path) {
    let _ = Command::new("icacls")
        .arg(dir)
        .args(["/remove:d", "*S-1-1-0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn deny_listing(_dir: &Path) -> bool {
    false
}

#[cfg(not(windows))]
fn allow_listing(_dir: &Path) {}

/// A 24-bit bottom-up BMP with a gradient.
pub(crate) fn bmp(w: u32, h: u32) -> Vec<u8> {
    let row = (w * 3).div_ceil(4) * 4;
    let size = 54 + row * h;
    let mut b = Vec::with_capacity(size as usize);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&size.to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(w as i32).to_le_bytes());
    b.extend_from_slice(&(h as i32).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&(row * h).to_le_bytes());
    b.extend_from_slice(&[0; 16]);
    for y in 0..h {
        for x in 0..w {
            b.extend_from_slice(&[(x * 4) as u8, (y * 5) as u8, 200]);
        }
        b.resize(b.len() + (row - w * 3) as usize, 0);
    }
    b
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// An RGB PNG with one stored (uncompressed) deflate block.
pub(crate) fn png(w: u32, h: u32) -> Vec<u8> {
    let mut raw = Vec::new();
    for y in 0..h {
        raw.push(0); // filter: none
        for x in 0..w {
            raw.extend_from_slice(&[(x * 30) as u8, (y * 30) as u8, 120]);
        }
    }
    let mut z = vec![0x78, 0x01, 0x01];
    let len = raw.len() as u16;
    z.extend_from_slice(&len.to_le_bytes());
    z.extend_from_slice(&(!len).to_le_bytes());
    z.extend_from_slice(&raw);
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let chunk = |out: &mut Vec<u8>, kind: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A minimal PDF with `pages` A4 pages, each showing its number.
pub(crate) fn pdf(pages: usize) -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    // 1: catalog, 2: pages, 3: font, then (page, content) pairs.
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 4 + i * 2)).collect();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_owned());
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {pages} >>",
        kids.join(" ")
    ));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned());
    for i in 0..pages {
        let content = format!("BT /F1 48 Tf 72 700 Td (Page {}) Tj ET", i + 1);
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            5 + i * 2
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

// ---------------------------------------------------------------------------------- job object

#[cfg(windows)]
pub(crate) mod job {
    use std::os::windows::io::AsRawHandle as _;
    use std::process::Child;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    /// A job object that kills its processes when this process ends (crash, Ctrl+C included).
    pub struct Job(HANDLE);

    impl Job {
        pub fn new() -> Self {
            // SAFETY: both pointers may be null (default security, unnamed job).
            let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if h.is_null() {
                return Self(h);
            }
            // SAFETY: all-zero is a valid JOBOBJECT_EXTENDED_LIMIT_INFORMATION (plain data).
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `h` is a job handle we own; `info` lives across the call and the size
            // matches the information class.
            unsafe {
                SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
            }
            Self(h)
        }

        /// Best effort: without the job the explicit kill still runs.
        pub fn assign(&self, child: &Child) {
            if self.0.is_null() {
                return;
            }
            // SAFETY: both handles are valid for the duration of the call.
            unsafe {
                AssignProcessToJobObject(self.0, child.as_raw_handle());
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: we own the handle; closing it kills processes still in the job.
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }
}

#[cfg(not(windows))]
pub(crate) mod job {
    use std::process::Child;

    pub struct Job;

    impl Job {
        pub fn new() -> Self {
            Self
        }

        pub fn assign(&self, _child: &Child) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_checksums() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        let p = png(2, 2);
        assert!(p.starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn pdf_has_pages_and_a_valid_xref_offset() {
        let p = pdf(3);
        let text = String::from_utf8_lossy(&p);
        assert!(text.contains("/Count 3"));
        let start: usize = text
            .rsplit("startxref\n")
            .next()
            .and_then(|t| t.lines().next())
            .and_then(|n| n.parse().ok())
            .unwrap();
        assert!(text[start..].starts_with("xref"));
    }

    #[test]
    fn bmp_size_matches_header() {
        let b = bmp(5, 3);
        let size = u32::from_le_bytes([b[2], b[3], b[4], b[5]]) as usize;
        assert_eq!(size, b.len());
    }
}
