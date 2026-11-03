// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask sbom` (WP-82): the CycloneDX 1.5 SBOM of the host installer payload, and the check
//! of the dependency list embedded in the executables.
//!
//! - `cargo xtask sbom [--out-dir <dir>]` writes `gander-host.cdx.json` (default `target/sbom/`):
//!   every crate in the dependency closure of `gander-host` and `gander-render` for
//!   `x86_64-pc-windows-msvc`, from `cargo metadata` (names, versions, licences, repositories) and
//!   `Cargo.lock` (the SHA-256 of each `.crate` file, which is what Cargo verifies on download).
//!   Crates linked into the binaries are `scope: required`; build-script and proc-macro crates are
//!   `scope: excluded` (they are in the build, not in the product) and say so in a property. The
//!   document is validated against the CycloneDX 1.5 JSON schema before it is written, and the
//!   command fails rather than write an invalid file.
//! - `cargo xtask sbom --check [<file>...]` validates existing documents (default: every
//!   `target/sbom/*.cdx.json`), for example the Android one from `./gradlew cyclonedxSbom`.
//!
//! The same dependency walk ([`auditable::collect`]) writes the `.dep-v0` section that
//! `host/gander-host/build.rs` and `host/gander-render/build.rs` embed, so the two cannot drift;
//! [`check_binary`] reads that section back out of a built exe and compares it with the SBOM.
//!
//! The output is deterministic: sorted, a serial number derived from the content, and a timestamp
//! from `SOURCE_DATE_EPOCH` or else the HEAD commit time (the wall clock only outside a git
//! checkout). The only machine-dependent parts are the `rustc` tool entry and the path-free
//! property values.
//!
//! The schema files in `xtask/schemas/cyclonedx-1.5/` are the unmodified `bom-1.5.schema.json`,
//! `spdx.schema.json` and `jsf-0.82.schema.json` from <https://github.com/CycloneDX/specification>
//! (tag `1.5`, Apache-2.0). The validator is [`crate::jsonschema`].

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::jsonschema::Schemas;
use crate::notices::{SHIPPED_BINARIES, TARGET};
use crate::proc::{display_path, workspace_root};

// The dependency walk and the `.dep-v0` writer, shared with the two build scripts.
#[path = "../../host/build-support/auditable.rs"]
pub mod auditable;

const BOM_SCHEMA: &str = include_str!("../schemas/cyclonedx-1.5/bom-1.5.schema.json");
const SPDX_SCHEMA: &str = include_str!("../schemas/cyclonedx-1.5/spdx.schema.json");
const JSF_SCHEMA: &str = include_str!("../schemas/cyclonedx-1.5/jsf-0.82.schema.json");
const BOM_SCHEMA_NAME: &str = "bom-1.5.schema.json";

/// The file the MSI ships and CI uploads.
pub const HOST_SBOM: &str = "gander-host.cdx.json";

/// The product's manufacturer (the same string as the MSI's `Manufacturer`).
const MANUFACTURER: &str = "Arlo";

fn schemas() -> &'static Schemas {
    static SCHEMAS: OnceLock<Schemas> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        Schemas::new(&[
            (BOM_SCHEMA_NAME, BOM_SCHEMA),
            ("spdx.schema.json", SPDX_SCHEMA),
            ("jsf-0.82.schema.json", JSF_SCHEMA),
        ])
        .expect("the vendored CycloneDX schemas parse")
    })
}

/// The SPDX licence ids CycloneDX 1.5 accepts in `license.id`.
fn spdx_ids() -> &'static HashSet<String> {
    static IDS: OnceLock<HashSet<String>> = OnceLock::new();
    IDS.get_or_init(|| {
        schemas()
            .document("spdx.schema.json")
            .and_then(|d| d["enum"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    })
}

// ---------------------------------------------------------------------------------------------
// Inputs: Cargo.lock, the clock, the toolchain
// ---------------------------------------------------------------------------------------------

/// `(name, version)` to the SHA-256 (hex) Cargo.lock records for the crate's `.crate` file.
pub fn lock_checksums(lock_text: &str) -> Result<HashMap<(String, String), String>> {
    let doc: toml::Table = lock_text.parse().context("Cargo.lock is not valid TOML")?;
    let mut out = HashMap::new();
    for package in doc
        .get("package")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        let field = |key: &str| package.get(key).and_then(toml::Value::as_str);
        if let (Some(name), Some(version), Some(checksum)) =
            (field("name"), field("version"), field("checksum"))
        {
            out.insert(
                (name.to_owned(), version.to_owned()),
                checksum.to_ascii_lowercase(),
            );
        }
    }
    Ok(out)
}

/// An RFC 3339 UTC timestamp (`2026-11-02T17:50:17Z`) from Unix seconds (proleptic Gregorian,
/// days-from-civil inverse).
pub fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

/// The document timestamp: `SOURCE_DATE_EPOCH`, else the HEAD commit time, else now.
fn timestamp(root: &Path) -> String {
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .or_else(|| {
            let out = Command::new("git")
                .current_dir(root)
                .args(["log", "-1", "--format=%ct"])
                .output()
                .ok()?;
            String::from_utf8_lossy(&out.stdout).trim().parse().ok()
        })
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
        });
    rfc3339(secs)
}

/// `(release, commit-hash, host)` of the `rustc` that builds the workspace, from `rustc -vV`.
fn rustc_version() -> Option<(String, String, String)> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let out = Command::new(rustc).arg("-vV").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_owned())
    };
    Some((
        field("release:")?,
        field("commit-hash:").unwrap_or_default(),
        field("host:").unwrap_or_default(),
    ))
}

