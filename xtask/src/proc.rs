// SPDX-License-Identifier: Apache-2.0
//! Small process and path helpers shared by the xtask commands.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

/// Output of a finished child process, lossily decoded.
#[derive(Debug)]
pub struct Captured {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Captured {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// stdout followed by stderr: some tools (`java -version`) print to stderr.
    pub fn combined(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }

    /// First non-empty line of stdout and stderr combined, trimmed.
    pub fn first_line(&self) -> String {
        self.combined()
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_string()
    }
}

/// Run `program` with `args` to completion and capture its output.
pub fn capture<S: AsRef<OsStr>>(program: S, args: &[&str]) -> Result<Captured> {
    let program = program.as_ref();
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("could not run {}", program.to_string_lossy()))?;
    Ok(Captured {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// The repository root: the parent of this crate's manifest directory.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

/// Find `name` on `PATH` (honouring `PATHEXT` on Windows).
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extensions: Vec<String> = if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        std::iter::once(String::new())
            .chain(
                pathext
                    .split(';')
                    .filter(|e| !e.is_empty())
                    .map(str::to_ascii_lowercase),
            )
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &extensions {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Display a path with forward slashes so reports read the same on every platform.
pub fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_path_uses_forward_slashes() {
        assert_eq!(display_path(Path::new(r"C:\a\b")), "C:/a/b");
    }

    #[test]
    fn first_line_skips_blank_lines() {
        let c = Captured {
            code: Some(0),
            stdout: "\n  \n hello \nworld".into(),
            stderr: String::new(),
        };
        assert_eq!(c.first_line(), "hello");
    }

    #[test]
    fn workspace_root_contains_workspace_manifest() {
        assert!(workspace_root().join("Cargo.toml").is_file());
    }
}
