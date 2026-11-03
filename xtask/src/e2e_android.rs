// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask e2e-android` (WP-29, gate G3): the Android client against the real host on the
//! `gander` emulator.
//!
//! 1. Builds the test-hooks host, the render worker and `gander-e2e` (as `e2e-host` does), and the
//!    debug and androidTest APKs (`gradlew --no-daemon assembleDebug assembleDebugAndroidTest`).
//! 2. Without the `gander` AVD: runs the JVM unit suite only and reports
//!    `deferred: needs emulator` (exit 0 when the unit suite passes).
//! 3. Generates a fixture share (images, a PDF, a text file, a 4 s H.264 MP4 made with ffmpeg, a
//!    50 MiB file), starts the host on 127.0.0.1 port 0 with `--test-approve` and a full grant on
//!    it, and pairs two devices from this machine with `gander-e2e pair`: the fixture device and
//!    a device that is revoked mid-suite. Their tokens go to the instrumentation only; every file
//!    this task writes is redacted.
//! 4. Boots the AVD headless on console port 5584 (serial `emulator-5584`), waits for
//!    `sys.boot_completed`, installs both APKs.
//! 5. Runs `am instrument -r` for the whole androidTest suite (minus the after-revoke case, the QR
//!    case and `StreamViewerFixtureTest`, which `cargo xtask e2e-stream --android` runs), then
//!    writes `revoke <device>` to the host's stdin, waits for `revoked`, and runs
//!    `HostSessionFixtureTest#aRevokedTokenIsReportedAsTokenRevoked`.
//! 6. Writes `TEST-<class>.xml` (JUnit), `summary.json`, `summary.txt`, the raw instrumentation
//!    output, logcat and the host log to `target/e2e/android/`.
//! 7. Always kills the emulator (`adb emu kill`, then the process tree) and the host, and checks
//!    that no emulator or qemu process is left.
//!
//! Green means: both runs completed, no failure, and no skip (every argument-gated test got its
//! arguments, so a skip is unexpected). The one exception is [`DEFERRED`]: a skip in a class listed
//! there is reported by name as a deferral and does not fail the gate; a failure in it still does.

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};

use crate::e2e::{self, HostProcess, TempTree, job};
use crate::preflight;
use crate::proc::{display_path, find_on_path, workspace_root};

const AVD: &str = "gander";
const CONSOLE_PORT: u16 = 5584;
const APP_ID: &str = "fyi.arlo.gander";
const RUNNER: &str = "fyi.arlo.gander.test/androidx.test.runner.AndroidJUnitRunner";
const SHARE: &str = "shr_android";
const REVOKE_CASE: &str =
    "fyi.arlo.gander.HostSessionFixtureTest#aRevokedTokenIsReportedAsTokenRevoked";
/// Flow A (QR) through the Add host logic: needs a code from an open pairing window, which the
/// host writes on `qr <file>`; run on its own after the main suite (it closes the window).
const QR_CASE: &str =
    "fyi.arlo.gander.PairingInstrumentedTest#qrPairingThroughTheAddHostLogicFinishes";

/// Classes whose skips are a named, known deferral rather than an unexpected skip: their
/// arguments come from a harness that does not exist yet. Exact class names only.
///
/// `StreamViewerFixtureTest` needs a host with Stream mode on the test window and a parked box
/// (WP-57): `cargo xtask e2e-stream --android` runs it, so this suite leaves it out
/// ([`STREAM_CLASS`] is in `notClass`); the entry stays as the note for a `--class` run.
const DEFERRED: &[(&str, &str)] = &[(
    STREAM_CLASS,
    "deferred: run by cargo xtask e2e-stream --android (WP-57)",
)];

/// The Stream viewer's fixture test, run by `cargo xtask e2e-stream --android`.
pub(crate) const STREAM_CLASS: &str = "fyi.arlo.gander.StreamViewerFixtureTest";

/// The deferral note for a skipped test in a [`DEFERRED`] class; `None` for anything else.
fn deferral(t: &TestResult) -> Option<&'static str> {
    if t.status != Status::Skipped {
        return None;
    }
    DEFERRED
        .iter()
        .find(|(class, _)| *class == t.class)
        .map(|(_, note)| *note)
}

const BOOT_TIMEOUT: Duration = Duration::from_secs(420);
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const SUITE_TIMEOUT: Duration = Duration::from_secs(40 * 60);
const LARGE_LEN: usize = 50 * 1024 * 1024;

const IMAGES: [&str; 2] = ["images/photo.bmp", "images/pixel.png"];
const PDF: &str = "docs/three-pages.pdf";
const TEXT: &str = "docs/notes.txt";
const VIDEO: &str = "video/clip.mp4";
const LARGE: &str = "large/fifty.bin";
/// The name of LARGE's byte pattern (see `write_fixture`); change it when the pattern changes.
const LARGE_PATTERN: &str = "e2e-android-v1";

struct Options {
    build: bool,
    address: String,
    adb_reverse: bool,
    /// `--class`: run only these classes or methods (debugging); the revoke step is skipped and
    /// the run is never reported green.
    only: Option<String>,
}

