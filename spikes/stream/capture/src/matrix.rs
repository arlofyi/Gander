// SPDX-License-Identifier: Apache-2.0
//! The automatable stream-spike matrix items for capture (C1-C5, C9-C11, C15-C17).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use gander_testwin::layout::{PATCH_A_RGB, Rgb};
use spike_capture::clock;
use spike_capture::d3d::{self, Adapter};
use spike_capture::readback::{Image, Readback};
use spike_capture::wgc::{self, BorderlessAccess, Capture, CaptureOptions, WindowGeometry};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D11::ID3D11Device;

use crate::harness::{self, Monitor, Occluder, Testwin};
use crate::measure::{self, Analyzer, Run, Stats};
use crate::wc;

/// The C2 occluder colour: absent from everything gander-testwin paints.
pub const OCCLUDER_RGB: Rgb = Rgb(0, 170, 170);

/// One printed table row.
#[derive(Debug, Clone)]
pub struct Row {
    pub id: String,
    pub condition: String,
    pub result: &'static str,
    pub stats: Option<Stats>,
    pub notes: String,
}

impl Row {
    fn new(
        id: &str,
        condition: &str,
        result: &'static str,
        stats: Option<Stats>,
        notes: String,
    ) -> Row {
        Row {
            id: id.into(),
            condition: condition.into(),
            result,
            stats,
            notes,
        }
    }

    pub fn skip(id: &str, condition: &str, why: &str) -> Row {
        Row::new(id, condition, "SKIP", None, why.into())
    }
}

/// Shared run settings.
#[derive(Debug)]
pub struct Ctx {
    pub exe: PathBuf,
    pub secs: f64,
    pub adapters: Vec<Adapter>,
    pub monitors: Vec<Monitor>,
    pub dump: Option<PathBuf>,
}

impl Ctx {
    fn dur(&self) -> Duration {
        Duration::from_secs_f64(self.secs)
    }

    fn primary(&self) -> Option<&Monitor> {
        self.monitors.iter().find(|m| m.primary)
    }

    /// Spawns a test window and puts it near the top-left of the primary monitor's work area.
    fn spawn(&self, title: &str) -> Result<Testwin> {
        let w = Testwin::spawn(&self.exe, title)?;
        if let Some(m) = self.primary() {
            harness::move_window(w.hwnd, m.work.left + 60, m.work.top + 60)?;
        }
        // Started from a background console, the window opens behind the foreground window
        // (focus-stealing prevention). Try to activate it; either way make it topmost so it is
        // fully visible on screen (WGC itself does not need either, see C2).
        let _ = harness::try_foreground(w.hwnd);
        harness::raise_topmost(w.hwnd)?;
        std::thread::sleep(Duration::from_millis(250));
        Ok(w)
    }

    fn dump(&self, name: &str, img: &Image) {
        if let Some(dir) = &self.dump {
            let _ = std::fs::create_dir_all(dir);
            let _ = img.write_ppm(&dir.join(format!("{name}.ppm")));
        }
    }

    fn adapter_name(&self, luid: i64) -> String {
        self.adapters
            .iter()
            .find(|a| a.info.luid == luid)
            .map(|a| a.info.name.clone())
            .unwrap_or_else(|| format!("luid {luid:#x}"))
    }
}

fn opts() -> CaptureOptions {
    CaptureOptions::default()
}

/// Starts a capture and discards the first frames (start-up transients).
fn start(hwnd: HWND, device: &ID3D11Device, o: &CaptureOptions) -> Result<Capture> {
    let cap = Capture::start(hwnd, device, o)?;
    std::thread::sleep(Duration::from_millis(250));
    cap.drain();
    Ok(cap)
}

/// Collects one capture with the basic analysis (counter + patches).
fn basic_run(cap: &Capture, rb: &mut Readback, an: &mut Analyzer, dur: Duration) -> Result<Run> {
    let mut runs = measure::collect(&[cap], rb, dur, |_, f, img| {
        let (c, a, b) = an.basic(img, f.size());
        (c, a, b, 0.0)
    })?;
    Ok(runs.remove(0))
}

fn pct(n: usize, of: usize) -> String {
    if of == 0 {
        "0/0".into()
    } else {
        format!("{n}/{of}")
    }
}

fn sizes(run: &Run) -> String {
    let mut s: Vec<(u32, u32)> = run.frames.iter().map(|f| f.size).collect();
    s.dedup();
    let v: Vec<String> = s.iter().take(4).map(|(w, h)| format!("{w}x{h}")).collect();
    if v.is_empty() {
        "-".into()
    } else {
        v.join(">")
    }
}

