// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask notices [--out-dir <dir>]`: the licence texts the installer ships (WP-1A).
//!
//! Writes two files (default directory `target/installer/`):
//!
//! - `THIRD-PARTY-NOTICES.txt`: every crate that is linked into `gander-host.exe` or
//!   `gander-render.exe` for Windows (the normal-dependency closure of the two binaries, read from
//!   `cargo metadata`; build-only and dev-only crates are not shipped, so they are left out), with
//!   its licence expression and the licence files found in its source. Crates that share the same
//!   text are grouped under one copy of it. The OFL-1.1 and Ubuntu-font-1.0 texts of egui's bundled
//!   fonts (`epaint_default_fonts`) and the Boost texts of `clipboard-win` and `error-code` are in
//!   there because those are ordinary licence files of the crates (deny.toml's per-crate
//!   exceptions point here).
//! - `License.rtf`: the repository's `LICENSE` as RTF, for the installer's licence page.
//!
//! No external tool (cargo-about and the like) is needed: the data is `cargo metadata` plus the
//! crates' own files in the local registry cache.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::proc::{display_path, workspace_root};

/// The binaries whose dependency closure is shipped.
pub const SHIPPED_BINARIES: [&str; 2] = ["gander-host", "gander-render"];

/// The platform the installer is built for.
pub const TARGET: &str = "x86_64-pc-windows-msvc";

/// One third-party crate that ships.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crate {
    pub name: String,
    pub version: String,
    /// SPDX licence expression from Cargo.toml (may be empty for `license-file` crates).
    pub license: String,
    pub repository: String,
    pub authors: Vec<String>,
    /// Source directory in the registry cache.
    pub dir: PathBuf,
    /// `license-file` from Cargo.toml, relative to `dir`.
    pub license_file: Option<String>,
}

/// Entry point for `cargo xtask notices`.
pub fn run(args: &[String]) -> Result<ExitCode> {
    let root = workspace_root();
    let mut out_dir = root.join("target").join("installer");
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out-dir" => {
                out_dir = PathBuf::from(it.next().context("--out-dir needs a value")?);
            }
            other => {
                bail!("unknown argument {other}; usage: cargo xtask notices [--out-dir <dir>]")
            }
        }
    }
    let summary = generate(&root, &out_dir)?;
    println!(
        "notices: {} third-party crate(s), {} distinct licence text(s) -> {}",
        summary.crates,
        summary.texts,
        display_path(&summary.notices)
    );
    println!("notices: licence page -> {}", display_path(&summary.rtf));
    for name in &summary.without_text {
        println!("[WARN] {name}: no licence file in the crate source (the expression is listed)");
    }
    Ok(ExitCode::SUCCESS)
}

/// What [`generate`] produced.
#[derive(Debug)]
pub struct Summary {
    pub notices: PathBuf,
    pub rtf: PathBuf,
    pub crates: usize,
    pub texts: usize,
    /// `name version` of crates that have no licence file at all.
    pub without_text: Vec<String>,
}

/// Writes both files into `out_dir`.
pub fn generate(root: &Path, out_dir: &Path) -> Result<Summary> {
    let metadata = cargo_metadata(root)?;
    let crates = shipped_crates(&metadata)?;
    let license = std::fs::read_to_string(root.join("LICENSE")).context("reading LICENSE")?;
    let (text, texts, without_text) = render_notices(&crates, &license)?;
    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let notices = out_dir.join("THIRD-PARTY-NOTICES.txt");
    std::fs::write(&notices, text).with_context(|| format!("writing {}", notices.display()))?;
    let rtf = out_dir.join("License.rtf");
    std::fs::write(&rtf, rtf_from_text(&license))
        .with_context(|| format!("writing {}", rtf.display()))?;
    Ok(Summary {
        notices,
        rtf,
        crates: crates.len(),
        texts,
        without_text,
    })
}

pub(crate) fn cargo_metadata(root: &Path) -> Result<Value> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = Command::new(cargo)
        .current_dir(root)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--filter-platform",
            TARGET,
        ])
        .output()
        .context("running cargo metadata")?;
    if !out.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    serde_json::from_slice(&out.stdout).context("cargo metadata returned invalid JSON")
}