fn parse(args: &[String]) -> Result<Options> {
    let mut o = Options {
        build: true,
        address: "10.0.2.2".to_owned(),
        adb_reverse: false,
        only: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--no-build" => o.build = false,
            "--class" => o.only = Some(it.next().context("--class needs a value")?.clone()),
            "--adb-reverse" => {
                o.adb_reverse = true;
                o.address = "127.0.0.1".to_owned();
            }
            "--address" => o.address = it.next().context("--address needs a value")?.clone(),
            other => bail!(
                "unknown option {other} (e2e-android [--no-build] [--address <a>] [--adb-reverse] [--class <c>])"
            ),
        }
    }
    Ok(o)
}

pub fn run(args: &[String]) -> Result<ExitCode> {
    let opts = parse(args)?;
    let root = workspace_root();
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|t| if t.is_absolute() { t } else { root.join(t) })
        .unwrap_or_else(|| root.join("target"));
    let out = target.join("e2e").join("android");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out)?;

    let sdk =
        preflight::android_sdk_root().context("ANDROID_HOME (or ANDROID_SDK_ROOT) is not set")?;
    let adb = preflight::adb_path(&sdk);
    let emulator = preflight::emulator_path(&sdk);
    let avd_home = preflight::avd_home();
    let gradlew =
        crate::testall::gradle_wrapper(&root).context("no client/android Gradle project")?;

    if !has_avd(&emulator, avd_home.as_deref()) {
        return deferred(&root, &gradlew, &out);
    }

    if opts.build {
        e2e::build(&root)?;
        gradle(
            &root,
            &gradlew,
            &["assembleDebug", "assembleDebugAndroidTest"],
        )?;
    }
    let bin = e2e::install(&target.join("debug"), &target.join("e2e").join("bin"))?;
    let apk_dir = root.join("client/android/app/build/outputs/apk");
    let app_apk = apk_dir.join("debug/app-debug.apk");
    let test_apk = apk_dir.join("androidTest/debug/app-debug-androidTest.apk");
    for apk in [&app_apk, &test_apk] {
        if !apk.is_file() {
            bail!("{} is missing (run without --no-build)", display_path(apk));
        }
    }

    // Host and fixture.
    let work = TempTree::new()?;
    let share = work.path().join("share");
    let video = write_fixture(&share)?;
    let config_dir = work.path().join("config");
    let data_dir = work.path().join("data");
    std::fs::create_dir_all(&config_dir)?;
    std::fs::create_dir_all(&data_dir)?;
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            "# Generated by cargo xtask e2e-android.\nversion = 1\n\n[[share]]\nid = \"{SHARE}\"\nname = \"Android E2E\"\nroot = '{}'\n",
            share.display()
        ),
    )?;
    let job = job::Job::new();
    let host_log = out.join("host.log");
    let grant = format!("{SHARE}=list,preview,fetch,stream");
    let mut host = HostProcess::start(
        &bin.host,
        &config_dir,
        &data_dir,
        &host_log,
        &job,
        &[&grant],
        None,
    )?;
    let (addr, host_id) = match host.wait_ready(READY_TIMEOUT) {
        Ok(r) => r,
        Err(e) => bail!("{e:#} (host log: {})", display_path(&host_log)),
    };
    let port = addr.rsplit(':').next().unwrap_or_default().to_owned();
    eprintln!("host ready on {addr}, host id {host_id}");

    let fixture_dev = pair(
        &bin.suite,
        &addr,
        &host_id,
        "android fixture",
        &work.path().join("fixture.json"),
    )?;
    let revoke_dev = pair(
        &bin.suite,
        &addr,
        &host_id,
        "android revoke",
        &work.path().join("revoke.json"),
    )?;
    let secrets = vec![fixture_dev.token.clone(), revoke_dev.token.clone()];

    // Emulator.
    // adb starts a server on first use; stop it afterwards unless it was already running.
    let adb_was_running = adb_server_running();
    let mut emu = Emulator::boot(&emulator, &adb, avd_home.as_deref(), &out, &job)?;
    let outcome = (|| -> Result<Outcome> {
        emu.prepare(&[&app_apk, &test_apk])?;
        if opts.adb_reverse {
            emu.adb(&["reverse", &format!("tcp:{port}"), &format!("tcp:{port}")])?;
        }
        let mut args: Vec<(String, String)> = vec![
            ("gander.host.port".into(), port.clone()),
            ("gander.host.id".into(), host_id.clone()),
            ("gander.host.address".into(), opts.address.clone()),
            ("gander.host.addresses".into(), opts.address.clone()),
            ("gander.host.token".into(), fixture_dev.token.clone()),
            ("gander.revoke.token".into(), revoke_dev.token.clone()),
            ("gander.fixture.share".into(), SHARE.into()),
            ("gander.fixture.images".into(), IMAGES.join(",")),
            ("gander.fixture.pdf".into(), PDF.into()),
            ("gander.fixture.text".into(), TEXT.into()),
            ("gander.fixture.large".into(), LARGE.into()),
            // How `write_fixture` generates LARGE, so `FilesAppProviderFixtureTest` (WP-60) can
            // check every byte it reads through the Files-app provider.
            ("gander.fixture.large_pattern".into(), LARGE_PATTERN.into()),
            ("gander.fixture.list".into(), "images".into()),
            (
                "gander.fixture.list_expect".into(),
                "photo.bmp,pixel.png".into(),
            ),
        ];
        if video {
            args.push(("gander.fixture.video".into(), VIDEO.into()));
        }

        eprintln!("==> instrumented suite");
        let mut main_args = args.clone();
        match &opts.only {
            Some(only) => main_args.push(("class".into(), only.clone())),
            None => main_args.push((
                "notClass".into(),
                format!("{REVOKE_CASE},{QR_CASE},{STREAM_CLASS}"),
            )),
        }
        let main = emu.instrument(&main_args, &out.join("instrument-main.txt"), &secrets)?;
        if opts.only.is_some() {
            if let Ok(log) = emu.adb_capture(&["logcat", "-d", "-v", "threadtime"]) {
                std::fs::write(out.join("logcat.txt"), redact(&log, &secrets))?;
            }
            return Ok(Outcome {
                runs: vec![main],
                video,
                boot: emu.boot_time,
                image: emu.image.clone(),
            });
        }

        eprintln!("==> flow A (QR code from the host)");
        let qr_file = work.path().join("qr.txt");
        host.command(&format!("qr {}", qr_file.display()))?;
        host.wait_line("qr written", Duration::from_secs(15))?;
        let qr = std::fs::read_to_string(&qr_file).context("read the QR code the host wrote")?;
        let _ = std::fs::remove_file(&qr_file);
        // The secret is the part to hide; adbd itself logs the `am instrument` command line.
        let mut qr_secrets = secrets.clone();
        qr_secrets.extend(
            qr.trim()
                .split('&')
                .find_map(|p| p.strip_prefix("s="))
                .map(str::to_owned),
        );
        let mut qr_args = args.clone();
        // `adb shell` hands the words to the device's shell: quote the `?` and `&` of the URI.
        qr_args.push(("gander.pair.qr".into(), format!("'{}'", qr.trim())));
        qr_args.push(("class".into(), QR_CASE.into()));
        let qr_run = emu.instrument(&qr_args, &out.join("instrument-qr.txt"), &qr_secrets)?;

        eprintln!("==> revoke {} on the host", revoke_dev.device_id);
        host.command(&format!("revoke {}", revoke_dev.device_id))?;
        host.wait_line("revoked", Duration::from_secs(15))?;

        eprintln!("==> after the revoke");
        let mut after_args = args;
        after_args.push(("gander.revoke.done".into(), "true".into()));
        after_args.push(("class".into(), REVOKE_CASE.into()));
        let after = emu.instrument(&after_args, &out.join("instrument-revoke.txt"), &secrets)?;

        if let Ok(log) = emu.adb_capture(&["logcat", "-d", "-v", "threadtime"]) {
            std::fs::write(out.join("logcat.txt"), redact(&log, &qr_secrets))?;
        }
        Ok(Outcome {
            runs: vec![main, qr_run, after],
            video,
            boot: emu.boot_time,
            image: emu.image.clone(),
        })
    })();
    let dead = emu.shutdown();
    if !adb_was_running {
        let _ = Command::new(&adb)
            .arg("kill-server")
            .stdin(Stdio::null())
            .output();
    }
    host.kill();
    drop(work);
    redact_file(&host_log, &secrets);

    let outcome = outcome?;
    let ok = report(&outcome, &out, dead)?;
    if opts.only.is_some() {
        println!(
            "e2e-android: PARTIAL (--class run; the revoke step was skipped, not a gate result)"
        );
        return Ok(ExitCode::from(1));
    }
    if ok && dead {
        println!("e2e-android: OK");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("e2e-android: FAILED");
        Ok(ExitCode::from(1))
    }
}