// ---------------------------------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------------------------------

fn purl(package: &auditable::Package) -> String {
    format!("pkg:cargo/{}@{}", package.name, package.version)
}

/// Cargo's `license` field as a CycloneDX licence choice: one SPDX id the schema knows becomes
/// `{"license": {"id": ...}}`; anything else is an SPDX `expression` (the old `A/B` spelling of
/// `A OR B` is rewritten); a crate with only a `license-file` says so by name.
fn licenses(meta: &Value, known_ids: &HashSet<String>) -> Option<Value> {
    let raw = meta["license"].as_str().unwrap_or_default().trim();
    if raw.is_empty() {
        let file = meta["license_file"].as_str()?;
        return Some(
            json!([{ "license": { "name": format!("See {file} in the package source") } }]),
        );
    }
    let expression = raw
        .replace('/', " OR ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if known_ids.contains(&expression) {
        Some(json!([{ "license": { "id": expression } }]))
    } else {
        Some(json!([{ "expression": expression }]))
    }
}

fn external_references(meta: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for (field, kind) in [
        ("repository", "vcs"),
        ("homepage", "website"),
        ("documentation", "documentation"),
    ] {
        let Some(url) = meta[field].as_str().map(str::trim) else {
            continue;
        };
        let usable = (url.starts_with("https://") || url.starts_with("http://"))
            && !url.contains(char::is_whitespace);
        let entry = json!({ "type": kind, "url": url });
        if usable && !out.contains(&entry) {
            out.push(entry);
        }
    }
    out
}

fn component(
    package: &auditable::Package,
    meta: &Value,
    checksums: &HashMap<(String, String), String>,
    known_ids: &HashSet<String>,
) -> Value {
    let reference = purl(package);
    let kind = match package.kind {
        auditable::Kind::Runtime => "runtime",
        auditable::Kind::Build => "build",
    };
    let first_party = package.source == "local";
    let mut c = json!({
        "type": if first_party && SHIPPED_BINARIES.contains(&package.name.as_str()) {
            "application"
        } else {
            "library"
        },
        "bom-ref": reference,
        "name": package.name,
        "version": package.version,
        "purl": reference,
        "scope": if package.kind == auditable::Kind::Runtime { "required" } else { "excluded" },
        "properties": [
            { "name": "gander:cargo-kind", "value": kind },
            { "name": "gander:cargo-source", "value": package.source },
        ],
    });
    if let Some(description) = meta["description"].as_str().map(str::trim)
        && !description.is_empty()
    {
        c["description"] = json!(description);
    }
    if let Some(licenses) = licenses(meta, known_ids) {
        c["licenses"] = licenses;
    }
    if let Some(checksum) = checksums.get(&(package.name.clone(), package.version.clone())) {
        c["hashes"] = json!([{ "alg": "SHA-256", "content": checksum }]);
    }
    let references = external_references(meta);
    if !references.is_empty() {
        c["externalReferences"] = json!(references);
    }
    c
}

/// A UUID-shaped serial number from the content (UUID version 8, the "custom" version): the same
/// inputs always give the same number.
fn serial_number(seed: &str) -> String {
    let hash = blake3::hash(seed.as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&hash.as_bytes()[..16]);
    b[6] = (b[6] & 0x0f) | 0x80;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Builds the CycloneDX 1.5 document from `cargo metadata` output, the Cargo.lock checksums, the
/// timestamp and the tool entries. Pure.
pub fn build_bom(
    metadata: &Value,
    checksums: &HashMap<(String, String), String>,
    timestamp: &str,
    tools: Vec<Value>,
    known_ids: &HashSet<String>,
) -> Result<Value> {
    let roots = SHIPPED_BINARIES;
    let list = auditable::collect(metadata, &roots).map_err(anyhow::Error::msg)?;
    let meta_by_id: HashMap<&str, &Value> = metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p["id"].as_str()?, p)))
        .collect();
    let host = list
        .iter()
        .position(|p| p.name == roots[0] && p.root)
        .context("gander-host is not in the dependency list")?;

    let refs: Vec<String> = list.iter().map(purl).collect();
    let mut components: Vec<(String, Value)> = Vec::new();
    let mut root_component = Value::Null;
    for (i, package) in list.iter().enumerate() {
        let meta = meta_by_id
            .get(package.id.as_str())
            .copied()
            .unwrap_or(&Value::Null);
        let c = component(package, meta, checksums, known_ids);
        if i == host {
            root_component = c;
        } else {
            components.push((refs[i].clone(), c));
        }
    }
    components.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some(object) = root_component.as_object_mut() {
        // The product itself has no scope; it says what it is and what it ships.
        object.remove("scope");
        object.insert(
            "description".to_owned(),
            json!("Gander host: the tray application and its preview worker"),
        );
        object.insert(
            "properties".to_owned(),
            json!([
                { "name": "gander:target", "value": TARGET },
                { "name": "gander:binaries", "value": roots.iter().map(|r| format!("{r}.exe")).collect::<Vec<_>>().join(", ") },
                { "name": "gander:scope", "value": "crates linked into the binaries (scope required) and crates needed only to build them (scope excluded)" },
            ]),
        );
    }

    let mut dependencies: Vec<(String, Vec<String>)> = Vec::new();
    for (i, package) in list.iter().enumerate() {
        let mut depends: BTreeSet<String> = package
            .dependencies
            .iter()
            .map(|d| refs[*d].clone())
            .collect();
        if i == host {
            // The host starts the render worker beside it; it ships and runs as one product.
            depends.extend(
                list.iter()
                    .enumerate()
                    .filter(|(_, p)| p.root && p.name != roots[0])
                    .map(|(j, _)| refs[j].clone()),
            );
        }
        dependencies.push((refs[i].clone(), depends.into_iter().collect()));
    }
    dependencies.sort_by(|a, b| a.0.cmp(&b.0));

    let mut seed = String::from(timestamp);
    for ((name, version), checksum) in checksums
        .iter()
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .filter(|((name, version), _)| {
            list.iter()
                .any(|p| &p.name == name && &p.version == version)
        })
    {
        seed.push_str(&format!("\n{name}@{version}:{checksum}"));
    }
    for r in &refs {
        seed.push_str(&format!("\n{r}"));
    }

    Ok(json!({
        "$schema": "http://cyclonedx.org/schema/bom-1.5.schema.json",
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": serial_number(&seed),
        "version": 1,
        "metadata": {
            "timestamp": timestamp,
            "tools": { "components": tools },
            "manufacture": { "name": MANUFACTURER },
            "component": root_component,
        },
        "components": components.into_iter().map(|(_, c)| c).collect::<Vec<_>>(),
        "dependencies": dependencies
            .into_iter()
            .map(|(r, d)| json!({ "ref": r, "dependsOn": d }))
            .collect::<Vec<_>>(),
    }))
}

