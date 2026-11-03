// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask installer-check` and `cargo xtask installer-build` (WP-1A).
//!
//! **installer-check** is the automated gate for the installer package. It never touches the
//! firewall, Task Scheduler or the registry, and installs nothing:
//!
//! 1. `installer/main.wxs` (WiX 5, the v4 schema) is well-formed XML and keeps the expected ids
//!    (features, components, files, custom actions, the icon), is per-user (no HKLM, no machine
//!    folders, no elevated or deferred custom action), every custom action is best effort, every
//!    component with a file has an explicit GUID, every `!(bindpath.x)` is one the build passes
//!    and every source file that is committed exists.
//! 2. `installer/autostart-task.xml` is a least-privilege logon task with all placeholders.
//! 3. Every `installer/*.ps1` is ASCII only, uses no PowerShell 7-only cmdlet, and parses with
//!    the Windows PowerShell 5.1 parser (`powershell.exe`) and with `pwsh` when present; and
//!    `verify-rules.ps1 -SelfTest` (pure functions: CIDR and port validation, config reading, the
//!    rule plan, refusing Any) passes under both. Neither reads or writes the firewall.
//! 4. `cargo xtask notices` runs, and its output carries the font and Boost licence texts.
//! 5. The host embeds its icon (`build.rs`, `assets/gander.ico`) and no cargo-wix metadata is left;
//!    both executables' build scripts embed their dependency list (`.dep-v0`, WP-82).
//! 6. `cargo xtask sbom` writes a CycloneDX 1.5 SBOM that validates against the schema; the MSI
//!    ships it as `sbom/gander-host.cdx.json`.
//! 7. With the WiX 5 tool present: a dry `wix build` of the real .wxs against stand-in binaries
//!    (a few seconds) followed by `wix msi validate` (the ICE rules); without it, `[DEFER]`.
//! 8. With `--binary` (builds the release host, a few minutes): the release `gander-host.exe` is a
//!    GUI-subsystem program, carries a `.dep-v0` list that matches the SBOM, its `--ui-smoke`
//!    output still reaches a pipe, and started without any console or redirection it writes the
//!    same output to `host.log` under `--data-dir`.
//!
//! **installer-build** needs the .NET SDK, the `wix` tool (5.0.x) and its UI extension (see
//! [`INSTALL_HINT`]). It checks the committed icons are current, regenerates the notices and the
//! SBOM, builds the release binaries (`--locked`), checks that both carry a `.dep-v0` list that
//! matches the SBOM, runs `wix build` with the bind paths the .wxs expects, validates the MSI
//! (`wix msi validate`) and lists what is inside it with an administrative extract (`msiexec /a`,
//! which copies files to a temporary folder and installs nothing). The MSI is
//! `target/installer/Gander-<version>-x64.msi`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

use crate::proc::{capture, display_path, find_on_path, workspace_root};

/// The WiX v4 schema namespace (WiX 5 reads it too).
const WIX_NS: &str = "http://wixtoolset.org/schemas/v4/wxs";

/// Ids the .wxs must keep (the installer-check contract).
const FEATURES: [&str; 2] = ["Feature_Host", "Feature_Network"];
const COMPONENTS: [&str; 7] = [
    "Cmp_HostExe",
    "Cmp_RenderExe",
    "Cmp_Docs",
    "Cmp_Sbom",
    "Cmp_NetworkScripts",
    "Cmp_StartMenu",
    "Cmp_NetworkShortcut",
];
const FILES: [&str; 8] = [
    "Fil_HostExe",
    "Fil_RenderExe",
    "Fil_License",
    "Fil_ThirdPartyNotices",
    "Fil_Sbom",
    "Fil_FirewallCommon",
    "Fil_FirewallRules",
    "Fil_VerifyRules",
];
const CUSTOM_ACTIONS: [&str; 4] = [
    "CA_AutostartOn",
    "CA_AutostartOff",
    "CA_NetworkSetup",
    "CA_NetworkRemove",
];

/// The package upgrade code: changing it breaks upgrades from every earlier release.
const UPGRADE_CODE: &str = "D86137D4-C26A-43BB-AA8E-C77C7B536282";

/// The WiX 5 tool and its extensions; the major version is what the .wxs is written for.
const WIX_MAJOR: u32 = 5;
const WIX_VERSION: &str = "5.0.2";
const WIX_EXTENSIONS: [&str; 1] = ["WixToolset.UI.wixext"];

/// The `!(bindpath.x)` names the .wxs may use, with the folder each maps to (see [`bind_paths`]).
const BINDPATHS: [&str; 6] = ["bin", "repo", "installer", "notices", "sbom", "assets"];

/// Directories a per-user installer must not install into.
const MACHINE_DIRECTORIES: [&str; 6] = [
    "ProgramFilesFolder",
    "ProgramFiles64Folder",
    "ProgramFiles6432Folder",
    "CommonFilesFolder",
    "CommonFiles64Folder",
    "CommonAppDataFolder",
];

/// Cmdlets and parameters that exist only in PowerShell 7 (or later); the 5.1 parser cannot see
/// these, so they are looked for as text.
const POWERSHELL_7_ONLY: [&str; 9] = [
    "-AsHashtable",
    "-AsByteStream",
    "Join-String",
    "-Parallel",
    "$IsWindows",
    "$IsLinux",
    "Get-Error",
    "-SkipHttpErrorCheck",
    "Test-Json",
];

/// Outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Fail,
    Warn,
    Defer,
}

/// One line of the report.
#[derive(Debug, Clone)]
pub struct Check {
    pub status: Status,
    pub name: String,
    pub detail: String,
}

