// SPDX-License-Identifier: Apache-2.0
//! The adb-driven tap test: aims `adb shell input tap` at the moving box on the emulator screen,
//! and reads the phone's own event log (`adb logcat`, tag `GanderSpike`) to pair every tap with
//! the time the phone saw the new colour in the decoded stream.
//!
//! The aim uses the host's knowledge of the box (`state?` over the test window's pipe) and the
//! phone's reported picture rectangle, and predicts where the box will be when the touch lands
//! (`adb shell input tap` takes a few hundred ms to start, which the driver measures and then
//! compensates for). A tap that still misses is counted, not retried: the hit rate is a result.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use spike_webrtc::metrics::{now_100ns, pct};
use spike_webrtc::testwin::pipe_send;

use crate::inject::{normalise_client, parse_reply, read_state};
use crate::serve::TapCtx;

/// Events the phone logged, in arrival order.
#[derive(Debug, Default)]
struct Phone {
    events: Mutex<Vec<(Instant, Value)>>,
    raw: Mutex<Vec<String>>,
}

impl Phone {
    fn find_last(&self, ev: &str) -> Option<Value> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .rev()
            .find(|(_, v)| v.get("ev").and_then(Value::as_str) == Some(ev))
            .map(|(_, v)| v.clone())
    }

    fn find_seq(&self, ev: &str, seq: u64) -> Option<Value> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .rev()
            .find(|(_, v)| {
                v.get("ev").and_then(Value::as_str) == Some(ev)
                    && v.get("seq").and_then(Value::as_u64) == Some(seq)
            })
            .map(|(_, v)| v.clone())
    }

    fn count(&self, ev: &str) -> usize {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, v)| v.get("ev").and_then(Value::as_str) == Some(ev))
            .count()
    }
}

/// `adb` with `-s <serial>` when a device is named (so a second emulator is never addressed).
fn adb_cmd(adb: &Path, serial: Option<&str>) -> Command {
    let mut c = Command::new(adb);
    if let Some(s) = serial {
        c.args(["-s", s]);
    }
    c
}

fn spawn_logcat(
    adb: &Path,
    serial: Option<&str>,
    phone: &Arc<Phone>,
    stop: &Arc<AtomicBool>,
) -> Result<Child> {
    let mut child = adb_cmd(adb, serial)
        .args(["logcat", "-s", "GanderSpike:I"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn adb logcat")?;
    let out = child.stdout.take().context("no logcat stdout")?;
    let phone = phone.clone();
    let stop = stop.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines() {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let Ok(line) = line else { break };
            // threadtime: `11-02 11:05:00.123  1234  1250 I GanderSpike: {json}`; brief:
            // `I/GanderSpike( 1234): {json}`. Take the JSON object after the tag either way.
            let Some(i) = line.find("GanderSpike") else {
                continue;
            };
            let Some(j) = line[i..].find('{') else {
                continue;
            };
            let body = line[i + j..].trim();
            if let Ok(v) = serde_json::from_str::<Value>(body) {
                phone
                    .raw
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(body.to_owned());
                phone
                    .events
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((Instant::now(), v));
            }
        }
    });
    Ok(child)
}

/// Folds a position that moves linearly and reflects at `lo` and `hi` (a triangle wave).
fn reflect(p: f64, lo: f64, hi: f64) -> f64 {
    let w = (hi - lo).max(1.0);
    let t = (p - lo).rem_euclid(2.0 * w);
    lo + if t > w { 2.0 * w - t } else { t }
}

/// Box rectangle (x, y, w, h) and its velocity in px per frame, from three samples 25 ms apart;
/// retried until the two velocities agree (a bounce in between makes them differ).
type BoxSample = ((i32, i32, i32, i32), u32, f64, f64);

fn sample_box(pipe: &str) -> Result<BoxSample> {
    for _ in 0..40 {
        let s0 = read_state(pipe)?;
        std::thread::sleep(Duration::from_millis(25));
        let s1 = read_state(pipe)?;
        std::thread::sleep(Duration::from_millis(25));
        let s2 = read_state(pipe)?;
        let (df01, df12) = (
            f64::from(s1.frame) - f64::from(s0.frame),
            f64::from(s2.frame) - f64::from(s1.frame),
        );
        if df01 < 1.0 || df12 < 1.0 {
            continue;
        }
        let v01 = (
            f64::from(s1.rect.0 - s0.rect.0) / df01,
            f64::from(s1.rect.1 - s0.rect.1) / df01,
        );
        let v12 = (
            f64::from(s2.rect.0 - s1.rect.0) / df12,
            f64::from(s2.rect.1 - s1.rect.1) / df12,
        );
        if (v01.0 - v12.0).abs() < 0.4 && (v01.1 - v12.1).abs() < 0.4 {
            return Ok((s2.rect, s2.frame, v12.0, v12.1));
        }
    }
    bail!("could not get a steady box velocity")
}

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64)
}