// ---------------------------------------------------------------------------------- deferral

pub(crate) fn has_avd(emulator: &Path, avd_home: Option<&Path>) -> bool {
    if !emulator.is_file() {
        return false;
    }
    let mut c = Command::new(emulator);
    c.arg("-list-avds");
    if let Some(h) = avd_home {
        c.env("ANDROID_AVD_HOME", h);
    }
    c.output().is_ok_and(|o| {
        preflight::parse_avd_list(&String::from_utf8_lossy(&o.stdout))
            .iter()
            .any(|a| a == AVD)
    })
}

fn deferred(root: &Path, gradlew: &Path, out: &Path) -> Result<ExitCode> {
    println!("no `{AVD}` AVD: G3 deferred: needs emulator; running the JVM unit suite only");
    let unit = gradle(root, gradlew, &["testDebugUnitTest"]);
    let summary = serde_json::json!({
        "status": "deferred: needs emulator",
        "unit_tests": if unit.is_ok() { "passed" } else { "failed" },
        "fix": "sdkmanager \"system-images;android-36;google_apis;x86_64\" && avdmanager create avd -n gander -k \"system-images;android-36;google_apis;x86_64\"",
    });
    std::fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!(
        "e2e-android: deferred: needs emulator (unit suite {})",
        summary["unit_tests"]
    );
    Ok(if unit.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

pub(crate) fn gradle(root: &Path, gradlew: &Path, tasks: &[&str]) -> Result<()> {
    eprintln!("==> gradlew --no-daemon {}", tasks.join(" "));
    let status = Command::new(gradlew)
        .arg("--no-daemon")
        .args(tasks)
        .current_dir(root.join("client").join("android"))
        .status()
        .context("run gradlew")?;
    if !status.success() {
        bail!("gradlew {} failed", tasks.join(" "));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------- fixture

/// Writes the fixture tree; returns whether the video could be made (ffmpeg on PATH).
fn write_fixture(share: &Path) -> Result<bool> {
    let w = |rel: &str, bytes: &[u8]| -> Result<()> {
        let p = share.join(rel);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, bytes).with_context(|| format!("write fixture {rel}"))
    };
    w(IMAGES[0], &e2e::bmp(64, 48))?;
    w(IMAGES[1], &e2e::png(8, 8))?;
    w(PDF, &e2e::pdf(3))?;
    let text: String = (1..=200)
        .map(|i| format!("Gander e2e-android notes, line {i}.\n"))
        .collect();
    w(TEXT, text.as_bytes())?;

    let large = share.join(LARGE);
    std::fs::create_dir_all(large.parent().context("large has a parent")?)?;
    let mut f = std::io::BufWriter::new(std::fs::File::create(&large)?);
    let chunk: Vec<u8> = (0..1024 * 1024u32)
        .map(|i| (i.wrapping_mul(131) % 253) as u8)
        .collect();
    for i in 0..LARGE_LEN / chunk.len() {
        f.write_all(&chunk)?;
        f.write_all(&(i as u64).to_le_bytes())?; // no two MiB alike
    }
    f.flush()?;

    let Some(ffmpeg) = find_on_path("ffmpeg") else {
        eprintln!("note: ffmpeg is not on PATH; the video case will be skipped (and the run fail)");
        return Ok(false);
    };
    let video = share.join(VIDEO);
    std::fs::create_dir_all(video.parent().context("video has a parent")?)?;
    let status = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
        .args(["-i", "testsrc=duration=4:size=320x240:rate=25"])
        .args([
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ])
        .arg(&video)
        .stdin(Stdio::null())
        .status()
        .context("run ffmpeg")?;
    Ok(status.success() && video.is_file())
}

struct Device {
    device_id: String,
    token: String,
}

fn pair(suite: &Path, addr: &str, host_id: &str, name: &str, file: &Path) -> Result<Device> {
    let status = Command::new(suite)
        .args(["pair", "--addr", addr, "--host-id", host_id, "--name", name])
        .args(["--share", SHARE, "--perms", "list,preview,fetch"])
        .arg("--out")
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .context("run gander-e2e pair")?;
    if !status.success() {
        bail!("gander-e2e pair ({name}) failed");
    }
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(file)?)?;
    let _ = std::fs::remove_file(file);
    Ok(Device {
        device_id: v["device_id"].as_str().context("device_id")?.to_owned(),
        token: v["token"].as_str().context("token")?.to_owned(),
    })
}

pub(crate) fn redact(text: &str, secrets: &[String]) -> String {
    let mut s = text.to_owned();
    for secret in secrets.iter().filter(|x| !x.is_empty()) {
        s = s.replace(secret.as_str(), "<redacted>");
    }
    s
}

pub(crate) fn redact_file(path: &Path, secrets: &[String]) {
    if let Ok(text) = std::fs::read_to_string(path) {
        let _ = std::fs::write(path, redact(&text, secrets));
    }
}

// ---------------------------------------------------------------------------------- emulator

pub(crate) struct Emulator {
    adb: PathBuf,
    serial: String,
    child: Child,
    pub(crate) boot_time: Duration,
    pub(crate) image: String,
}

impl Emulator {
    pub(crate) fn boot(
        emulator: &Path,
        adb: &Path,
        avd_home: Option<&Path>,
        out: &Path,
        job: &job::Job,
    ) -> Result<Self> {
        if let Some(p) = emulator_processes().first() {
            bail!("an emulator is already running ({p}); only one may run");
        }
        let log = std::fs::File::create(out.join("emulator.log"))?;
        let mut cmd = Command::new(emulator);
        cmd.args(["-avd", AVD, "-port", &CONSOLE_PORT.to_string()])
            .args([
                "-no-window",
                "-no-audio",
                "-no-snapshot-save",
                "-no-boot-anim",
            ])
            .args(["-gpu", "swiftshader_indirect"])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        if let Some(h) = avd_home {
            cmd.env("ANDROID_AVD_HOME", h);
        }
        let started = Instant::now();
        let child = cmd.spawn().context("start the emulator")?;
        job.assign(&child);
        let mut emu = Self {
            adb: adb.to_path_buf(),
            serial: format!("emulator-{CONSOLE_PORT}"),
            child,
            boot_time: Duration::ZERO,
            image: String::new(),
        };
        eprintln!("==> booting `{AVD}` as {}", emu.serial);
        loop {
            if let Ok(Some(s)) = emu.child.try_wait() {
                bail!("the emulator exited during boot ({s}); see emulator.log");
            }
            if started.elapsed() > BOOT_TIMEOUT {
                emu.shutdown();
                bail!(
                    "the emulator did not boot within {} s",
                    BOOT_TIMEOUT.as_secs()
                );
            }
            if emu
                .adb_capture(&["shell", "getprop", "sys.boot_completed"])
                .is_ok_and(|v| v.trim() == "1")
            {
                break;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        emu.boot_time = started.elapsed();
        emu.image = emu
            .adb_capture(&["shell", "getprop", "ro.build.fingerprint"])
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        eprintln!("booted in {} s: {}", emu.boot_time.as_secs(), emu.image);
        Ok(emu)
    }

    pub(crate) fn adb_capture(&self, args: &[&str]) -> Result<String> {
        let out = Command::new(&self.adb)
            .args(["-s", &self.serial])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .context("run adb")?;
        if !out.status.success() {
            bail!(
                "adb {} failed: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub(crate) fn adb(&self, args: &[&str]) -> Result<()> {
        self.adb_capture(args).map(|_| ())
    }

    /// Unlocks the screen, turns animations off, installs the APKs and clears logcat.
    pub(crate) fn prepare(&self, apks: &[&Path]) -> Result<()> {
        let _ = self.adb(&["shell", "wm", "dismiss-keyguard"]);
        let _ = self.adb(&["shell", "input", "keyevent", "82"]);
        for setting in [
            "window_animation_scale",
            "transition_animation_scale",
            "animator_duration_scale",
        ] {
            let _ = self.adb(&["shell", "settings", "put", "global", setting, "0"]);
        }
        let _ = self.adb(&["uninstall", APP_ID]);
        for apk in apks {
            eprintln!("installing {}", display_path(apk));
            let path = apk.to_string_lossy();
            self.adb(&["install", "-r", "-t", &path])?;
        }
        let _ = self.adb(&["logcat", "-c"]);
        Ok(())
    }

    /// `am instrument -w -r` with `args`; the raw output (redacted) goes to `raw`.
    pub(crate) fn instrument(
        &self,
        args: &[(String, String)],
        raw: &Path,
        secrets: &[String],
    ) -> Result<Run> {
        let mut cmd = Command::new(&self.adb);
        cmd.args(["-s", &self.serial, "shell", "am", "instrument", "-w", "-r"]);
        for (k, v) in args {
            cmd.args(["-e", k, v]);
        }
        cmd.arg(RUNNER)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().context("run am instrument")?;
        let stdout = child.stdout.take().context("stdout")?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send((Instant::now(), line));
            }
        });
        let started = Instant::now();
        let mut lines = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok((at, line)) => {
                    if line.starts_with("INSTRUMENTATION_STATUS: test=") {
                        eprintln!(
                            "  {}",
                            line.trim_start_matches("INSTRUMENTATION_STATUS: test=")
                        );
                    }
                    lines.push((at, line));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if started.elapsed() > SUITE_TIMEOUT {
                let _ = child.kill();
                bail!(
                    "am instrument did not finish within {} s",
                    SUITE_TIMEOUT.as_secs()
                );
            }
        }
        let _ = child.wait();
        let text: String = lines.iter().map(|(_, l)| format!("{l}\n")).collect();
        std::fs::write(raw, redact(&text, secrets))?;
        let mut run = parse_instrumentation(&lines);
        for t in &mut run.tests {
            if let Some(m) = &mut t.message {
                *m = redact(m, secrets);
            }
        }
        if let Some(e) = &mut run.error {
            *e = redact(e, secrets);
        }
        Ok(run)
    }

    /// Kills the emulator and returns true when no emulator or qemu process is left.
    pub(crate) fn shutdown(&mut self) -> bool {
        eprintln!("==> stopping {}", self.serial);
        let _ = Command::new(&self.adb)
            .args(["-s", &self.serial, "emu", "kill"])
            .stdin(Stdio::null())
            .output();
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) && emulator_processes().is_empty() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        kill_emulator_processes();
        std::thread::sleep(Duration::from_secs(2));
        emulator_processes().is_empty()
    }
}

impl Drop for Emulator {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.shutdown();
        }
    }
}