impl Check {
    fn new(status: Status, name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            status,
            name: name.into(),
            detail: detail.into(),
        }
    }

    fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(Status::Ok, name, detail)
    }

    fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(Status::Fail, name, detail)
    }

    fn from_problems(name: &str, ok_detail: &str, problems: Vec<String>) -> Self {
        if problems.is_empty() {
            Self::ok(name, ok_detail)
        } else {
            Self::fail(name, problems.join("; "))
        }
    }

    fn print(&self) {
        let tag = match self.status {
            Status::Ok => "[OK   ]",
            Status::Fail => "[FAIL ]",
            Status::Warn => "[WARN ]",
            Status::Defer => "[DEFER]",
        };
        if self.detail.is_empty() {
            println!("{tag} {}", self.name);
        } else {
            println!("{tag} {}: {}", self.name, self.detail);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The .wxs
// ---------------------------------------------------------------------------------------------

fn ids_of<'a>(doc: &'a roxmltree::Document<'_>, tag: &str) -> Vec<&'a str> {
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == tag)
        .filter_map(|n| n.attribute("Id"))
        .collect()
}

/// The `!(bindpath.x)` names a .wxs uses.
fn bindpaths_used(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(at) = rest.find("!(bindpath.") {
        rest = &rest[at + "!(bindpath.".len()..];
        if let Some(end) = rest.find(')') {
            out.insert(rest[..end].to_owned());
        }
    }
    out
}

/// `(bindpath name, file name)` of every `Source="!(bindpath.x)\file"` in the text.
fn bound_sources(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("\"!(bindpath.") {
        rest = &rest[at + "\"!(bindpath.".len()..];
        let Some(close) = rest.find(')') else { break };
        let name = rest[..close].to_owned();
        let after = &rest[close + 1..];
        if let Some(end) = after.find('"') {
            let file = after[..end].trim_start_matches(['\\', '/']).to_owned();
            out.push((name, file));
        }
    }
    out
}

/// Checks the text of `main.wxs`. Pure: no file access.
pub fn check_wxs_text(text: &str) -> Vec<Check> {
    let doc = match roxmltree::Document::parse(text) {
        Ok(d) => d,
        Err(e) => return vec![Check::fail("main.wxs is well-formed XML", e.to_string())],
    };
    let mut out = vec![Check::ok("main.wxs is well-formed XML", "")];
    let root = doc.root_element();
    if root.tag_name().name() != "Wix" || root.tag_name().namespace() != Some(WIX_NS) {
        out.push(Check::fail(
            "main.wxs root is <Wix> in the WiX v4 (WiX 5) namespace",
            format!("found <{}>", root.tag_name().name()),
        ));
        return out;
    }

    // Expected ids.
    let mut missing = Vec::new();
    for (tag, wanted) in [
        ("Feature", FEATURES.as_slice()),
        ("Component", COMPONENTS.as_slice()),
        ("File", FILES.as_slice()),
        ("CustomAction", CUSTOM_ACTIONS.as_slice()),
        ("Icon", ["Gander.ico"].as_slice()),
    ] {
        let have = ids_of(&doc, tag);
        for id in wanted {
            if !have.contains(id) {
                missing.push(format!("{tag} {id}"));
            }
        }
    }
    out.push(Check::from_problems(
        "main.wxs has the expected feature, component, file, custom action and icon ids",
        &format!(
            "{} features, {} components, {} files, {} custom actions",
            FEATURES.len(),
            COMPONENTS.len(),
            FILES.len(),
            CUSTOM_ACTIONS.len()
        ),
        missing.iter().map(|m| format!("missing {m}")).collect(),
    ));

    // Unique ids within each table, and references that resolve.
    let mut problems = Vec::new();
    for tag in [
        "Component",
        "File",
        "Feature",
        "CustomAction",
        "Directory",
        "Shortcut",
        "RemoveFolder",
        "Property",
        "Icon",
    ] {
        let mut seen = BTreeSet::new();
        for id in ids_of(&doc, tag) {
            if !seen.insert(id) {
                problems.push(format!("duplicate {tag} id {id}"));
            }
        }
    }
    let components: BTreeSet<&str> = ids_of(&doc, "Component").into_iter().collect();
    let custom_actions: BTreeSet<&str> = ids_of(&doc, "CustomAction").into_iter().collect();
    let files: BTreeSet<&str> = ids_of(&doc, "File").into_iter().collect();
    let icons: BTreeSet<&str> = ids_of(&doc, "Icon").into_iter().collect();
    let mut referenced = BTreeSet::new();
    for n in doc.descendants().filter(|n| n.is_element()) {
        match n.tag_name().name() {
            "ComponentRef" => {
                let id = n.attribute("Id").unwrap_or_default();
                if !components.contains(id) {
                    problems.push(format!("ComponentRef {id} has no Component"));
                }
                referenced.insert(id);
            }
            "Custom" => {
                let id = n.attribute("Action").unwrap_or_default();
                if !custom_actions.contains(id) {
                    problems.push(format!("Custom {id} has no CustomAction"));
                }
                if n.text().is_some_and(|t| !t.trim().is_empty()) {
                    problems.push(format!(
                        "Custom {id} has inner text; WiX 5 wants a Condition attribute"
                    ));
                }
            }
            "CustomAction" => {
                if let Some(key) = n.attribute("FileRef")
                    && !files.contains(key)
                {
                    problems.push(format!("CustomAction FileRef {key} has no File"));
                }
                if n.attribute("FileKey").is_some() {
                    problems
                        .push("CustomAction uses FileKey (WiX 3); WiX 5 says FileRef".to_owned());
                }
            }
            "Shortcut" => {
                if let Some(icon) = n.attribute("Icon")
                    && !icons.contains(icon)
                {
                    problems.push(format!("Shortcut Icon {icon} has no Icon"));
                }
            }
            "Property" if n.attribute("Id") == Some("ARPPRODUCTICON") => {
                let value = n.attribute("Value").unwrap_or_default();
                if !icons.contains(value) {
                    problems.push(format!("ARPPRODUCTICON {value} has no Icon"));
                }
            }
            _ => {}
        }
    }
    for c in &components {
        if !referenced.contains(c) {
            problems.push(format!("Component {c} is in no Feature"));
        }
    }
    if !doc
        .descendants()
        .any(|n| n.is_element() && n.attribute("Id") == Some("ARPPRODUCTICON"))
    {
        problems.push("no ARPPRODUCTICON property (the icon in Settings, Apps)".to_owned());
    }
    out.push(Check::from_problems(
        "main.wxs ids are unique and every reference resolves",
        "",
        problems,
    ));

    // Per-user and no silent elevation.
    let mut problems = Vec::new();
    let package = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "Package");
    match package {
        Some(p) if p.attribute("Scope") == Some("perUser") => {}
        _ => problems.push("Package Scope is not perUser".to_owned()),
    }
    for n in doc.descendants().filter(|n| n.is_element()) {
        let tag = n.tag_name().name();
        // A RegistrySearch only reads (the OS build number for the launch condition).
        if n.attribute("Root") == Some("HKLM") && tag != "RegistrySearch" {
            problems.push(format!("<{tag}> writes under HKLM"));
        }
        if (tag == "Directory" || tag == "StandardDirectory")
            && n.attribute("Id")
                .is_some_and(|id| MACHINE_DIRECTORIES.contains(&id))
        {
            problems.push(format!(
                "{tag} {} is a machine-wide folder",
                n.attribute("Id").unwrap_or_default()
            ));
        }
        if tag == "Property" && n.attribute("Id") == Some("ALLUSERS") {
            problems.push("Property ALLUSERS is set".to_owned());
        }
        if tag == "CustomAction" {
            let id = n.attribute("Id").unwrap_or_default();
            if n.attribute("Impersonate") == Some("no") {
                problems.push(format!("CustomAction {id} runs without impersonation"));
            }
            if n.attribute("Execute").is_some_and(|e| e != "immediate") {
                problems.push(format!("CustomAction {id} is not immediate"));
            }
            if n.attribute("Return") != Some("ignore") {
                problems.push(format!(
                    "CustomAction {id} can fail the install (Return is not ignore)"
                ));
            }
        }
        if tag == "Component" {
            let id = n.attribute("Id").unwrap_or_default();
            let registry_key = n.children().any(|c| {
                c.is_element()
                    && c.tag_name().name() == "RegistryValue"
                    && c.attribute("KeyPath") == Some("yes")
                    && c.attribute("Root") == Some("HKCU")
            });
            if !registry_key {
                problems.push(format!("Component {id} has no HKCU registry key path"));
            }
            // WiX refuses an automatic GUID for a component that has a file and a registry key
            // path (WIX0230); the GUID must also stay the same in every release.
            let has_file = n
                .children()
                .any(|c| c.is_element() && c.tag_name().name() == "File");
            if has_file && n.attribute("Guid").is_none_or(|g| g == "*") {
                problems.push(format!("Component {id} has files but no fixed Guid"));
            }
        }
    }
    out.push(Check::from_problems(
        "main.wxs is per-user: no HKLM, no machine folders, no elevated custom action",
        "",
        problems,
    ));

    // The Start menu entry opens the settings window.
    let opens_settings = doc.descendants().any(|n| {
        n.is_element()
            && n.attribute("Id") == Some("Shc_Gander")
            && n.attribute("Arguments")
                .is_some_and(|a| a.contains("--open"))
    });
    out.push(Check::from_problems(
        "main.wxs Start menu entry Gander starts gander-host.exe --open",
        "",
        if opens_settings {
            Vec::new()
        } else {
            vec!["Shortcut Shc_Gander lacks Arguments=\"--open\"".to_owned()]
        },
    ));

    // Bind paths: only names the build passes.
    let mut problems = Vec::new();
    for name in bindpaths_used(text) {
        if !BINDPATHS.contains(&name.as_str()) {
            problems.push(format!(
                "!(bindpath.{name}) is not one `wix build` is given"
            ));
        }
    }
    out.push(Check::from_problems(
        "main.wxs uses only the bind paths installer-build passes",
        &BINDPATHS.join(", "),
        problems,
    ));
    out
}

/// The `UpgradeCode` of the product.
fn wxs_upgrade_code(text: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(text).ok()?;
    doc.descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "Package")
        .and_then(|p| p.attribute("UpgradeCode"))
        .map(str::to_owned)
}

/// Every committed file the .wxs installs must exist at the folder its bind path names. (Built
/// files, under `bin` and `notices`, are not looked for.)
fn check_wxs_sources(root: &Path, text: &str) -> Check {
    let folders = bind_paths(root, &root.join("target").join("release"));
    let mut problems = Vec::new();
    let mut looked = 0;
    for (name, file) in bound_sources(text) {
        // Built files: the executables, the notices and the SBOM.
        if name == "bin" || name == "notices" || name == "sbom" {
            continue;
        }
        let Some((_, dir)) = folders.iter().find(|(n, _)| *n == name) else {
            continue;
        };
        looked += 1;
        if !dir.join(&file).is_file() {
            problems.push(format!("{name}: {file} not found in {}", display_path(dir)));
        }
    }
    Check::from_problems(
        "every committed file main.wxs installs exists",
        &format!("{looked} file(s)"),
        problems,
    )
}

// ---------------------------------------------------------------------------------------------
// The autostart task
// ---------------------------------------------------------------------------------------------

