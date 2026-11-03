// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask mockups`: renders every screen of the clickable UI mockups to PNG, in the light and
//! the dark theme, and writes the index of them.
//!
//! | Output | From |
//! | --- | --- |
//! | `docs/design/mockups/png/<group>/<id>-<light,dark>.png` | `docs/design/mockups/index.html`, one headless-browser screenshot per screen and theme |
//! | `docs/design/mockups/README.md` | the screen registry inside the same page (names, purposes), so the index cannot drift |
//!
//! How it works. The page has a capture mode (`?capture`): no sidebar, header or notes, no frame
//! shadow, no animation, the device frame at scale 1 in the top left corner on a transparent
//! background. `?capture&manifest` also logs the screen registry and the measured size of every
//! frame to the browser console (`GANDER-SCREEN {json}`), which this task reads back from the
//! browser's log file (a headless browser on Windows has no usable stdout). The window of each
//! screenshot is then exactly the frame, so the image is the frame and nothing else.
//!
//! The browser is Microsoft Edge or another Chromium in the new headless mode, run with a throw-away
//! `--user-data-dir` under `target/mockups/` (never the user's profile) and killed on a timeout.
//! The web fonts (Roboto Flex, Roboto Mono) come from Google Fonts: without a network the page
//! falls back to system fonts and the images would differ, so the task stops unless
//! `--allow-fallback-fonts` is given. With the same browser and fonts the output is byte for byte
//! the same on every run: the PNGs are decoded and encoded again with the `png` crate, which also
//! drops every ancillary chunk (no text, no colour profile, no path or machine detail).

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::proc::{display_path, find_on_path, workspace_root};

/// The mockup page, relative to the repository root.
const PAGE: &str = "docs/design/mockups/index.html";
/// Where the images and the index go, relative to the repository root.
const OUT_DIR: &str = "docs/design/mockups";
/// The themes the page knows, in the order the index shows them.
const THEMES: [&str; 2] = ["light", "dark"];
/// How long one browser run may take to deliver its screenshot before it is killed (the first one
/// downloads the fonts).
const RUN_TIMEOUT: Duration = Duration::from_secs(90);
/// How long the browser may take to exit after its screenshot is written.
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);
/// The virtual-time budget given to the page to load its fonts before the screenshot.
const VIRTUAL_TIME_BUDGET_MS: u32 = 4000;
/// Prefix of the console lines the page writes in manifest mode.
const SCREEN_MARKER: &str = "GANDER-SCREEN ";
const FONTS_MARKER: &str = "GANDER-FONTS ";

const USAGE: &str = "usage: cargo xtask mockups [--browser <path>] [--only <id>[,<id>...]] \
                     [--scale <n>] [--jobs <n>] [--allow-fallback-fonts]";

/// One screen of the mockups, as the page's registry describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub id: String,
    pub group: String,
    pub name: String,
    /// `phone`, `tablet`, `laptop` or `page`.
    pub form: String,
    pub purpose: String,
    /// Frame size in CSS pixels, measured by the page.
    pub width: u32,
    pub height: u32,
}

impl Screen {
    /// The folder under `png/`: the group name in lower case with dashes.
    pub fn group_slug(&self) -> String {
        slug(&self.group)
    }

    /// Path of the image relative to the mockups folder, with forward slashes.
    pub fn image(&self, theme: &str) -> String {
        format!("png/{}/{}-{theme}.png", self.group_slug(), self.id)
    }
}

/// What the browser logged in manifest mode.
#[derive(Debug, PartialEq, Eq)]
pub struct Manifest {
    pub screens: Vec<Screen>,
    /// Font faces the page declares and how many of them loaded; none loaded means no network.
    pub font_faces: u64,
    pub fonts_loaded: u64,
}

#[derive(Debug)]
struct Options {
    browser: Option<PathBuf>,
    only: Vec<String>,
    scale: u32,
    jobs: usize,
    allow_fallback_fonts: bool,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut options = Options {
        browser: std::env::var_os("GANDER_BROWSER").map(PathBuf::from),
        only: Vec::new(),
        scale: 1,
        jobs: std::thread::available_parallelism().map_or(2, |n| n.get().min(4)),
        allow_fallback_fonts: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .with_context(|| format!("{name} needs a value; {USAGE}"))
        };
        match arg.as_str() {
            "--browser" => options.browser = Some(PathBuf::from(value("--browser")?)),
            "--only" => {
                options.only = value("--only")?
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            "--scale" => options.scale = value("--scale")?.parse().context("--scale")?,
            "--jobs" => options.jobs = value("--jobs")?.parse().context("--jobs")?,
            "--allow-fallback-fonts" => options.allow_fallback_fonts = true,
            other => bail!("unknown argument {other}; {USAGE}"),
        }
    }
    if !(1..=4).contains(&options.scale) {
        bail!("--scale must be 1 to 4 (the images are 1x by default to keep the repository small)");
    }
    options.jobs = options.jobs.clamp(1, 8);
    Ok(options)
}