const EMULATOR_IMAGES: [&str; 3] = [
    "emulator.exe",
    "qemu-system-x86_64.exe",
    "qemu-system-x86_64-headless.exe",
];

/// Running emulator and qemu processes (Windows: `tasklist`; elsewhere `pgrep`).
/// Whether an adb server process is running.
pub(crate) fn adb_server_running() -> bool {
    if cfg!(windows) {
        Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq adb.exe", "/FO", "CSV", "/NH"])
            .output()
            .is_ok_and(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .to_ascii_lowercase()
                    .contains("adb.exe")
            })
    } else {
        Command::new("pgrep")
            .args(["-x", "adb"])
            .output()
            .is_ok_and(|o| o.status.success())
    }
}

pub(crate) fn emulator_processes() -> Vec<String> {
    if cfg!(windows) {
        let Ok(out) = Command::new("tasklist")
            .args(["/FO", "CSV", "/NH"])
            .output()
        else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split(',').next())
            .map(|n| n.trim_matches('"').to_owned())
            .filter(|n| {
                let lower = n.to_ascii_lowercase();
                EMULATOR_IMAGES.contains(&lower.as_str()) || lower.starts_with("qemu-system")
            })
            .collect()
    } else {
        Command::new("pgrep")
            .args(["-l", "qemu-system|emulator"])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn kill_emulator_processes() {
    if cfg!(windows) {
        for image in EMULATOR_IMAGES {
            let _ = Command::new("taskkill")
                .args(["/F", "/T", "/IM", image])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    } else {
        let _ = Command::new("pkill").args(["-f", "qemu-system"]).status();
    }
}

// ---------------------------------------------------------------------------------- results

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug)]
pub(crate) struct TestResult {
    pub(crate) class: String,
    pub(crate) name: String,
    pub(crate) status: Status,
    pub(crate) secs: f64,
    /// Failure stack or skip reason.
    pub(crate) message: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Run {
    pub(crate) tests: Vec<TestResult>,
    /// The run did not complete (`INSTRUMENTATION_FAILED`, a crash, no final code).
    pub(crate) error: Option<String>,
}

struct Outcome {
    runs: Vec<Run>,
    video: bool,
    boot: Duration,
    image: String,
}

/// Parses `am instrument -r` output: `INSTRUMENTATION_STATUS: k=v` blocks (values may run over
/// several lines) closed by `INSTRUMENTATION_STATUS_CODE: n` (1 start, 0 ok, -1 error, -2 failure,
/// -3 ignored, -4 assumption failure), then `INSTRUMENTATION_RESULT`/`INSTRUMENTATION_CODE`.
pub(crate) fn parse_instrumentation(lines: &[(Instant, String)]) -> Run {
    let mut run = Run::default();
    let mut block: BTreeMap<String, String> = BTreeMap::new();
    let mut result: BTreeMap<String, String> = BTreeMap::new();
    let mut last_key: Option<(bool, String)> = None;
    let mut started: Option<(String, String, Instant)> = None;
    let mut final_code = None;
    for (at, line) in lines {
        if let Some(kv) = line.strip_prefix("INSTRUMENTATION_STATUS: ") {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            block.insert(k.to_owned(), v.to_owned());
            last_key = Some((false, k.to_owned()));
        } else if let Some(code) = line.strip_prefix("INSTRUMENTATION_STATUS_CODE: ") {
            let code: i32 = code.trim().parse().unwrap_or(-1);
            let class = block.get("class").cloned().unwrap_or_default();
            let name = block.get("test").cloned().unwrap_or_default();
            if code == 1 {
                started = Some((class, name, *at));
            } else {
                let secs = match &started {
                    Some((c, n, t)) if *c == class && *n == name => {
                        at.duration_since(*t).as_secs_f64()
                    }
                    _ => 0.0,
                };
                started = None;
                let (status, message) = match code {
                    0 => (Status::Passed, None),
                    -3 | -4 => (Status::Skipped, block.get("stack").map(|s| first_line(s))),
                    _ => (Status::Failed, block.get("stack").cloned()),
                };
                if !class.is_empty() {
                    run.tests.push(TestResult {
                        class,
                        name,
                        status,
                        secs,
                        message,
                    });
                }
            }
            block.clear();
            last_key = None;
        } else if let Some(kv) = line.strip_prefix("INSTRUMENTATION_RESULT: ") {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            result.insert(k.to_owned(), v.to_owned());
            last_key = Some((true, k.to_owned()));
        } else if let Some(code) = line.strip_prefix("INSTRUMENTATION_CODE: ") {
            final_code = code.trim().parse::<i32>().ok();
            last_key = None;
        } else if line.starts_with("INSTRUMENTATION_FAILED") {
            run.error = Some(line.clone());
        } else if let Some((is_result, k)) = &last_key {
            let map = if *is_result { &mut result } else { &mut block };
            if let Some(v) = map.get_mut(k) {
                v.push('\n');
                v.push_str(line);
            }
        }
    }
    if let Some((class, name, _)) = started {
        run.tests.push(TestResult {
            class,
            name,
            status: Status::Failed,
            secs: 0.0,
            message: Some("the test started but never finished (process crash?)".to_owned()),
        });
    }
    if run.error.is_none() {
        if let Some(msg) = result.get("shortMsg") {
            run.error = Some(format!("instrumentation: {msg}"));
        } else if final_code.is_none() {
            run.error = Some("am instrument printed no INSTRUMENTATION_CODE".to_owned());
        }
    }
    run
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_owned()
}

fn xml_escape(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .fold(String::new(), |mut out, c| {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                '\'' => out.push_str("&apos;"),
                _ => out.push(c),
            }
            out
        })
}