/// The third-party (registry) crates in the normal-dependency closure of [`SHIPPED_BINARIES`],
/// sorted by name and version.
pub fn shipped_crates(metadata: &Value) -> Result<Vec<Crate>> {
    let packages = metadata["packages"]
        .as_array()
        .context("metadata has no packages")?;
    let by_id: HashMap<&str, &Value> = packages
        .iter()
        .filter_map(|p| Some((p["id"].as_str()?, p)))
        .collect();
    let members: HashSet<&str> = metadata["workspace_members"]
        .as_array()
        .context("metadata has no workspace_members")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let roots: Vec<&str> = members
        .iter()
        .copied()
        .filter(|id| {
            by_id
                .get(id)
                .and_then(|p| p["name"].as_str())
                .is_some_and(|n| SHIPPED_BINARIES.contains(&n))
        })
        .collect();
    if roots.len() != SHIPPED_BINARIES.len() {
        bail!(
            "expected the workspace members {SHIPPED_BINARIES:?}, found {} of them",
            roots.len()
        );
    }
    let nodes: HashMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .context("metadata has no resolve graph")?
        .iter()
        .filter_map(|n| Some((n["id"].as_str()?, n)))
        .collect();

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut stack = roots;
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = nodes.get(id) else { continue };
        for dep in node["deps"].as_array().into_iter().flatten() {
            // `kind` is null for a normal dependency, "build" or "dev" otherwise.
            let normal = dep["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|k| k["kind"].is_null());
            if normal && let Some(pkg) = dep["pkg"].as_str() {
                stack.push(pkg);
            }
        }
    }

    let mut out = Vec::new();
    for id in seen {
        if members.contains(id) {
            continue;
        }
        let Some(p) = by_id.get(id) else { continue };
        // Path and git sources are Gander's own or spikes; registry crates are third party.
        if p["source"].is_null() {
            continue;
        }
        let manifest = p["manifest_path"].as_str().unwrap_or_default();
        out.push(Crate {
            name: p["name"].as_str().unwrap_or_default().to_owned(),
            version: p["version"].as_str().unwrap_or_default().to_owned(),
            license: p["license"].as_str().unwrap_or_default().to_owned(),
            repository: p["repository"].as_str().unwrap_or_default().to_owned(),
            authors: p["authors"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str().map(str::to_owned))
                .collect(),
            dir: Path::new(manifest)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            license_file: p["license_file"].as_str().map(str::to_owned),
        });
    }
    out.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    Ok(out)
}

/// Whether a file name looks like a licence text.
pub fn is_license_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("licen")
        || n.starts_with("copying")
        || n.starts_with("notice")
        || n.starts_with("unlicense")
        || n == "ofl.txt"
        || n == "ufl.txt"
}

/// Directories inside a crate that never hold the licence of what ships.
const SKIPPED_DIRS: [&str; 6] = ["target", "tests", "test", "benches", "examples", ".git"];

/// Licence files of a crate: top-level licence-named files, the `license-file` from Cargo.toml,
/// and (one level down) licence-named files and the `*.txt` files next to bundled fonts. Sorted
/// by relative path.
pub fn license_files(krate: &Crate) -> Vec<(String, String)> {
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    let mut add = |rel: String, path: &Path| {
        if let Ok(bytes) = std::fs::read(path) {
            let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
            if !text.trim().is_empty() {
                found.insert(rel, text);
            }
        }
    };
    if let Some(file) = &krate.license_file {
        add(file.replace('\\', "/"), &krate.dir.join(file));
    }
    let Ok(entries) = std::fs::read_dir(&krate.dir) else {
        return found.into_iter().collect();
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if path.is_file() {
            if is_license_name(&name) {
                add(name, &path);
            }
        } else if path.is_dir() && !SKIPPED_DIRS.contains(&name.as_str()) {
            let fonts = name.eq_ignore_ascii_case("fonts");
            let Ok(inner) = std::fs::read_dir(&path) else {
                continue;
            };
            for file in inner.flatten() {
                let file_name = file.file_name().to_string_lossy().into_owned();
                let is_txt = file_name.to_ascii_lowercase().ends_with(".txt");
                if file.path().is_file() && (is_license_name(&file_name) || (fonts && is_txt)) {
                    add(format!("{name}/{file_name}"), &file.path());
                }
            }
        }
    }
    found.into_iter().collect()
}