// ---------------------------------------------------------------------------------------------
// Finding and running the browser
// ---------------------------------------------------------------------------------------------

/// The browser to use: the explicit path, `GANDER_BROWSER`, Edge in its usual places, then a
/// Chromium-family browser on `PATH`.
fn find_browser(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        bail!("browser {} does not exist", path.display());
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(base) = std::env::var_os(var) {
            candidates.push(Path::new(&base).join("Microsoft/Edge/Application/msedge.exe"));
        }
    }
    candidates.push("C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe".into());
    candidates.push("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".into());
    if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
        return Ok(found);
    }
    for name in [
        "msedge",
        "microsoft-edge",
        "microsoft-edge-stable",
        "chromium",
        "chromium-browser",
        "google-chrome",
    ] {
        if let Some(found) = find_on_path(name) {
            return Ok(found);
        }
    }
    bail!(
        "no Microsoft Edge or Chromium found; install Edge or pass --browser <path> \
         (or set GANDER_BROWSER)"
    )
}

/// A headless Chromium run configuration.
struct Browser {
    exe: PathBuf,
    scale: u32,
}

impl Browser {
    /// Runs the browser once with its own throw-away profile until `output` (the screenshot) is
    /// complete and the browser has let go of the profile.
    ///
    /// On Windows `msedge.exe` starts the real browser and exits at once, so the process handle
    /// says nothing about progress: the screenshot file is the signal, and the profile's
    /// `lockfile` (held until the browser exits) says when the profile is free again. A browser
    /// that does not deliver within [`RUN_TIMEOUT`] is killed.
    fn run(&self, profile: &Path, output: &Path, extra: &[String]) -> Result<()> {
        let _ = std::fs::remove_file(output);
        let mut args: Vec<String> = [
            "--headless=new",
            "--disable-gpu",
            "--hide-scrollbars",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-sync",
            "--mute-audio",
            // Deterministic text: no hinting, no dependence on the display's scale setting.
            "--font-render-hinting=none",
            "--default-background-color=00000000",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        args.push(format!("--force-device-scale-factor={}", self.scale));
        args.push(format!("--user-data-dir={}", display_path(profile)));
        args.push(format!("--virtual-time-budget={VIRTUAL_TIME_BUDGET_MS}"));
        args.push(format!("--screenshot={}", display_path(output)));
        args.extend(extra.iter().cloned());
        let mut child = Command::new(&self.exe)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("could not start {}", self.exe.display()))?;
        let started = Instant::now();
        // 1. The screenshot: present, not empty, and the same size on two polls in a row.
        let mut last_len = None;
        loop {
            if let Ok(meta) = std::fs::metadata(output)
                && meta.len() > 0
            {
                if last_len == Some(meta.len()) {
                    break;
                }
                last_len = Some(meta.len());
            }
            if started.elapsed() > RUN_TIMEOUT {
                kill_browser(&mut child, profile);
                bail!("the browser delivered no screenshot within {RUN_TIMEOUT:?} and was killed");
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        // 2. The browser exits on its own once the screenshot is written; wait for the profile.
        let exit_by = Instant::now() + EXIT_TIMEOUT;
        loop {
            let alive = child.try_wait()?.is_none();
            if !alive && !profile_in_use(profile) {
                return Ok(());
            }
            if Instant::now() > exit_by {
                eprintln!("mockups: the browser did not exit after its screenshot; killing it");
                kill_browser(&mut child, profile);
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Whether a running browser still holds the profile (Windows `lockfile`, Unix `SingletonLock`).
fn profile_in_use(profile: &Path) -> bool {
    ["lockfile", "SingletonLock"]
        .iter()
        .any(|name| std::fs::symlink_metadata(profile.join(name)).is_ok())
}

/// Kills the browser started for `profile`: the child, and every process whose command line names
/// the profile folder (on Windows the launcher has exited and the real browser is not its child).
fn kill_browser(child: &mut Child, profile: &Path) {
    let _ = child.kill();
    let _ = child.wait();
    let needle = display_path(profile);
    if cfg!(windows) {
        let script = format!(
            "Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -and \
             $_.CommandLine.Contains('{}') }} | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force }}",
            needle.replace('\'', "''")
        );
        let _ = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    } else {
        let _ = Command::new("pkill")
            .args(["-f", &needle])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// `file:///C:/a%20b/index.html` for a local path.
fn file_url(path: &Path) -> String {
    let text = display_path(path);
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    let mut url = String::from("file://");
    if !text.starts_with('/') {
        url.push('/');
    }
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                url.push(char::from(byte));
            }
            other => url.push_str(&format!("%{other:02X}")),
        }
    }
    url
}

// ---------------------------------------------------------------------------------------------
// The manifest
// ---------------------------------------------------------------------------------------------

/// The text of a console line between the marker and the closing `", source: ...` the browser
/// appends. Inner quotes of the message are not escaped in the log, so the last `", source: ` wins.
fn marked_payload<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    let start = line.find(marker)? + marker.len();
    let rest = &line[start..];
    let end = rest.rfind("\", source: ").unwrap_or(rest.trim_end().len());
    Some(rest[..end].trim_end_matches('"'))
}

/// Reads the screens and the font report out of the browser log.
pub fn parse_manifest(log: &str) -> Result<Manifest> {
    let mut screens: Vec<Screen> = Vec::new();
    let mut fonts = (0u64, 0u64);
    for line in log.lines() {
        if let Some(json) = marked_payload(line, SCREEN_MARKER) {
            let v: serde_json::Value = serde_json::from_str(json)
                .with_context(|| format!("bad {SCREEN_MARKER}line: {json}"))?;
            let text = |key: &str| -> Result<String> {
                v.get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .with_context(|| format!("screen record without `{key}`: {json}"))
            };
            let number = |key: &str| -> Result<u32> {
                v.get(key)
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .with_context(|| format!("screen record without a positive `{key}`: {json}"))
            };
            let screen = Screen {
                id: text("id")?,
                group: text("group")?,
                name: text("name")?,
                form: text("form")?,
                purpose: text("purpose")?,
                width: number("width")?,
                height: number("height")?,
            };
            if screens.iter().any(|s| s.id == screen.id) {
                bail!("screen id `{}` appears twice in the registry", screen.id);
            }
            screens.push(screen);
        } else if let Some(json) = marked_payload(line, FONTS_MARKER) {
            let v: serde_json::Value = serde_json::from_str(json)
                .with_context(|| format!("bad {FONTS_MARKER}line: {json}"))?;
            let count = |key: &str| v.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0);
            fonts = (count("faces"), count("loaded"));
        }
    }
    if screens.is_empty() {
        bail!(
            "the page logged no screens; is {PAGE} missing its `?capture&manifest` mode, or did \
             the browser fail to start?"
        );
    }
    Ok(Manifest {
        screens,
        font_faces: fonts.0,
        fonts_loaded: fonts.1,
    })
}

/// The group name as a folder name: `Android phone` becomes `android-phone`.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// The ids registered in the page source (`add(GROUP, 'id', ...)` calls), without a browser.
#[cfg(test)]
fn registry_ids(html: &str) -> Vec<String> {
    html.lines()
        .filter_map(|line| {
            let args = line.strip_prefix("add(")?;
            let after_group = args.split_once(", '")?.1;
            Some(after_group.split_once('\'')?.0.to_owned())
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// PNG clean-up
// ---------------------------------------------------------------------------------------------

/// Decodes a PNG and encodes it again with the strongest compression, as RGB when no pixel is
/// transparent. The result carries only the header, the pixel data and the end chunk: no text,
/// no colour profile, no gamma, no physical size.
pub fn clean_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let mut reader = png::Decoder::new(Cursor::new(bytes))
        .read_info()
        .context("not a readable PNG")?;
    let size = reader
        .output_buffer_size()
        .context("PNG too large to decode")?;
    let mut pixels = vec![0u8; size];
    let info = reader.next_frame(&mut pixels).context("decoding the PNG")?;
    pixels.truncate(info.buffer_size());
    if info.bit_depth != png::BitDepth::Eight {
        bail!("expected an 8-bit PNG, got {:?}", info.bit_depth);
    }
    let (color, data) = match info.color_type {
        png::ColorType::Rgba => {
            if pixels.chunks_exact(4).all(|p| p[3] == 255) {
                let rgb: Vec<u8> = pixels
                    .chunks_exact(4)
                    .flat_map(|p| [p[0], p[1], p[2]])
                    .collect();
                (png::ColorType::Rgb, rgb)
            } else {
                (png::ColorType::Rgba, pixels)
            }
        }
        png::ColorType::Rgb => (png::ColorType::Rgb, pixels),
        other => bail!("expected an RGB or RGBA PNG, got {other:?}"),
    };
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, info.width, info.height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::High);
        encoder.set_filter(png::Filter::Adaptive);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&data)?;
    }
    Ok((info.width, info.height, out))
}

/// The chunk types of a PNG, in order (used by tests and the report).
#[cfg(test)]
fn chunk_types(png: &[u8]) -> Vec<String> {
    let mut types = Vec::new();
    let mut at = 8;
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes([png[at], png[at + 1], png[at + 2], png[at + 3]]) as usize;
        types.push(String::from_utf8_lossy(&png[at + 4..at + 8]).into_owned());
        at += 12 + len;
    }
    types
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

/// What happened to one image.
#[derive(Debug)]
struct Outcome {
    path: String,
    bytes: usize,
    written: bool,
}

/// Screenshots one screen in one theme, cleans the PNG and writes it when it changed.
fn render_one(
    browser: &Browser,
    profile: &Path,
    scratch: &Path,
    page_url: &str,
    screen: &Screen,
    theme: &str,
    out_root: &Path,
) -> Result<Outcome> {
    let relative = screen.image(theme);
    let target = out_root.join(&relative);
    let shot = scratch.join(format!("{}-{theme}.png", screen.id));
    let (want_w, want_h) = (screen.width * browser.scale, screen.height * browser.scale);
    // Hash routing: the part after `#` is not part of the query.
    let url = format!("{page_url}?theme={theme}&capture#/{}", screen.id);
    let mut last_error = String::new();
    for attempt in 1..=2 {
        let extra = [
            format!("--window-size={},{}", screen.width, screen.height),
            url.clone(),
        ];
        if let Err(e) = browser.run(profile, &shot, &extra) {
            last_error = format!("{e:#}");
            continue;
        }
        let raw = match std::fs::read(&shot) {
            Ok(raw) => raw,
            Err(e) => {
                last_error = format!("no screenshot written: {e}");
                continue;
            }
        };
        let (w, h, bytes) = clean_png(&raw).with_context(|| relative.clone())?;
        if (w, h) != (want_w, want_h) {
            last_error =
                format!("attempt {attempt}: image is {w}x{h}, the frame is {want_w}x{want_h}");
            continue;
        }
        let _ = std::fs::remove_file(&shot);
        let written = std::fs::read(&target).ok().as_deref() != Some(bytes.as_slice());
        if written {
            if let Some(dir) = target.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&target, &bytes).with_context(|| format!("writing {relative}"))?;
        }
        return Ok(Outcome {
            path: relative,
            bytes: bytes.len(),
            written,
        });
    }
    bail!("{} ({theme}): {last_error}", screen.id)
}

/// Renders every `(screen, theme)` pair on `jobs` worker threads, each with its own profile.
fn render_all(
    browser: &Browser,
    work: &Path,
    page_url: &str,
    screens: &[&Screen],
    jobs: usize,
    out_root: &Path,
) -> (Vec<Outcome>, Vec<String>) {
    let tasks: Vec<(&Screen, &str)> = screens
        .iter()
        .flat_map(|s| THEMES.iter().map(move |t| (*s, *t)))
        .collect();
    let next = AtomicUsize::new(0);
    let done = Mutex::new((Vec::new(), Vec::new()));
    std::thread::scope(|scope| {
        for worker in 0..jobs.min(tasks.len()) {
            let (tasks, next, done) = (&tasks, &next, &done);
            scope.spawn(move || {
                let profile = work.join(format!("profile-{worker}"));
                let scratch = work.join(format!("shots-{worker}"));
                let _ = std::fs::create_dir_all(&scratch);
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some((screen, theme)) = tasks.get(i).copied() else {
                        break;
                    };
                    let result = render_one(
                        browser, &profile, &scratch, page_url, screen, theme, out_root,
                    );
                    let mut done = done.lock().unwrap_or_else(|p| p.into_inner());
                    match result {
                        Ok(o) => {
                            println!(
                                "[{}] {} ({} KiB)",
                                if o.written { "wrote    " } else { "unchanged" },
                                o.path,
                                o.bytes.div_ceil(1024)
                            );
                            done.0.push(o);
                        }
                        Err(e) => {
                            println!("[FAIL     ] {e:#}");
                            done.1.push(format!("{e:#}"));
                        }
                    }
                }
            });
        }
    });
    done.into_inner().unwrap_or_else(|p| p.into_inner())
}