/// Runs `count` aimed taps and writes the paired results.
pub fn run(
    adb: &Path,
    serial: Option<&str>,
    count: u32,
    ctx: &Arc<TapCtx>,
    results: Option<&Path>,
    label: &str,
) -> Result<()> {
    let phone = Arc::new(Phone::default());
    let stop = Arc::new(AtomicBool::new(false));
    let mut logcat = spawn_logcat(adb, serial, &phone, &stop)?;
    let r = drive(adb, serial, count, ctx, &phone, results, label);
    stop.store(true, Ordering::Relaxed);
    let _ = logcat.kill();
    let _ = logcat.wait();
    r
}

fn drive(
    adb: &Path,
    serial: Option<&str>,
    count: u32,
    ctx: &Arc<TapCtx>,
    phone: &Arc<Phone>,
    results: Option<&Path>,
    label: &str,
) -> Result<()> {
    // Wait for the phone: a layout with a picture rectangle and 30 decoded frames.
    let ready_by = Instant::now() + Duration::from_secs(90);
    let layout = loop {
        let l = phone
            .find_last("layout")
            .filter(|l| num(l, "pic_w").is_some_and(|w| w > 0.0));
        let decoded = phone
            .find_last("inbound_video")
            .and_then(|v| num(&v, "frames_decoded"))
            .unwrap_or(0.0);
        if let Some(l) = l
            && decoded >= 30.0
        {
            break l;
        }
        if Instant::now() > ready_by {
            bail!("the phone never reported a layout and 30 decoded frames");
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    println!("driver: phone layout {layout}");
    let view_x = num(&layout, "view_x").unwrap_or(0.0);
    let view_y = num(&layout, "view_y").unwrap_or(0.0);
    let (pl, pt, pw, ph) = (
        num(&layout, "pic_l").unwrap_or(0.0),
        num(&layout, "pic_t").unwrap_or(0.0),
        num(&layout, "pic_w").unwrap_or(1.0),
        num(&layout, "pic_h").unwrap_or(1.0),
    );
    let (hwnd, pipe) = ctx.window().context("no test window")?;
    let lm = parse_reply(&pipe_send(&pipe, "layout?")?);
    let arena: Vec<f64> = lm
        .get("arena")
        .context("no arena in layout?")?
        .split(',')
        .map(|v| v.parse().unwrap_or(0.0))
        .collect();
    let (ax, ay, aw, ah) = (arena[0], arena[1], arena[2], arena[3]);

    let mut lags: Vec<f64> = Vec::new();
    let mut lag_ms = 350.0;
    let mut issued: Vec<(f64, f64)> = Vec::new(); // the client centre each tap was aimed at
    for i in 0..count {
        let (rect, _frame, vx, vy) = sample_box(&pipe)?;
        let ahead = lag_ms / 1000.0 * 60.0;
        let (bw, bh) = (f64::from(rect.2), f64::from(rect.3));
        let px = reflect(f64::from(rect.0) + vx * ahead, ax + 1.0, ax + aw - bw - 1.0);
        let py = reflect(f64::from(rect.1) + vy * ahead, ay + 1.0, ay + ah - bh - 1.0);
        let (cx, cy) = (px + bw / 2.0, py + bh / 2.0);
        let (nx, ny) = normalise_client(hwnd, cx.round() as i32, cy.round() as i32)?;
        let sx = (view_x + pl + nx * pw).round() as i32;
        let sy = (view_y + pt + ny * ph).round() as i32;
        let known = ctx.taps.lock().unwrap_or_else(|e| e.into_inner()).len();
        let t_cmd = now_100ns();
        let mut child = adb_cmd(adb, serial)
            .args(["shell", "input", "tap", &sx.to_string(), &sy.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("spawn adb shell input tap")?;
        issued.push((cx, cy));
        // The host's tap hook records the tap when it arrives.
        let by = Instant::now() + Duration::from_secs(6);
        let rec = loop {
            {
                let g = ctx.taps.lock().unwrap_or_else(|e| e.into_inner());
                if g.len() > known {
                    break Some(g[known].clone());
                }
            }
            if Instant::now() > by {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if let Some(rec) = &rec {
            let l = (rec.rx - t_cmd) as f64 / 10_000.0;
            lags.push(l);
            lag_ms = pct(&lags, 50.0);
            // Let the phone report the changed pixels, then move on.
            let by = Instant::now() + Duration::from_millis(1500);
            while phone.find_seq("tap_effect", rec.seq).is_none() && Instant::now() < by {
                std::thread::sleep(Duration::from_millis(20));
            }
            println!(
                "driver: tap {i} at screen ({sx}, {sy}) aimed at client ({cx:.0}, {cy:.0}); host rx {l:.0} ms after the command; phone tap_effect {}",
                phone
                    .find_seq("tap_effect", rec.seq)
                    .and_then(|v| num(&v, "ms"))
                    .map_or("none".to_owned(), |m| format!("{m:.0} ms"))
            );
        } else {
            println!("driver: tap {i} at screen ({sx}, {sy}): the host never received it");
        }
        let _ = child.wait();
        std::thread::sleep(Duration::from_millis(900));
    }
    std::thread::sleep(Duration::from_millis(1500));

    // Summary.
    let taps = ctx.taps.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut s = String::new();
    use std::fmt::Write as _;
    let _ = writeln!(
        s,
        "# adb-driven taps ({label}): {count} issued, {} received by the host",
        taps.len()
    );
    let _ = writeln!(
        s,
        "| seq | aimed client | mode | clicked | hit | host inject->colour ms | phone tap->pixels ms | echo rtt ms |"
    );
    let _ = writeln!(s, "| --- | --- | --- | --- | --- | --- | --- | --- |");
    let mut phone_ms: Vec<f64> = Vec::new();
    let mut host_ms: Vec<f64> = Vec::new();
    let mut hits = 0;
    for (n, t) in taps.iter().enumerate() {
        let eff = phone
            .find_seq("tap_effect", t.seq)
            .and_then(|v| num(&v, "ms"));
        let echo = phone
            .find_seq("echo", t.seq)
            .and_then(|v| num(&v, "rtt_ms"));
        if let Some(e) = eff {
            phone_ms.push(e);
        }
        let h = t.effect.map(|e| (e - t.inject_end) as f64 / 10_000.0);
        if let Some(h) = h {
            host_ms.push(h);
        }
        if t.hit {
            hits += 1;
        }
        let aimed = issued
            .get(n)
            .map_or(String::new(), |i| format!("({:.0}, {:.0})", i.0, i.1));
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            t.seq,
            aimed,
            t.mode,
            t.clicked,
            t.hit,
            h.map_or("-".into(), |v| format!("{v:.1}")),
            eff.map_or("-".into(), |v| format!("{v:.0}")),
            echo.map_or("-".into(), |v| format!("{v:.1}"))
        );
    }
    let _ = writeln!(
        s,
        "hits {hits}/{} taps; phone tap->changed pixels p50 {:.0} / p95 {:.0} / max {:.0} ms (n={}); host inject->colour change (pipe poll) p50 {:.1} ms (n={}); command->host rx p50 {:.0} ms",
        taps.len(),
        pct(&phone_ms, 50.0),
        pct(&phone_ms, 95.0),
        pct(&phone_ms, 100.0),
        phone_ms.len(),
        pct(&host_ms, 50.0),
        host_ms.len(),
        pct(&lags, 50.0)
    );
    // Phone-side stream stats from the stats events: first/last `inbound_video` of the run.
    let evs = phone
        .events
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let inbound: Vec<&Value> = evs
        .iter()
        .filter(|(_, v)| v.get("ev").and_then(Value::as_str) == Some("inbound_video"))
        .map(|(_, v)| v)
        .collect();
    if let (Some(first), Some(last)) = (inbound.first(), inbound.last()) {
        let dt = (num(last, "t_ns").unwrap_or(0.0) - num(first, "t_ns").unwrap_or(0.0)) / 1e9;
        let df = num(last, "frames_decoded").unwrap_or(0.0)
            - num(first, "frames_decoded").unwrap_or(0.0);
        let _ = writeln!(
            s,
            "phone decoded {df:.0} frames in {dt:.1} s = {:.1} fps; last stats: {last}",
            if dt > 0.0 { df / dt } else { f64::NAN }
        );
    }
    let _ = writeln!(
        s,
        "phone events: {} total, {} echo, {} tap_effect, {} colour_change",
        evs.len(),
        phone.count("echo"),
        phone.count("tap_effect"),
        phone.count("colour_change")
    );
    print!("\n{s}");
    if let Some(dir) = results {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join(format!("{label}-driver.txt")), &s)?;
        let raw = phone
            .raw
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .join("\n");
        std::fs::write(dir.join(format!("{label}-phone-events.jsonl")), raw)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::reflect;

    #[test]
    fn reflect_folds_a_triangle_wave() {
        assert_eq!(reflect(5.0, 0.0, 10.0), 5.0);
        assert_eq!(reflect(12.0, 0.0, 10.0), 8.0);
        assert_eq!(reflect(-3.0, 0.0, 10.0), 3.0);
        assert_eq!(reflect(23.0, 0.0, 10.0), 3.0);
    }
}
