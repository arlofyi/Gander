// SPDX-License-Identifier: Apache-2.0
//! The dependency list of a binary in the `cargo auditable` format, and the build-script glue that
//! embeds it in a Windows executable (WP-82). Included, not compiled as a crate, by
//! `host/gander-host/build.rs`, `host/gander-render/build.rs` and `xtask/src/sbom.rs`
//! (`#[path = "../../build-support/auditable.rs"]`), so the list inside the exe and the SBOM
//! come from one piece of code.
//!
//! Why this exists: `cargo auditable` is not installed on this machine and installing tools is not
//! allowed, so the same artefact is written by hand. The format (the `auditable-serde` crate's
//! `VersionInfo`, "format 0") is JSON, compressed as a zlib stream, stored in a section named
//! `.dep-v0`, which `rust-audit-info`, `cargo audit bin` and `osv-scanner` read:
//!
//! ```text
//! {"format":0,"packages":[{"name":"serde","version":"1.0.0","source":"crates.io"},
//!   {"name":"gander-host","version":"0.0.0","source":"local","dependencies":[0],"root":true}]}
//! ```
//!
//! `source` is `crates.io`, `registry`, `git`, `local` or `other`; `kind` is omitted for a runtime
//! package and is `"build"` for one that is only needed to build (build scripts, proc macros);
//! `dependencies` holds indices into `packages` (sorted by name, version, source); `root` marks the
//! binary's own package. The one difference from the real tool: `cargo auditable` wraps `rustc`
//! and so sees the exact feature set of the compilation, while a build script can only ask
//! `cargo metadata`, which resolves the features of the whole workspace. The list is therefore
//! never smaller than what is linked, and may name a crate that a `-p` build leaves out.
//!
//! The section is added by passing a tiny COFF object (one data section, no code) to the linker as
//! a link argument: MSVC `link.exe` keeps every non-COMDAT section of an object it is handed, and
//! `/OPT:REF` does not remove it. Windows MSVC only; other targets get no section.
#![allow(dead_code)]

use std::collections::{BTreeSet, HashMap};
use std::io::Write as _;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

/// Name of the PE/COFF section that carries the list.
pub const SECTION_NAME: &str = ".dep-v0";

/// The `format` field of the document.
pub const FORMAT_VERSION: u64 = 0;

/// Why a package is in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Linked into the binary (a normal dependency, transitively).
    Runtime,
    /// Only needed to build it: a build-script dependency or a procedural macro.
    Build,
}

/// One package of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// The `cargo metadata` package id (not part of the serialised list).
    pub id: String,
    pub name: String,
    pub version: String,
    /// `crates.io`, `registry`, `git`, `local` or `other`.
    pub source: &'static str,
    pub kind: Kind,
    /// Indices into the sorted package list.
    pub dependencies: Vec<usize>,
    pub root: bool,
}

fn source_kind(source: &Value) -> &'static str {
    match source.as_str() {
        None => "local",
        Some("registry+https://github.com/rust-lang/crates.io-index") => "crates.io",
        Some("sparse+https://index.crates.io/") => "crates.io",
        Some(s) if s.starts_with("registry+") || s.starts_with("sparse+") => "registry",
        Some(s) if s.starts_with("git+") => "git",
        Some(_) => "other",
    }
}

