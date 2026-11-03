// SPDX-License-Identifier: Apache-2.0
//! The fixed rules of the stream launch policy: hard-refused extensions (step 1), the never list
//! of handler executables (step 2 of the decision), and browser detection (step 6).
//!
//! Everything here is pure string logic on file names and paths as Windows spells them. Path
//! comparisons are case-insensitive (ASCII), accept `/` or `\` and ignore the `\\?\` prefix that
//! `GetFinalPathNameByHandle` adds.

use std::path::Path;

/// Step 1: extensions that are never streamable, whatever their handler is.
///
/// The list in the spec, plus the macro-enabled Office formats. Lower case, without the dot.
/// `ppsm` (macro-enabled PowerPoint show) is missing from the spec's list and added here because
/// the security non-negotiable refuses every macro-enabled document.
pub const HARD_REFUSED_EXTENSIONS: &[&str] = &[
    "exe",
    "com",
    "scr",
    "pif",
    "bat",
    "cmd",
    "ps1",
    "psm1",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "msi",
    "msp",
    "msc",
    "hta",
    "cpl",
    "reg",
    "inf",
    "lnk",
    "url",
    "appx",
    "appxbundle",
    "msix",
    "msixbundle",
    "jar",
    "py",
    "pyw",
    "pyc",
    "rb",
    "pl",
    "sh",
    "ahk",
    "iso",
    "img",
    "vhd",
    "vhdx",
    // Macro-enabled Office.
    "docm",
    "dotm",
    "xlsm",
    "xltm",
    "xlam",
    "pptm",
    "potm",
    "ppam",
    "sldm",
    "ppsm",
];

/// Handler executables that are refused even for a benign extension (spec, decision step 2).
///
/// Matched against the handler's file name, case-insensitively; `*` matches any run of
/// characters. The first block is the spec's list verbatim. The second block is stricter than the
/// spec: interpreters and script hosts that live outside `System32` (the Python launcher sits in
/// `%SystemRoot%`, `hh.exe` runs scripts in compiled help, `regedit.exe` imports `.reg`).
pub const NEVER_LIST: &[&str] = &[
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "wscript.exe",
    "cscript.exe",
    "mshta.exe",
    "rundll32.exe",
    "regsvr32.exe",
    "msiexec.exe",
    "python*.exe",
    "pythonw*.exe",
    "java.exe",
    "javaw.exe",
    "node.exe",
    "ruby.exe",
    "perl.exe",
    "wsl.exe",
    "bash.exe",
    "explorer.exe",
    "control.exe",
    "conhost.exe",
    "wt.exe",
    // Stricter than the spec (see above).
    "py.exe",
    "pyw.exe",
    "javaws.exe",
    "hh.exe",
    "regedit.exe",
    "windowsterminal.exe",
];

/// Handlers under the Windows system directories that are nevertheless allowed (they still need
/// owner approval through the catalogue). Lower case.
pub const SYSTEM_DIR_ALLOWLIST: &[&str] = &["notepad.exe", "mspaint.exe"];

/// Directories below the system root whose executables are refused (except
/// [`SYSTEM_DIR_ALLOWLIST`]). The spec names `System32`; `SysWOW64` is its 32-bit twin and is
/// refused for the same reason.
pub const SYSTEM_SUBDIRS: &[&str] = &["System32", "SysWOW64"];