fn tool_entries() -> Vec<Value> {
    let mut tools = vec![json!({
        "type": "application",
        "author": MANUFACTURER,
        "name": "gander-xtask",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "cargo xtask sbom",
    })];
    if let Some((release, commit, host)) = rustc_version() {
        tools.push(json!({
            "type": "application",
            "author": "The Rust Project Developers",
            "name": "rustc",
            "version": release,
            "properties": [
                { "name": "commit-hash", "value": commit },
                { "name": "host", "value": host },
            ],
        }));
    }
    tools
}

// ---------------------------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------------------------

/// The CycloneDX 1.5 schema problems of a document.
pub fn schema_problems(bom: &Value) -> Vec<String> {
    schemas().validate(BOM_SCHEMA_NAME, bom)
}

/// Problems the schema cannot express: every `bom-ref` is unique, every `dependencies` entry
/// refers to a component that exists, and (for the host document) the properties of a
/// supply-chain SBOM hold: a version, a purl, a licence for every crate and a SHA-256 for every
/// registry crate.
pub fn semantic_problems(bom: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    // The schema file is the 1.5 one, so it cannot tell a 1.6 document from a 1.5 one.
    if bom["specVersion"].as_str() != Some("1.5") {
        problems.push(format!(
            "specVersion is {}, this check is for CycloneDX 1.5",
            bom["specVersion"]
        ));
    }
    let mut refs: Vec<&str> = Vec::new();
    let mut all_components: Vec<&Value> = Vec::new();
    fn walk<'a>(components: &'a Value, refs: &mut Vec<&'a str>, all: &mut Vec<&'a Value>) {
        for c in components.as_array().into_iter().flatten() {
            if let Some(r) = c["bom-ref"].as_str() {
                refs.push(r);
            }
            all.push(c);
            walk(&c["components"], refs, all);
        }
    }
    if let Some(r) = bom["metadata"]["component"]["bom-ref"].as_str() {
        refs.push(r);
    }
    walk(&bom["components"], &mut refs, &mut all_components);
    let unique: HashSet<&str> = refs.iter().copied().collect();
    if unique.len() != refs.len() {
        problems.push("two components share a bom-ref".to_owned());
    }
    for d in bom["dependencies"].as_array().into_iter().flatten() {
        let Some(r) = d["ref"].as_str() else { continue };
        if !unique.contains(r) {
            problems.push(format!("dependency entry for unknown component {r}"));
        }
        for target in d["dependsOn"].as_array().into_iter().flatten() {
            if let Some(t) = target.as_str()
                && !unique.contains(t)
            {
                problems.push(format!("{r} depends on unknown component {t}"));
            }
        }
    }
    if bom["metadata"]["component"]["name"].as_str() == Some(SHIPPED_BINARIES[0]) {
        let mut names = BTreeSet::new();
        for c in &all_components {
            let name = c["name"].as_str().unwrap_or("?");
            names.insert(name);
            if c["version"].as_str().is_none_or(str::is_empty) {
                problems.push(format!("component {name} has no version"));
            }
            if !c["purl"]
                .as_str()
                .is_some_and(|p| p.starts_with("pkg:cargo/"))
            {
                problems.push(format!("component {name} has no pkg:cargo purl"));
            }
            if c["licenses"].as_array().is_none_or(Vec::is_empty) {
                problems.push(format!("component {name} names no licence"));
            }
            let registry = c["properties"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|p| p["name"] == "gander:cargo-source" && p["value"] == "crates.io");
            if registry && c["hashes"].as_array().is_none_or(Vec::is_empty) {
                problems.push(format!("component {name} has no hash"));
            }
        }
        for binary in &SHIPPED_BINARIES[1..] {
            if !names.contains(binary) {
                problems.push(format!("{binary} is missing from the components"));
            }
        }
    }
    problems
}

/// Every problem of a document: schema first, then the semantic ones.
pub fn validate(bom: &Value) -> Vec<String> {
    let mut problems = schema_problems(bom);
    problems.extend(semantic_problems(bom));
    problems
}

// ---------------------------------------------------------------------------------------------
// The dependency list inside an executable
// ---------------------------------------------------------------------------------------------

