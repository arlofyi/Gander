// SPDX-License-Identifier: Apache-2.0
//! `spike-input` (WP-44): the host side of the Android spike and the input matrix.
//!
//! ```text
//! spike-input matrix [--results DIR]
//!     No phone. Clicks started test windows with SendInput and PostMessage under the conditions
//!     of matrix items C19 and C20 (window not foreground, covered by another window, session
//!     locked) and measures the normalised-tap mapping at two DPI scalings (C3).
//!
//! spike-input serve [--port N] [--advertise IP] [--inject postmessage|sendinput|alternate|none]
//!     [--size WxH] [--vendor intel|nvidia] [--secs N] [--adb PATH --drive N] [--results DIR]
//!     [--label S] [--strip-answer-candidates]
//!     Streams a gander-testwin window (WGC -> hardware H.264 -> WebRTC, the WP-43 stack) to the
//!     Android spike app, and injects the phone's taps into the window.
//! ```
//!
//! Network: ICE binds 127.0.0.1 only and the signalling WebSocket listens on 127.0.0.1 only.
//! `--advertise 10.0.2.2` changes only the address written into the SDP offer, so the Android
//! emulator (which reaches the host's loopback as 10.0.2.2) sends to a socket that stays bound to
//! loopback. Nothing listens on a wildcard address, the LAN or the tailnet.
//!
//! Only `gander-testwin` windows are captured and clicked.
#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod driver;
#[cfg(windows)]
mod inject;
#[cfg(windows)]
mod matrix;
#[cfg(windows)]
mod serve;

/// NVIDIA Optimus opt-in (exported by build.rs); needed for the NVIDIA encoder MFT (WP-42).
#[cfg(windows)]
#[unsafe(no_mangle)]
#[used]
#[allow(non_upper_case_globals)]
pub static NvOptimusEnablement: u32 = 1;

#[cfg(not(windows))]
fn main() {
    eprintln!("spike-input needs Windows (WGC capture, SendInput, PostMessage)");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    let mut args = std::env::args().skip(1);
    let code = match args.next().as_deref() {
        Some("matrix") => run(|| matrix::run(results_arg(args.collect()))),
        Some("serve") => run(|| serve::run(args.collect())),
        _ => {
            eprintln!(
                "usage: spike-input matrix [--results DIR] | serve [options], see the module docs"
            );
            2
        }
    };
    std::process::exit(code);
}

#[cfg(windows)]
fn run(f: impl FnOnce() -> anyhow::Result<i32>) -> i32 {
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    // Per-monitor DPI aware so window rectangles and clicks are physical pixels.
    // SAFETY: plain process-wide call with a valid constant.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    // Per process since Windows 10 2004 (WP-42 finding 2: the Intel MFT needs it).
    // SAFETY: plain call; undone below.
    unsafe { windows::Win32::Media::timeBeginPeriod(1) };
    let r = f();
    // SAFETY: matches the call above.
    unsafe { windows::Win32::Media::timeEndPeriod(1) };
    match r {
        Ok(c) => c,
        Err(e) => {
            eprintln!("spike-input: {e:#}");
            1
        }
    }
}

#[cfg(windows)]
fn results_arg(args: Vec<String>) -> Option<std::path::PathBuf> {
    let i = args.iter().position(|a| a == "--results")?;
    args.get(i + 1).map(std::path::PathBuf::from)
}