/// Deletes PNGs under `png/` that no screen produces any more, then empty folders.
fn prune_stale(out_root: &Path, keep: &BTreeSet<String>) -> Result<usize> {
    let png_dir = out_root.join("png");
    let mut removed = 0;
    let Ok(groups) = std::fs::read_dir(&png_dir) else {
        return Ok(0);
    };
    for group in groups {
        let group = group?.path();
        if !group.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(&group)? {
            let file = file?.path();
            let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let relative = format!(
                "png/{}/{name}",
                group.file_name().and_then(|n| n.to_str()).unwrap_or("")
            );
            if file.extension().is_some_and(|e| e == "png") && !keep.contains(&relative) {
                std::fs::remove_file(&file)?;
                println!("[removed  ] {relative} (no screen produces it)");
                removed += 1;
            }
        }
        if std::fs::read_dir(&group)?.next().is_none() {
            std::fs::remove_dir(&group)?;
        }
    }
    Ok(removed)
}

// ---------------------------------------------------------------------------------------------
// The index (README.md)
// ---------------------------------------------------------------------------------------------

/// Markdown-safe text for a table cell.
fn escape_cell(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '|' | '\\' | '*' | '[' | ']' | '`' => {
                out.push('\\');
                out.push(c);
            }
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            other => out.push(other),
        }
    }
    out
}

