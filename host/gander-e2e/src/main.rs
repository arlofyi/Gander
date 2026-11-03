// SPDX-License-Identifier: Apache-2.0
//! `gander-e2e`: the WP-1B HTTP conformance suite. It talks to a running test host (built with
//! `--features test-hooks`, started with `--test-approve`) as a pinned TLS 1.3 client, pairs
//! through the test hook, then exercises every endpoint of `protocol/openapi.yaml` (a success
//! case validated against the schema, plus every documented error code that can be produced
//! from outside) and `/v1/events`, and writes a JSON report.
//!
//! Normally run by `cargo xtask e2e-host`, which builds the binaries, generates the fixture tree
//! and `config.toml`, starts the host and wires this program's stdout to the host's stdin (the
//! test host's `revoke <device-id>` command). Everything human-readable goes to stderr; stdout
//! carries host commands only.
//!
//! ```text
//! gander-e2e --addr 127.0.0.1:<port> --host-id <id> --share-root <dir> --openapi <yaml>
//!            --report <json> [--host-commands] [--locked]
//! ```
//!
//! `gander-e2e pair ...` pairs one device and writes its token to a file instead (see `pair.rs`,
//! used by `cargo xtask e2e-android`). `gander-e2e stream ...` runs the Stream path end to end
//! and `gander-e2e park-watch ...` parks the test windows of the Android stage (see `stream.rs`,
//! used by `cargo xtask e2e-stream`).
//!
//! Exit status: 0 when every case passed and every error code is covered or explained as not
//! reachable, 1 otherwise, 2 on a usage or setup error.

mod client;
mod decode;
mod pair;
mod report;
mod stream;
mod suite;
#[cfg(windows)]
mod testwin;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context as _, bail};

/// Command-line options.
#[derive(Debug)]
pub struct Options {
    pub addr: SocketAddr,
    pub host_id: String,
    pub share_root: PathBuf,
    pub openapi: PathBuf,
    pub report: PathBuf,
    /// stdout is connected to the host's stdin (revocation is reachable).
    pub host_commands: bool,
    /// The fixture's `locked/` directory carries a deny ACE (forbidden is reachable).
    pub locked: bool,
}

fn parse() -> anyhow::Result<Options> {
    let mut addr = None;
    let mut host_id = None;
    let mut share_root = None;
    let mut openapi = None;
    let mut report = None;
    let mut host_commands = false;
    let mut locked = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = || it.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--addr" => addr = Some(value()?.parse::<SocketAddr>()?),
            "--host-id" => host_id = Some(value()?),
            "--share-root" => share_root = Some(PathBuf::from(value()?)),
            "--openapi" => openapi = Some(PathBuf::from(value()?)),
            "--report" => report = Some(PathBuf::from(value()?)),
            "--host-commands" => host_commands = true,
            "--locked" => locked = true,
            other => bail!("unknown argument {other}"),
        }
    }
    let addr = addr.context("--addr is required")?;
    if !addr.ip().is_loopback() {
        bail!("--addr must be a loopback address");
    }
    Ok(Options {
        addr,
        host_id: host_id.context("--host-id is required")?,
        share_root: share_root.context("--share-root is required")?,
        openapi: openapi.context("--openapi is required")?,
        report: report.context("--report is required")?,
        host_commands,
        locked,
    })
}

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("pair") => return pair_main(),
        Some("stream") => return stream_main(),
        #[cfg(windows)]
        Some("park-watch") => {
            return match stream::park_watch(std::env::args().skip(2)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("gander-e2e park-watch: {e:#}");
                    ExitCode::from(2)
                }
            };
        }
        _ => {}
    }
    let options = match parse() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("gander-e2e: {e:#}");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("gander-e2e: runtime: {e}");
            return ExitCode::from(2);
        }
    };
    match runtime.block_on(suite::run(&options)) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("gander-e2e: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn pair_main() -> ExitCode {
    let options = match pair::parse(std::env::args().skip(2)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("gander-e2e pair: {e:#}");
            return ExitCode::from(2);
        }
    };
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|rt| rt.block_on(pair::run(&options)));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gander-e2e pair: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn stream_main() -> ExitCode {
    let options = match stream::parse(std::env::args().skip(2)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("gander-e2e stream: {e:#}");
            return ExitCode::from(2);
        }
    };
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|rt| rt.block_on(stream::run(&options)));
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("gander-e2e stream: {e:#}");
            ExitCode::from(2)
        }
    }
}