/// C1: visible, foreground, primary monitor; once per adapter plus the default device, plus
/// the windows-capture comparison.
pub fn c1(ctx: &Ctx) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut targets: Vec<(String, Option<&Adapter>)> = vec![("default device".into(), None)];
    for a in &ctx.adapters {
        targets.push((format!("adapter {}", a.info.index), Some(a)));
    }
    for (label, adapter) in targets {
        let cond = format!("Visible, foreground, primary monitor; own WGC wrapper, {label}");
        rows.push(
            c1_one(ctx, adapter)
                .unwrap_or_else(|e| Row::new("C1", &cond, "ERROR", None, format!("{e:#}"))),
        );
        if let Some(r) = rows.last_mut() {
            r.condition = cond;
        }
    }
    rows.push(c1_windows_capture(ctx).unwrap_or_else(|e| {
        Row::new(
            "C1",
            "windows-capture 2.0.1 (own device)",
            "ERROR",
            None,
            format!("{e:#}"),
        )
    }));
    rows
}

fn c1_one(ctx: &Ctx, adapter: Option<&Adapter>) -> Result<Row> {
    let w = ctx.spawn("Gander Spike C1")?;
    let device = d3d::create_device(adapter.map(|a| &a.adapter))?;
    let dev_luid = d3d::device_luid(&device)?;
    let fg = harness::foreground() == w.hwnd;
    let cap = start(w.hwnd, &device, &opts())?;
    let mut rb = Readback::new(&device)?;
    let mut an = Analyzer::new(w.hwnd);
    let mut tex_luid = None;
    let mut calib = None;
    let mut first: Option<Image> = None;
    let mut runs = measure::collect(&[&cap], &mut rb, ctx.dur(), |_, f, img| {
        if tex_luid.is_none() {
            // SAFETY: plain getter on a live texture.
            tex_luid = unsafe { f.texture().GetDevice() }
                .ok()
                .and_then(|d| d3d::device_luid(&d).ok());
        }
        let r = an.basic(img, f.size());
        if calib.is_none()
            && let Some(l) = an.layout()
        {
            calib = measure::find_run(img, PATCH_A_RGB, l.patch_a.w)
                .map(|(x, y)| (x - l.patch_a.x, y - l.patch_a.y));
            first = Some(img.clone());
        }
        (r.0, r.1, r.2, 0.0)
    })?;
    let run = runs.remove(0);
    if let Some(img) = &first {
        ctx.dump(&format!("c1-{dev_luid:x}"), img);
    }
    drop(cap);
    let g = WindowGeometry::of(w.hwnd)?;
    w.quit();
    let s = run.stats();
    let ok = s.frames > 0 && s.fps >= 30.0 && s.patch_a_ok == s.frames && s.decoded == s.frames;
    let notes = format!(
        "device on {} (video={}, mt={}); texture on same adapter: {}; foreground={fg}; size {} (DWM frame {}x{}); client offset DWM {:?} vs located patch A {:?}; patches A {} B {}; missed src {} repeats {}; dropped(full) {}",
        ctx.adapter_name(dev_luid),
        d3d::has_video_support(&device),
        d3d::is_multithread_protected(&device),
        tex_luid
            .map(|l| if l == dev_luid {
                "yes".to_string()
            } else {
                format!("NO ({})", ctx.adapter_name(l))
            })
            .unwrap_or_else(|| "?".into()),
        sizes(&run),
        g.frame_size().0,
        g.frame_size().1,
        an.offset,
        calib,
        pct(s.patch_a_ok, s.frames),
        pct(s.patch_b_ok, s.frames),
        s.missed_source,
        s.repeats,
        run.dropped_full,
    );
    Ok(Row::new(
        "C1",
        "",
        if ok { "PASS" } else { "FAIL" },
        Some(s),
        notes,
    ))
}

fn c1_windows_capture(ctx: &Ctx) -> Result<Row> {
    let w = ctx.spawn("Gander Spike C1 wc")?;
    let (run, facts) = wc::run(w.hwnd, ctx.dur())?;
    w.quit();
    let s = run.stats();
    let notes = format!(
        "library-created device on {} (video={}, mt={}); size {}; frame lent only inside on_frame_arrived (1-buffer pool)",
        ctx.adapter_name(facts.luid),
        facts.video_support,
        facts.multithread_protected,
        sizes(&run),
    );
    Ok(Row::new(
        "C1",
        "windows-capture 2.0.1 (own device)",
        if s.frames > 0 { "INFO" } else { "FAIL" },
        Some(s),
        notes,
    ))
}