/// The displayed width of a screen's image in the index, in pixels.
fn thumb_width(form: &str) -> u32 {
    if form == "phone" { 200 } else { 420 }
}

/// The index of every screen, generated from the registry.
pub fn readme(screens: &[Screen], scale: u32) -> String {
    let mut out = String::new();
    out.push_str(
        "<!-- Generated by `cargo xtask mockups` from the screen registry in index.html. \
         Do not edit by hand. -->\n\n",
    );
    out.push_str("# Gander UI mockups\n\n");
    out.push_str(&format!(
        "The approved mockups of the Android phone and tablet client and the Windows host, as \
         clickable HTML and as images of every screen: {} screens, each in a light and a dark \
         theme. The design and the reasons for it are in \
         [the UI redesign](../2026-11-02-ui-redesign.md).\n\n",
        screens.len()
    ));
    out.push_str(
        "- **Clickable mockups.** Open [`index.html`](index.html) in a browser (one file, no build). \
         Every screen has a route, `index.html#/<id>`, where `<id>` is the code shown under the \
         screen name below. `?theme=light` or `?theme=dark` picks the theme, the left and right \
         arrow keys step through the screens, and \"Tap targets\" outlines what can be tapped. \
         Roboto Flex and Roboto Mono load from Google Fonts; offline, system fonts take over.\n",
    );
    out.push_str(&format!(
        "- **Images.** `png/<group>/<id>-light.png` and `png/<group>/<id>-dark.png`, the device \
         frame only, at {scale}x, with transparent corners and no metadata.\n"
    ));
    out.push_str(
        "- **Sample data.** Everything on screen is invented: the computers (`studio-pc`, \
         `workshop-laptop`), the account (`alice@example.com`), the folders, the network names \
         and the `100.101.102.x` addresses. Phone and tablet models (Pixel 9, Galaxy Tab S9) are \
         only examples. No real people, machines, paths or tokens.\n",
    );
    out.push_str(
        "- **Regenerate.** `cargo xtask mockups` renders every screen again with headless \
         Microsoft Edge (or another Chromium, `--browser <path>`) and rewrites the images and this \
         index. Run it after changing `index.html`, and commit the result. `--only <id>` renders \
         one screen.\n\n",
    );
    let mut groups: Vec<&str> = Vec::new();
    for s in screens {
        if !groups.contains(&s.group.as_str()) {
            groups.push(&s.group);
        }
    }
    let mut number = 0;
    for group in groups {
        let in_group: Vec<&Screen> = screens.iter().filter(|s| s.group == group).collect();
        out.push_str(&format!("## {group}\n\n"));
        out.push_str("| Light | Screen |\n| --- | --- |\n");
        for s in in_group {
            number += 1;
            let light = s.image("light");
            let dark = s.image("dark");
            out.push_str(&format!(
                "| <a href=\"{light}\"><img src=\"{light}\" width=\"{}\" alt=\"{} (light)\"></a> \
                 | **{number:02}. {}**<br>`{}`<br><br>{}<br><br>[Light]({light}) \u{b7} [Dark]({dark}) |\n",
                thumb_width(&s.form),
                escape_cell(&s.name),
                escape_cell(&s.name),
                s.id,
                escape_cell(&s.purpose),
            ));
        }
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------------------------

/// Entry point for `cargo xtask mockups`.
pub fn run(args: &[String]) -> Result<ExitCode> {
    let options = parse_args(args)?;
    let root = workspace_root();
    let page = root.join(PAGE);
    if !page.is_file() {
        bail!("{} not found", display_path(&page));
    }
    let browser = Browser {
        exe: find_browser(options.browser.as_deref())?,
        scale: options.scale,
    };
    println!("mockups: browser {}", display_path(&browser.exe));
    let work = root.join("target").join("mockups");
    remove_dir_retrying(&work);
    std::fs::create_dir_all(&work)?;
    let result = run_in(&options, &browser, &root, &work, &page);
    // The profiles are throw-away; every browser has exited, so they can go.
    remove_dir_retrying(&work);
    result
}

/// Removes a folder, trying a few times: a browser that has just exited may still hold a file.
fn remove_dir_retrying(dir: &Path) {
    for _ in 0..20 {
        if std::fs::remove_dir_all(dir).is_ok() || !dir.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    eprintln!(
        "mockups: could not remove {}; delete it by hand",
        display_path(dir)
    );
}

fn run_in(
    options: &Options,
    browser: &Browser,
    root: &Path,
    work: &Path,
    page: &Path,
) -> Result<ExitCode> {
    let page_url = file_url(page);
    // 1. The registry and the frame sizes, from the page itself.
    let log = work.join("manifest.log");
    let probe = work.join("manifest.png");
    browser.run(
        &work.join("profile-manifest"),
        &probe,
        &[
            "--enable-logging".to_owned(),
            "--v=0".to_owned(),
            format!("--log-file={}", display_path(&log)),
            "--window-size=200,100".to_owned(),
            format!("{page_url}?capture&manifest"),
        ],
    )?;
    let log_text = std::fs::read(&log)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .context("the browser wrote no log")?;
    let manifest = parse_manifest(&log_text)?;
    println!(
        "mockups: {} screens; web fonts: {} of {} faces loaded",
        manifest.screens.len(),
        manifest.fonts_loaded,
        manifest.font_faces
    );
    if manifest.fonts_loaded == 0 && !options.allow_fallback_fonts {
        eprintln!(
            "mockups: the web fonts did not load (offline?). The images would use system fonts \
             and differ from the committed ones. Connect to the network, or pass \
             --allow-fallback-fonts to render anyway."
        );
        return Ok(ExitCode::from(1));
    }

    // 2. Which screens.
    let selected: Vec<&Screen> = if options.only.is_empty() {
        manifest.screens.iter().collect()
    } else {
        for id in &options.only {
            if !manifest.screens.iter().any(|s| &s.id == id) {
                bail!("no screen `{id}`; ids are listed in docs/design/mockups/README.md");
            }
        }
        manifest
            .screens
            .iter()
            .filter(|s| options.only.contains(&s.id))
            .collect()
    };

    // 3. The images.
    let out_root = root.join(OUT_DIR);
    let (outcomes, failures) =
        render_all(browser, work, &page_url, &selected, options.jobs, &out_root);
    if !failures.is_empty() {
        eprintln!("mockups: {} image(s) failed", failures.len());
        return Ok(ExitCode::from(1));
    }
    if options.only.is_empty() {
        let keep: BTreeSet<String> = manifest
            .screens
            .iter()
            .flat_map(|s| THEMES.iter().map(move |t| s.image(t)))
            .collect();
        prune_stale(&out_root, &keep)?;
    }

    // 4. The index.
    let index = readme(&manifest.screens, options.scale);
    let index_path = out_root.join("README.md");
    let index_changed =
        std::fs::read_to_string(&index_path).ok().as_deref() != Some(index.as_str());
    if index_changed {
        std::fs::write(&index_path, &index)?;
    }

    let written = outcomes.iter().filter(|o| o.written).count();
    let total: usize = outcomes.iter().map(|o| o.bytes).sum();
    println!(
        "mockups: {} image(s) ({written} written, {} unchanged), {:.2} MiB; README.md {}",
        outcomes.len(),
        outcomes.len() - written,
        total as f64 / (1024.0 * 1024.0),
        if index_changed {
            "written"
        } else {
            "unchanged"
        }
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "[1:2:1103/120006.575:INFO:CONSOLE:1498] \"GANDER-SCREEN {\"id\":\"hosts\",\"group\":\"Android phone\",\"name\":\"Computers\",\"form\":\"phone\",\"purpose\":\"Home: \\\"your\\\" computers.\",\"states\":[\"Online\"],\"width\":412,\"height\":915}\", source: file:///C:/x/index.html?capture&manifest (1498)\r\n\
        [1:2:1103/120006.577:INFO:CONSOLE:1498] \"GANDER-SCREEN {\"id\":\"tokens\",\"group\":\"Design system\",\"name\":\"Tokens\",\"form\":\"page\",\"purpose\":\"p\",\"states\":[],\"width\":900,\"height\":780}\", source: file:///C:/x/index.html (1498)\r\n\
        [1:2:1103/120007.000:INFO:CONSOLE:1502] \"GANDER-FONTS {\"faces\":42,\"loaded\":7}\", source: file:///C:/x/index.html (1502)\r\n\
        [1:2:1103/120007.001:INFO:CONSOLE:6233] \"[ProtocolLaunch] handler constructed\", source: chrome-extension://abc/background.js (6233)\r\n";

    fn screen(id: &str, group: &str, form: &str, purpose: &str) -> Screen {
        Screen {
            id: id.into(),
            group: group.into(),
            name: format!("Name of {id}"),
            form: form.into(),
            purpose: purpose.into(),
            width: 412,
            height: 915,
        }
    }

    #[test]
    fn the_manifest_is_read_from_the_console_lines() {
        let m = parse_manifest(LOG).unwrap();
        assert_eq!(m.screens.len(), 2);
        assert_eq!(m.screens[0].id, "hosts");
        assert_eq!(m.screens[0].group, "Android phone");
        assert_eq!((m.screens[0].width, m.screens[0].height), (412, 915));
        assert_eq!(m.screens[0].purpose, "Home: \"your\" computers.");
        assert_eq!((m.screens[1].width, m.screens[1].height), (900, 780));
        assert_eq!((m.font_faces, m.fonts_loaded), (42, 7));
    }

    #[test]
    fn a_log_without_screens_or_with_a_repeated_id_is_an_error() {
        assert!(parse_manifest("nothing useful\n").is_err());
        let twice = format!("{LOG}{}", LOG.lines().next().unwrap());
        assert!(parse_manifest(&twice).is_err());
    }

    #[test]
    fn local_paths_become_file_urls() {
        assert_eq!(
            file_url(Path::new(r"C:\Users\Some One\repo\index.html")),
            "file:///C:/Users/Some%20One/repo/index.html"
        );
        assert_eq!(
            file_url(Path::new("/home/a/b.html")),
            "file:///home/a/b.html"
        );
        assert_eq!(
            file_url(Path::new(r"\\?\C:\x\y.html")),
            "file:///C:/x/y.html"
        );
    }

    #[test]
    fn groups_become_folder_names() {
        assert_eq!(slug("Android phone"), "android-phone");
        assert_eq!(slug("Windows host"), "windows-host");
        assert_eq!(slug("Design system"), "design-system");
        let s = screen("hosts", "Android phone", "phone", "");
        assert_eq!(s.image("dark"), "png/android-phone/hosts-dark.png");
    }

    #[test]
    fn cleaning_drops_text_chunks_and_transparency_that_is_not_used() {
        // A 2x2 opaque RGBA image with a text chunk, as a browser might write it.
        let mut raw = Vec::new();
        {
            let mut e = png::Encoder::new(&mut raw, 2, 2);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            e.add_text_chunk("Comment".into(), r"C:\Users\someone\secret".into())
                .unwrap();
            let mut w = e.write_header().unwrap();
            let px: Vec<u8> = (0..4).flat_map(|i| [i * 40, 10, 200, 255]).collect();
            w.write_image_data(&px).unwrap();
        }
        assert!(chunk_types(&raw).iter().any(|t| t == "tEXt"));
        let (w, h, clean) = clean_png(&raw).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(chunk_types(&clean), ["IHDR", "IDAT", "IEND"]);
        assert!(!String::from_utf8_lossy(&clean).contains("secret"));
        // Opaque pixels are kept, as RGB.
        let mut reader = png::Decoder::new(Cursor::new(&clean)).read_info().unwrap();
        assert_eq!(reader.output_color_type().0, png::ColorType::Rgb);
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut buf).unwrap();
        assert_eq!(&buf[..3], &[0, 10, 200]);
        assert_eq!(&buf[3..6], &[40, 10, 200]);
    }

    #[test]
    fn cleaning_keeps_real_transparency() {
        let mut raw = Vec::new();
        {
            let mut e = png::Encoder::new(&mut raw, 1, 2);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            let mut w = e.write_header().unwrap();
            w.write_image_data(&[1, 2, 3, 0, 4, 5, 6, 255]).unwrap();
        }
        let (_, _, clean) = clean_png(&raw).unwrap();
        let mut reader = png::Decoder::new(Cursor::new(&clean)).read_info().unwrap();
        assert_eq!(reader.output_color_type().0, png::ColorType::Rgba);
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut buf).unwrap();
        assert_eq!(buf[3], 0);
        assert_eq!(buf[7], 255);
        assert!(clean_png(b"not a png").is_err());
    }

    #[test]
    fn the_index_lists_every_screen_with_both_themes_and_escapes_text() {
        let screens = vec![
            screen("hosts", "Android phone", "phone", "A | B <tag> & *star*"),
            screen("h-tray", "Windows host", "laptop", "Tray."),
        ];
        let text = readme(&screens, 1);
        assert!(text.contains("## Android phone"));
        assert!(text.contains("## Windows host"));
        assert!(text.contains("src=\"png/android-phone/hosts-light.png\" width=\"200\""));
        assert!(text.contains("src=\"png/windows-host/h-tray-light.png\" width=\"420\""));
        assert!(text.contains("[Dark](png/android-phone/hosts-dark.png)"));
        assert!(text.contains("[Dark](png/windows-host/h-tray-dark.png)"));
        assert!(text.contains("A \\| B &lt;tag&gt; &amp; \\*star\\*"));
        assert!(text.contains("**01. Name of hosts**"));
        assert!(text.contains("**02. Name of h-tray**"));
        assert!(text.contains("2 screens"));
        assert!(!text.contains('\u{2014}'));
    }

    #[test]
    fn the_registry_is_found_in_the_page_source() {
        let src = "add(DS, 'tokens', 'Tokens', 'page', tokens, 'p', []);\nadd(PH, 'hosts', 'x');\nconst add = (g, id) => 1;\n";
        assert_eq!(registry_ids(src), ["tokens", "hosts"]);
    }

    #[test]
    fn the_page_has_the_capture_hooks_this_task_relies_on() {
        let html = std::fs::read_to_string(workspace_root().join(PAGE)).unwrap();
        for needle in [
            "html.capture",
            "GANDER-SCREEN",
            "GANDER-FONTS",
            "has('capture')",
        ] {
            assert!(html.contains(needle), "{PAGE} lost `{needle}`");
        }
    }

    #[test]
    fn every_registered_screen_has_both_images_and_an_index_entry() {
        let root = workspace_root();
        let html = std::fs::read_to_string(root.join(PAGE)).unwrap();
        let ids = registry_ids(&html);
        assert!(ids.len() >= 40, "found only {} screens", ids.len());
        let readme = std::fs::read_to_string(root.join(OUT_DIR).join("README.md"))
            .expect("README.md; run `cargo xtask mockups`");
        let mut on_disk = 0;
        for group in std::fs::read_dir(root.join(OUT_DIR).join("png")).unwrap() {
            for file in std::fs::read_dir(group.unwrap().path()).unwrap() {
                let name = file.unwrap().file_name().to_string_lossy().into_owned();
                assert!(
                    THEMES.iter().any(|t| name.ends_with(&format!("-{t}.png"))),
                    "unexpected file {name}"
                );
                on_disk += 1;
            }
        }
        assert_eq!(
            on_disk,
            ids.len() * THEMES.len(),
            "png/ does not hold exactly one light and one dark image per screen; run `cargo xtask mockups`"
        );
        for id in &ids {
            assert!(
                readme.contains(&format!("`{id}`")),
                "{id} is missing from README.md; run `cargo xtask mockups`"
            );
            for theme in THEMES {
                let found = [
                    "android-phone",
                    "android-tablet",
                    "windows-host",
                    "design-system",
                ]
                .iter()
                .any(|g| {
                    root.join(OUT_DIR)
                        .join(format!("png/{g}/{id}-{theme}.png"))
                        .is_file()
                });
                assert!(
                    found,
                    "no {theme} image for {id}; run `cargo xtask mockups`"
                );
            }
        }
    }
}
