// SPDX-License-Identifier: Apache-2.0
//! The input matrix without a phone: what `SendInput` and `PostMessage` do to a `gander-testwin`
//! window under the conditions of matrix items C19 and C20, and how exactly a normalised tap maps
//! onto the window at 150 % and 100 % scaling (C3).
//!
//! Every experiment clicks only `gander-testwin` windows started by this process. The box is
//! aimed at from the window's own `state?` reply, so a click that lands counts as a `hit`.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use spike_webrtc::testwin::{self, Testwin};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, ScreenToClient,
};
use windows::core::BOOL;

use crate::inject::{
    self, Colour, Guard, Mapped, Policy, desktop_state, map_tap, normalise_client, post_click,
    read_colour, read_state, root_under, send_click,
};

/// A test window with its control pipe.
struct Win {
    tw: Testwin,
    hwnd: HWND,
    pipe: String,
}

impl Win {
    fn spawn(exe: &std::path::Path, name: &str, x: i32, y: i32, w: i32, h: i32) -> Result<Win> {
        let tw = Testwin::spawn(exe, name, 600)?;
        let hwnd = tw.hwnd;
        let pipe = tw.pipe().to_owned();
        testwin::place(hwnd, x, y, w, h)?;
        std::thread::sleep(Duration::from_millis(600));
        Ok(Win { tw, hwnd, pipe })
    }

    fn colour(&self) -> Result<Colour> {
        read_colour(&self.pipe)
    }

    fn quit(self) {
        self.tw.quit();
    }
}

/// One row of the result table.
struct Row {
    id: &'static str,
    what: String,
    outcome: String,
}

fn delta(a: Colour, b: Colour) -> (u32, u32) {
    (b.hits - a.hits, b.clicks - a.clicks)
}

/// Aims at the box centre as the window currently shows it; returns the normalised point.
fn aim_at_box(w: &Win) -> Result<(f64, f64)> {
    let s = read_state(&w.pipe)?;
    let (x, y, bw, bh) = s.rect;
    normalise_client(w.hwnd, x + bw / 2, y + bh / 2)
}

/// (hits, clicks) delta of one window.
type Deltas = (u32, u32);

/// Deltas of the target, deltas of the other window, distinct injector outcomes.
type SeriesOut = (Deltas, Deltas, Vec<String>);