/// C2: an occluder (this process's topmost window) covers the whole client area.
pub fn c2(ctx: &Ctx, device: &ID3D11Device) -> Result<Row> {
    let w = ctx.spawn("Gander Spike C2")?;
    let g = WindowGeometry::of(w.hwnd)?;
    let rect = RECT {
        left: g.client_origin.x,
        top: g.client_origin.y,
        right: g.client_origin.x + g.client_w,
        bottom: g.client_origin.y + g.client_h,
    };
    let occ = Occluder::show(rect, (OCCLUDER_RGB.0, OCCLUDER_RGB.1, OCCLUDER_RGB.2))?;
    let probe = POINT {
        x: g.client_origin.x + 20,
        y: g.client_origin.y + 20,
    };
    // Only our own occluder's pixel is read, and only after checking that it is the window there.
    let at_probe = harness::window_from_point(probe);
    let occ_at_probe = at_probe == occ.hwnd();
    let probe_desc = harness::describe(at_probe);
    // Z-order proof that holds even when the session is locked (nothing on screen to read).
    let above = harness::is_above(occ.hwnd(), w.hwnd);
    let covering = occ_at_probe
        && harness::screen_pixel(probe) == (OCCLUDER_RGB.0, OCCLUDER_RGB.1, OCCLUDER_RGB.2);
    let cap = start(w.hwnd, device, &opts())?;
    let mut rb = Readback::new(device)?;
    let mut an = Analyzer::new(w.hwnd);
    let mut first = None;
    let mut runs = measure::collect(&[&cap], &mut rb, ctx.dur(), |_, f, img| {
        if first.is_none() {
            first = Some(img.clone());
        }
        let (c, a, b) = an.basic(img, f.size());
        (c, a, b, measure::count_colour(img, OCCLUDER_RGB) as f64)
    })?;
    drop(cap);
    drop(occ);
    w.quit();
    if let Some(img) = &first {
        ctx.dump("c2", img);
    }
    let run = runs.remove(0);
    let s = run.stats();
    let leak: f64 = run.frames.iter().map(|f| f.extra).sum();
    let patches = s.patch_a_ok == s.frames && s.patch_b_ok == s.frames;
    let result = if leak > 0.0 {
        "FAIL"
    } else if !(covering || above) || s.frames == 0 {
        "INFO"
    } else if patches {
        "PASS"
    } else {
        "FAIL"
    };
    let notes = format!(
        "occluder RGB(0,170,170) over the whole client area, above the test window in z-order: {above}; visible on screen at the probe point: {covering} (window there: {probe_desc}); occluder-colour pixels in frames: {leak}; patch A exact {} B exact {}; counter decoded {}",
        pct(s.patch_a_ok, s.frames),
        pct(s.patch_b_ok, s.frames),
        pct(s.decoded, s.frames),
    );
    Ok(Row::new(
        "C2",
        "Window fully covered (client) by another window",
        result,
        Some(s),
        notes,
    ))
}