/// The dependency list of the workspace members named in `roots`, from `cargo metadata` output
/// (`--format-version 1`, ideally with `--filter-platform`): normal and build dependencies are
/// followed, dev dependencies are not. A package is [`Kind::Runtime`] when it is reachable through
/// normal dependencies only and is not a procedural macro. Sorted by name, version, source.
pub fn collect(metadata: &Value, roots: &[&str]) -> Result<Vec<Package>, String> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata has no packages")?;
    let by_id: HashMap<&str, &Value> = packages
        .iter()
        .filter_map(|p| Some((p["id"].as_str()?, p)))
        .collect();
    let nodes: HashMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata has no resolve graph")?
        .iter()
        .filter_map(|n| Some((n["id"].as_str()?, n)))
        .collect();
    let members: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .ok_or("cargo metadata has no workspace_members")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let mut root_ids = Vec::new();
    for root in roots {
        let id = members
            .iter()
            .copied()
            .find(|id| by_id.get(id).and_then(|p| p["name"].as_str()) == Some(*root))
            .ok_or_else(|| format!("workspace member {root} not found"))?;
        root_ids.push(id);
    }

    let is_proc_macro = |id: &str| {
        by_id.get(id).is_some_and(|p| {
            p["targets"].as_array().into_iter().flatten().any(|t| {
                t["kind"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|k| k.as_str() == Some("proc-macro"))
            })
        })
    };
    // `(dependency id, reached through a normal dependency)` for every non-dev edge of a node.
    let edges = |id: &str| -> Vec<(&str, bool)> {
        let Some(node) = nodes.get(id) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for dep in node["deps"].as_array().into_iter().flatten() {
            let Some(pkg) = dep["pkg"].as_str() else {
                continue;
            };
            let kinds: Vec<&Value> = dep["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|k| &k["kind"])
                .collect();
            let normal = kinds.iter().any(|k| k.is_null());
            let build = kinds.iter().any(|k| k.as_str() == Some("build"));
            if normal || build {
                out.push((pkg, normal));
            }
        }
        out
    };

    // Everything that is needed (normal or build edges), and the runtime subset (normal edges
    // only, not through a procedural macro).
    let mut all: BTreeSet<&str> = BTreeSet::new();
    let mut stack = root_ids.clone();
    while let Some(id) = stack.pop() {
        if all.insert(id) {
            stack.extend(edges(id).into_iter().map(|(dep, _)| dep));
        }
    }
    let mut runtime: BTreeSet<&str> = BTreeSet::new();
    let mut stack = root_ids.clone();
    while let Some(id) = stack.pop() {
        if runtime.insert(id) {
            stack.extend(
                edges(id)
                    .into_iter()
                    .filter(|(dep, normal)| *normal && !is_proc_macro(dep))
                    .map(|(dep, _)| dep),
            );
        }
    }

    let mut list: Vec<Package> = Vec::new();
    for id in &all {
        let Some(p) = by_id.get(id) else { continue };
        list.push(Package {
            id: (*id).to_owned(),
            name: p["name"].as_str().unwrap_or_default().to_owned(),
            version: p["version"].as_str().unwrap_or_default().to_owned(),
            source: source_kind(&p["source"]),
            kind: if runtime.contains(id) {
                Kind::Runtime
            } else {
                Kind::Build
            },
            dependencies: Vec::new(),
            root: root_ids.contains(id),
        });
    }
    list.sort_by(|a, b| {
        (&a.name, &a.version, a.source, a.kind).cmp(&(&b.name, &b.version, b.source, b.kind))
    });
    let index: HashMap<String, usize> = list
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id.clone(), i))
        .collect();
    for package in &mut list {
        let mut deps: Vec<usize> = edges(&package.id)
            .into_iter()
            .filter_map(|(dep, _)| index.get(dep).copied())
            .collect();
        deps.sort_unstable();
        deps.dedup();
        package.dependencies = deps;
    }
    Ok(list)
}

/// The JSON document of the list (the uncompressed payload of `.dep-v0`).
pub fn to_json(packages: &[Package]) -> Vec<u8> {
    let list: Vec<Value> = packages
        .iter()
        .map(|p| {
            let mut entry = json!({
                "name": p.name,
                "version": p.version,
                "source": p.source,
            });
            if p.kind == Kind::Build {
                entry["kind"] = json!("build");
            }
            if !p.dependencies.is_empty() {
                entry["dependencies"] = json!(p.dependencies);
            }
            if p.root {
                entry["root"] = json!(true);
            }
            entry
        })
        .collect();
    serde_json::to_vec(&json!({ "packages": list, "format": FORMAT_VERSION })).unwrap_or_default()
}

/// The JSON document as a zlib stream (the payload of `.dep-v0`).
pub fn compress(json: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    encoder
        .write_all(json)
        .map_err(|e| format!("compressing the dependency list: {e}"))?;
    encoder
        .finish()
        .map_err(|e| format!("compressing the dependency list: {e}"))
}