/// The Boost Software License 1.0 (no copyright line, so one text serves every crate).
const BOOST_TEXT: &str = r#"Boost Software License - Version 1.0 - August 17th, 2003

Permission is hereby granted, free of charge, to any person or organization
obtaining a copy of the software and accompanying documentation covered by
this license (the "Software") to use, reproduce, display, distribute,
execute, and transmit the Software, and to prepare derivative works of the
Software, and to permit third-parties to whom the Software is furnished to
do so, all subject to the following:

The copyright notices in the Software and this entire statement, including
the above license grant, this restriction and the following disclaimer,
must be included in all copies of the Software, in whole or in part, and
all derivative works of the Software, unless such copies or derivative
works are solely in the form of machine-executable object code generated by
a source language processor.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE, TITLE AND NON-INFRINGEMENT. IN NO EVENT
SHALL THE COPYRIGHT HOLDERS OR ANYONE DISTRIBUTING THE SOFTWARE BE LIABLE
FOR ANY DAMAGES OR OTHER LIABILITY, WHETHER IN CONTRACT, TORT OR OTHERWISE,
ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE."#;

/// The MIT licence text; `{holders}` is the copyright line.
const MIT_TEXT: &str = r#"MIT License (this crate publishes no licence file; the standard text follows)

Copyright (c) {holders}

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE."#;

/// The licence ids named by a (simple) SPDX expression, and whether it has an `AND` (all of
/// them apply). `/` is the pre-SPDX spelling of `OR`.
fn spdx_ids(expression: &str) -> (Vec<&str>, bool) {
    let mut has_and = false;
    let ids = expression
        .split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == '/')
        .filter(|t| !t.is_empty())
        .filter(|t| match *t {
            "AND" => {
                has_and = true;
                false
            }
            "OR" | "WITH" => false,
            _ => true,
        })
        .collect();
    (ids, has_and)
}

/// A licence text for a crate that publishes none, when its expression allows an honest one:
/// Apache-2.0 (the standard text, which carries no per-crate copyright line) if it is offered,
/// the Boost text for BSL-1.0, or the MIT text with the Cargo.toml authors as the copyright line.
/// Expressions that need several texts (`AND`) get none, so they stay on the warning list.
pub fn fallback_text(krate: &Crate, apache: &str) -> Option<String> {
    let (ids, has_and) = spdx_ids(&krate.license);
    if has_and {
        return None;
    }
    if ids.contains(&"Apache-2.0") {
        return Some(format!(
            "[Apache-2.0 elected: this crate publishes no licence file; the standard text follows]\n\n{}",
            apache.trim()
        ));
    }
    if ids.contains(&"BSL-1.0") {
        return Some(BOOST_TEXT.to_owned());
    }
    if ids.contains(&"MIT") {
        let holders = if krate.authors.is_empty() {
            format!("the authors of {}", krate.name)
        } else {
            krate.authors.join(", ")
        };
        return Some(MIT_TEXT.replace("{holders}", &holders));
    }
    None
}