/// C3: moved to the secondary monitor (and back) mid-capture.
pub fn c3(ctx: &Ctx, device: &ID3D11Device) -> Result<Row> {
    let cond = "Window moved to secondary monitor (other DPI) mid-capture";
    let (Some(primary), Some(second)) = (ctx.primary(), ctx.monitors.iter().find(|m| !m.primary))
    else {
        return Ok(Row::skip("C3", cond, "only one monitor attached"));
    };
    let w = ctx.spawn("Gander Spike C3")?;
    let cap = start(w.hwnd, device, &opts())?;
    let mut rb = Readback::new(device)?;
    let mut an = Analyzer::new(w.hwnd);
    let r1 = basic_run(&cap, &mut rb, &mut an, Duration::from_secs(1))?;
    let g1 = WindowGeometry::of(w.hwnd)?;
    harness::move_window(w.hwnd, second.work.left + 60, second.work.top + 60)?;
    // Wait for the DPI change to settle (the window resizes itself on WM_DPICHANGED).
    std::thread::sleep(Duration::from_millis(800));
    cap.drain();
    let r2 = basic_run(&cap, &mut rb, &mut an, ctx.dur())?;
    let g2 = WindowGeometry::of(w.hwnd)?;
    let layout2 = w.send("layout?")?;
    harness::move_window(w.hwnd, primary.work.left + 60, primary.work.top + 60)?;
    std::thread::sleep(Duration::from_millis(800));
    cap.drain();
    let r3 = basic_run(&cap, &mut rb, &mut an, Duration::from_secs(1))?;
    let g3 = WindowGeometry::of(w.hwnd)?;
    let resized = cap.resized();
    drop(cap);
    w.quit();
    let (s1, s2, s3) = (r1.stats(), r2.stats(), r3.stats());
    let size_ok = r2
        .frames
        .last()
        .is_some_and(|f| (f.size.0 as i32, f.size.1 as i32) == g2.frame_size());
    let ok = s2.frames > 0
        && s2.patch_a_ok == s2.frames
        && s2.decoded == s2.frames
        && size_ok
        && s3.frames > 0;
    let notes = format!(
        "primary {} dpi {} {}Hz -> secondary {} dpi {} {}Hz; window dpi {}->{}->{}; frame sizes before {} on secondary {} back {}; DWM frame on secondary {}x{} matches: {size_ok}; pool recreated {resized}x; on secondary: patch A {} counter {}; back on primary fps {:.1}; testwin layout? on secondary: {}",
        primary.name,
        primary.dpi,
        primary.hz,
        second.name,
        second.dpi,
        second.hz,
        g1.dpi,
        g2.dpi,
        g3.dpi,
        sizes(&r1),
        sizes(&r2),
        sizes(&r3),
        g2.frame_size().0,
        g2.frame_size().1,
        pct(s2.patch_a_ok, s2.frames),
        pct(s2.decoded, s2.frames),
        s3.fps,
        layout2
            .split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join(" "),
    );
    let _ = s1;
    Ok(Row::new(
        "C3",
        cond,
        if ok { "PASS" } else { "FAIL" },
        Some(s2),
        notes,
    ))
}

fn mean_luma(img: &Image) -> f64 {
    if img.px.is_empty() {
        return 0.0;
    }
    let sum: u64 = img
        .px
        .iter()
        .map(|p| {
            let (r, g, b) = ((p >> 16) & 255, (p >> 8) & 255, p & 255);
            u64::from((r * 299 + g * 587 + b * 114) / 1000)
        })
        .sum();
    sum as f64 / img.px.len() as f64
}

/// C4: minimised, then restored, mid-capture.
pub fn c4(ctx: &Ctx, device: &ID3D11Device) -> Result<Row> {
    let w = ctx.spawn("Gander Spike C4")?;
    let cap = start(w.hwnd, device, &opts())?;
    let mut rb = Readback::new(device)?;
    let mut an = Analyzer::new(w.hwnd);
    let r1 = basic_run(&cap, &mut rb, &mut an, Duration::from_secs(1))?;
    let reply = w.send("minimise")?;
    let t_min = clock::now_100ns();
    let r2 = measure::collect(&[&cap], &mut rb, Duration::from_secs(2), |_, _, img| {
        (None, false, false, mean_luma(img))
    })?
    .remove(0);
    let restore = w.send("restore")?;
    let t_restore = clock::now_100ns();
    let r3 = basic_run(&cap, &mut rb, &mut an, Duration::from_secs(2))?;
    drop(cap);
    w.quit();
    let (s1, s2, s3) = (r1.stats(), r2.stats(), r3.stats());
    let first_after = r3
        .frames
        .first()
        .map(|f| (f.arrival_100ns - t_restore) as f64 / 10_000.0);
    let last_min = r2
        .frames
        .last()
        .map(|f| (f.arrival_100ns - t_min) as f64 / 10_000.0);
    let luma: Vec<String> = r2
        .frames
        .iter()
        .take(3)
        .map(|f| format!("{:.0}", f.extra))
        .collect();
    let notes = format!(
        "before: {:.1} fps; minimise ({reply}) -> {} frames in 2 s (sizes {}, last at +{} ms, mean luma {}); restore ({restore}) -> {:.1} fps, first frame +{} ms after restore, counter decoded {}",
        s1.fps,
        s2.frames,
        sizes(&r2),
        last_min.map_or("-".into(), |v| format!("{v:.0}")),
        if luma.is_empty() {
            "-".into()
        } else {
            luma.join(",")
        },
        s3.fps,
        first_after.map_or("-".into(), |v| format!("{v:.0}")),
        pct(s3.decoded, s3.frames),
    );
    let ok = s3.frames > 0 && s3.decoded > 0;
    Ok(Row::new(
        "C4",
        "Window minimised, then SW_RESTORE",
        if ok { "INFO" } else { "FAIL" },
        Some(s2),
        notes,
    ))
}