/// A COFF object file with one initialised read-only data section named [`SECTION_NAME`] that
/// holds `payload`, and one external symbol `AUDITABLE_VERSION_INFO` at its start. `machine` is
/// the COFF machine type (`0x8664` x64, `0xAA64` ARM64). No timestamp, so the bytes depend only on
/// the payload.
pub fn coff_object(machine: u16, payload: &[u8]) -> Vec<u8> {
    const FILE_HEADER: usize = 20;
    const SECTION_HEADER: usize = 40;
    const SYMBOL: usize = 18;
    const SYMBOL_NAME: &str = "AUDITABLE_VERSION_INFO";
    // IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_ALIGN_1BYTES | IMAGE_SCN_MEM_READ
    const CHARACTERISTICS: u32 = 0x0000_0040 | 0x0010_0000 | 0x4000_0000;

    let data_offset = FILE_HEADER + SECTION_HEADER;
    let symbol_offset = data_offset + payload.len();
    let mut out = Vec::with_capacity(symbol_offset + SYMBOL + 4 + SYMBOL_NAME.len() + 1);
    // IMAGE_FILE_HEADER
    out.extend_from_slice(&machine.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // NumberOfSections
    out.extend_from_slice(&0u32.to_le_bytes()); // TimeDateStamp
    out.extend_from_slice(&(symbol_offset as u32).to_le_bytes()); // PointerToSymbolTable
    out.extend_from_slice(&1u32.to_le_bytes()); // NumberOfSymbols
    out.extend_from_slice(&0u16.to_le_bytes()); // SizeOfOptionalHeader
    out.extend_from_slice(&0u16.to_le_bytes()); // Characteristics
    // IMAGE_SECTION_HEADER
    let mut name = [0u8; 8];
    name[..SECTION_NAME.len()].copy_from_slice(SECTION_NAME.as_bytes());
    out.extend_from_slice(&name);
    out.extend_from_slice(&0u32.to_le_bytes()); // VirtualSize
    out.extend_from_slice(&0u32.to_le_bytes()); // VirtualAddress
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // SizeOfRawData
    out.extend_from_slice(&(data_offset as u32).to_le_bytes()); // PointerToRawData
    out.extend_from_slice(&0u32.to_le_bytes()); // PointerToRelocations
    out.extend_from_slice(&0u32.to_le_bytes()); // PointerToLinenumbers
    out.extend_from_slice(&0u16.to_le_bytes()); // NumberOfRelocations
    out.extend_from_slice(&0u16.to_le_bytes()); // NumberOfLinenumbers
    out.extend_from_slice(&CHARACTERISTICS.to_le_bytes());
    // Raw data
    out.extend_from_slice(payload);
    // Symbol table: one external symbol in section 1, its name in the string table.
    out.extend_from_slice(&[0u8; 4]); // Name: zeroes, then the string table offset
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // Value
    out.extend_from_slice(&1i16.to_le_bytes()); // SectionNumber
    out.extend_from_slice(&0u16.to_le_bytes()); // Type
    out.push(2); // StorageClass: IMAGE_SYM_CLASS_EXTERNAL
    out.push(0); // NumberOfAuxSymbols
    // String table: its own size (including the size field), then the NUL-terminated name.
    out.extend_from_slice(&((4 + SYMBOL_NAME.len() + 1) as u32).to_le_bytes());
    out.extend_from_slice(SYMBOL_NAME.as_bytes());
    out.push(0);
    out
}

/// `cargo metadata` for the workspace that holds `manifest`, restricted to `target`.
pub fn cargo_metadata(cargo: &str, manifest: &Path, target: &str) -> Result<Value, String> {
    let out = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--locked", "--offline"])
        .args(["--filter-platform", target, "--manifest-path"])
        .arg(manifest)
        .output()
        .map_err(|e| format!("running cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata output: {e}"))
}

/// Build-script entry point: embeds the dependency list of the binary `binary` (a workspace
/// member of the same name) into it. Does nothing, with a `cargo:warning`, when the list cannot
/// be made; `cargo xtask installer-build` fails the build when the finished exe lacks the section.
pub fn embed(binary: &str) {
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    // Cargo.lock decides the list: rebuild when it, or any manifest, changes.
    println!("cargo:rerun-if-changed=../build-support/auditable.rs");
    println!("cargo:rerun-if-changed=../../Cargo.lock");
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    if var("CARGO_CFG_TARGET_OS") != "windows" || var("CARGO_CFG_TARGET_ENV") != "msvc" {
        return;
    }
    let machine: u16 = match var("CARGO_CFG_TARGET_ARCH").as_str() {
        "x86_64" => 0x8664,
        "aarch64" => 0xAA64,
        other => {
            println!("cargo:warning=auditable: no .dep-v0 section for the {other} architecture");
            return;
        }
    };
    let result = (|| -> Result<std::path::PathBuf, String> {
        let manifest = Path::new(&var("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let metadata = cargo_metadata(&var("CARGO"), &manifest, &var("TARGET"))?;
        let packages = collect(&metadata, &[binary])?;
        // The manifests of the workspace crates in this binary's closure decide its features.
        for package in packages.iter().filter(|p| p.source == "local") {
            let manifest = metadata["packages"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|m| m["id"].as_str() == Some(package.id.as_str()))
                .and_then(|m| m["manifest_path"].as_str());
            if let Some(path) = manifest {
                println!("cargo:rerun-if-changed={path}");
            }
        }
        let object = coff_object(machine, &compress(&to_json(&packages))?);
        let path = Path::new(&var("OUT_DIR")).join("dep-v0.obj");
        std::fs::write(&path, object).map_err(|e| format!("writing {}: {e}", path.display()))?;
        Ok(path)
    })();
    match result {
        Ok(path) => println!(
            "cargo:rustc-link-arg-bin={binary}={}",
            path.display().to_string().replace('\\', "/")
        ),
        Err(e) => println!(
            "cargo:warning=auditable: {e}; {binary} will not carry a {SECTION_NAME} section"
        ),
    }
}