/// Checks the text of `autostart-task.xml`. Pure.
pub fn check_task_text(text: &str) -> Vec<Check> {
    let doc = match roxmltree::Document::parse(text) {
        Ok(d) => d,
        Err(e) => {
            return vec![Check::fail(
                "autostart-task.xml is well-formed XML",
                e.to_string(),
            )];
        }
    };
    let mut out = vec![Check::ok("autostart-task.xml is well-formed XML", "")];
    let texts = |tag: &str| -> Vec<String> {
        doc.descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == tag)
            .map(|n| n.text().unwrap_or_default().trim().to_owned())
            .collect()
    };
    let mut problems = Vec::new();
    if !doc
        .descendants()
        .any(|n| n.is_element() && n.tag_name().name() == "LogonTrigger")
    {
        problems.push("no LogonTrigger".to_owned());
    }
    if texts("RunLevel") != ["LeastPrivilege"] {
        problems.push(format!(
            "RunLevel must be LeastPrivilege, found {:?}",
            texts("RunLevel")
        ));
    }
    if doc
        .descendants()
        .any(|n| n.is_element() && n.text().is_some_and(|t| t.contains("HighestAvailable")))
    {
        problems.push("mentions HighestAvailable".to_owned());
    }
    if texts("LogonType") != ["InteractiveToken"] {
        problems.push("LogonType must be InteractiveToken".to_owned());
    }
    if texts("ExecutionTimeLimit") != ["PT0S"] {
        problems.push(
            "ExecutionTimeLimit must be PT0S (the default would kill the host after 72 hours)"
                .to_owned(),
        );
    }
    for placeholder in ["@USER_SID@", "@EXE_PATH@", "@WORKING_DIR@"] {
        if !text.contains(placeholder) {
            problems.push(format!("placeholder {placeholder} is missing"));
        }
    }
    out.push(Check::from_problems(
        "autostart-task.xml is a least-privilege logon task with its placeholders",
        "",
        problems,
    ));
    out
}

// ---------------------------------------------------------------------------------------------
// PowerShell scripts
// ---------------------------------------------------------------------------------------------

/// Problems with the text of a script that the 5.1 parser cannot see: non-ASCII characters (an
/// em-dash breaks a BOM-less file read as ANSI) and PowerShell 7-only names.
pub fn script_text_problems(name: &str, text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if let Some((col, c)) = line.char_indices().find(|(_, c)| !c.is_ascii()) {
            problems.push(format!(
                "{name}:{}:{}: non-ASCII character U+{:04X}",
                i + 1,
                col + 1,
                c as u32
            ));
            break;
        }
    }
    if text.starts_with('\u{feff}') {
        problems.push(format!("{name}: starts with a byte-order mark"));
    }
    let lower = text.to_ascii_lowercase();
    for token in POWERSHELL_7_ONLY {
        if lower.contains(&token.to_ascii_lowercase()) {
            problems.push(format!("{name}: uses {token}, which is PowerShell 7 only"));
        }
    }
    problems
}

fn installer_scripts(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut scripts: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ps1")))
        .collect();
    scripts.sort();
    Ok(scripts)
}