/// Fraction of frames where a popup rectangle shows (foreign fraction above 0.2), and the mean.
fn visible(fracs: &[f64]) -> String {
    let v: Vec<f64> = fracs.iter().copied().filter(|f| !f.is_nan()).collect();
    if v.is_empty() {
        return "n/a".into();
    }
    let shown = v.iter().filter(|&&f| f > 0.2).count();
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    format!("{shown}/{} (mean {:.2})", v.len(), mean)
}

/// C5: popup menu + tooltip, and a modal dialog, with IncludeSecondaryWindows unset/false/true.
pub fn c5(ctx: &Ctx, device: &ID3D11Device) -> Vec<Row> {
    let present = wgc::session_property_present("IncludeSecondaryWindows");
    let mut settings = vec![None];
    if present {
        settings.push(Some(false));
        settings.push(Some(true));
    }
    settings
        .into_iter()
        .map(|inc| {
            let cond = format!(
                "Popup menu + tooltip, modal dialog; IncludeSecondaryWindows={}",
                inc.map_or("unset".to_string(), |b| b.to_string())
            );
            c5_one(ctx, device, inc, present).map_or_else(
                |e| Row::new("C5", &cond, "ERROR", None, format!("{e:#}")),
                |mut r| {
                    r.condition = cond.clone();
                    r
                },
            )
        })
        .collect()
}

fn popup_rects(pid: u32, class: &str) -> Vec<RECT> {
    harness::process_windows(pid)
        .into_iter()
        .filter(|(_, c, _, _)| c == class)
        .map(|(_, _, _, r)| r)
        .collect()
}

fn c5_phase(
    ctx: &Ctx,
    cap: &Capture,
    rb: &mut Readback,
    an: &mut Analyzer,
    rects: &[(String, RECT)],
    tag: &str,
) -> Result<(Run, Vec<Vec<f64>>)> {
    let mut fr: Vec<Vec<f64>> = rects.iter().map(|_| Vec::new()).collect();
    let mut first = None;
    cap.drain();
    let run = measure::collect(&[cap], rb, Duration::from_millis(1500), |_, f, img| {
        let (c, a, b) = an.basic(img, f.size());
        if first.is_none() {
            first = Some(img.clone());
        }
        if let Some((g, layout)) = an.context(f.size()) {
            for (i, (_, r)) in rects.iter().enumerate() {
                fr[i].push(measure::foreign_fraction(img, &g, &layout, c, *r));
            }
        }
        (c, a, b, 0.0)
    })?
    .remove(0);
    if let Some(img) = &first {
        ctx.dump(tag, img);
    }
    Ok((run, fr))
}

