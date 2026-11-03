// SPDX-License-Identifier: Apache-2.0
//! `gander-testwin`: a deterministic Win32 test window for the stream spike and tests.
//!
//! See `host/gander-testwin/README.md` for the pixel layout and the control pipe protocol.

use std::process::ExitCode;

use gander_testwin::cli;

#[cfg(windows)]
mod win;

fn main() -> ExitCode {
    let args = match cli::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("gander-testwin: {e}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    if args.help {
        println!("{}", cli::USAGE);
        return ExitCode::SUCCESS;
    }
    run(args)
}

#[cfg(windows)]
fn run(args: cli::Args) -> ExitCode {
    match win::run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gander-testwin: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn run(_args: cli::Args) -> ExitCode {
    eprintln!("gander-testwin: this tool needs Windows (Win32 window and named pipe)");
    ExitCode::from(2)
}