/// The final extension of a file name, lower-cased, without the dot.
///
/// `report.pdf.exe` gives `exe`. A name without a dot, a name ending in a dot, and a dot-file such
/// as `.bashrc` (whose only dot starts the name) have no extension.
pub fn final_extension(file_name: &str) -> Option<String> {
    // Windows ignores trailing dots and spaces in names; `evil.exe.` would run as `evil.exe`.
    let trimmed = file_name.trim_end_matches(['.', ' ']);
    let dot = trimmed.rfind('.')?;
    if dot == 0 {
        return None;
    }
    let ext = &trimmed[dot + 1..];
    if ext.is_empty() {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

/// Step 1: whether `ext` (lower case, no dot) is never streamable.
pub fn is_hard_refused_extension(ext: &str) -> bool {
    HARD_REFUSED_EXTENSIONS
        .iter()
        .any(|e| e.eq_ignore_ascii_case(ext))
}

/// Normalises a Windows path for comparison: strips `\\?\` / `\\?\UNC\`, turns `/` into `\`,
/// removes trailing separators and lower-cases ASCII.
pub fn normalize_windows_path(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('/', "\\");
    let raw = if let Some(rest) = raw.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{rest}")
    } else if let Some(rest) = raw.strip_prefix("\\\\?\\") {
        rest.to_owned()
    } else {
        raw
    };
    raw.trim_end_matches('\\').to_ascii_lowercase()
}

/// Whether two Windows paths name the same file once normalised.
pub fn same_windows_path(a: &Path, b: &Path) -> bool {
    normalize_windows_path(a) == normalize_windows_path(b)
}

/// The file name of a Windows path (after the last `\` or `/`), as given.
pub fn windows_file_name(path: &Path) -> String {
    let raw = path.to_string_lossy();
    raw.rsplit(['\\', '/'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// Whether `pattern` (lower case, `*` wildcards) matches `name` (any case).
fn glob_matches(pattern: &str, name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return name == pattern;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || name.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    if !name.ends_with(last) {
        return false;
    }
    for middle in &parts[1..parts.len() - 1] {
        match rest.find(middle) {
            Some(i) => rest = &rest[i + middle.len()..],
            None => return false,
        }
    }
    true
}

/// Whether the executable file name is on [`NEVER_LIST`].
pub fn is_never_listed_name(exe_file_name: &str) -> bool {
    NEVER_LIST.iter().any(|p| glob_matches(p, exe_file_name))
}

/// Whether `exe` sits anywhere below `system_root\System32` or `system_root\SysWOW64` and is not
/// on [`SYSTEM_DIR_ALLOWLIST`].
pub fn is_refused_system_binary(exe: &Path, system_root: &Path) -> bool {
    let exe_norm = normalize_windows_path(exe);
    let root = normalize_windows_path(system_root);
    let under_system_dir = SYSTEM_SUBDIRS.iter().any(|dir| {
        let prefix = format!("{root}\\{}\\", dir.to_ascii_lowercase());
        exe_norm.starts_with(&prefix)
    });
    if !under_system_dir {
        return false;
    }
    let name = windows_file_name(exe).to_ascii_lowercase();
    !SYSTEM_DIR_ALLOWLIST.contains(&name.as_str())
}

/// Browser engine family: decides the isolated-window launch template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrowserFamily {
    /// Chromium based: Edge, Chrome, Brave, Opera, Vivaldi, Arc.
    Chromium,
    /// Firefox (Gecko).
    Firefox,
}

/// A browser recognised as a handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Browser {
    /// Microsoft Edge.
    Edge,
    /// Google Chrome.
    Chrome,
    /// Mozilla Firefox.
    Firefox,
    /// Brave.
    Brave,
    /// Opera.
    Opera,
    /// Vivaldi.
    Vivaldi,
    /// Arc.
    Arc,
}

impl Browser {
    /// Display name, used in "opens in Microsoft Edge (isolated window)".
    pub const fn display_name(self) -> &'static str {
        match self {
            Browser::Edge => "Microsoft Edge",
            Browser::Chrome => "Google Chrome",
            Browser::Firefox => "Mozilla Firefox",
            Browser::Brave => "Brave",
            Browser::Opera => "Opera",
            Browser::Vivaldi => "Vivaldi",
            Browser::Arc => "Arc",
        }
    }

    /// Engine family.
    pub const fn family(self) -> BrowserFamily {
        match self {
            Browser::Firefox => BrowserFamily::Firefox,
            _ => BrowserFamily::Chromium,
        }
    }

    /// Detects a browser by executable file name, then by Authenticode publisher.
    ///
    /// Publishers are used only where the publisher ships nothing but browsers that could be a
    /// file handler; "Google LLC" and "Microsoft Corporation" sign many non-browser apps, so for
    /// them the executable name decides.
    pub fn detect(exe_file_name: &str, publisher: Option<&str>) -> Option<Browser> {
        let name = exe_file_name.to_ascii_lowercase();
        let by_name = match name.as_str() {
            "msedge.exe" => Some(Browser::Edge),
            "chrome.exe" => Some(Browser::Chrome),
            "firefox.exe" => Some(Browser::Firefox),
            "brave.exe" => Some(Browser::Brave),
            "opera.exe" => Some(Browser::Opera),
            "vivaldi.exe" => Some(Browser::Vivaldi),
            "arc.exe" => Some(Browser::Arc),
            _ => None,
        };
        if by_name.is_some() {
            return by_name;
        }
        let publisher = publisher?.to_ascii_lowercase();
        if publisher.starts_with("mozilla corporation") {
            Some(Browser::Firefox)
        } else if publisher.starts_with("brave software") {
            Some(Browser::Brave)
        } else if publisher.starts_with("opera norway") {
            Some(Browser::Opera)
        } else if publisher.starts_with("vivaldi technologies") {
            Some(Browser::Vivaldi)
        } else if publisher.starts_with("the browser company") {
            Some(Browser::Arc)
        } else {
            None
        }
    }
}

/// The isolated-window launch for a browser handler (spec, decision step 6): a dedicated profile
/// keeps the owner's tabs, sessions and extensions out of the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IsolatedBrowserLaunch {
    /// The browser.
    pub browser: Browser,
}

impl IsolatedBrowserLaunch {
    /// Name of the dedicated Firefox profile.
    pub const FIREFOX_PROFILE: &'static str = "gander-stream";