fn c5_one(ctx: &Ctx, device: &ID3D11Device, inc: Option<bool>, present: bool) -> Result<Row> {
    let w = ctx.spawn("Gander Spike C5")?;
    let o = CaptureOptions {
        include_secondary_windows: inc,
        ..opts()
    };
    let cap = start(w.hwnd, device, &o)?;
    let applied = format!("{:?}", cap.applied().include_secondary_windows);
    let mut rb = Readback::new(device)?;
    let mut an = Analyzer::new(w.hwnd);
    let tag = format!(
        "c5-{}",
        inc.map_or("unset", |b| if b { "true" } else { "false" })
    );

    w.send("popup")?;
    w.wait_state("menu", "1", Duration::from_secs(3))?;
    std::thread::sleep(Duration::from_millis(300));
    let mut rects: Vec<(String, RECT)> = Vec::new();
    // The menu window can appear a little after the state flag flips.
    let mut menus = popup_rects(w.pid, "#32768");
    for _ in 0..30 {
        if !menus.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        menus = popup_rects(w.pid, "#32768");
    }
    for r in menus {
        rects.push(("menu".into(), r));
    }
    for r in popup_rects(w.pid, "tooltips_class32") {
        rects.push(("tooltip".into(), r));
    }
    let (run_menu, fr_menu) =
        c5_phase(ctx, &cap, &mut rb, &mut an, &rects, &format!("{tag}-menu"))?;
    w.send("dismiss")?;
    std::thread::sleep(Duration::from_millis(300));

    w.send("dialog")?;
    w.wait_state("dialog", "1", Duration::from_secs(3))?;
    std::thread::sleep(Duration::from_millis(400));
    // MessageBoxW centres on the monitor, not on the owner: move the window under the dialog so
    // the dialog overlaps the arena (the dialog does not follow its owner).
    let mut dialog_overlapped = None;
    if let Some(d) = popup_rects(w.pid, "#32770").first() {
        let wr = harness::window_rect(w.hwnd);
        dialog_overlapped =
            Some(d.left < wr.right && d.right > wr.left && d.top < wr.bottom && d.bottom > wr.top);
        let g = WindowGeometry::of(w.hwnd)?;
        let (dcx, dcy) = ((d.left + d.right) / 2, (d.top + d.bottom) / 2);
        let (ccx, ccy) = (
            g.client_origin.x + g.client_w / 2,
            g.client_origin.y + g.client_h * 2 / 3,
        );
        harness::move_window(w.hwnd, wr.left + dcx - ccx, wr.top + dcy - ccy)?;
        std::thread::sleep(Duration::from_millis(300));
    }
    let drects: Vec<(String, RECT)> = popup_rects(w.pid, "#32770")
        .into_iter()
        .map(|r| ("dialog".to_string(), r))
        .collect();
    // The window moved without resizing: start a fresh analyzer so the geometry is re-read.
    let mut an = Analyzer::new(w.hwnd);
    let (run_dlg, fr_dlg) = c5_phase(
        ctx,
        &cap,
        &mut rb,
        &mut an,
        &drects,
        &format!("{tag}-dialog"),
    )?;
    w.send("dismiss")?;
    drop(cap);
    w.quit();

    let mut parts = Vec::new();
    for (i, (name, _)) in rects.iter().enumerate() {
        parts.push(format!("{name} shown in {}", visible(&fr_menu[i])));
    }
    if rects.is_empty() {
        parts.push("menu/tooltip window not found".into());
    }
    for (i, (name, _)) in drects.iter().enumerate() {
        parts.push(format!("{name} shown in {}", visible(&fr_dlg[i])));
    }
    if drects.is_empty() {
        parts.push("dialog window not found".into());
    }
    let s = run_menu.stats();
    let sd = run_dlg.stats();
    let notes = format!(
        "property present: {present}; setter: {applied}; {}; dialog overlapped its owner where MessageBoxW put it: {dialog_overlapped:?} (window then moved under it); frames menu phase {} ({:.1} fps, sizes {}), dialog phase {} (sizes {}); a popup counts as shown when >20% of its arena area is not arena colour",
        parts.join("; "),
        s.frames,
        s.fps,
        sizes(&run_menu),
        sd.frames,
        sizes(&run_dlg),
    );
    Ok(Row::new("C5", "", "INFO", Some(s), notes))
}

/// C9: moved, then resized, mid-capture.
pub fn c9(ctx: &Ctx, device: &ID3D11Device) -> Result<Row> {
    let w = ctx.spawn("Gander Spike C9")?;
    let cap = start(w.hwnd, device, &opts())?;
    let mut rb = Readback::new(device)?;
    let mut an = Analyzer::new(w.hwnd);
    let r1 = basic_run(&cap, &mut rb, &mut an, Duration::from_secs(1))?;
    let rc = harness::window_rect(w.hwnd);
    harness::move_window(w.hwnd, rc.left + 40, rc.top + 30)?;
    let r2 = basic_run(&cap, &mut rb, &mut an, Duration::from_secs(1))?;
    let (ow, oh) = (rc.right - rc.left, rc.bottom - rc.top);
    let (nw, nh) = (ow * 5 / 4, oh * 6 / 5);
    let t0 = clock::now_100ns();
    harness::set_window_rect(w.hwnd, rc.left + 40, rc.top + 30, nw, nh)?;
    let g_new = WindowGeometry::of(w.hwnd)?;
    let r3 = basic_run(&cap, &mut rb, &mut an, ctx.dur())?;
    let resized = cap.resized();
    drop(cap);
    w.quit();
    let (s1, s2, s3) = (r1.stats(), r2.stats(), r3.stats());
    let want = g_new.frame_size();
    let first_new = r3
        .frames
        .iter()
        .find(|f| (f.size.0 as i32, f.size.1 as i32) == want);
    let follow_ms = first_new.map(|f| (f.arrival_100ns - t0) as f64 / 10_000.0);
    let after: Vec<_> = r3
        .frames
        .iter()
        .filter(|f| (f.size.0 as i32, f.size.1 as i32) == want)
        .collect();
    let dec_after = after
        .iter()
        .filter(|f| f.counter.is_some() && f.patch_a && f.patch_b)
        .count();
    let ok = s2.frames > 0 && follow_ms.is_some() && dec_after == after.len() && !after.is_empty();
    let notes = format!(
        "before {:.1} fps size {}; after move {:.1} fps size {} (counter {}); resize to {}x{} outer -> frames {} (DWM frame {}x{}); first frame at new size +{} ms; pool recreated {resized}x; frames at new size with counter+patches ok {}",
        s1.fps,
        sizes(&r1),
        s2.fps,
        sizes(&r2),
        pct(s2.decoded, s2.frames),
        nw,
        nh,
        sizes(&r3),
        want.0,
        want.1,
        follow_ms.map_or("-".into(), |v| format!("{v:.0}")),
        pct(dec_after, after.len()),
    );
    Ok(Row::new(
        "C9",
        "Window moved then resized mid-capture",
        if ok { "PASS" } else { "FAIL" },
        Some(s3),
        notes,
    ))
}