/// The bytes of the PE section `name` of an image, or `None` when it is not a PE or has no such
/// section. Uses the smaller of VirtualSize and SizeOfRawData: the linker pads the raw data to the
/// file alignment.
pub fn pe_section<'a>(bytes: &'a [u8], name: &str) -> Option<&'a [u8]> {
    let u16_at = |o: usize| {
        bytes
            .get(o..o + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |o: usize| {
        bytes
            .get(o..o + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if bytes.get(0..2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(0x3c)? as usize;
    if bytes.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let sections = usize::from(u16_at(pe + 6)?);
    let optional = usize::from(u16_at(pe + 20)?);
    let table = pe + 24 + optional;
    for i in 0..sections {
        let at = table + 40 * i;
        let raw_name = bytes.get(at..at + 8)?;
        let end = raw_name.iter().position(|b| *b == 0).unwrap_or(8);
        if &raw_name[..end] != name.as_bytes() {
            continue;
        }
        let virtual_size = u32_at(at + 8)? as usize;
        let raw_size = u32_at(at + 16)? as usize;
        let raw_offset = u32_at(at + 20)? as usize;
        let size = if virtual_size == 0 {
            raw_size
        } else {
            virtual_size.min(raw_size)
        };
        return bytes.get(raw_offset..raw_offset + size);
    }
    None
}

/// Reads and decodes the `.dep-v0` list of an executable image.
pub fn embedded_list(bytes: &[u8]) -> Result<Value> {
    let section = pe_section(bytes, auditable::SECTION_NAME)
        .with_context(|| format!("no {} section", auditable::SECTION_NAME))?;
    let mut json = Vec::new();
    flate2::read::ZlibDecoder::new(section)
        .read_to_end(&mut json)
        .context("the section is not a zlib stream")?;
    serde_json::from_slice(&json).context("the section does not hold JSON")
}

/// Checks the list inside an executable: format 0, one root with the expected name, indices in
/// range, every package also in `bom` (the SBOM). Returns a one-line summary.
pub fn check_binary(bytes: &[u8], expected_root: &str, bom: &Value) -> Result<String> {
    let list = embedded_list(bytes)?;
    if list["format"].as_u64() != Some(auditable::FORMAT_VERSION) {
        bail!(
            "format is {:?}, expected {}",
            list["format"],
            auditable::FORMAT_VERSION
        );
    }
    let packages = list["packages"].as_array().context("no packages array")?;
    let roots: Vec<&Value> = packages.iter().filter(|p| p["root"] == true).collect();
    match roots.as_slice() {
        [root] if root["name"] == expected_root => {}
        other => bail!(
            "expected the one root {expected_root}, found {:?}",
            other.iter().map(|p| p["name"].clone()).collect::<Vec<_>>()
        ),
    }
    let mut in_bom: HashSet<(&str, &str)> = HashSet::new();
    if let (Some(n), Some(v)) = (
        bom["metadata"]["component"]["name"].as_str(),
        bom["metadata"]["component"]["version"].as_str(),
    ) {
        in_bom.insert((n, v));
    }
    for c in bom["components"].as_array().into_iter().flatten() {
        if let (Some(n), Some(v)) = (c["name"].as_str(), c["version"].as_str()) {
            in_bom.insert((n, v));
        }
    }
    let mut build_only = 0;
    for p in packages {
        let name = p["name"].as_str().unwrap_or_default();
        let version = p["version"].as_str().unwrap_or_default();
        if !in_bom.contains(&(name, version)) {
            bail!("{name} {version} is in the executable's list but not in the SBOM");
        }
        if p["kind"] == "build" {
            build_only += 1;
        }
        for d in p["dependencies"].as_array().into_iter().flatten() {
            if d.as_u64().is_none_or(|i| i as usize >= packages.len()) {
                bail!("{name} has the dependency index {d}, out of range");
            }
        }
    }
    let compressed = pe_section(bytes, auditable::SECTION_NAME).map_or(0, <[u8]>::len);
    Ok(format!(
        "{} packages ({build_only} build-only), root {expected_root}, {compressed} bytes compressed, all in the SBOM",
        packages.len()
    ))
}

// ---------------------------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------------------------

/// What [`generate`] wrote.
#[derive(Debug)]
pub struct Summary {
    pub path: PathBuf,
    pub bytes: usize,
    /// Third-party (registry) components.
    pub third_party: usize,
    /// Of those, the ones only needed to build.
    pub build_only: usize,
    pub bom: Value,
}

/// Writes `gander-host.cdx.json` into `out_dir`, validated.
pub fn generate(root: &Path, out_dir: &Path) -> Result<Summary> {
    let metadata = crate::notices::cargo_metadata(root)?;
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).context("reading Cargo.lock")?;
    let checksums = lock_checksums(&lock)?;
    let bom = build_bom(
        &metadata,
        &checksums,
        &timestamp(root),
        tool_entries(),
        spdx_ids(),
    )?;
    let problems = validate(&bom);
    if !problems.is_empty() {
        for p in problems.iter().take(20) {
            eprintln!("sbom: {p}");
        }
        bail!(
            "the generated SBOM is not valid CycloneDX 1.5 ({} problem(s)); nothing was written",
            problems.len()
        );
    }
    let mut text = serde_json::to_string_pretty(&bom)?;
    text.push('\n');
    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let path = out_dir.join(HOST_SBOM);
    std::fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
    let components = bom["components"].as_array().map_or(&[][..], Vec::as_slice);
    let registry: Vec<&Value> = components
        .iter()
        .filter(|c| c["hashes"].is_array())
        .collect();
    Ok(Summary {
        path,
        bytes: text.len(),
        third_party: registry.len(),
        build_only: registry.iter().filter(|c| c["scope"] == "excluded").count(),
        bom,
    })
}

/// Entry point for `cargo xtask sbom`.
pub fn run(args: &[String]) -> Result<ExitCode> {
    const USAGE: &str =
        "usage: cargo xtask sbom [--out-dir <dir>] | cargo xtask sbom --check [<file>...]";
    let root = workspace_root();
    let mut out_dir = root.join("target").join("sbom");
    let mut check = false;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out-dir" => {
                out_dir = PathBuf::from(it.next().context("--out-dir needs a value")?);
            }
            "--check" => check = true,
            other if check && !other.starts_with("--") => files.push(PathBuf::from(other)),
            other => bail!("unknown argument {other}; {USAGE}"),
        }
    }
    if check {
        return run_check(&out_dir, files);
    }
    let summary = generate(&root, &out_dir)?;
    println!(
        "sbom: {} third-party crate(s) ({} build-only), {} bytes, valid CycloneDX 1.5 -> {}",
        summary.third_party,
        summary.build_only,
        summary.bytes,
        display_path(&summary.path)
    );
    Ok(ExitCode::SUCCESS)
}