/// Runs `rounds` injections at the box and returns (hits, clicks) deltas of `w` and of `other`,
/// plus the text of every distinct outcome the injector reported.
fn series(
    w: &Win,
    other: Option<&Win>,
    rounds: u32,
    mut inject: impl FnMut(&Mapped) -> String,
) -> Result<SeriesOut> {
    let a0 = w.colour()?;
    let b0 = other.map(Win::colour).transpose()?;
    let mut outcomes: Vec<String> = Vec::new();
    for _ in 0..rounds {
        let (nx, ny) = aim_at_box(w)?;
        let m = map_tap(w.hwnd, nx, ny)?;
        let o = inject(&m);
        if !outcomes.contains(&o) {
            outcomes.push(o);
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    let a1 = w.colour()?;
    let db = match (other, b0) {
        (Some(o), Some(b0)) => delta(b0, o.colour()?),
        _ => (0, 0),
    };
    Ok((delta(a0, a1), db, outcomes))
}

fn monitors() -> Vec<(RECT, bool)> {
    unsafe extern "system" fn cb(m: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
        // SAFETY: `lp` is the address of the Vec passed below, alive for the whole enumeration.
        let v = unsafe { &mut *(lp.0 as *mut Vec<(RECT, bool)>) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: valid out struct with its size set.
        if unsafe { GetMonitorInfoW(m, &mut info) }.as_bool() {
            v.push((info.rcWork, info.dwFlags & 1 != 0));
        }
        BOOL(1)
    }
    let mut v: Vec<(RECT, bool)> = Vec::new();
    // SAFETY: the callback only touches the Vec behind the pointer, which outlives the call.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(cb),
            LPARAM((&mut v as *mut Vec<_>) as isize),
        );
    }
    v
}

/// The foreground window at the time of the call, for the row.
fn fg() -> String {
    inject::foreground().map_or("foreground: none".to_owned(), |(_, class, pid)| {
        format!("foreground: {class} pid {pid}")
    })
}

fn describe_post(o: &inject::PostOutcome) -> String {
    match o.error {
        None => format!("PostMessage accepted {}/3", o.posted),
        Some(e) => format!("PostMessage accepted {}/3, error {e}", o.posted),
    }
}

fn describe_send(o: &inject::SendOutcome) -> String {
    let guard = match &o.guard {
        Guard::Ok => "guard ok".to_owned(),
        Guard::NoWindow => "guard: no window under the point".to_owned(),
        Guard::Covered { class, pid, .. } => {
            format!("guard: covered by {class} pid {pid}, NOT sent")
        }
    };
    match o.accepted {
        None => guard,
        Some(n) => {
            let moved = match (o.cursor_before, o.cursor_after) {
                (Some(b), Some(a)) => a.x != b.x || a.y != b.y,
                _ => false,
            };
            format!(
                "{guard}; SendInput accepted {n}/3{}; real cursor moved by the call: {moved} (put back afterwards)",
                o.error
                    .map_or(String::new(), |e| format!(", GetLastError {e}")),
            )
        }
    }
}

/// Mapping accuracy at the window's current DPI: normalise known client pixels, map them back,
/// compare with the OS's own `ScreenToClient`, and post the click and read `last_click`.
fn mapping(w: &Win) -> Result<String> {
    let layout = testwin::pipe_send(&w.pipe, "layout?")?;
    let lm = inject::parse_reply(&layout);
    let client = lm.get("client").cloned().unwrap_or_default();
    let (cw, ch) = client
        .split_once('x')
        .and_then(|(a, b)| Some((a.parse::<i32>().ok()?, b.parse::<i32>().ok()?)))
        .context("no client size in layout?")?;
    let dpi = lm.get("dpi").cloned().unwrap_or_default();
    let mut worst_round_trip = 0;
    let mut worst_os = 0;
    let mut worst_last_click = 0;
    let mut covered = 0;
    let mut covered_by = String::new();
    let mut n = 0;
    // Interior points, plus the first and last pixel of the client area.
    let fracs = [0.0, 0.1, 0.5, 0.9, 1.0];
    for fy in fracs {
        for fx in fracs {
            let cx = ((f64::from(cw - 1) * fx).round() as i32).clamp(0, cw - 1);
            let cy = ((f64::from(ch - 1) * fy).round() as i32).clamp(0, ch - 1);
            let (nx, ny) = normalise_client(w.hwnd, cx, cy)?;
            let m = map_tap(w.hwnd, nx, ny)?;
            worst_round_trip = worst_round_trip
                .max((m.client.x - cx).abs())
                .max((m.client.y - cy).abs());
            let mut p = m.screen;
            // SAFETY: valid point, valid window.
            let _ = unsafe { ScreenToClient(w.hwnd, &mut p) };
            worst_os = worst_os
                .max((p.x - m.client.x).abs())
                .max((p.y - m.client.y).abs());
            if root_under(m.screen) != Some(w.hwnd) {
                covered += 1;
                if covered_by.is_empty() {
                    covered_by =
                        root_under(m.screen).map_or("no window".to_owned(), inject::class_of);
                }
            }
            post_click(w.hwnd, m.client);
            std::thread::sleep(Duration::from_millis(40));
            let s = read_state(&w.pipe)?;
            if let Some((lx, ly)) = s.last_click {
                worst_last_click = worst_last_click
                    .max((lx - m.client.x).abs())
                    .max((ly - m.client.y).abs());
            } else {
                worst_last_click = i32::MAX;
            }
            n += 1;
        }
    }
    Ok(format!(
        "dpi {dpi}, client {cw}x{ch}: {n} points; normalise->map round trip worst {worst_round_trip} px; map vs OS ScreenToClient worst {worst_os} px; posted vs testwin last_click worst {worst_last_click} px; points whose screen position is not over the window {covered}{}",
        if covered == 0 {
            String::new()
        } else {
            format!(" (top window there: {covered_by})")
        }
    ))
}

/// Runs the whole matrix. Exit code 0 when no experiment failed to run.
pub fn run(results: Option<PathBuf>) -> Result<i32> {
    let exe = testwin::exe(None)?;
    let mut rows: Vec<Row> = Vec::new();
    let mut report = String::new();
    let state = desktop_state();
    writeln!(report, "desktop state at start: {state}")?;
    println!("desktop state at start: {state}");

    let mons = monitors();
    let primary = mons
        .iter()
        .find(|m| m.1)
        .map(|m| m.0)
        .context("no primary monitor")?;
    let secondary = mons.iter().find(|m| !m.1).map(|m| m.0);
    writeln!(
        report,
        "monitors (work areas): {:?}",
        mons.iter()
            .map(|m| (m.0.left, m.0.top, m.0.right, m.0.bottom, m.1))
            .collect::<Vec<_>>()
    )?;

    // Window A: the target, on the primary monitor.
    let (aw, ah) = testwin::fit(primary, 1080, 2280, 16);
    let a = Win::spawn(&exe, "wp44-A", primary.left + 16, primary.top + 16, aw, ah)?;
    let g = spike_capture::wgc::WindowGeometry::of(a.hwnd)?;
    writeln!(
        report,
        "A: frame {:?} client origin ({}, {}) client {}x{} dpi {}",
        (g.frame.left, g.frame.top, g.frame.right, g.frame.bottom),
        g.client_origin.x,
        g.client_origin.y,
        g.client_w,
        g.client_h,
        g.dpi
    )?;
    writeln!(
        report,
        "A is foreground: {}",
        inject::foreground().is_some_and(|f| f.0 == a.hwnd.0 as isize)
    )?;

    // C20: PostMessage, window not foreground, nothing covering it.
    let (ah_, _, o) = series(&a, None, 5, |m| {
        describe_post(&post_click(a.hwnd, m.client))
    })?;
    rows.push(Row {
        id: "C20 testwin",
        what: "PostMessage (move, down, up) to the HWND at the box centre, 5 rounds".into(),
        outcome: format!(
            "hits +{} / clicks +{}; {}; {}",
            ah_.0,
            ah_.1,
            o.join("; "),
            fg()
        ),
    });

    // C19 probe: does the system accept injected mouse input at all right now? Events that change
    // nothing (a zero relative move, an absolute move onto the current pixel); no click, so no
    // window can be hit.
    let p = inject::probe_send_input();
    rows.push(Row {
        id: "C19 probe",
        what: "SendInput with no-op mouse moves (relative (0,0); absolute onto the cursor's pixel), no click".into(),
        outcome: format!(
            "relative move accepted {}/1{}; absolute move accepted {:?}/1{}; cursor {:?} -> {:?}",
            p.relative,
            p.relative_error.map_or(String::new(), |e| format!(" (GetLastError {e})")),
            p.absolute,
            p.absolute_error.map_or(String::new(), |e| format!(" (GetLastError {e})")),
            p.cursor.0.map(|c| (c.x, c.y)),
            p.cursor.1.map(|c| (c.x, c.y)),
        ),
    });

    // C19: SendInput, nothing covering the window.
    let (ah_, _, o) = series(&a, None, 5, |m| {
        describe_send(&send_click(a.hwnd, m.screen, Policy::TargetOnly))
    })?;
    rows.push(Row {
        id: "C19 uncovered",
        what: "SendInput (absolute move, left down, left up) at the box centre, 5 rounds, nothing covering the window".into(),
        outcome: format!("A hits +{} / clicks +{}; {}; {}", ah_.0, ah_.1, o.join("; "), fg()),
    });
    let st = desktop_state();
    writeln!(report, "after SendInput: {st}")?;

    // Occluder B over A: same class, topmost, above A.
    let b = Win::spawn(
        &exe,
        "wp44-B-occluder",
        primary.left + 16,
        primary.top + 16,
        aw,
        ah,
    )?;
    let probe = {
        let (nx, ny) = aim_at_box(&a)?;
        map_tap(a.hwnd, nx, ny)?
    };
    let top = root_under(probe.screen);
    writeln!(
        report,
        "occluder: root window under A's box centre is {} (A {:#x}, B {:#x})",
        top.map_or("none".to_owned(), |h| format!("{:#x}", h.0 as isize)),
        a.hwnd.0 as isize,
        b.hwnd.0 as isize
    )?;

    let (ah_, bh_, o) = series(&a, Some(&b), 5, |m| {
        describe_send(&send_click(a.hwnd, m.screen, Policy::TargetOnly))
    })?;
    rows.push(Row {
        id: "C19 covered, guard",
        what: "SendInput at A's box centre while test window B covers it, production policy (target only)".into(),
        outcome: format!("A hits +{} / clicks +{}, B clicks +{}; {}; {}", ah_.0, ah_.1, bh_.1, o.join("; "), fg()),
    });

    let (ah_, bh_, o) = series(&a, Some(&b), 5, |m| {
        describe_send(&send_click(a.hwnd, m.screen, Policy::AnyTestwin))
    })?;
    rows.push(Row {
        id: "C19 covered, forced",
        what: "SendInput at A's box centre while B covers it, guard relaxed to any test window (where do clicks land)".into(),
        outcome: format!("A hits +{} / clicks +{}, B clicks +{} (B hits +{}); {}; {}", ah_.0, ah_.1, bh_.1, bh_.0, o.join("; "), fg()),
    });

    let (ah_, bh_, o) = series(&a, Some(&b), 5, |m| {
        describe_post(&post_click(a.hwnd, m.client))
    })?;
    rows.push(Row {
        id: "C20 covered",
        what: "PostMessage to A at its box centre while B covers it".into(),
        outcome: format!(
            "A hits +{} / clicks +{}, B clicks +{}; {}; {}",
            ah_.0,
            ah_.1,
            bh_.1,
            o.join("; "),
            fg()
        ),
    });
    b.quit();

    // Mapping at the primary monitor's DPI.
    let map1 = mapping(&a)?;
    rows.push(Row {
        id: "C3 primary",
        what: "normalised tap -> window mapping, 25 points, primary monitor".into(),
        outcome: map1,
    });

    // Secondary monitor, another DPI.
    if let Some(sec) = secondary {
        let (sw, sh) = testwin::fit(sec, 1080, 2280, 16);
        testwin::place(a.hwnd, sec.left + 16, sec.top + 16, sw, sh)?;
        std::thread::sleep(Duration::from_millis(900));
        let g = spike_capture::wgc::WindowGeometry::of(a.hwnd)?;
        writeln!(
            report,
            "A on the secondary monitor: frame {:?} dpi {}",
            (g.frame.left, g.frame.top, g.frame.right, g.frame.bottom),
            g.dpi
        )?;
        let map2 = mapping(&a)?;
        rows.push(Row {
            id: "C3 secondary",
            what: "same, after moving A to the secondary monitor".into(),
            outcome: map2,
        });
        let (ah_, _, o) = series(&a, None, 5, |m| {
            describe_post(&post_click(a.hwnd, m.client))
        })?;
        rows.push(Row {
            id: "C20 secondary",
            what: "PostMessage at the box centre on the secondary monitor, 5 rounds".into(),
            outcome: format!(
                "hits +{} / clicks +{}; {}; {}",
                ah_.0,
                ah_.1,
                o.join("; "),
                fg()
            ),
        });
    } else {
        writeln!(report, "no secondary monitor: second DPI not measured")?;
    }
    a.quit();

    println!("\n| Id | What | Outcome |\n| --- | --- | --- |");
    writeln!(report, "\n| Id | What | Outcome |\n| --- | --- | --- |")?;
    for r in &rows {
        let line = format!("| {} | {} | {} |", r.id, r.what, r.outcome);
        println!("{line}");
        writeln!(report, "{line}")?;
    }
    let end = desktop_state();
    println!("\ndesktop state at end: {end}");
    writeln!(report, "\ndesktop state at end: {end}")?;
    if let Some(dir) = results {
        std::fs::create_dir_all(&dir)?;
        let state = if inject::session_locked() == Some(true) {
            "locked"
        } else {
            "unlocked"
        };
        std::fs::write(dir.join(format!("input-matrix-{state}.txt")), &report)?;
    }
    Ok(0)
}