/// Builds the notices text. `apache` is the standard Apache-2.0 text (the repository's LICENSE).
/// Returns the text with the number of distinct licence texts and the crates that have none.
pub fn render_notices(crates: &[Crate], apache: &str) -> Result<(String, usize, Vec<String>)> {
    // text -> crates that carry it, in crate order
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut without = Vec::new();
    let mut listing = String::new();
    for krate in crates {
        let label = format!("{} {}", krate.name, krate.version);
        let license = if krate.license.is_empty() {
            "(see its licence file)"
        } else {
            krate.license.as_str()
        };
        let mut line = format!("{label}: {license}");
        if !krate.repository.is_empty() {
            let _ = write!(line, " <{}>", krate.repository);
        }
        let _ = writeln!(listing, "{line}");
        let mut files = license_files(krate);
        if files.is_empty() {
            match fallback_text(krate, apache) {
                Some(text) => files.push(("(standard text)".to_owned(), text)),
                None => {
                    without.push(label);
                    continue;
                }
            }
        }
        for (_, text) in files {
            let list = groups.entry(text.trim().to_owned()).or_default();
            if !list.contains(&label) {
                list.push(label.clone());
            }
        }
    }

    let mut out = String::new();
    out.push_str(
        "GANDER THIRD-PARTY NOTICES\n\
         ==========================\n\n\
         Gander is licensed under the Apache License 2.0 (see LICENSE.txt). It includes the\n\
         third-party software listed below, each under its own licence. Part 1 lists every\n\
         component with its licence expression; part 2 reproduces the licence texts, each text\n\
         once, followed by the components it applies to.\n\n\
         Fonts bundled with the settings window (egui's epaint_default_fonts) are under the SIL\n\
         Open Font Licence 1.1 (Hack's DejaVu-derived glyphs, Noto Emoji), the Ubuntu Font\n\
         Licence 1.0 (Ubuntu-Light) and the MIT licence (emoji-icon-font); their texts are in\n\
         part 2 under epaint_default_fonts. clipboard-win and error-code are under the Boost\n\
         Software Licence 1.0.\n\n",
    );
    out.push_str("PART 1: COMPONENTS\n------------------\n\n");
    out.push_str(&listing);
    out.push_str("\n\nPART 2: LICENCE TEXTS\n---------------------\n");
    for (i, (text, users)) in groups.iter().enumerate() {
        let _ = write!(
            out,
            "\n\n=== Licence text {} of {} ===\nApplies to:\n",
            i + 1,
            groups.len()
        );
        for user in users {
            let _ = writeln!(out, "  - {user}");
        }
        out.push('\n');
        out.push_str(text);
        out.push('\n');
    }
    if !without.is_empty() {
        out.push_str("\n\n=== Components without a licence file in their source ===\n");
        out.push_str("Their licence expression is given in part 1.\n");
        for name in &without {
            let _ = writeln!(out, "  - {name}");
        }
    }
    Ok((out, groups.len(), without))
}

