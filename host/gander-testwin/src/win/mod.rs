// SPDX-License-Identifier: Apache-2.0
//! The Win32 runtime: window thread, 60 Hz ticker thread and control pipe thread.

pub mod audio;
pub mod pipe;
pub mod scenario;
pub mod shared;
pub mod ticker;
pub mod window;

use std::io::Write;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use gander_testwin::cli::Args;
use gander_testwin::protocol;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{SW_SHOWNOACTIVATE, SW_SHOWNORMAL};

/// Opens the window, starts the ticker and the pipe, and runs until the window is closed, `quit`
/// arrives on the pipe, or the self-test deadline passes.
pub fn run(args: Args) -> Result<()> {
    if let Some(key) = &args.reuse
        && let scenario::Forward::Forwarded(reply) =
            scenario::forward(key, &args.title).context("forwarding to the running instance")?
    {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(
            out,
            "gander-testwin forwarded key={key} reply={reply} title={}",
            args.title
        );
        let _ = out.flush();
        return Ok(());
    }
    let s = shared::init(args.title.clone());
    if args.dpi_unaware {
        window::set_dpi_unaware();
    } else {
        window::set_dpi_awareness();
    }
    window::register_class()?;
    if let Some(duration) = args.splash {
        scenario::show_splash(&args.title, duration).context("showing the splash window")?;
    }
    let show = if args.no_focus {
        SW_SHOWNOACTIVATE
    } else {
        SW_SHOWNORMAL
    };
    let hwnd = window::create_window(window::Role::Main, &args.title, show)
        .context("creating the main window")?;
    s.main_hwnd.store(hwnd.0 as isize, Ordering::Relaxed);
    // SAFETY: plain query.
    s.ui_thread
        .store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
    if args.twin {
        window::create_window(
            window::Role::Second,
            &format!("{} (second)", args.title),
            SW_SHOWNOACTIVATE,
        )
        .context("creating the twin window")?;
    }

    // SAFETY: plain query.
    let pid = unsafe { GetCurrentProcessId() };
    let pipe = pipe::start(pid).context("starting the control pipe")?;
    let reuse = match &args.reuse {
        Some(key) => Some(
            pipe::start_named(protocol::reuse_pipe_name(key))
                .context("starting the single-instance pipe")?,
        ),
        None => None,
    };
    let ticker = ticker::spawn(args.self_test).context("starting the frame clock")?;

    let mut out = std::io::stdout().lock();
    let _ = writeln!(
        out,
        "gander-testwin ready pid={pid} pipe={} hwnd=0x{:X} title={}",
        pipe.name, hwnd.0 as isize, args.title
    );
    let _ = out.flush();
    drop(out);

    window::message_loop();

    s.stop.store(true, Ordering::Relaxed);
    pipe.shutdown();
    if let Some(reuse) = reuse {
        reuse.shutdown();
    }
    let _ = ticker.join();
    window::destroy_all();
    Ok(())
}