/// One JUnit `<testsuite>` document for `class`.
fn junit_xml(class: &str, tests: &[&TestResult]) -> String {
    let count = |s: Status| tests.iter().filter(|t| t.status == s).count();
    let time: f64 = tests.iter().map(|t| t.secs).sum();
    let mut x = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"0\" skipped=\"{}\" time=\"{time:.3}\">\n",
        xml_escape(class),
        tests.len(),
        count(Status::Failed),
        count(Status::Skipped)
    );
    for t in tests {
        x += &format!(
            "  <testcase classname=\"{}\" name=\"{}\" time=\"{:.3}\"",
            xml_escape(class),
            xml_escape(&t.name),
            t.secs
        );
        let msg = t.message.as_deref().unwrap_or("");
        match t.status {
            Status::Passed => x += "/>\n",
            Status::Skipped => {
                x += &format!(
                    ">\n    <skipped message=\"{}\"/>\n  </testcase>\n",
                    xml_escape(msg)
                )
            }
            Status::Failed => {
                x += &format!(
                    ">\n    <failure message=\"{}\">{}</failure>\n  </testcase>\n",
                    xml_escape(&first_line(msg)),
                    xml_escape(msg)
                );
            }
        }
    }
    x += "</testsuite>\n";
    x
}

/// Writes the JUnit files and summaries; returns whether the run is green.
fn report(o: &Outcome, out: &Path, emulator_dead: bool) -> Result<bool> {
    let all: Vec<&TestResult> = o.runs.iter().flat_map(|r| &r.tests).collect();
    let mut by_class: BTreeMap<&str, Vec<&TestResult>> = BTreeMap::new();
    for t in &all {
        by_class.entry(t.class.as_str()).or_default().push(t);
    }
    let mut classes = Vec::new();
    let mut text =
        String::from("class                                         passed failed skipped\n");
    for (class, tests) in &by_class {
        std::fs::write(
            out.join(format!("TEST-{class}.xml")),
            junit_xml(class, tests),
        )?;
        let n = |s: Status| tests.iter().filter(|t| t.status == s).count();
        let short = class.rsplit('.').next().unwrap_or(class);
        let note = tests.iter().find_map(|t| deferral(t));
        text += &format!(
            "{short:<45} {:>6} {:>6} {:>7}{}\n",
            n(Status::Passed),
            n(Status::Failed),
            n(Status::Skipped),
            note.map(|d| format!("  {d}")).unwrap_or_default()
        );
        classes.push(serde_json::json!({
            "class": class,
            "passed": n(Status::Passed),
            "failed": n(Status::Failed),
            "skipped": n(Status::Skipped),
            "deferred": note,
        }));
    }
    let n = |s: Status| all.iter().filter(|t| t.status == s).count();
    let deferred: Vec<&TestResult> = all
        .iter()
        .copied()
        .filter(|t| deferral(t).is_some())
        .collect();
    let unexpected_skips = n(Status::Skipped) - deferred.len();
    let problems: Vec<serde_json::Value> = all
        .iter()
        .filter(|t| t.status != Status::Passed && deferral(t).is_none())
        .map(|t| {
            serde_json::json!({
                "test": format!("{}#{}", t.class, t.name),
                "status": if t.status == Status::Failed { "failed" } else { "skipped (unexpected)" },
                "message": t.message.as_deref().map(first_line),
            })
        })
        .collect();
    let run_errors: Vec<&str> = o.runs.iter().filter_map(|r| r.error.as_deref()).collect();
    let revoke_ran = all
        .iter()
        .any(|t| format!("{}#{}", t.class, t.name) == REVOKE_CASE && t.status == Status::Passed);
    let ok = run_errors.is_empty()
        && n(Status::Failed) == 0
        && unexpected_skips == 0
        && !all.is_empty()
        && revoke_ran
        && o.video;
    text += &format!(
        "total: {} passed, {} failed, {} skipped (of {}; {} of the skips deferred)\n",
        n(Status::Passed),
        n(Status::Failed),
        n(Status::Skipped),
        all.len(),
        deferred.len()
    );
    for t in &deferred {
        text += &format!("  {}: {}#{}\n", deferral(t).unwrap_or(""), t.class, t.name);
    }
    for p in &problems {
        text += &format!(
            "  {}: {} {}\n",
            p["status"].as_str().unwrap_or(""),
            p["test"].as_str().unwrap_or(""),
            p["message"].as_str().unwrap_or("")
        );
    }
    for e in &run_errors {
        text += &format!("  run error: {e}\n");
    }
    text += &format!(
        "emulator: {} s boot, {}; stopped: {}\n",
        o.boot.as_secs(),
        o.image,
        if emulator_dead {
            "yes, no emulator/qemu process left"
        } else {
            "NO, a process is left"
        }
    );
    let summary = serde_json::json!({
        "ok": ok,
        "passed": n(Status::Passed),
        "failed": n(Status::Failed),
        "skipped": n(Status::Skipped),
        "deferred": deferred
            .iter()
            .map(|t| serde_json::json!({
                "test": format!("{}#{}", t.class, t.name),
                "status": deferral(t),
            }))
            .collect::<Vec<_>>(),
        "total": all.len(),
        "classes": classes,
        "problems": problems,
        "run_errors": run_errors,
        "revoke_case_passed": revoke_ran,
        "video_fixture": o.video,
        "emulator": {
            "avd": AVD,
            "image": o.image,
            "boot_secs": o.boot.as_secs(),
            "stopped": emulator_dead,
        },
    });
    std::fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    std::fs::write(out.join("summary.txt"), &text)?;
    print!("{text}");
    println!("report: {}", display_path(&out.join("summary.json")));
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<(Instant, String)> {
        let now = Instant::now();
        text.lines().map(|l| (now, l.to_owned())).collect()
    }

    const RAW: &str = "\
INSTRUMENTATION_STATUS: class=fyi.arlo.gander.A
INSTRUMENTATION_STATUS: test=ok
INSTRUMENTATION_STATUS_CODE: 1
INSTRUMENTATION_STATUS: class=fyi.arlo.gander.A
INSTRUMENTATION_STATUS: test=ok
INSTRUMENTATION_STATUS_CODE: 0
INSTRUMENTATION_STATUS: class=fyi.arlo.gander.A
INSTRUMENTATION_STATUS: test=bad
INSTRUMENTATION_STATUS_CODE: 1
INSTRUMENTATION_STATUS: class=fyi.arlo.gander.A
INSTRUMENTATION_STATUS: stack=java.lang.AssertionError: boom
\tat fyi.arlo.gander.A.bad(A.kt:3)
INSTRUMENTATION_STATUS: test=bad
INSTRUMENTATION_STATUS_CODE: -2
INSTRUMENTATION_STATUS: class=fyi.arlo.gander.B
INSTRUMENTATION_STATUS: test=skip
INSTRUMENTATION_STATUS_CODE: 1
INSTRUMENTATION_STATUS: class=fyi.arlo.gander.B
INSTRUMENTATION_STATUS: stack=org.junit.AssumptionViolatedException: needs a host
INSTRUMENTATION_STATUS: test=skip
INSTRUMENTATION_STATUS_CODE: -4
INSTRUMENTATION_RESULT: stream=
Time: 1.2

OK (3 tests)
INSTRUMENTATION_CODE: -1
";

    #[test]
    fn parses_pass_fail_and_assumption_skip() {
        let run = parse_instrumentation(&lines(RAW));
        assert!(run.error.is_none(), "{:?}", run.error);
        let s: Vec<_> = run
            .tests
            .iter()
            .map(|t| (t.name.as_str(), t.status))
            .collect();
        assert_eq!(
            s,
            [
                ("ok", Status::Passed),
                ("bad", Status::Failed),
                ("skip", Status::Skipped)
            ]
        );
        assert!(run.tests[1].message.as_deref().unwrap().contains("A.kt:3"));
        assert_eq!(
            run.tests[2].message.as_deref(),
            Some("org.junit.AssumptionViolatedException: needs a host")
        );
    }

    #[test]
    fn a_crash_is_a_failure_and_a_run_error() {
        let raw = "\
INSTRUMENTATION_STATUS: class=fyi.arlo.gander.A
INSTRUMENTATION_STATUS: test=dies
INSTRUMENTATION_STATUS_CODE: 1
INSTRUMENTATION_RESULT: shortMsg=Process crashed.
INSTRUMENTATION_CODE: 0
";
        let run = parse_instrumentation(&lines(raw));
        assert_eq!(run.tests[0].status, Status::Failed);
        assert_eq!(
            run.error.as_deref(),
            Some("instrumentation: Process crashed.")
        );
        assert!(parse_instrumentation(&lines("")).error.is_some());
    }

    #[test]
    fn junit_counts_and_escapes() {
        let t = [
            TestResult {
                class: "C".into(),
                name: "a<b".into(),
                status: Status::Passed,
                secs: 1.0,
                message: None,
            },
            TestResult {
                class: "C".into(),
                name: "f".into(),
                status: Status::Failed,
                secs: 0.5,
                message: Some("x & y\nat z".into()),
            },
        ];
        let x = junit_xml("C", &t.iter().collect::<Vec<_>>());
        assert!(x.contains("tests=\"2\" failures=\"1\""));
        assert!(x.contains("name=\"a&lt;b\""));
        assert!(x.contains("message=\"x &amp; y\""));
    }

    fn result(class: &str, name: &str, status: Status) -> TestResult {
        TestResult {
            class: class.into(),
            name: name.into(),
            status,
            secs: 0.0,
            message: None,
        }
    }

    /// Runs `report` over `tests` (plus a passing revoke case) and returns (ok, summary.txt).
    fn report_on(mut tests: Vec<TestResult>, tag: &str) -> (bool, String) {
        let (class, name) = REVOKE_CASE.split_once('#').unwrap();
        tests.push(result(class, name, Status::Passed));
        let outcome = Outcome {
            runs: vec![Run { tests, error: None }],
            video: true,
            boot: Duration::from_secs(1),
            image: "test".into(),
        };
        let out =
            std::env::temp_dir().join(format!("gander-xtask-report-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&out).unwrap();
        let ok = report(&outcome, &out, true).unwrap();
        let text = std::fs::read_to_string(out.join("summary.txt")).unwrap();
        let _ = std::fs::remove_dir_all(&out);
        (ok, text)
    }

    #[test]
    fn only_the_named_class_is_a_deferral_and_every_other_skip_fails() {
        const FIXTURE: &str = "fyi.arlo.gander.StreamViewerFixtureTest";
        let deferred_only = vec![
            result("fyi.arlo.gander.A", "ok", Status::Passed),
            result(FIXTURE, "colours", Status::Skipped),
            result(FIXTURE, "box", Status::Skipped),
        ];
        let (ok, text) = report_on(deferred_only, "deferred");
        assert!(ok, "{text}");
        assert!(
            text.contains("deferred: run by cargo xtask e2e-stream --android (WP-57)"),
            "{text}"
        );
        assert!(!text.contains("skipped (unexpected)"), "{text}");

        // Any other class's skip still fails the gate, also next to the deferral.
        let other_skip = vec![
            result(FIXTURE, "colours", Status::Skipped),
            result("fyi.arlo.gander.StreamViewerTest", "x", Status::Skipped),
        ];
        let (ok, text) = report_on(other_skip, "other");
        assert!(!ok, "{text}");
        assert!(
            text.contains("skipped (unexpected): fyi.arlo.gander.StreamViewerTest#x"),
            "{text}"
        );

        // A class whose name only starts with the deferred one is not deferred.
        let lookalike = vec![result(
            "fyi.arlo.gander.StreamViewerFixtureTestTwo",
            "x",
            Status::Skipped,
        )];
        assert!(!report_on(lookalike, "lookalike").0);

        // A failure in the deferred class is still a failure.
        let failed = vec![result(FIXTURE, "colours", Status::Failed)];
        assert!(!report_on(failed, "failed").0);
    }

    #[test]
    fn redact_replaces_every_secret() {
        let s = redact(
            "a TOKEN1 b TOKEN2 TOKEN1",
            &["TOKEN1".into(), "TOKEN2".into(), String::new()],
        );
        assert_eq!(s, "a <redacted> b <redacted> <redacted>");
    }
}