    /// Command-line arguments after the executable, one element per argument (the launcher
    /// quotes each one; nothing is built by string concatenation).
    ///
    /// Chromium: `--new-window --app=<file_url> --user-data-dir=<profile_dir>`.
    /// Firefox: `-new-window -P gander-stream <file_url>`.
    pub fn args(&self, file_url: &str, profile_dir: &Path) -> Vec<String> {
        match self.browser.family() {
            BrowserFamily::Chromium => vec![
                "--new-window".to_owned(),
                format!("--app={file_url}"),
                format!("--user-data-dir={}", profile_dir.display()),
            ],
            BrowserFamily::Firefox => vec![
                "-new-window".to_owned(),
                "-P".to_owned(),
                Self::FIREFOX_PROFILE.to_owned(),
                file_url.to_owned(),
            ],
        }
    }

    /// What the phone is told: "opens in Microsoft Edge (isolated window)".
    pub fn phone_notice(&self) -> String {
        format!("opens in {} (isolated window)", self.browser.display_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_extension_rules() {
        assert_eq!(final_extension("A.PDF.EXE").as_deref(), Some("exe"));
        assert_eq!(final_extension("report.pdf.exe").as_deref(), Some("exe"));
        assert_eq!(final_extension("deck.PPTX").as_deref(), Some("pptx"));
        assert_eq!(final_extension("Makefile"), None);
        assert_eq!(final_extension("trailing."), None);
        assert_eq!(final_extension(".bashrc"), None);
        assert_eq!(final_extension("evil.exe.").as_deref(), Some("exe"));
        assert_eq!(final_extension("evil.exe . .").as_deref(), Some("exe"));
    }

    #[test]
    fn globs() {
        assert!(is_never_listed_name("python.exe"));
        assert!(is_never_listed_name("Python3.12.EXE"));
        assert!(is_never_listed_name("pythonw3.exe"));
        assert!(is_never_listed_name("CMD.EXE"));
        assert!(!is_never_listed_name("pythonista.dll"));
        assert!(!is_never_listed_name("POWERPNT.EXE"));
        assert!(!is_never_listed_name("cmd.exe.bak"));
    }

    #[test]
    fn system_dir_rule() {
        let root = Path::new("C:\\Windows");
        assert!(is_refused_system_binary(
            Path::new("\\\\?\\C:\\WINDOWS\\system32\\dllhost.exe"),
            root
        ));
        assert!(is_refused_system_binary(
            Path::new("C:/Windows/SysWOW64/sub/x.exe"),
            root
        ));
        assert!(!is_refused_system_binary(
            Path::new("C:\\Windows\\System32\\notepad.exe"),
            root
        ));
        assert!(!is_refused_system_binary(
            Path::new("C:\\Windows\\System32\\MSPAINT.EXE"),
            root
        ));
        assert!(!is_refused_system_binary(
            Path::new("C:\\Windows\\System32x\\foo.exe"),
            root
        ));
        assert!(!is_refused_system_binary(
            Path::new("C:\\Program Files\\App\\app.exe"),
            root
        ));
    }

    #[test]
    fn path_normalisation() {
        assert!(same_windows_path(
            Path::new("\\\\?\\C:\\Program Files\\X\\A.EXE"),
            Path::new("c:/program files/x/a.exe")
        ));
        assert_eq!(
            normalize_windows_path(Path::new("\\\\?\\UNC\\srv\\share\\a.exe")),
            "\\\\srv\\share\\a.exe"
        );
        assert_eq!(
            windows_file_name(Path::new("C:\\a\\B.exe")),
            "B.exe".to_owned()
        );
    }

    #[test]
    fn browser_detection() {
        assert_eq!(Browser::detect("MSEDGE.EXE", None), Some(Browser::Edge));
        assert_eq!(
            Browser::detect("whatever.exe", Some("Mozilla Corporation")),
            Some(Browser::Firefox)
        );
        assert_eq!(Browser::detect("drive.exe", Some("Google LLC")), None);
        assert_eq!(
            Browser::detect("POWERPNT.EXE", Some("Microsoft Corporation")),
            None
        );
    }

    #[test]
    fn isolated_templates() {
        let edge = IsolatedBrowserLaunch {
            browser: Browser::Edge,
        };
        let args = edge.args("file:///C:/s/a.pdf", Path::new("C:\\p"));
        assert_eq!(args[0], "--new-window");
        assert_eq!(args[1], "--app=file:///C:/s/a.pdf");
        assert!(args[2].starts_with("--user-data-dir="));
        assert_eq!(
            edge.phone_notice(),
            "opens in Microsoft Edge (isolated window)"
        );
        let ff = IsolatedBrowserLaunch {
            browser: Browser::Firefox,
        };
        assert_eq!(
            ff.args("file:///a", Path::new("x")),
            vec!["-new-window", "-P", "gander-stream", "file:///a"]
        );
    }
}