/// A PowerShell to run scripts with: `powershell.exe` (Windows PowerShell 5.1) and `pwsh`.
fn find_shell(windows_powershell: bool) -> Option<PathBuf> {
    if windows_powershell {
        // 5.1 is part of Windows; look in System32 first so a stray PATH entry cannot stand in.
        let system_root = std::env::var_os("SystemRoot")?;
        let p = PathBuf::from(system_root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        p.is_file().then_some(p)
    } else {
        find_on_path("pwsh")
    }
}

fn run_shell(shell: &Path, script: &Path, args: &[&str]) -> Result<crate::proc::Captured> {
    let mut all: Vec<String> = vec![
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-File".into(),
        script.display().to_string(),
    ];
    all.extend(args.iter().map(|a| (*a).to_owned()));
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    capture(shell, &refs)
}

fn check_scripts(installer: &Path) -> Result<Vec<Check>> {
    let mut out = Vec::new();
    let scripts = installer_scripts(installer)?;
    if scripts.is_empty() {
        out.push(Check::fail(
            "installer scripts exist",
            "no *.ps1 under installer/",
        ));
        return Ok(out);
    }
    let mut problems = Vec::new();
    for script in &scripts {
        let name = script
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        match std::fs::read(script) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => problems.extend(script_text_problems(&name, &text)),
                Err(_) => problems.push(format!("{name}: not valid UTF-8")),
            },
            Err(e) => problems.push(format!("{name}: {e}")),
        }
    }
    out.push(Check::from_problems(
        "installer scripts are ASCII only and use no PowerShell 7-only names",
        &format!("{} script(s)", scripts.len()),
        problems,
    ));

    let parse_check = installer.join("parse-check.ps1");
    let verify = installer.join("verify-rules.ps1");
    let script_args: Vec<String> = scripts.iter().map(|s| s.display().to_string()).collect();
    for (windows_powershell, label) in [
        (true, "Windows PowerShell 5.1"),
        (false, "PowerShell 7 (pwsh)"),
    ] {
        let Some(shell) = find_shell(windows_powershell) else {
            // Windows PowerShell 5.1 ships with Windows: missing there is a failure.
            let status = if windows_powershell && cfg!(windows) {
                Status::Fail
            } else {
                Status::Warn
            };
            out.push(Check::new(
                status,
                format!("{label} parse and self-test"),
                "shell not found, skipped",
            ));
            continue;
        };
        let refs: Vec<&str> = script_args.iter().map(String::as_str).collect();
        match run_shell(&shell, &parse_check, &refs) {
            Ok(r) => {
                let version_ok = !windows_powershell || r.stdout.contains("PowerShell 5.");
                let detail = if r.success() && version_ok {
                    r.stdout.lines().next().unwrap_or_default().to_owned()
                } else if !version_ok {
                    format!("expected Windows PowerShell 5.x, got: {}", r.first_line())
                } else {
                    r.stdout
                        .lines()
                        .filter(|l| l.starts_with("FAIL"))
                        .collect::<Vec<_>>()
                        .join(" | ")
                        + &r.stderr
                };
                out.push(Check::new(
                    if r.success() && version_ok {
                        Status::Ok
                    } else {
                        Status::Fail
                    },
                    format!("{label} parses every installer script"),
                    detail,
                ));
            }
            Err(e) => out.push(Check::fail(format!("{label} parse"), format!("{e:#}"))),
        }
        match run_shell(&shell, &verify, &["-SelfTest"]) {
            Ok(r) => {
                let summary = r
                    .stdout
                    .lines()
                    .rev()
                    .find(|l| l.starts_with("selftest:"))
                    .unwrap_or("no summary line")
                    .to_owned();
                let detail = if r.success() {
                    summary
                } else {
                    let fails: Vec<&str> = r
                        .stdout
                        .lines()
                        .filter(|l| l.starts_with("FAIL") || l.starts_with("ERROR"))
                        .collect();
                    format!("{summary}; {}{}", fails.join(" | "), r.stderr.trim())
                };
                out.push(Check::new(
                    if r.success() {
                        Status::Ok
                    } else {
                        Status::Fail
                    },
                    format!(
                        "{label}: verify-rules.ps1 -SelfTest (pure checks, no firewall access)"
                    ),
                    detail,
                ));
            }
            Err(e) => out.push(Check::fail(format!("{label} self-test"), format!("{e:#}"))),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Notices and Cargo.toml
// ---------------------------------------------------------------------------------------------

fn check_notices(root: &Path) -> Vec<Check> {
    let out_dir = root.join("target").join("installer");
    let summary = match crate::notices::generate(root, &out_dir) {
        Ok(s) => s,
        Err(e) => return vec![Check::fail("cargo xtask notices", format!("{e:#}"))],
    };
    let text = std::fs::read_to_string(&summary.notices).unwrap_or_default();
    let rtf = std::fs::read_to_string(&summary.rtf).unwrap_or_default();
    let mut problems = Vec::new();
    for (what, needle) in [
        (
            "the OFL-1.1 text (egui fonts)",
            "SIL OPEN FONT LICENSE Version 1.1",
        ),
        (
            "the Ubuntu Font Licence text",
            "UBUNTU FONT LICENCE Version 1.0",
        ),
        ("the Boost Software License text", "Boost Software License"),
        ("epaint_default_fonts", "epaint_default_fonts"),
        ("clipboard-win", "clipboard-win"),
        ("error-code", "error-code"),
        ("the Apache-2.0 text", "Apache License"),
    ] {
        if !text.contains(needle) {
            problems.push(format!("THIRD-PARTY-NOTICES.txt lacks {what}"));
        }
    }
    if !rtf.starts_with("{\\rtf1") || !rtf.contains("Apache License") {
        problems.push("License.rtf is not an RTF copy of LICENSE".to_owned());
    }
    // Compliance gap: licences that require the text to ship, and no text found.
    let mut checks = vec![Check::from_problems(
        "cargo xtask notices writes the third-party notices and the licence page",
        &format!(
            "{} crate(s), {} distinct text(s), {}",
            summary.crates,
            summary.texts,
            display_path(&summary.notices)
        ),
        problems,
    )];
    if !summary.without_text.is_empty() {
        checks.push(Check::new(
            Status::Warn,
            "crates without a licence file in their source",
            summary.without_text.join(", "),
        ));
    }
    checks
}

/// The host embeds its icon and the cargo-wix metadata is gone; the upgrade code never changes.
fn check_host_packaging(root: &Path, wxs: Option<&str>) -> Vec<Check> {
    let host = root.join("host").join("gander-host");
    let mut problems = Vec::new();
    match std::fs::read_to_string(host.join("Cargo.toml")) {
        Ok(text) => {
            if text.contains("[package.metadata.wix]") {
                problems.push(
                    "Cargo.toml still has [package.metadata.wix] (cargo-wix is gone)".to_owned(),
                );
            }
            if !text.contains("embed-resource") {
                problems.push("Cargo.toml has no embed-resource build-dependency".to_owned());
            }
        }
        Err(e) => problems.push(format!("Cargo.toml: {e}")),
    }
    match std::fs::read_to_string(host.join("build.rs")) {
        Ok(text) => {
            if !text.contains("gander.ico") {
                problems.push("build.rs does not embed assets/gander.ico".to_owned());
            }
        }
        Err(e) => problems.push(format!("build.rs: {e}")),
    }
    if !host.join("assets").join("gander.ico").is_file() {
        problems.push("assets/gander.ico is missing (run `cargo xtask icons`)".to_owned());
    }
    let mut checks = vec![Check::from_problems(
        "gander-host embeds its icon (build.rs, assets/gander.ico) and has no cargo-wix metadata",
        "",
        problems,
    )];
    // WP-82: both executables embed their dependency list (cargo-auditable format) through the
    // shared build-support file.
    let mut problems = Vec::new();
    if !root
        .join("host")
        .join("build-support")
        .join("auditable.rs")
        .is_file()
    {
        problems.push("host/build-support/auditable.rs is missing".to_owned());
    }
    for binary in crate::notices::SHIPPED_BINARIES {
        let build_rs = root.join("host").join(binary).join("build.rs");
        let needle = format!("auditable::embed(\"{binary}\")");
        match std::fs::read_to_string(&build_rs) {
            Ok(text) if text.contains(&needle) => {}
            Ok(_) => problems.push(format!("{binary}/build.rs does not call {needle}")),
            Err(e) => problems.push(format!("{binary}/build.rs: {e}")),
        }
    }
    checks.push(Check::from_problems(
        "gander-host and gander-render embed their dependency list (.dep-v0, build.rs)",
        "",
        problems,
    ));
    checks.push(match wxs.and_then(wxs_upgrade_code) {
        Some(code) if code.eq_ignore_ascii_case(UPGRADE_CODE) => {
            Check::ok("main.wxs keeps the UpgradeCode", &code)
        }
        Some(code) => Check::fail(
            "main.wxs keeps the UpgradeCode",
            format!("{code} is not {UPGRADE_CODE}; every earlier install would stop upgrading"),
        ),
        None => Check::fail(
            "main.wxs keeps the UpgradeCode",
            "no Package UpgradeCode found",
        ),
    });
    checks
}

// ---------------------------------------------------------------------------------------------
// WiX tool detection and the build
// ---------------------------------------------------------------------------------------------

/// What is installed of the MSI toolchain.
#[derive(Debug, Clone, Default)]
pub struct WixTools {
    /// The folder that holds `dotnet.exe` (the .NET SDK or runtime).
    pub dotnet_root: Option<PathBuf>,
    /// The `wix` tool.
    pub wix: Option<PathBuf>,
    /// The version `wix --version` reports (for example `5.0.2+aa65968c`).
    pub version: Option<String>,
    /// Global extensions, as `name version` lines of `wix extension list -g`.
    pub extensions: Vec<String>,
}

impl WixTools {
    /// Whether the version is the WiX major the .wxs is written for.
    pub fn version_ok(&self) -> bool {
        self.version
            .as_deref()
            .and_then(|v| v.split('.').next())
            .and_then(|m| m.parse::<u32>().ok())
            == Some(WIX_MAJOR)
    }

    /// Extensions from [`WIX_EXTENSIONS`] that are not installed globally.
    pub fn missing_extensions(&self) -> Vec<&'static str> {
        WIX_EXTENSIONS
            .iter()
            .copied()
            .filter(|want| !self.extensions.iter().any(|e| e.starts_with(want)))
            .collect()
    }

    /// Whether `wix build` can run: the tool, the right major version and its extensions.
    pub fn complete(&self) -> bool {
        self.wix.is_some() && self.version_ok() && self.missing_extensions().is_empty()
    }

    /// A `wix` command with the environment it needs: `DOTNET_ROOT` (the tool is a .NET apphost
    /// that otherwise looks only in `Program Files`), no telemetry and no first-run banner.
    pub fn command(&self) -> Option<Command> {
        let wix = self.wix.as_ref()?;
        let mut command = Command::new(wix);
        if let Some(root) = &self.dotnet_root {
            command.env("DOTNET_ROOT", root);
        }
        command
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_NOLOGO", "1");
        Some(command)
    }
}

/// Candidate folders of the `wix` tool: PATH, `DOTNET_CLI_HOME`, the profile, and the real user
/// profile (a launcher that overrides `USERPROFILE` hides it from the first two).
fn wix_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = find_on_path("wix") {
        out.push(p);
    }
    let tools = |home: PathBuf| home.join(".dotnet").join("tools").join("wix.exe");
    for var in ["DOTNET_CLI_HOME", "USERPROFILE", "HOME"] {
        if let Some(home) = std::env::var_os(var) {
            out.push(tools(PathBuf::from(home)));
        }
    }
    if let (Some(drive), Some(user)) = (
        std::env::var_os("SystemDrive"),
        std::env::var_os("USERNAME"),
    ) {
        out.push(tools(
            PathBuf::from(drive)
                .join(std::path::MAIN_SEPARATOR_STR)
                .join("Users")
                .join(user),
        ));
    }
    out
}

/// Candidate folders of `dotnet.exe`: `DOTNET_ROOT`, PATH, the per-user install the
/// `dotnet-install` script makes, and Program Files.
fn dotnet_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(root) = std::env::var_os("DOTNET_ROOT") {
        out.push(PathBuf::from(root));
    }
    if let Some(exe) = find_on_path("dotnet")
        && let Some(dir) = exe.parent()
    {
        out.push(dir.to_path_buf());
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        out.push(PathBuf::from(local).join("Microsoft").join("dotnet"));
    }
    if let Some(files) = std::env::var_os("ProgramFiles") {
        out.push(PathBuf::from(files).join("dotnet"));
    }
    out
}

/// Looks for the .NET SDK, the `wix` tool and its global extensions. Read-only.
pub fn detect_wix() -> WixTools {
    let mut tools = WixTools {
        dotnet_root: dotnet_candidates()
            .into_iter()
            .find(|d| d.join("dotnet.exe").is_file() || d.join("dotnet").is_file()),
        wix: wix_candidates().into_iter().find(|p| p.is_file()),
        ..WixTools::default()
    };
    if let Some(mut command) = tools.command() {
        if let Ok(out) = command.arg("--version").output()
            && out.status.success()
        {
            tools.version = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(str::to_owned);
        }
        if let Some(mut command) = tools.command()
            && let Ok(out) = command.args(["extension", "list", "-g"]).output()
            && out.status.success()
        {
            tools.extensions = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|l| l.contains("wixext"))
                .map(str::to_owned)
                .collect();
        }
    }
    tools
}

