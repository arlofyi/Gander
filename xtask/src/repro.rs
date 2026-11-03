// SPDX-License-Identifier: Apache-2.0
//! Release builds without local paths (`docs/release/reproducible-builds.md`).
//!
//! Panic locations and generated-code `include!`s put source paths into an executable: the
//! workspace (and its `target/` folder), the cargo registry and, for the standard library, the
//! toolchain. Each machine has different ones (the owner's user name among them), so the release
//! builds of `cargo xtask installer-build` and `installer-check --binary` map them to fixed
//! prefixes with `--remap-path-prefix`. The prefixes are computed here from the environment at
//! build time; no tracked file names a real path. They are passed in `CARGO_ENCODED_RUSTFLAGS`
//! (separated by 0x1F) because a workspace path may contain spaces. The `/Brepro` linker switch
//! (no time stamp in the PE header) comes from the two binaries' `build.rs`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The fixed prefix each local folder is mapped to.
const WORKSPACE_AS: &str = "/gander";
const CARGO_HOME_AS: &str = "/cargo";
const RUSTUP_HOME_AS: &str = "/rustup";
const SYSROOT_AS: &str = "/rustc-sysroot";

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// `CARGO_HOME`, else `~/.cargo` (cargo's own default).
fn cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".cargo")))
}

/// `RUSTUP_HOME`, else `~/.rustup` (rustup's own default).
fn rustup_home() -> Option<PathBuf> {
    std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".rustup")))
}

/// The active toolchain's sysroot (`rustc --print sysroot`).
fn sysroot() -> Option<PathBuf> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let out = Command::new(rustc)
        .args(["--print", "sysroot"])
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim();
    (out.status.success() && !s.is_empty()).then(|| PathBuf::from(s))
}

/// The local folders and the prefix each maps to, most general first (rustc lets the last
/// matching flag win).
pub fn remaps(root: &Path) -> Vec<(PathBuf, &'static str)> {
    let mut v = Vec::new();
    if let Some(p) = rustup_home() {
        v.push((p, RUSTUP_HOME_AS));
    }
    if let Some(p) = sysroot() {
        v.push((p, SYSROOT_AS));
    }
    if let Some(p) = cargo_home() {
        v.push((p, CARGO_HOME_AS));
    }
    v.push((root.to_path_buf(), WORKSPACE_AS));
    v
}

/// `CARGO_ENCODED_RUSTFLAGS` for a release build: whatever the caller already had, plus one
/// `--remap-path-prefix` per local folder.
pub fn encoded_rustflags(root: &Path) -> OsString {
    let mut flags: Vec<OsString> = Vec::new();
    if let Some(existing) = std::env::var_os("CARGO_ENCODED_RUSTFLAGS") {
        if !existing.is_empty() {
            flags.push(existing);
        }
    } else if let Some(existing) = std::env::var_os("RUSTFLAGS") {
        flags.extend(
            existing
                .to_string_lossy()
                .split_whitespace()
                .map(OsString::from),
        );
    }
    for (from, to) in remaps(root) {
        let mut f = OsString::from("--remap-path-prefix=");
        f.push(from.as_os_str());
        f.push("=");
        f.push(to);
        flags.push(f);
    }
    flags.join(std::ffi::OsStr::new("\u{1f}"))
}

/// MSVC flags for C code built by build scripts (the bundled libopus, through `cc` and `cmake`):
/// `/d1trimfile:` strips each local folder from `__FILE__`, which libopus puts in its assertion
/// messages. `cc` splits `CFLAGS` on white space, so a folder whose path holds a space is left
/// out (the path check after the build then names it).
pub fn msvc_cflags(root: &Path) -> String {
    let mut flags: Vec<String> = Vec::new();
    if let Some(existing) = std::env::var_os("CFLAGS_x86_64_pc_windows_msvc") {
        flags.push(existing.to_string_lossy().into_owned());
    }
    for (from, _) in remaps(root) {
        let mut text = from.to_string_lossy().replace('/', "\\");
        if text.contains(char::is_whitespace) {
            continue;
        }
        if !text.ends_with('\\') {
            text.push('\\');
        }
        flags.push(format!("/d1trimfile:{text}"));
    }
    flags.join(" ")
}

/// Applies [`encoded_rustflags`] and [`msvc_cflags`] to a `cargo build --release` command.
pub fn apply(cmd: &mut Command, root: &Path) {
    cmd.env("CARGO_ENCODED_RUSTFLAGS", encoded_rustflags(root))
        .env_remove("RUSTFLAGS");
    if cfg!(windows) {
        cmd.env("CFLAGS_x86_64_pc_windows_msvc", msvc_cflags(root));
    }
}

/// Case-insensitive count of `needle` in `hay`, as 8-bit text and as UTF-16LE, with either path
/// separator. Used on the executables' bytes.
pub fn count_path(hay: &[u8], needle: &Path) -> usize {
    let text = needle.to_string_lossy().replace('/', "\\");
    if text.is_empty() {
        return 0;
    }
    let mut total = 0;
    for sep in ['\\', '/'] {
        let t: String = text.replace('\\', &sep.to_string()).to_ascii_lowercase();
        let narrow = t.as_bytes().to_vec();
        let wide: Vec<u8> = t.encode_utf16().flat_map(u16::to_le_bytes).collect();
        for n in [narrow, wide] {
            total += count_ci(hay, &n);
        }
    }
    total
}

fn count_ci(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || hay.len() < needle.len() {
        return 0;
    }
    hay.windows(needle.len())
        .filter(|w| w.eq_ignore_ascii_case(needle))
        .count()
}

/// The local folders a release executable must not mention: the user profile and every remapped
/// folder.
pub fn local_folders(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = remaps(root).into_iter().map(|(p, _)| p).collect();
    if let Some(h) = home() {
        v.push(h);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_both_separators_cases_and_utf16() {
        let p = Path::new("C:\\Users\\Someone\\src");
        let mut hay = b"xx c:/users/someone/SRC/a.rs yy C:\\Users\\Someone\\src\\b.rs".to_vec();
        hay.extend(
            "C:\\Users\\Someone\\src"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        );
        assert_eq!(count_path(&hay, p), 3);
        assert_eq!(count_path(b"nothing here", p), 0);
    }

    #[test]
    fn flags_are_one_per_folder_and_name_no_space_split() {
        let root = Path::new("C:\\A Folder With Spaces\\gander");
        let flags = encoded_rustflags(root);
        let flags = flags.to_string_lossy();
        assert!(flags.contains("--remap-path-prefix=C:\\A Folder With Spaces\\gander=/gander"));
        // The workspace mapping comes last (most specific wins).
        assert!(flags.ends_with("=/gander"));
    }

    #[test]
    fn msvc_trim_flags_skip_folders_with_spaces() {
        let flags = msvc_cflags(Path::new(r"C:\A Folder With Spaces\gander"));
        assert!(!flags.contains("A Folder"));
        for f in flags.split(' ').filter(|f| !f.is_empty()) {
            if f.starts_with("/d1trimfile:") {
                assert!(f.ends_with('\\'), "{f}");
            }
        }
    }
}