fn ring_yellow(img: &Image) -> usize {
    let mut n = 0;
    for y in 0..img.h {
        for x in 0..img.w {
            if x >= 3 && y >= 3 && x < img.w - 3 && y < img.h - 3 {
                continue;
            }
            if let Some(p) = img.at(x, y)
                && p.0 >= 200
                && p.1 >= 150
                && p.2 <= 90
            {
                n += 1;
            }
        }
    }
    n
}

/// C10: IsBorderRequired true vs false.
pub fn c10(ctx: &Ctx, device: &ID3D11Device) -> Result<Row> {
    let present = wgc::session_property_present("IsBorderRequired");
    let access = wgc::request_borderless(Duration::from_secs(5));
    let w = ctx.spawn("Gander Spike C10")?;
    let mut parts = vec![format!(
        "property present: {present}; RequestAccessAsync(Borderless): {}",
        match &access {
            BorderlessAccess::Status(4) => "Allowed (4), no prompt".to_string(),
            BorderlessAccess::Status(s) => format!("status {s}"),
            BorderlessAccess::TimedOut => "no answer in 5 s (prompt?)".into(),
            BorderlessAccess::Error(e) => format!("error {e}"),
        }
    )];
    let mut last = None;
    for border in [true, false] {
        let o = CaptureOptions {
            border_required: Some(border),
            ..opts()
        };
        let cap = start(w.hwnd, device, &o)?;
        let applied = format!("{:?}", cap.applied().border_required);
        let mut rb = Readback::new(device)?;
        let mut an = Analyzer::new(w.hwnd);
        let run = measure::collect(&[&cap], &mut rb, Duration::from_secs(1), |_, f, img| {
            let (c, a, b) = an.basic(img, f.size());
            (c, a, b, ring_yellow(img) as f64)
        })?
        .remove(0);
        let yellow: f64 = run.frames.iter().map(|f| f.extra).fold(0.0, f64::max);
        let s = run.stats();
        parts.push(format!(
            "IsBorderRequired={border}: setter {applied}, {} frames {:.1} fps, max yellow pixels in 3 px edge ring {yellow}",
            s.frames, s.fps
        ));
        last = Some(s);
    }
    w.quit();
    parts.push("on-screen border visibility needs a human eye (not read: would sample desktop pixels outside the test window)".into());
    Ok(Row::new(
        "C10",
        "IsBorderRequired = false",
        "INFO",
        last,
        parts.join("; "),
    ))
}

fn non_magenta_in_patch(img: &Image, offset: (i32, i32), r: gander_testwin::layout::Rect) -> usize {
    let mut n = 0;
    for y in r.y..r.bottom() {
        for x in r.x..r.right() {
            if img.at(x + offset.0, y + offset.1) != Some(PATCH_A_RGB) {
                n += 1;
            }
        }
    }
    n
}