/// What the owner runs to get the MSI toolchain (also in installer/README.md).
pub const INSTALL_HINT: &str = "the .NET SDK 8 (winget install --id Microsoft.DotNet.SDK.8 -e, needs one UAC click; or the per-user script: https://dot.net/v1/dotnet-install.ps1 -Channel 8.0)\n\
     dotnet tool install --global wix --version 5.0.2\n\
     wix extension add -g WixToolset.UI.wixext/5.0.2\n\
     then: cargo xtask installer-build";

/// Why the toolchain is not usable, for a message.
fn wix_missing_text(tools: &WixTools) -> String {
    let mut parts = Vec::new();
    match &tools.wix {
        None => parts.push("the wix tool was not found".to_owned()),
        Some(p) => {
            parts.push(format!("wix: {}", display_path(p)));
            match &tools.version {
                None => parts.push("it does not run (is the .NET SDK installed?)".to_owned()),
                Some(v) if !tools.version_ok() => {
                    parts.push(format!(
                        "version {v}, but this installer is written for WiX {WIX_MAJOR}"
                    ));
                }
                Some(_) => {}
            }
        }
    }
    for ext in tools.missing_extensions() {
        parts.push(format!(
            "extension {ext} is not installed (wix extension add -g {ext}/{WIX_VERSION})"
        ));
    }
    parts.join("; ")
}

/// The repository version, from `[workspace.package]` in the root `Cargo.toml`.
pub fn workspace_version(root: &Path) -> Result<String> {
    let text = std::fs::read_to_string(root.join("Cargo.toml")).context("reading Cargo.toml")?;
    let section = text
        .split("[workspace.package]")
        .nth(1)
        .context("no [workspace.package] in Cargo.toml")?;
    let line = section
        .lines()
        .take_while(|l| !l.trim_start().starts_with('['))
        .find(|l| l.trim_start().starts_with("version"))
        .context("no version in [workspace.package]")?;
    let version = line
        .split('"')
        .nth(1)
        .context("the workspace version is not a quoted string")?;
    msi_version(version)?;
    Ok(version.to_owned())
}

/// An MSI `ProductVersion` is `major.minor.build` with major and minor below 256 and build below
/// 65536 (a fourth field is ignored by upgrades), so anything else is refused.
pub fn msi_version(version: &str) -> Result<()> {
    let parts: Vec<&str> = version.split('.').collect();
    let ok = parts.len() == 3
        && parts[0].parse::<u32>().is_ok_and(|n| n < 256)
        && parts[1].parse::<u32>().is_ok_and(|n| n < 256)
        && parts[2].parse::<u32>().is_ok_and(|n| n < 65536);
    if !ok {
        bail!(
            "version {version:?} is not a valid MSI version (major.minor.build, for example 0.1.0)"
        );
    }
    Ok(())
}

/// The `-bindpath name=folder` pairs for [`BINDPATHS`].
fn bind_paths(root: &Path, bin: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("bin", bin.to_path_buf()),
        ("repo", root.to_path_buf()),
        ("installer", root.join("installer")),
        ("notices", root.join("target").join("installer")),
        ("sbom", root.join("target").join("sbom")),
        (
            "assets",
            root.join("host").join("gander-host").join("assets"),
        ),
    ]
}

/// What a `wix` run printed, split by severity.
#[derive(Debug, Default)]
pub struct WixOutput {
    pub ok: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

fn wix_output(out: &std::process::Output) -> WixOutput {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let mut result = WixOutput {
        ok: out.status.success(),
        ..WixOutput::default()
    };
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.contains(" error WIX") || line.contains(": error ") {
            result.errors.push(line.to_owned());
        } else if line.contains(" warning WIX") || line.contains(": warning ") {
            result.warnings.push(line.to_owned());
        }
    }
    result
}

/// `wix build` of `installer/main.wxs` into `out`, with `bin` as the folder of the two executables.
pub fn wix_build(
    tools: &WixTools,
    root: &Path,
    version: &str,
    bin: &Path,
    out: &Path,
) -> Result<WixOutput> {
    let mut command = tools.command().context("the wix tool was not found")?;
    command.current_dir(root).args([
        "build",
        "-arch",
        "x64",
        "-ext",
        "WixToolset.UI.wixext",
        "-pdbtype",
        "none",
        "-d",
    ]);
    command.arg(format!("Version={version}"));
    for (name, dir) in bind_paths(root, bin) {
        command
            .arg("-bindpath")
            .arg(format!("{name}={}", dir.display()));
    }
    command
        .arg("-o")
        .arg(out)
        .arg(root.join("installer").join("main.wxs"));
    let output = command.output().context("running wix build")?;
    Ok(wix_output(&output))
}

/// `wix msi validate` (the ICE rules) of a built MSI.
pub fn wix_validate(tools: &WixTools, msi: &Path) -> Result<WixOutput> {
    let mut command = tools.command().context("the wix tool was not found")?;
    let output = command
        // ICE91 only matters for a per-machine install of a package that writes to a user folder;
        // this one is per-user only.
        .args(["msi", "validate", "-sice", "ICE91"])
        .arg(msi)
        .output()
        .context("running wix msi validate")?;
    Ok(wix_output(&output))
}

/// The file names the .wxs installs (`<File Name=...>`).
fn wxs_file_names(text: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(text) else {
        return Vec::new();
    };
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "File")
        .filter_map(|n| n.attribute("Name"))
        .map(str::to_owned)
        .collect()
}

/// Lists what is inside an MSI by an administrative install (`msiexec /a`) into a fresh temporary
/// folder: it copies the files and writes nothing else (no registry, no shortcuts, no custom
/// actions, no tasks, no rules). Returns `(relative path, size)` per file, MSI itself excluded.
#[cfg(windows)]
pub fn admin_extract(msi: &Path) -> Result<Vec<(String, u64)>> {
    let target = unique_temp_dir("msi-extract")?;
    let log = target.join("msiexec.log");
    // msiexec wants backslashes in TARGETDIR and a trailing-slash-free path.
    let status = Command::new("msiexec.exe")
        .arg("/a")
        .arg(msi)
        .arg("/qn")
        .arg(format!("TARGETDIR={}", target.display()))
        .arg("/l*v")
        .arg(&log)
        .status()
        .context("running msiexec /a")?;
    let mut files = Vec::new();
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, u64)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else if let Ok(meta) = entry.metadata() {
                let relative = path.strip_prefix(base).unwrap_or(&path);
                out.push((display_path(relative), meta.len()));
            }
        }
    }
    walk(&target, &target, &mut files);
    let msi_name = msi
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    files.retain(|(name, _)| *name != msi_name && name != "msiexec.log");
    files.sort();
    let _ = std::fs::remove_dir_all(&target);
    if !status.success() {
        bail!("msiexec /a exited with {:?}", status.code());
    }
    Ok(files)
}

#[cfg(not(windows))]
pub fn admin_extract(_msi: &Path) -> Result<Vec<(String, u64)>> {
    bail!("msiexec is Windows only")
}

/// The extract holds every file the .wxs names, each non-empty.
fn check_extract(files: &[(String, u64)], expected: &[String]) -> Check {
    let mut problems = Vec::new();
    for name in expected {
        match files.iter().find(|(f, _)| f.ends_with(&format!("/{name}"))) {
            None => problems.push(format!("{name} is not in the MSI")),
            Some((_, 0)) => problems.push(format!("{name} is empty")),
            Some(_) => {}
        }
    }
    Check::from_problems(
        "administrative extract holds every installed file",
        &format!("{} file(s)", files.len()),
        problems,
    )
}

// ---------------------------------------------------------------------------------------------
// The SBOM and the dependency list inside the executables (WP-82)
// ---------------------------------------------------------------------------------------------

/// `cargo xtask sbom`: writes `target/sbom/gander-host.cdx.json`, validated. Returns the checks and
/// the document (for [`check_embedded_list`]).
fn check_sbom(root: &Path) -> (Vec<Check>, Option<serde_json::Value>) {
    match crate::sbom::generate(root, &root.join("target").join("sbom")) {
        Ok(s) => (
            vec![Check::ok(
                "cargo xtask sbom writes a CycloneDX 1.5 SBOM that validates against the schema",
                format!(
                    "{} third-party crate(s), {} build-only, {} bytes, {}",
                    s.third_party,
                    s.build_only,
                    s.bytes,
                    display_path(&s.path)
                ),
            )],
            Some(s.bom),
        ),
        Err(e) => (
            vec![Check::fail("cargo xtask sbom", format!("{e:#}"))],
            None,
        ),
    }
}

