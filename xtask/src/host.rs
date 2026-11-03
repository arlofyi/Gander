// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask build-host` and `cargo xtask run-host`: the host together with its render worker.
//!
//! `cargo run -p gander-host` builds only `gander-host.exe`. Previews and thumbnails need
//! `gander-render.exe` next to it (the host spawns it per job), and an artifact dependency that
//! would make cargo build both is unstable. These tasks build both packages in one cargo call,
//! with the same profile, so the worker always lands beside the host.
//!
//! ```text
//! cargo xtask build-host [--release]
//! cargo xtask run-host [--release] [-- <gander-host arguments>]
//! ```

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use anyhow::{Context as _, Result, bail};

use crate::proc::{display_path, workspace_root};

/// The packages `build-host` builds, in one cargo call.
pub(crate) const PACKAGES: [&str; 2] = ["gander-host", "gander-render"];

#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    release: bool,
    host_args: Vec<String>,
}

fn parse(args: &[String], allow_host_args: bool) -> Result<Options> {
    let mut o = Options::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--release" => o.release = true,
            "--" if allow_host_args => {
                o.host_args = it.by_ref().cloned().collect();
            }
            other => {
                bail!("unknown option {other} (build-host|run-host [--release] [-- <host args>])")
            }
        }
    }
    Ok(o)
}

/// The `cargo build` arguments for both packages.
fn build_args(release: bool) -> Vec<&'static str> {
    let mut v = vec!["build"];
    for p in PACKAGES {
        v.extend(["-p", p]);
    }
    if release {
        v.push("--release");
    }
    v
}

fn target_dir() -> PathBuf {
    let root = workspace_root();
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|t| if t.is_absolute() { t } else { root.join(t) })
        .unwrap_or_else(|| root.join("target"))
}

fn build(release: bool) -> Result<PathBuf> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(workspace_root())
        .args(build_args(release))
        .status()
        .context("run cargo build")?;
    if !status.success() {
        bail!("cargo build failed");
    }
    let dir = target_dir().join(if release { "release" } else { "debug" });
    let exe = std::env::consts::EXE_SUFFIX;
    for name in PACKAGES {
        let p = dir.join(format!("{name}{exe}"));
        if !p.is_file() {
            bail!("{} is missing after the build", display_path(&p));
        }
    }
    Ok(dir.join(format!("gander-host{exe}")))
}

pub fn run_build(args: &[String]) -> Result<ExitCode> {
    let o = parse(args, false)?;
    let host = build(o.release)?;
    println!("built {} and gander-render beside it", display_path(&host));
    Ok(ExitCode::SUCCESS)
}

pub fn run_host(args: &[String]) -> Result<ExitCode> {
    let o = parse(args, true)?;
    let host = build(o.release)?;
    let status = Command::new(&host)
        .args(&o.host_args)
        .status()
        .with_context(|| format!("start {}", display_path(&host)))?;
    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(c) => ExitCode::from(u8::try_from(c).unwrap_or(1)),
        None => ExitCode::from(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn both_packages_are_built_in_one_call_with_the_same_profile() {
        assert_eq!(
            build_args(true),
            [
                "build",
                "-p",
                "gander-host",
                "-p",
                "gander-render",
                "--release"
            ]
        );
        assert_eq!(
            build_args(false),
            ["build", "-p", "gander-host", "-p", "gander-render"]
        );
    }

    #[test]
    fn host_arguments_follow_the_double_dash() {
        let o = parse(&s(&["--release", "--", "--no-ui", "--port", "1"]), true).unwrap();
        assert!(o.release);
        assert_eq!(o.host_args, s(&["--no-ui", "--port", "1"]));
        assert!(parse(&s(&["--", "--no-ui"]), false).is_err());
        assert!(parse(&s(&["--bogus"]), true).is_err());
    }
}