/// Plain text as RTF (Consolas, 9 pt): `\`, `{`, `}` escaped, other non-ASCII as `\uN?`, one
/// paragraph per line.
pub fn rtf_from_text(text: &str) -> String {
    let mut out = String::from(
        "{\\rtf1\\ansi\\ansicpg1252\\deff0{\\fonttbl{\\f0\\fmodern\\fcharset0 Consolas;}}\\f0\\fs18\n",
    );
    for line in text.replace("\r\n", "\n").lines() {
        for c in line.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '{' => out.push_str("\\{"),
                '}' => out.push_str("\\}"),
                '\t' => out.push_str("\\tab "),
                c if c.is_ascii() && !c.is_ascii_control() => out.push(c),
                c => {
                    // RTF wants UTF-16 code units as signed 16-bit decimals.
                    let mut buf = [0u16; 2];
                    for unit in c.encode_utf16(&mut buf) {
                        let _ = write!(out, "\\u{}?", *unit as i16);
                    }
                }
            }
        }
        out.push_str("\\par\n");
    }
    out.push('}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "xtask-notices-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn licence_names_are_recognised() {
        for n in [
            "LICENSE",
            "LICENSE-MIT",
            "license-apache-2.0",
            "LICENCE.md",
            "COPYING",
            "NOTICE",
            "UNLICENSE",
            "OFL.txt",
            "UFL.txt",
            "emoji-icon-font-mit-license.txt",
        ] {
            assert!(is_license_name(n), "{n}");
        }
        for n in [
            "Cargo.toml",
            "README.md",
            "build.rs",
            "lib.rs",
            "Hack-Regular.ttf",
        ] {
            assert!(!is_license_name(n), "{n}");
        }
    }

    #[test]
    fn rtf_escapes_and_wraps() {
        let rtf = rtf_from_text("a {b} \\ c\r\n\tcaf\u{e9} \u{2014} \u{1f600}\n");
        assert!(rtf.starts_with("{\\rtf1"));
        assert!(rtf.ends_with('}'));
        assert!(rtf.contains("a \\{b\\} \\\\ c\\par"));
        assert!(rtf.contains("\\tab caf\\u233? \\u8212? "));
        // U+1F600 is the surrogate pair D83D DE00.
        assert!(rtf.contains("\\u-10179?\\u-8704?"), "{rtf}");
        assert!(rtf.is_ascii());
    }

    #[test]
    fn closure_follows_normal_dependencies_only() {
        let meta = json!({
            "workspace_members": ["path+host#gander-host@0.0.0", "path+host#gander-render@0.0.0"],
            "packages": [
                {"id": "path+host#gander-host@0.0.0", "name": "gander-host", "version": "0.0.0", "source": null, "manifest_path": "C:/w/host/Cargo.toml"},
                {"id": "path+host#gander-render@0.0.0", "name": "gander-render", "version": "0.0.0", "source": null, "manifest_path": "C:/w/render/Cargo.toml"},
                {"id": "reg#normal@1.0.0", "name": "normal", "version": "1.0.0", "source": "registry+x", "license": "MIT", "manifest_path": "C:/r/normal/Cargo.toml", "authors": ["A"], "repository": "https://example.com/n"},
                {"id": "reg#transitive@2.0.0", "name": "transitive", "version": "2.0.0", "source": "registry+x", "license": "Apache-2.0", "manifest_path": "C:/r/transitive/Cargo.toml"},
                {"id": "reg#buildonly@1.0.0", "name": "buildonly", "version": "1.0.0", "source": "registry+x", "license": "MIT", "manifest_path": "C:/r/buildonly/Cargo.toml"},
                {"id": "reg#devonly@1.0.0", "name": "devonly", "version": "1.0.0", "source": "registry+x", "license": "MIT", "manifest_path": "C:/r/devonly/Cargo.toml"},
                {"id": "reg#both@1.0.0", "name": "both", "version": "1.0.0", "source": "registry+x", "license": "MIT", "manifest_path": "C:/r/both/Cargo.toml"}
            ],
            "resolve": {"nodes": [
                {"id": "path+host#gander-host@0.0.0", "deps": [
                    {"pkg": "reg#normal@1.0.0", "dep_kinds": [{"kind": null, "target": null}]},
                    {"pkg": "reg#buildonly@1.0.0", "dep_kinds": [{"kind": "build", "target": null}]},
                    {"pkg": "reg#devonly@1.0.0", "dep_kinds": [{"kind": "dev", "target": null}]},
                    {"pkg": "reg#both@1.0.0", "dep_kinds": [{"kind": "build", "target": null}, {"kind": null, "target": null}]}
                ]},
                {"id": "path+host#gander-render@0.0.0", "deps": [
                    {"pkg": "reg#normal@1.0.0", "dep_kinds": [{"kind": null, "target": null}]}
                ]},
                {"id": "reg#normal@1.0.0", "deps": [
                    {"pkg": "reg#transitive@2.0.0", "dep_kinds": [{"kind": null, "target": null}]}
                ]},
                {"id": "reg#transitive@2.0.0", "deps": []},
                {"id": "reg#buildonly@1.0.0", "deps": []},
                {"id": "reg#devonly@1.0.0", "deps": []},
                {"id": "reg#both@1.0.0", "deps": []}
            ]}
        });
        let crates = shipped_crates(&meta).unwrap();
        let names: Vec<_> = crates.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["both", "normal", "transitive"]);
        assert_eq!(crates[1].authors, ["A"]);
        assert_eq!(crates[1].dir, Path::new("C:/r/normal"));

        // Without the render worker among the members the check refuses.
        let mut missing = meta.clone();
        missing["workspace_members"] = json!(["path+host#gander-host@0.0.0"]);
        assert!(shipped_crates(&missing).is_err());
    }

    #[test]
    fn licence_files_are_found_at_top_level_and_in_font_folders() {
        let dir = scratch("files");
        std::fs::write(dir.join("LICENSE-MIT"), "MIT text\r\nline two").unwrap();
        std::fs::write(dir.join("README.md"), "not a licence").unwrap();
        std::fs::write(dir.join("empty-LICENSE"), "   \n").unwrap();
        std::fs::create_dir_all(dir.join("fonts")).unwrap();
        std::fs::write(dir.join("fonts").join("OFL.txt"), "OFL text").unwrap();
        std::fs::write(dir.join("fonts").join("Hack-Regular.txt"), "Hack statement").unwrap();
        std::fs::write(dir.join("fonts").join("Hack-Regular.ttf"), "binary").unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        std::fs::write(dir.join("tests").join("LICENSE"), "test fixture").unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src").join("notes.txt"), "not a licence").unwrap();
        let krate = Crate {
            name: "x".into(),
            version: "1".into(),
            license: "MIT".into(),
            repository: String::new(),
            authors: vec![],
            dir: dir.clone(),
            license_file: None,
        };
        let files = license_files(&krate);
        let names: Vec<_> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["LICENSE-MIT", "fonts/Hack-Regular.txt", "fonts/OFL.txt"]
        );
        assert_eq!(files[0].1, "MIT text\nline two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crates_without_a_file_get_an_honest_fallback_or_a_warning() {
        let mk = |license: &str, authors: &[&str]| Crate {
            name: "x".into(),
            version: "1.0.0".into(),
            license: license.into(),
            repository: String::new(),
            authors: authors.iter().map(|a| (*a).to_owned()).collect(),
            dir: PathBuf::new(),
            license_file: None,
        };
        let apache = "Apache License text";
        let dual = fallback_text(&mk("MIT OR Apache-2.0", &["A"]), apache).unwrap();
        assert!(dual.contains("Apache License text") && dual.contains("Apache-2.0 elected"));
        let slash = fallback_text(&mk("MIT/Apache-2.0", &[]), apache).unwrap();
        assert!(slash.contains("Apache License text"));
        let boost = fallback_text(&mk("BSL-1.0", &[]), apache).unwrap();
        assert!(boost.starts_with("Boost Software License - Version 1.0"));
        let mit = fallback_text(&mk("MIT", &["Jo <jo@example.com>", "Al"]), apache).unwrap();
        assert!(
            mit.contains("Copyright (c) Jo <jo@example.com>, Al"),
            "{mit}"
        );
        assert!(mit.contains("THE SOFTWARE IS PROVIDED \"AS IS\""));
        let nameless = fallback_text(&mk("MIT", &[]), apache).unwrap();
        assert!(nameless.contains("the authors of x"));
        // Several texts apply, or nothing known: no invented text.
        assert!(fallback_text(&mk("(MIT OR Apache-2.0) AND OFL-1.1", &[]), apache).is_none());
        assert!(fallback_text(&mk("GPL-3.0-only", &[]), apache).is_none());
        assert!(fallback_text(&mk("", &[]), apache).is_none());
    }

    #[test]
    fn identical_texts_are_grouped_and_missing_ones_listed() {
        let dir_a = scratch("a");
        let dir_b = scratch("b");
        let dir_c = scratch("c");
        std::fs::write(dir_a.join("LICENSE"), "Same text\n").unwrap();
        std::fs::write(dir_b.join("LICENSE"), "Same text").unwrap();
        let mk = |name: &str, dir: &Path| Crate {
            name: name.into(),
            version: "1.0.0".into(),
            license: "MIT".into(),
            repository: "https://example.com".into(),
            authors: vec![],
            dir: dir.to_path_buf(),
            license_file: None,
        };
        let mut c = mk("c", &dir_c);
        c.license = "GPL-3.0-only".into(); // no honest fallback text exists
        let crates = [mk("a", &dir_a), mk("b", &dir_b), c];
        let (text, distinct, without) = render_notices(&crates, "Apache text").unwrap();
        assert_eq!(distinct, 1);
        assert_eq!(without, ["c 1.0.0"]);
        assert_eq!(text.matches("Same text").count(), 1);
        assert!(text.contains("  - a 1.0.0\n  - b 1.0.0\n"));
        assert!(text.contains("a 1.0.0: MIT <https://example.com>"));
        assert!(text.contains("Components without a licence file"));
        for d in [dir_a, dir_b, dir_c] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