/// The executable at `exe` carries a `.dep-v0` list whose root is `name` and whose packages are all
/// in the SBOM `bom`.
fn check_embedded_list(exe: &Path, name: &str, bom: &serde_json::Value) -> Check {
    let title = format!("{name}.exe carries its dependency list (.dep-v0, cargo-auditable format)");
    match std::fs::read(exe) {
        Ok(bytes) => match crate::sbom::check_binary(&bytes, name, bom) {
            Ok(detail) => Check::ok(title, detail),
            Err(e) => Check::fail(title, format!("{e:#}")),
        },
        Err(e) => Check::fail(title, format!("{}: {e}", display_path(exe))),
    }
}

// ---------------------------------------------------------------------------------------------
// Release binary checks (--binary)
// ---------------------------------------------------------------------------------------------

/// The PE `Subsystem` field: 2 is Windows GUI, 3 is Windows console.
pub fn pe_subsystem(bytes: &[u8]) -> Option<u16> {
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
    let optional = pe + 24;
    let magic = u16_at(optional)?;
    if magic != 0x10b && magic != 0x20b {
        return None;
    }
    u16_at(optional + 68)
}

fn unique_temp_dir(tag: &str) -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("gander-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// A release exe must not name the user profile, the workspace, the cargo home or the toolchain
/// (`repro.rs`, `docs/release/reproducible-builds.md`). The folders are named in the detail only
/// by role, never by path.
fn check_no_local_paths(root: &Path, exe: &Path) -> Check {
    let name = format!(
        "{} names no local folder",
        exe.file_name().unwrap_or_default().to_string_lossy()
    );
    let Ok(bytes) = std::fs::read(exe) else {
        return Check::fail(&name, "cannot read the executable");
    };
    let hits: usize = crate::repro::local_folders(root)
        .iter()
        .map(|f| crate::repro::count_path(&bytes, f))
        .sum();
    if hits == 0 {
        Check::ok(
            name,
            "0 path strings (profile, workspace, cargo home, toolchain)",
        )
    } else {
        Check::fail(
            name,
            format!("{hits} path string(s) of a local folder; was it built without the remap?"),
        )
    }
}

fn check_release_binary(root: &Path, bom: Option<&serde_json::Value>) -> Vec<Check> {
    let mut out = Vec::new();
    if !cfg!(windows) {
        out.push(Check::new(
            Status::Warn,
            "release binary checks",
            "Windows only, skipped",
        ));
        return out;
    }
    println!("==> cargo build --locked --release -p gander-host (this takes a few minutes)");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(root)
        .args(["build", "--locked", "--release", "-p", "gander-host"]);
    crate::repro::apply(&mut cmd, root);
    let built = cmd.status();
    if !built.is_ok_and(|s| s.success()) {
        out.push(Check::fail(
            "cargo build --locked --release -p gander-host",
            "build failed",
        ));
        return out;
    }
    let exe = root.join("target").join("release").join("gander-host.exe");
    out.push(check_no_local_paths(root, &exe));
    match std::fs::read(&exe).ok().as_deref().and_then(pe_subsystem) {
        Some(2) => out.push(Check::ok(
            "release gander-host.exe is a GUI-subsystem program",
            "no console window opens",
        )),
        other => out.push(Check::fail(
            "release gander-host.exe is a GUI-subsystem program",
            format!("PE subsystem is {other:?}, expected Some(2)"),
        )),
    }

    match bom {
        Some(bom) => out.push(check_embedded_list(&exe, "gander-host", bom)),
        None => out.push(Check::fail(
            "gander-host.exe carries its dependency list (.dep-v0)",
            "no SBOM to compare with (cargo xtask sbom failed)",
        )),
    }

    let work = match unique_temp_dir("installer-check") {
        Ok(w) => w,
        Err(e) => {
            out.push(Check::fail("temporary directory", format!("{e:#}")));
            return out;
        }
    };

    // 1. Redirected pipes keep working: this is how the harnesses read the host.
    let shots = work.join("shots-pipe");
    let data = work.join("data-pipe");
    let exe_arg = exe.display().to_string();
    let result = capture(
        &exe_arg,
        &[
            "--ui-smoke",
            "--ui-no-native",
            "--ui-out",
            &shots.display().to_string(),
            "--data-dir",
            &data.display().to_string(),
        ],
    );
    match result {
        Ok(r) if r.success() && r.stdout.contains("ui-smoke: OK") => out.push(Check::ok(
            "release host: --ui-smoke output reaches a pipe",
            r.stdout.lines().last().unwrap_or_default().to_owned(),
        )),
        Ok(r) => out.push(Check::fail(
            "release host: --ui-smoke output reaches a pipe",
            format!(
                "exit {:?}; stdout {:?}; stderr {:?}",
                r.code,
                r.stdout.trim(),
                r.stderr.trim()
            ),
        )),
        Err(e) => out.push(Check::fail("release host: --ui-smoke", format!("{e:#}"))),
    }
    if data.join("logs").exists() {
        out.push(Check::fail(
            "release host leaves redirected output alone",
            "a log file was created although stdout and stderr were redirected",
        ));
    } else {
        out.push(Check::ok(
            "release host leaves redirected output alone",
            "no log file written",
        ));
    }

    // 2. No console and no redirection (how Task Scheduler and the Start menu start it): the same
    //    output lands in host.log. `start /wait` runs a GUI program with no inherited handles, and
    //    DETACHED_PROCESS gives cmd.exe no console, so the host has no parent console to attach to
    //    (run from a terminal it would otherwise print there, which is the point of attaching).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        let shots = work.join("shots-log");
        let data = work.join("data-log");
        let command = format!(
            "/c start \"\" /wait \"{}\" --ui-smoke --ui-no-native --ui-out \"{}\" --data-dir \"{}\"",
            exe.display(),
            shots.display(),
            data.display()
        );
        let status = Command::new("cmd")
            .raw_arg(command)
            .creation_flags(DETACHED_PROCESS)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let log = std::fs::read_to_string(data.join("logs").join("host.log")).unwrap_or_default();
        match status {
            Ok(s)
                if s.success() && log.contains("ui-smoke: OK") && log.contains("started (pid") =>
            {
                out.push(Check::ok(
                    "release host with no console writes its output to logs/host.log",
                    format!("{} line(s)", log.lines().count()),
                ))
            }
            Ok(s) => out.push(Check::fail(
                "release host with no console writes its output to logs/host.log",
                format!("exit {:?}; log {:?}", s.code(), log.trim()),
            )),
            Err(e) => out.push(Check::fail(
                "release host started without a console",
                e.to_string(),
            )),
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    out
}

// ---------------------------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------------------------

/// A real `wix build` of the .wxs against stand-in executables (the release build is not needed),
/// then `wix msi validate`. Catches a schema error, a missing file or an ICE failure in seconds.
fn check_wix_dry_run(root: &Path, tools: &WixTools, wxs: &str) -> Vec<Check> {
    let work = match unique_temp_dir("wix-dry-run") {
        Ok(w) => w,
        Err(e) => return vec![Check::fail("wix dry run", format!("{e:#}"))],
    };
    let bin = work.join("bin");
    let result = (|| -> Result<Vec<Check>> {
        std::fs::create_dir_all(&bin)?;
        for stand_in in ["gander-host.exe", "gander-render.exe"] {
            std::fs::write(bin.join(stand_in), b"stand-in for the dry run")?;
        }
        let msi = work.join("dry-run.msi");
        let mut checks = Vec::new();
        let built = wix_build(tools, root, "0.0.0", &bin, &msi)?;
        checks.push(Check::new(
            if built.ok && built.errors.is_empty() {
                Status::Ok
            } else {
                Status::Fail
            },
            "wix build of installer/main.wxs (dry run with stand-in binaries)",
            if built.ok {
                format!("{} warning(s)", built.warnings.len())
            } else {
                built.errors.join(" | ")
            },
        ));
        if !built.ok {
            return Ok(checks);
        }
        let validated = wix_validate(tools, &msi)?;
        checks.push(Check::new(
            if validated.ok && validated.errors.is_empty() {
                Status::Ok
            } else {
                Status::Fail
            },
            "wix msi validate (ICE rules) of the dry-run MSI",
            if validated.ok {
                format!("{} warning(s)", validated.warnings.len())
            } else {
                validated.errors.join(" | ")
            },
        ));
        if cfg!(windows) && validated.ok {
            match admin_extract(&msi) {
                Ok(files) => checks.push(check_extract(&files, &wxs_file_names(wxs))),
                Err(e) => checks.push(Check::fail("administrative extract", format!("{e:#}"))),
            }
        }
        Ok(checks)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result.unwrap_or_else(|e| vec![Check::fail("wix dry run", format!("{e:#}"))])
}

/// Entry point for `cargo xtask installer-check [--binary]`.
pub fn run_check(args: &[String]) -> Result<ExitCode> {
    let mut binary = false;
    for a in args {
        match a.as_str() {
            "--binary" => binary = true,
            other => {
                bail!("unknown argument {other}; usage: cargo xtask installer-check [--binary]")
            }
        }
    }
    let root = workspace_root();
    let installer = root.join("installer");
    let mut checks = Vec::new();

    let wxs_path = installer.join("main.wxs");
    let wxs = std::fs::read_to_string(&wxs_path);
    match &wxs {
        Ok(text) => {
            checks.extend(check_wxs_text(text));
            checks.push(check_wxs_sources(&root, text));
        }
        Err(e) => checks.push(Check::fail("installer/main.wxs exists", e.to_string())),
    }
    match std::fs::read_to_string(installer.join("autostart-task.xml")) {
        Ok(text) => checks.extend(check_task_text(&text)),
        Err(e) => checks.push(Check::fail(
            "installer/autostart-task.xml exists",
            e.to_string(),
        )),
    }
    checks.extend(check_scripts(&installer)?);
    checks.extend(check_host_packaging(&root, wxs.as_deref().ok()));
    checks.extend(check_notices(&root));
    let (sbom_checks, bom) = check_sbom(&root);
    checks.extend(sbom_checks);
    if binary {
        checks.extend(check_release_binary(&root, bom.as_ref()));
    }

    let tools = detect_wix();
    match (&wxs, tools.complete()) {
        (Ok(text), true) => {
            checks.push(Check::ok(
                "MSI toolchain present",
                format!(
                    "wix {} ({}), {}",
                    tools.version.as_deref().unwrap_or("?"),
                    tools
                        .wix
                        .as_ref()
                        .map_or(String::new(), |p| display_path(p)),
                    tools.extensions.join(", ")
                ),
            ));
            checks.extend(check_wix_dry_run(&root, &tools, text));
        }
        _ => {
            checks.push(Check::new(
                Status::Defer,
                "MSI build (wix build)",
                format!(
                    "needs the WiX Toolset 5 tool and its UI extension ({}); installing them is a human-gated step: {}",
                    wix_missing_text(&tools),
                    INSTALL_HINT.replace('\n', " ; ")
                ),
            ));
        }
    }

    println!("installer-check");
    let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
    for c in &checks {
        c.print();
        *counts
            .entry(match c.status {
                Status::Ok => "ok",
                Status::Fail => "fail",
                Status::Warn => "warn",
                Status::Defer => "deferred",
            })
            .or_default() += 1;
    }
    println!(
        "installer-check: {} ok, {} fail, {} warn, {} deferred",
        counts.get("ok").unwrap_or(&0),
        counts.get("fail").unwrap_or(&0),
        counts.get("warn").unwrap_or(&0),
        counts.get("deferred").unwrap_or(&0)
    );
    if counts.contains_key("fail") {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

/// Entry point for `cargo xtask installer-build`.
pub fn run_build(args: &[String]) -> Result<ExitCode> {
    if !args.is_empty() {
        bail!("usage: cargo xtask installer-build");
    }
    let root = workspace_root();
    let tools = detect_wix();
    if !tools.complete() {
        eprintln!(
            "installer-build: the MSI needs the WiX Toolset {WIX_MAJOR} ({}).\nInstall it, then run this again:\n{INSTALL_HINT}",
            wix_missing_text(&tools),
        );
        return Ok(ExitCode::from(1));
    }
    let version = workspace_version(&root)?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    // 1. The committed icons are what the SVGs produce (the exe icon and the MSI icon come from
    //    the same file).
    println!("==> cargo xtask icons --check");
    if crate::icons::run(&["--check".to_owned()])? != ExitCode::SUCCESS {
        bail!("the committed icons are stale; run `cargo xtask icons` and commit the result");
    }
    // 2. Notices and the SBOM, then the release binaries (build.rs embeds the icon, the version and
    //    the dependency list into the executables).
    println!("==> cargo xtask notices");
    let notices = crate::notices::generate(&root, &root.join("target").join("installer"))?;
    println!(
        "notices: {} crate(s) -> {}",
        notices.crates,
        display_path(&notices.notices)
    );
    println!("==> cargo xtask sbom");
    let sbom = crate::sbom::generate(&root, &root.join("target").join("sbom"))?;
    println!(
        "sbom: {} third-party crate(s) ({} build-only), {} bytes -> {}",
        sbom.third_party,
        sbom.build_only,
        sbom.bytes,
        display_path(&sbom.path)
    );
    let build_args = [
        "build",
        "--locked",
        "--release",
        "-p",
        "gander-host",
        "-p",
        "gander-render",
    ];
    println!("==> cargo {}", build_args.join(" "));
    let mut cmd = Command::new(&cargo);
    cmd.current_dir(&root).args(build_args);
    // Local folders remapped (`repro.rs`); `/Brepro` comes from the build scripts.
    crate::repro::apply(&mut cmd, &root);
    let status = cmd.status().context("running cargo build --release")?;
    if !status.success() {
        bail!("cargo build --release failed");
    }
    // 3. Both executables carry their dependency list and every package in it is in the SBOM.
    let bin = root.join("target").join("release");
    for exe in ["gander-host.exe", "gander-render.exe"] {
        if !bin.join(exe).is_file() {
            bail!("{} was not built", display_path(&bin.join(exe)));
        }
    }
    for name in crate::notices::SHIPPED_BINARIES {
        let check = check_embedded_list(&bin.join(format!("{name}.exe")), name, &sbom.bom);
        check.print();
        if check.status == Status::Fail {
            bail!("{name}.exe does not carry a valid dependency list");
        }
        let check = check_no_local_paths(&root, &bin.join(format!("{name}.exe")));
        check.print();
        if check.status == Status::Fail {
            bail!("{name}.exe names a local folder");
        }
    }
    // 4. wix build.
    let out_dir = root.join("target").join("installer");
    std::fs::create_dir_all(&out_dir)?;
    let msi = out_dir.join(format!("Gander-{version}-x64.msi"));
    println!("==> wix build -> {}", display_path(&msi));
    let built = wix_build(&tools, &root, &version, &bin, &msi)?;
    for w in &built.warnings {
        println!("warning: {w}");
    }
    if !built.ok || !built.errors.is_empty() {
        for e in &built.errors {
            eprintln!("error: {e}");
        }
        bail!("wix build failed");
    }
    // 5. Validate (ICE rules), then look inside with an administrative extract.
    println!("==> wix msi validate");
    let validated = wix_validate(&tools, &msi)?;
    for w in &validated.warnings {
        println!("warning: {w}");
    }
    if !validated.ok || !validated.errors.is_empty() {
        for e in &validated.errors {
            eprintln!("error: {e}");
        }
        bail!("wix msi validate failed");
    }
    println!(
        "validate: no errors, {} warning(s)",
        validated.warnings.len()
    );
    if cfg!(windows) {
        println!(
            "==> msiexec /a (administrative extract into a temporary folder; installs nothing)"
        );
        let files = admin_extract(&msi)?;
        for (name, size) in &files {
            println!("  {size:>10}  {name}");
        }
        let wxs = std::fs::read_to_string(root.join("installer").join("main.wxs"))?;
        let check = check_extract(&files, &wxs_file_names(&wxs));
        check.print();
        if check.status == Status::Fail {
            bail!("the MSI does not hold every file");
        }
    }
    let size = std::fs::metadata(&msi)?.len();
    println!(
        "installer-build: {} ({size} bytes, {:.1} MiB), unsigned; see installer/README.md for signing",
        display_path(&msi),
        size as f64 / 1_048_576.0
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wxs() -> String {
        std::fs::read_to_string(workspace_root().join("installer").join("main.wxs")).unwrap()
    }

    fn failures(checks: &[Check]) -> Vec<String> {
        checks
            .iter()
            .filter(|c| c.status == Status::Fail)
            .map(|c| format!("{}: {}", c.name, c.detail))
            .collect()
    }

    #[test]
    fn the_real_wxs_passes() {
        assert_eq!(failures(&check_wxs_text(&wxs())), Vec::<String>::new());
    }

    #[test]
    fn the_real_task_template_passes() {
        let text = std::fs::read_to_string(
            workspace_root()
                .join("installer")
                .join("autostart-task.xml"),
        )
        .unwrap();
        assert_eq!(failures(&check_task_text(&text)), Vec::<String>::new());
    }

    #[test]
    fn a_broken_wxs_is_caught() {
        assert!(!failures(&check_wxs_text("<Wix xmlns=\"x\"><oops></Wix>")).is_empty());
        // A comment containing a double hyphen is not XML.
        assert!(!failures(&check_wxs_text(&wxs().replacen("<!--", "<!-- a -- b", 1))).is_empty());
        // Wrong namespace.
        assert!(!failures(&check_wxs_text("<Wix xmlns=\"urn:other\"/>")).is_empty());
    }

    #[test]
    fn dropping_an_id_or_going_machine_wide_is_caught() {
        let text = wxs();
        let no_firewall_file = text.replace("Fil_FirewallRules", "Fil_Renamed");
        assert!(
            failures(&check_wxs_text(&no_firewall_file))
                .iter()
                .any(|f| f.contains("Fil_FirewallRules"))
        );

        // WP-82: the SBOM file and its component are part of the contract.
        let no_sbom = text.replace("Fil_Sbom", "Fil_Renamed");
        assert!(
            failures(&check_wxs_text(&no_sbom))
                .iter()
                .any(|f| f.contains("Fil_Sbom"))
        );

        let machine = text.replace("Scope=\"perUser\"", "Scope=\"perMachine\"");
        assert!(
            failures(&check_wxs_text(&machine))
                .iter()
                .any(|f| f.contains("perUser"))
        );

        let hklm = text.replacen("Root=\"HKCU\"", "Root=\"HKLM\"", 1);
        assert!(
            failures(&check_wxs_text(&hklm))
                .iter()
                .any(|f| f.contains("HKLM"))
        );

        let elevated = text.replacen("Impersonate=\"yes\"", "Impersonate=\"no\"", 1);
        assert!(
            failures(&check_wxs_text(&elevated))
                .iter()
                .any(|f| f.contains("without impersonation"))
        );

        let fatal = text.replace("Return=\"ignore\"", "Return=\"check\"");
        assert!(
            failures(&check_wxs_text(&fatal))
                .iter()
                .any(|f| f.contains("Return"))
        );

        let dangling = text.replace(
            "<Custom Action=\"CA_NetworkSetup\"",
            "<Custom Action=\"CA_Nowhere\"",
        );
        assert!(
            failures(&check_wxs_text(&dangling))
                .iter()
                .any(|f| f.contains("CA_Nowhere"))
        );
    }

    #[test]
    fn wix_5_specifics_are_caught() {
        let text = wxs();
        // WIX0230: a component with a file and a registry key path needs a fixed GUID.
        let auto_guid = text.replacen(
            "Guid=\"DEECB382-A3FF-4583-BC1F-C6133D969CDB\"",
            "Guid=\"*\"",
            1,
        );
        let found = failures(&check_wxs_text(&auto_guid));
        assert!(
            found.iter().any(|f| f.contains("no fixed Guid")),
            "{found:?}"
        );
        // WiX 3 spelling.
        let old = text.replacen("FileRef=", "FileKey=", 1);
        assert!(!failures(&check_wxs_text(&old)).is_empty());
        // The product icon must exist.
        let no_icon = text.replace("Value=\"Gander.ico\"", "Value=\"Nope.ico\"");
        assert!(
            failures(&check_wxs_text(&no_icon))
                .iter()
                .any(|f| f.contains("ARPPRODUCTICON"))
        );
        // A bind path the build does not pass.
        let stray = text.replacen("!(bindpath.repo)", "!(bindpath.elsewhere)", 1);
        assert!(
            failures(&check_wxs_text(&stray))
                .iter()
                .any(|f| f.contains("elsewhere"))
        );
        // The Start menu entry opens the settings window.
        let no_open = text.replace("Arguments=\"--open\"", "Arguments=\"\"");
        assert!(
            failures(&check_wxs_text(&no_open))
                .iter()
                .any(|f| f.contains("--open"))
        );
    }

    #[test]
    fn the_committed_sources_the_wxs_names_exist() {
        let check = check_wxs_sources(&workspace_root(), &wxs());
        assert_eq!(check.status, Status::Ok, "{}", check.detail);
    }

    #[test]
    fn msi_versions_are_three_small_numbers() {
        assert!(msi_version("0.0.0").is_ok());
        assert!(msi_version("1.2.65535").is_ok());
        assert!(msi_version("256.0.0").is_err());
        assert!(msi_version("1.2").is_err());
        assert!(msi_version("1.2.3-beta").is_err());
        assert!(workspace_version(&workspace_root()).is_ok());
    }

    #[test]
    fn bind_paths_and_sources_are_read_from_the_text() {
        let text = r#"<File Source="!(bindpath.bin)\gander-host.exe" /><File Source="!(bindpath.repo)\LICENSE" />"#;
        assert_eq!(
            bindpaths_used(text).into_iter().collect::<Vec<_>>(),
            ["bin", "repo"]
        );
        assert_eq!(
            bound_sources(text),
            [
                ("bin".to_owned(), "gander-host.exe".to_owned()),
                ("repo".to_owned(), "LICENSE".to_owned())
            ]
        );
    }

    #[test]
    fn the_wix_version_must_be_the_major_the_source_is_written_for() {
        let mut tools = WixTools {
            version: Some("5.0.2+aa65968c".to_owned()),
            ..WixTools::default()
        };
        assert!(tools.version_ok());
        tools.version = Some("7.0.1".to_owned());
        assert!(!tools.version_ok());
        tools.version = None;
        assert!(!tools.version_ok());
        tools.extensions = vec!["WixToolset.UI.wixext 5.0.2".to_owned()];
        assert!(tools.missing_extensions().is_empty());
        tools.extensions.clear();
        assert_eq!(tools.missing_extensions(), ["WixToolset.UI.wixext"]);
    }

    #[test]
    fn an_elevated_task_is_caught() {
        let text = std::fs::read_to_string(
            workspace_root()
                .join("installer")
                .join("autostart-task.xml"),
        )
        .unwrap();
        let elevated = text.replace("LeastPrivilege</RunLevel>", "HighestAvailable</RunLevel>");
        let f = failures(&check_task_text(&elevated));
        assert!(f.iter().any(|m| m.contains("LeastPrivilege")), "{f:?}");
        let any_user = text.replace("@USER_SID@", "");
        assert!(!failures(&check_task_text(&any_user)).is_empty());
        assert!(!failures(&check_task_text("<Task>")).is_empty());
    }

    #[test]
    fn script_text_problems_find_em_dashes_boms_and_seven_only_names() {
        assert!(script_text_problems("a.ps1", "Write-Output 'ok'\n").is_empty());
        let dash = script_text_problems("a.ps1", "# fine\nWrite-Output 'a \u{2014} b'\n");
        assert_eq!(dash.len(), 1);
        assert!(
            dash[0].contains("a.ps1:2:") && dash[0].contains("U+2014"),
            "{dash:?}"
        );
        assert!(!script_text_problems("a.ps1", "\u{feff}Get-Date").is_empty());
        assert!(!script_text_problems("a.ps1", "$x | ConvertFrom-Json -AsHashtable").is_empty());
        assert!(!script_text_problems("a.ps1", "if ($IsWindows) {}").is_empty());
    }

    #[test]
    fn the_real_scripts_are_clean() {
        let dir = workspace_root().join("installer");
        for script in installer_scripts(&dir).unwrap() {
            let text = std::fs::read_to_string(&script).unwrap();
            let name = script.file_name().unwrap().to_string_lossy().into_owned();
            assert_eq!(script_text_problems(&name, &text), Vec::<String>::new());
        }
    }

    #[test]
    fn pe_subsystem_reads_the_header() {
        let mut pe = vec![0u8; 0x200];
        pe[0] = b'M';
        pe[1] = b'Z';
        pe[0x3c] = 0x80;
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        let optional = 0x80 + 24;
        pe[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        pe[optional + 68..optional + 70].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(pe_subsystem(&pe), Some(2));
        pe[optional + 68] = 3;
        assert_eq!(pe_subsystem(&pe), Some(3));
        assert_eq!(pe_subsystem(b"not a program"), None);
        assert_eq!(pe_subsystem(&pe[..0x90]), None);
        pe[optional] = 0;
        assert_eq!(pe_subsystem(&pe), None, "unknown optional header magic");
    }
}
