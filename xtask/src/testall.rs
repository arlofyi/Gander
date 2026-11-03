// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask test-all`: the local equivalent of the CI gates, run in one go.
//!
//! Runs every step even after a failure and prints a summary table, so one run shows the full
//! picture. Exit code is non-zero if any step failed.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Result, bail};

use crate::proc::workspace_root;

#[derive(Debug, PartialEq, Eq)]
enum Status {
    Passed,
    Failed,
    Skipped(&'static str),
}

struct Step {
    name: &'static str,
    status: Status,
}

fn cargo() -> std::ffi::OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

fn run_cargo(root: &Path, args: &[&str]) -> Status {
    println!("\n==> cargo {}", args.join(" "));
    match Command::new(cargo()).args(args).current_dir(root).status() {
        Ok(status) if status.success() => Status::Passed,
        Ok(_) => Status::Failed,
        Err(e) => {
            eprintln!("could not start cargo: {e}");
            Status::Failed
        }
    }
}

pub(crate) fn gradle_wrapper(root: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "gradlew.bat"
    } else {
        "gradlew"
    };
    let wrapper = root.join("client").join("android").join(name);
    wrapper.is_file().then_some(wrapper)
}

fn run_android_unit_tests(root: &Path, skip: bool) -> Status {
    if skip {
        return Status::Skipped("--skip-android");
    }
    let Some(wrapper) = gradle_wrapper(root) else {
        return Status::Skipped("no client/android Gradle project yet");
    };
    println!("\n==> gradlew testDebugUnitTest");
    match Command::new(wrapper)
        .arg("testDebugUnitTest")
        .current_dir(root.join("client").join("android"))
        .status()
    {
        Ok(status) if status.success() => Status::Passed,
        _ => Status::Failed,
    }
}

fn run_vectors_check() -> Status {
    println!("\n==> vectors-check");
    match crate::vectors::run(&[]) {
        Ok(code) if code == ExitCode::SUCCESS => Status::Passed,
        Ok(_) => Status::Failed,
        Err(e) => {
            eprintln!("vectors-check error: {e:#}");
            Status::Failed
        }
    }
}

fn run_installer_check() -> Status {
    println!("\n==> installer-check");
    match crate::installer::run_check(&[]) {
        Ok(code) if code == ExitCode::SUCCESS => Status::Passed,
        Ok(_) => Status::Failed,
        Err(e) => {
            eprintln!("installer-check error: {e:#}");
            Status::Failed
        }
    }
}

fn run_docs_check() -> Status {
    println!("\n==> docs-check");
    match crate::docs::run(&[]) {
        Ok(code) if code == ExitCode::SUCCESS => Status::Passed,
        Ok(_) => Status::Failed,
        Err(e) => {
            eprintln!("docs-check error: {e:#}");
            Status::Failed
        }
    }
}

/// Entry point for `cargo xtask test-all [--skip-deny] [--skip-android]`.
pub fn run(args: &[String]) -> Result<ExitCode> {
    let mut skip_deny = false;
    let mut skip_android = false;
    for arg in args {
        match arg.as_str() {
            "--skip-deny" => skip_deny = true,
            "--skip-android" => skip_android = true,
            other => {
                bail!(
                    "unknown test-all argument `{other}` (supported: --skip-deny, --skip-android)"
                )
            }
        }
    }
    let root = workspace_root();
    let steps = vec![
        Step {
            name: "fmt",
            status: run_cargo(&root, &["fmt", "--all", "--", "--check"]),
        },
        Step {
            name: "clippy",
            status: run_cargo(
                &root,
                &[
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            ),
        },
        Step {
            name: "test",
            status: run_cargo(&root, &["test", "--workspace"]),
        },
        Step {
            name: "deny",
            status: if skip_deny {
                Status::Skipped("--skip-deny")
            } else {
                run_cargo(&root, &["deny", "check"])
            },
        },
        Step {
            name: "vectors-check",
            status: run_vectors_check(),
        },
        Step {
            name: "installer-check",
            status: run_installer_check(),
        },
        Step {
            name: "docs-check",
            status: run_docs_check(),
        },
        Step {
            name: "android-unit",
            status: run_android_unit_tests(&root, skip_android),
        },
    ];

    println!("\n==== test-all summary ====");
    let mut failed = 0;
    for step in &steps {
        match &step.status {
            Status::Passed => println!("[PASS] {}", step.name),
            Status::Failed => {
                failed += 1;
                println!("[FAIL] {}", step.name);
            }
            Status::Skipped(why) => println!("[SKIP] {} ({why})", step.name),
        }
    }
    if failed > 0 {
        eprintln!("{failed} step(s) failed");
        return Ok(ExitCode::from(1));
    }
    println!("test-all passed");
    Ok(ExitCode::SUCCESS)
}