fn run_check(dir: &Path, mut files: Vec<PathBuf>) -> Result<ExitCode> {
    if files.is_empty() {
        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("reading {} (run `cargo xtask sbom` first)", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".cdx.json"))
            {
                files.push(path);
            }
        }
        files.sort();
    }
    if files.is_empty() {
        bail!("no *.cdx.json files in {}", dir.display());
    }
    let mut failed = false;
    for file in &files {
        let text =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let bom: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                println!("[FAIL ] {}: not JSON: {e}", display_path(file));
                failed = true;
                continue;
            }
        };
        let problems = validate(&bom);
        if problems.is_empty() {
            println!(
                "[OK   ] {}: valid CycloneDX {}, {} component(s)",
                display_path(file),
                bom["specVersion"].as_str().unwrap_or("?"),
                bom["components"].as_array().map_or(0, Vec::len)
            );
        } else {
            failed = true;
            println!(
                "[FAIL ] {}: {} problem(s)",
                display_path(file),
                problems.len()
            );
            for p in problems.iter().take(20) {
                println!("        {p}");
            }
        }
    }
    Ok(if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small workspace: host and render share `shared`; `macro_crate` is a proc macro;
    /// `build_dep` is a build-script dependency of host; `dev_only` is a dev dependency.
    fn metadata() -> Value {
        let pkg = |id: &str, name: &str, source: Value, license: &str, kind: &str| {
            json!({
                "id": id, "name": name, "version": "1.0.0", "source": source, "license": license,
                "description": format!("The {name} crate"),
                "repository": format!("https://github.com/example/{name}"),
                "targets": [{ "kind": [kind] }],
            })
        };
        let reg = json!("registry+https://github.com/rust-lang/crates.io-index");
        let dep = |pkg: &str, kind: Value| json!({ "pkg": pkg, "dep_kinds": [{ "kind": kind, "target": null }] });
        json!({
            "workspace_members": ["path+host#gander-host@1.0.0", "path+render#gander-render@1.0.0"],
            "packages": [
                pkg("path+host#gander-host@1.0.0", "gander-host", Value::Null, "Apache-2.0", "bin"),
                pkg("path+render#gander-render@1.0.0", "gander-render", Value::Null, "Apache-2.0", "bin"),
                pkg("reg#shared@1.0.0", "shared", reg.clone(), "MIT/Apache-2.0", "lib"),
                pkg("reg#leaf@1.0.0", "leaf", reg.clone(), "Apache-2.0 WITH LLVM-exception OR MIT", "lib"),
                pkg("reg#macro_crate@1.0.0", "macro_crate", reg.clone(), "MIT", "proc-macro"),
                pkg("reg#macro_helper@1.0.0", "macro_helper", reg.clone(), "MIT", "lib"),
                pkg("reg#build_dep@1.0.0", "build_dep", reg.clone(), "BSD-3-Clause", "lib"),
                pkg("reg#dev_only@1.0.0", "dev_only", reg, "MIT", "lib"),
            ],
            "resolve": { "nodes": [
                { "id": "path+host#gander-host@1.0.0", "deps": [
                    dep("reg#shared@1.0.0", Value::Null),
                    dep("reg#macro_crate@1.0.0", Value::Null),
                    dep("reg#build_dep@1.0.0", json!("build")),
                    dep("reg#dev_only@1.0.0", json!("dev")),
                ]},
                { "id": "path+render#gander-render@1.0.0", "deps": [dep("reg#shared@1.0.0", Value::Null)] },
                { "id": "reg#shared@1.0.0", "deps": [dep("reg#leaf@1.0.0", Value::Null)] },
                { "id": "reg#leaf@1.0.0", "deps": [] },
                { "id": "reg#macro_crate@1.0.0", "deps": [dep("reg#macro_helper@1.0.0", Value::Null)] },
                { "id": "reg#macro_helper@1.0.0", "deps": [] },
                { "id": "reg#build_dep@1.0.0", "deps": [] },
                { "id": "reg#dev_only@1.0.0", "deps": [] },
            ]},
        })
    }

    fn checksums() -> HashMap<(String, String), String> {
        ["shared", "leaf", "macro_crate", "macro_helper", "build_dep"]
            .iter()
            .enumerate()
            .map(|(i, n)| {
                (
                    ((*n).to_owned(), "1.0.0".to_owned()),
                    format!("{i:x}").repeat(64),
                )
            })
            .collect()
    }

    fn bom() -> Value {
        build_bom(
            &metadata(),
            &checksums(),
            "2026-11-02T10:00:00Z",
            vec![json!({"type": "application", "name": "t", "version": "1"})],
            spdx_ids(),
        )
        .unwrap()
    }

    #[test]
    fn the_walk_sorts_kinds_and_skips_dev_dependencies() {
        let list = auditable::collect(&metadata(), &["gander-host", "gander-render"]).unwrap();
        let names: Vec<&str> = list.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "build_dep",
                "gander-host",
                "gander-render",
                "leaf",
                "macro_crate",
                "macro_helper",
                "shared"
            ]
        );
        let kind = |n: &str| list.iter().find(|p| p.name == n).unwrap().kind;
        assert_eq!(kind("shared"), auditable::Kind::Runtime);
        assert_eq!(kind("leaf"), auditable::Kind::Runtime);
        assert_eq!(kind("gander-host"), auditable::Kind::Runtime);
        // Not linked: a build-script dependency, a proc macro and what the macro uses.
        assert_eq!(kind("build_dep"), auditable::Kind::Build);
        assert_eq!(kind("macro_crate"), auditable::Kind::Build);
        assert_eq!(kind("macro_helper"), auditable::Kind::Build);
        assert!(list.iter().all(|p| p.name != "dev_only"));
        assert_eq!(list.iter().filter(|p| p.root).count(), 2);
        // Indices point into the sorted list and are themselves sorted.
        let host = &list[1];
        assert_eq!(host.dependencies, [0, 4, 6]);
        let only_host = auditable::collect(&metadata(), &["gander-render"]).unwrap();
        assert_eq!(
            only_host
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["gander-render", "leaf", "shared"]
        );
        assert!(auditable::collect(&metadata(), &["missing"]).is_err());
    }

    #[test]
    fn the_embedded_list_round_trips_and_carries_the_cargo_auditable_fields() {
        let list = auditable::collect(&metadata(), &["gander-host"]).unwrap();
        let json = auditable::to_json(&list);
        let value: Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(value["format"], 0);
        let packages = value["packages"].as_array().unwrap();
        let host = packages
            .iter()
            .find(|p| p["name"] == "gander-host")
            .unwrap();
        assert_eq!(host["root"], true);
        assert_eq!(host["source"], "local");
        assert!(host["dependencies"].is_array());
        let build = packages.iter().find(|p| p["name"] == "build_dep").unwrap();
        assert_eq!(build["kind"], "build");
        assert_eq!(build["source"], "crates.io");
        let shared = packages.iter().find(|p| p["name"] == "shared").unwrap();
        assert!(shared.get("kind").is_none() && shared.get("root").is_none());
        // Deterministic and compressible.
        assert_eq!(json, auditable::to_json(&list));
        let packed = auditable::compress(&json).unwrap();
        assert_eq!(packed, auditable::compress(&json).unwrap());
        let mut back = Vec::new();
        flate2::read::ZlibDecoder::new(&packed[..])
            .read_to_end(&mut back)
            .unwrap();
        assert_eq!(back, json);
    }

    /// A minimal PE image with the sections named in `sections` (name, raw bytes), laid out the
    /// way a linker does: raw data padded, VirtualSize the exact size.
    fn pe_image(sections: &[(&str, &[u8])]) -> Vec<u8> {
        let mut image = vec![0u8; 0x80];
        image[0..2].copy_from_slice(b"MZ");
        image[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        image.extend_from_slice(b"PE\0\0");
        let mut coff = Vec::new();
        coff.extend_from_slice(&0x8664u16.to_le_bytes());
        coff.extend_from_slice(&(sections.len() as u16).to_le_bytes());
        coff.extend_from_slice(&[0u8; 12]);
        coff.extend_from_slice(&0xF0u16.to_le_bytes()); // SizeOfOptionalHeader
        coff.extend_from_slice(&0x22u16.to_le_bytes());
        image.extend_from_slice(&coff);
        image.extend_from_slice(&[0u8; 0xF0]);
        let mut data_at = image.len() + 40 * sections.len();
        data_at = data_at.div_ceil(512) * 512;
        let mut table = Vec::new();
        let mut data = Vec::new();
        for (name, bytes) in sections {
            let padded = bytes.len().div_ceil(512) * 512;
            let mut n = [0u8; 8];
            n[..name.len()].copy_from_slice(name.as_bytes());
            table.extend_from_slice(&n);
            table.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            table.extend_from_slice(&0x1000u32.to_le_bytes());
            table.extend_from_slice(&(padded as u32).to_le_bytes());
            table.extend_from_slice(&((data_at + data.len()) as u32).to_le_bytes());
            table.extend_from_slice(&[0u8; 16]);
            data.extend_from_slice(bytes);
            data.resize(data.len().div_ceil(512) * 512, 0xAA);
        }
        image.extend_from_slice(&table);
        image.resize(data_at, 0);
        image.extend_from_slice(&data);
        image
    }

    #[test]
    fn a_pe_section_is_found_and_trimmed_to_its_virtual_size() {
        let image = pe_image(&[(".text", b"code"), (".dep-v0", b"payload!")]);
        assert_eq!(pe_section(&image, ".dep-v0"), Some(&b"payload!"[..]));
        assert_eq!(pe_section(&image, ".text"), Some(&b"code"[..]));
        assert_eq!(pe_section(&image, ".rsrc"), None);
        assert_eq!(pe_section(b"not a program", ".dep-v0"), None);
        assert_eq!(pe_section(&image[..0x90], ".dep-v0"), None);
    }

    #[test]
    fn a_binary_with_the_list_passes_and_a_mismatch_is_named() {
        let bom = bom();
        let list = auditable::collect(&metadata(), &["gander-host"]).unwrap();
        let payload = auditable::compress(&auditable::to_json(&list)).unwrap();
        let image = pe_image(&[(".text", b"code"), (".dep-v0", &payload)]);
        let summary = check_binary(&image, "gander-host", &bom).unwrap();
        assert!(summary.contains("root gander-host"), "{summary}");
        assert!(summary.contains("3 build-only"), "{summary}");

        // Wrong root, missing section, and a package the SBOM does not know.
        assert!(check_binary(&image, "gander-render", &bom).is_err());
        let bare = pe_image(&[(".text", b"code")]);
        assert!(check_binary(&bare, "gander-host", &bom).is_err());
        let mut other = metadata();
        other["packages"][2]["version"] = json!("9.9.9");
        let stray = auditable::collect(&other, &["gander-host"]).unwrap();
        let payload = auditable::compress(&auditable::to_json(&stray)).unwrap();
        let image = pe_image(&[(".dep-v0", &payload)]);
        let err = check_binary(&image, "gander-host", &bom).unwrap_err();
        assert!(err.to_string().contains("shared 9.9.9"), "{err}");
    }

    /// Reads a COFF object back by the book: the section, its data and the symbol name.
    #[test]
    fn the_coff_object_is_well_formed() {
        let payload = b"zlib bytes go here";
        let obj = auditable::coff_object(0x8664, payload);
        let u16_at = |o: usize| u16::from_le_bytes([obj[o], obj[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes([obj[o], obj[o + 1], obj[o + 2], obj[o + 3]]);
        assert_eq!(u16_at(0), 0x8664);
        assert_eq!(u16_at(2), 1, "one section");
        assert_eq!(u32_at(4), 0, "no timestamp");
        let symbols = u32_at(8) as usize;
        assert_eq!(u32_at(12), 1, "one symbol");
        assert_eq!(&obj[20..27], b".dep-v0");
        assert_eq!(obj[27], 0);
        let size = u32_at(20 + 16) as usize;
        let offset = u32_at(20 + 20) as usize;
        assert_eq!(&obj[offset..offset + size], payload);
        assert_eq!(offset + size, symbols);
        // Symbol: name via the string table, section 1, external.
        assert_eq!(&obj[symbols..symbols + 4], &[0, 0, 0, 0]);
        let name_offset = u32_at(symbols + 4) as usize;
        assert_eq!(u16_at(symbols + 12), 1);
        assert_eq!(obj[symbols + 16], 2);
        let strings = symbols + 18;
        let table_size = u32_at(strings) as usize;
        assert_eq!(strings + table_size, obj.len());
        assert_eq!(
            &obj[strings + name_offset..obj.len() - 1],
            b"AUDITABLE_VERSION_INFO"
        );
        assert_eq!(obj[obj.len() - 1], 0);
        // Same input, same bytes.
        assert_eq!(obj, auditable::coff_object(0x8664, payload));
    }

    #[test]
    fn the_document_is_valid_cyclonedx_and_describes_the_closure() {
        let bom = bom();
        assert_eq!(validate(&bom), Vec::<String>::new());
        assert_eq!(bom["bomFormat"], "CycloneDX");
        assert_eq!(bom["specVersion"], "1.5");
        let root = &bom["metadata"]["component"];
        assert_eq!(root["name"], "gander-host");
        assert_eq!(root["purl"], "pkg:cargo/gander-host@1.0.0");
        assert_eq!(root["licenses"][0]["license"]["id"], "Apache-2.0");

        let components = bom["components"].as_array().unwrap();
        let names: Vec<&str> = components
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "build_dep",
                "gander-render",
                "leaf",
                "macro_crate",
                "macro_helper",
                "shared"
            ]
        );
        let find = |n: &str| components.iter().find(|c| c["name"] == n).unwrap();
        // Licences: one id, an expression, and the legacy slash.
        assert_eq!(
            find("build_dep")["licenses"][0]["license"]["id"],
            "BSD-3-Clause"
        );
        assert_eq!(
            find("leaf")["licenses"][0]["expression"],
            "Apache-2.0 WITH LLVM-exception OR MIT"
        );
        assert_eq!(
            find("shared")["licenses"][0]["expression"],
            "MIT OR Apache-2.0"
        );
        // Hashes come from Cargo.lock; first-party crates have none.
        assert_eq!(find("shared")["hashes"][0]["alg"], "SHA-256");
        assert_eq!(
            find("shared")["hashes"][0]["content"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert!(find("gander-render").get("hashes").is_none());
        assert_eq!(find("gander-render")["type"], "application");
        // Linked or only built.
        assert_eq!(find("shared")["scope"], "required");
        assert_eq!(find("macro_crate")["scope"], "excluded");
        assert_eq!(find("build_dep")["scope"], "excluded");
        assert_eq!(
            find("shared")["externalReferences"][0],
            json!({"type": "vcs", "url": "https://github.com/example/shared"})
        );
        // The graph: host depends on its crates and on the render worker.
        let deps = bom["dependencies"].as_array().unwrap();
        let host = deps
            .iter()
            .find(|d| d["ref"] == "pkg:cargo/gander-host@1.0.0")
            .unwrap();
        let depends: Vec<&str> = host["dependsOn"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(depends.contains(&"pkg:cargo/gander-render@1.0.0"));
        assert!(depends.contains(&"pkg:cargo/shared@1.0.0"));
        assert!(!depends.iter().any(|d| d.contains("dev_only")));
    }

    #[test]
    fn the_document_is_deterministic() {
        assert_eq!(bom(), bom());
        let later = build_bom(
            &metadata(),
            &checksums(),
            "2026-11-03T10:00:00Z",
            Vec::new(),
            spdx_ids(),
        )
        .unwrap();
        assert_ne!(bom()["serialNumber"], later["serialNumber"]);
        let serial = bom()["serialNumber"].as_str().unwrap().to_owned();
        assert!(
            serial.starts_with("urn:uuid:") && serial.len() == 45,
            "{serial}"
        );
    }

    #[test]
    fn breaking_the_document_is_caught() {
        let problems = |mutate: &dyn Fn(&mut Value)| {
            let mut b = bom();
            mutate(&mut b);
            validate(&b)
        };
        let has = |p: &[String], needle: &str| p.iter().any(|x| x.contains(needle));
        assert!(has(
            &problems(&|b| {
                b.as_object_mut().unwrap().remove("bomFormat");
            }),
            "bomFormat"
        ));
        assert!(has(
            &problems(&|b| b["bomFormat"] = json!("SPDX")),
            "/bomFormat"
        ));
        assert!(has(
            &problems(&|b| b["specVersion"] = json!("1.6")),
            "specVersion is"
        ));
        assert!(has(
            &problems(&|b| b["serialNumber"] = json!("urn:uuid:not-a-uuid")),
            "/serialNumber"
        ));
        assert!(has(
            &problems(&|b| b["components"][0]["hashes"][0]["content"] = json!("xyz")),
            "/components/0/hashes/0/content"
        ));
        assert!(has(
            &problems(&|b| b["components"][0]["hashes"][0]["alg"] = json!("CRC32")),
            "/components/0/hashes/0/alg"
        ));
        assert!(has(
            &problems(&|b| b["components"][0]["type"] = json!("crate")),
            "/components/0/type"
        ));
        assert!(has(
            &problems(&|b| b["components"][0]["surprise"] = json!(1)),
            "surprise"
        ));
        assert!(has(
            &problems(&|b| b["metadata"]["timestamp"] = json!("yesterday")),
            "date-time"
        ));
        assert!(has(
            &problems(&|b| b["components"][0]["licenses"][0] =
                json!({"license": {"id": "Not-An-SPDX-Id"}})),
            "licenses"
        ));
        // Beyond the schema: dangling references and a missing hash.
        assert!(has(
            &problems(&|b| b["dependencies"][0]["dependsOn"] = json!(["pkg:cargo/ghost@1.0.0"])),
            "unknown component"
        ));
        assert!(has(
            &problems(&|b| {
                b["components"][1] = b["components"][0].clone();
            }),
            "share a bom-ref"
        ));
        assert!(has(
            &problems(&|b| {
                b["components"][0].as_object_mut().unwrap().remove("hashes");
            }),
            "has no hash"
        ));
        assert!(has(
            &problems(&|b| {
                b["components"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("licenses");
            }),
            "names no licence"
        ));
    }

    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_709_210_096), "2024-02-29T12:34:56Z");
        assert_eq!(rfc3339(4_102_444_799), "2099-12-31T23:59:59Z");
        assert_eq!(rfc3339(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn checksums_are_read_from_the_lock_file() {
        let lock = r#"
version = 4

[[package]]
name = "serde"
version = "1.0.1"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789"

[[package]]
name = "gander-core"
version = "0.0.0"
"#;
        let sums = lock_checksums(lock).unwrap();
        assert_eq!(sums.len(), 1);
        assert_eq!(
            sums[&("serde".to_owned(), "1.0.1".to_owned())],
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        );
        assert!(lock_checksums("= not toml").is_err());
    }

    /// The real workspace: the document generates, validates, covers both binaries and every
    /// registry crate carries a hash and a licence.
    #[test]
    fn the_real_workspace_sbom_is_valid() {
        let root = workspace_root();
        let metadata = crate::notices::cargo_metadata(&root).unwrap();
        let checksums =
            lock_checksums(&std::fs::read_to_string(root.join("Cargo.lock")).unwrap()).unwrap();
        let bom = build_bom(
            &metadata,
            &checksums,
            "2026-11-02T00:00:00Z",
            Vec::new(),
            spdx_ids(),
        )
        .unwrap();
        assert_eq!(validate(&bom), Vec::<String>::new());
        let components = bom["components"].as_array().unwrap();
        assert!(components.len() > 200, "{} components", components.len());
        for name in [
            "gander-render",
            "gander-core",
            "serde",
            "tokio",
            "ring",
            "eframe",
            "windows",
        ] {
            assert!(
                components.iter().any(|c| c["name"] == name),
                "{name} missing"
            );
        }
        // Everything the notices ship is in the SBOM as a required component.
        let shipped = crate::notices::shipped_crates(&metadata).unwrap();
        for krate in shipped {
            assert!(
                components
                    .iter()
                    .any(|c| c["name"] == krate.name.as_str()
                        && c["version"] == krate.version.as_str()),
                "{} {} is in the notices but not the SBOM",
                krate.name,
                krate.version
            );
        }
    }
}
