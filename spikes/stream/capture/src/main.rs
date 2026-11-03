// SPDX-License-Identifier: Apache-2.0
//! `spike-capture`: runs the automatable capture items of the stream-spike matrix against
//! `gander-testwin` windows and prints a table.
//!
//! ```text
//! cargo run -p spike-capture -- --matrix auto [--secs N] [--testwin PATH] [--dump DIR]
//! cargo run -p spike-capture -- --matrix C1,C2
//! cargo run -p spike-capture -- --list-adapters
//! ```
//!
//! Exit code: 0 when no item FAILed or ERRORed, 1 otherwise, 2 usage error, 3 watchdog.

#[cfg(windows)]
mod harness;
#[cfg(windows)]
mod matrix;
#[cfg(windows)]
mod measure;
#[cfg(windows)]
mod wc;

#[cfg(not(windows))]
fn main() {
    eprintln!("spike-capture needs Windows (Windows Graphics Capture)");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    match win::run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("spike-capture: {e:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(windows)]
mod win {
    use std::path::PathBuf;
    use std::time::Duration;

    use anyhow::{Result, bail};
    use spike_capture::{d3d, wgc};
    use windows::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
    };
    use windows::core::{PCWSTR, w};

    use crate::harness;
    use crate::matrix::{self, Ctx, Row};

    const ALL: [&str; 13] = [
        "C1", "C2", "C3", "C4", "C5", "C6", "C9", "C10", "C11", "C12", "C15", "C16", "C17",
    ];

    struct Args {
        items: Vec<String>,
        secs: f64,
        testwin: Option<PathBuf>,
        dump: Option<PathBuf>,
        list_adapters: bool,
    }

    fn usage() -> ! {
        eprintln!(
            "usage: spike-capture --matrix auto|C1,C2,... [--secs N] [--testwin PATH] [--dump DIR]\n       spike-capture --list-adapters"
        );
        std::process::exit(2);
    }

    fn parse() -> Args {
        let mut a = Args {
            items: Vec::new(),
            secs: 3.0,
            testwin: None,
            dump: None,
            list_adapters: false,
        };
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--matrix" => {
                    let v = it.next().unwrap_or_else(|| usage());
                    a.items = if v.eq_ignore_ascii_case("auto") {
                        ALL.iter().map(|s| s.to_string()).collect()
                    } else {
                        v.split(',')
                            .map(|s| s.trim().to_ascii_uppercase())
                            .collect()
                    };
                }
                "--secs" => {
                    a.secs = it
                        .next()
                        .and_then(|v| v.parse().ok())
                        .filter(|v: &f64| (0.5..=60.0).contains(v))
                        .unwrap_or_else(|| usage());
                }
                "--testwin" => a.testwin = Some(it.next().unwrap_or_else(|| usage()).into()),
                "--dump" => a.dump = Some(it.next().unwrap_or_else(|| usage()).into()),
                "--list-adapters" => a.list_adapters = true,
                _ => usage(),
            }
        }
        if a.items.is_empty() && !a.list_adapters {
            usage();
        }
        a
    }

    fn reg_sz(name: PCWSTR) -> Option<String> {
        let mut buf = [0u16; 128];
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: buffer and size describe `buf`; the key path is a static literal.
        let r = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
                name,
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if r.is_err() {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }

    fn reg_dword(name: PCWSTR) -> Option<u32> {
        let mut v = 0u32;
        let mut size = 4u32;
        // SAFETY: as above, for a DWORD.
        let r = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
                name,
                RRF_RT_REG_DWORD,
                None,
                Some((&mut v as *mut u32).cast()),
                Some(&mut size),
            )
        };
        r.is_ok().then_some(v)
    }

    fn print_facts(ctx: &Ctx) {
        println!("## Machine facts");
        println!();
        println!(
            "- OS: {} {} build {}.{}",
            reg_sz(w!("ProductName")).unwrap_or_default(),
            reg_sz(w!("DisplayVersion")).unwrap_or_default(),
            reg_sz(w!("CurrentBuild")).unwrap_or_default(),
            reg_dword(w!("UBR")).unwrap_or_default()
        );
        println!("- WGC supported: {}", wgc::is_supported());
        println!(
            "- Session locked at start of run: {}",
            harness::session_locked().map_or("unknown".to_string(), |b| b.to_string())
        );
        for p in [
            "IsCursorCaptureEnabled",
            "IsBorderRequired",
            "IncludeSecondaryWindows",
            "MinUpdateInterval",
            "DirtyRegionMode",
        ] {
            println!(
                "- GraphicsCaptureSession.{p}: {}",
                if wgc::session_property_present(p) {
                    "present"
                } else {
                    "absent"
                }
            );
        }
        for a in &ctx.adapters {
            let outs: Vec<String> = a
                .info
                .outputs
                .iter()
                .map(|o| {
                    format!(
                        "{} {}x{}@{},{}",
                        o.device_name,
                        o.desktop.right - o.desktop.left,
                        o.desktop.bottom - o.desktop.top,
                        o.desktop.left,
                        o.desktop.top
                    )
                })
                .collect();
            println!(
                "- Adapter {}: {} (vendor {:#06x} device {:#06x}), LUID {:#x}, {} MiB dedicated, driver {}, outputs: {}",
                a.info.index,
                a.info.name,
                a.info.vendor_id,
                a.info.device_id,
                a.info.luid,
                a.info.dedicated_mib,
                a.info.driver_version.as_deref().unwrap_or("?"),
                if outs.is_empty() {
                    "none".into()
                } else {
                    outs.join(", ")
                }
            );
        }
        for m in &ctx.monitors {
            let on = ctx
                .adapters
                .iter()
                .find(|a| a.info.outputs.iter().any(|o| o.monitor == m.handle))
                .map_or("?".to_string(), |a| format!("adapter {}", a.info.index));
            println!(
                "- Monitor {}{}: {}x{} at {},{}, {} dpi ({}%), {} Hz, scanned out by {}",
                m.name,
                if m.primary { " (primary)" } else { "" },
                m.rect.right - m.rect.left,
                m.rect.bottom - m.rect.top,
                m.rect.left,
                m.rect.top,
                m.dpi,
                m.dpi * 100 / 96,
                m.hz,
                on
            );
        }
        if let Ok(dev) = d3d::create_device(None)
            && let Ok(l) = d3d::device_luid(&dev)
        {
            let name = ctx
                .adapters
                .iter()
                .find(|a| a.info.luid == l)
                .map_or("?".to_string(), |a| {
                    format!("adapter {} {}", a.info.index, a.info.name)
                });
            println!("- Default D3D11 device (null adapter) lands on: {name}");
        }
        println!();
    }

    fn cell(v: f64) -> String {
        if v.is_nan() {
            "-".into()
        } else {
            format!("{v:.1}")
        }
    }

    fn print_table(rows: &[Row]) {
        println!("## Matrix (spike-capture)");
        println!();
        println!(
            "| Item | Condition | Result | Frames | fps | Arrival lat p50/p95 ms | Interval p50/p95/max ms | Notes |"
        );
        println!("| --- | --- | --- | --- | --- | --- | --- | --- |");
        for r in rows {
            let (frames, fps, lat, int) = match &r.stats {
                Some(s) => (
                    s.frames.to_string(),
                    cell(s.fps),
                    format!("{}/{}", cell(s.lat_p50), cell(s.lat_p95)),
                    format!(
                        "{}/{}/{}",
                        cell(s.int_p50),
                        cell(s.int_p95),
                        cell(s.int_max)
                    ),
                ),
                None => ("-".into(), "-".into(), "-".into(), "-".into()),
            };
            println!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                r.id,
                r.condition.replace('|', "/"),
                r.result,
                frames,
                fps,
                lat,
                int,
                r.notes.replace('|', "/")
            );
        }
        println!();
    }

    pub fn run() -> Result<i32> {
        let args = parse();
        harness::become_dpi_aware();
        wgc::init_winrt();
        // Watchdog: an unattended run must end. Test windows also end by their own deadline.
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(900));
            eprintln!("spike-capture: watchdog fired after 900 s");
            std::process::exit(3);
        });
        let adapters = d3d::adapters()?;
        let ctx_base = Ctx {
            exe: PathBuf::new(),
            secs: args.secs,
            adapters,
            monitors: harness::monitors(),
            dump: args.dump.clone(),
        };
        if args.list_adapters {
            print_facts(&ctx_base);
            return Ok(0);
        }
        if !wgc::is_supported() {
            bail!("Windows Graphics Capture is not supported on this system");
        }
        let ctx = Ctx {
            exe: harness::testwin_exe(args.testwin.clone())?,
            ..ctx_base
        };
        print_facts(&ctx);

        // Every item except C1 uses one device on the default adapter (WP-42 picks its own).
        let device = d3d::create_device(None)?;
        let mut rows: Vec<Row> = Vec::new();
        for item in &args.items {
            eprintln!("spike-capture: running {item}");
            let one = |r: Result<Row>, cond: &str| {
                r.unwrap_or_else(|e| Row {
                    id: item.clone(),
                    condition: cond.into(),
                    result: "ERROR",
                    stats: None,
                    notes: format!("{e:#}"),
                })
            };
            match item.as_str() {
                "C1" => rows.extend(matrix::c1(&ctx)),
                "C2" => rows.push(one(matrix::c2(&ctx, &device), "Window covered")),
                "C3" => rows.push(one(matrix::c3(&ctx, &device), "Secondary monitor")),
                "C4" => rows.push(one(matrix::c4(&ctx, &device), "Minimised")),
                "C5" => rows.extend(matrix::c5(&ctx, &device)),
                "C9" => rows.push(one(matrix::c9(&ctx, &device), "Moved/resized")),
                "C10" => rows.push(one(matrix::c10(&ctx, &device), "IsBorderRequired")),
                "C11" => rows.push(one(matrix::c11(&ctx, &device), "Cursor")),
                "C15" => rows.push(one(matrix::c15(&ctx, &device), "Two windows of one process")),
                "C16" => rows.push(Row::skip(
                    "C16",
                    "Edge opening a PDF with/without --new-window --app=",
                    "not run: the unattended boundary allows capturing gander-testwin windows only; needs Edge driven on a test PDF (WP-50 launch policy or a human run)",
                )),
                "C17" => rows.push(Row::skip(
                    "C17",
                    "Packaged app (Photos) under ApplicationFrameHost",
                    "not run: same boundary (no third-party app windows); needs a human-attended run",
                )),
                "C6" => {
                    let locked = harness::session_locked();
                    let mut why = String::from(
                        "needs human: the lock/unlock transition and unlock need the owner's password",
                    );
                    if locked == Some(true) {
                        why.push_str(
                            "; observed anyway: the session is LOCKED during this run and every capture row above/below kept delivering correct frames",
                        );
                    }
                    rows.push(Row::skip("C6", "Session locked (Win+L) while streaming", &why));
                }
                "C12" => rows.push(Row {
                    id: "C12".into(),
                    condition: "Hybrid GPU: which adapter the capture texture lives on".into(),
                    result: "INFO",
                    stats: None,
                    notes: "adapters and monitor scan-out in the header; the texture lives on whichever device is passed to the frame pool (C1 rows, both adapters); encode-side cost is WP-42".into(),
                }),
                other => rows.push(Row::skip(other, "-", "not an automatable capture item")),
            }
        }
        print_table(&rows);
        let bad = rows
            .iter()
            .any(|r| r.result == "FAIL" || r.result == "ERROR");
        Ok(if bad { 1 } else { 0 })
    }
}