/// C11: cursor parked on patch A, IsCursorCaptureEnabled true vs false.
pub fn c11(ctx: &Ctx, device: &ID3D11Device) -> Result<Row> {
    let w = ctx.spawn("Gander Spike C11")?;
    let g = WindowGeometry::of(w.hwnd)?;
    let layout = gander_testwin::layout::Layout::new(g.client_w, g.client_h, g.dpi);
    let (cx, cy) = layout.patch_a.centre();
    let target = POINT {
        x: g.client_origin.x + cx,
        y: g.client_origin.y + cy,
    };
    let saved = harness::cursor_pos();
    harness::set_cursor(target);
    std::thread::sleep(Duration::from_millis(200));
    let under = harness::window_from_point(target) == w.hwnd;
    let mut parts = vec![format!(
        "cursor parked at patch A centre; window under cursor is the test window: {under}"
    )];
    let mut last = None;
    for cursor in [true, false] {
        let o = CaptureOptions {
            cursor: Some(cursor),
            ..opts()
        };
        let result = (|| -> Result<Stats> {
            let cap = start(w.hwnd, device, &o)?;
            let applied = format!("{:?}", cap.applied().cursor);
            let mut rb = Readback::new(device)?;
            let mut an = Analyzer::new(w.hwnd);
            let run = measure::collect(&[&cap], &mut rb, Duration::from_secs(1), |_, f, img| {
                let (c, a, b) = an.basic(img, f.size());
                let n = an
                    .layout()
                    .map_or(0, |l| non_magenta_in_patch(img, an.offset, l.patch_a));
                (c, a, b, n as f64)
            })?
            .remove(0);
            let max = run.frames.iter().map(|f| f.extra).fold(0.0, f64::max);
            let s = run.stats();
            parts.push(format!(
                "IsCursorCaptureEnabled={cursor}: setter {applied}, {} frames, max non-magenta px in patch A {max}",
                s.frames
            ));
            Ok(s)
        })();
        match result {
            Ok(s) => last = Some(s),
            Err(e) => parts.push(format!("cursor={cursor}: error {e:#}")),
        }
    }
    harness::set_cursor(saved);
    w.quit();
    Ok(Row::new(
        "C11",
        "Cursor over the window",
        "INFO",
        last,
        parts.join("; "),
    ))
}

/// C15: two windows of one process, captured at the same time by HWND; title matching.
pub fn c15(ctx: &Ctx, device: &ID3D11Device) -> Result<Row> {
    let main_title = "Gander Spike C15";
    let second_title = "GanderSpikeC15-second";
    let w = ctx.spawn(main_title)?;
    let reply = w.send(&format!("open-second {second_title}"))?;
    let h2 = harness::reply_field(&reply, "hwnd")
        .and_then(|v| v.strip_prefix("0x"))
        .and_then(|v| isize::from_str_radix(v, 16).ok())
        .map(harness::hwnd)
        .with_context(|| format!("open-second reply {reply:?}"))?;
    std::thread::sleep(Duration::from_millis(300));
    let rc = harness::window_rect(w.hwnd);
    let (ow, oh) = (rc.right - rc.left, rc.bottom - rc.top);
    harness::set_window_rect(h2, rc.left + 120, rc.top + 90, ow * 4 / 5, oh * 4 / 5)?;
    std::thread::sleep(Duration::from_millis(300));

    let wins = harness::process_windows(w.pid);
    let by_title = |t: &str| {
        wins.iter()
            .filter(|(_, _, title, _)| title == t)
            .map(|(h, ..)| *h)
            .collect::<Vec<_>>()
    };
    let m = by_title(main_title);
    let s2 = by_title(second_title);
    let titles_ok = m == vec![w.hwnd] && s2 == vec![h2];

    let cap1 = start(w.hwnd, device, &opts())?;
    let cap2 = start(h2, device, &opts())?;
    let mut rb = Readback::new(device)?;
    let mut an = [Analyzer::new(w.hwnd), Analyzer::new(h2)];
    let runs = measure::collect(&[&cap1, &cap2], &mut rb, ctx.dur(), |i, f, img| {
        let (c, a, b) = an[i].basic(img, f.size());
        (c, a, b, 0.0)
    })?;
    drop(cap1);
    drop(cap2);
    let g1 = WindowGeometry::of(w.hwnd)?;
    let g2 = WindowGeometry::of(h2)?;
    w.quit();
    let (st1, st2) = (runs[0].stats(), runs[1].stats());
    let size_ok = |r: &Run, g: &WindowGeometry| {
        r.frames
            .last()
            .is_some_and(|f| (f.size.0 as i32, f.size.1 as i32) == g.frame_size())
    };
    let (ok1, ok2) = (size_ok(&runs[0], &g1), size_ok(&runs[1], &g2));
    let ok = titles_ok && ok1 && ok2 && st1.decoded > 0 && st2.decoded > 0;
    let notes = format!(
        "testwin analogue (PowerPoint not driven: boundary is test windows only); EnumWindows title match finds each HWND exactly: {titles_ok}; main {:.1} fps size {} matches own DWM frame: {ok1}, counter {}; second {:.1} fps size {} matches: {ok2}, counter {}; one device, two concurrent sessions",
        st1.fps,
        sizes(&runs[0]),
        pct(st1.decoded, st1.frames),
        st2.fps,
        sizes(&runs[1]),
        pct(st2.decoded, st2.frames),
    );
    Ok(Row::new(
        "C15",
        "Two windows of one process, by HWND",
        if ok { "PASS" } else { "FAIL" },
        Some(st1),
        notes,
    ))
}
