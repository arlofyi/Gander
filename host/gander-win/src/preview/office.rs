// SPDX-License-Identifier: Apache-2.0
//! Which Office renderers this computer has: Microsoft Office applications (for COM
//! automation) and LibreOffice (headless PDF conversion).
//!
//! Detection reads the registry and well-known folders only; it never starts anything. The
//! lookups go through the [`Probe`] trait so the decision logic is tested with fake machines, and
//! the real [`SystemProbe`] is tested for the facts that are true of the development machine.
//!
//! Renderer preference for a document (protocol spec, "preview"): Office COM when the matching
//! application is installed, else LibreOffice when found, else none (the embedded thumbnail
//! still serves the list thumbnail, and a failed render can fall back to it for page 1).

use std::path::{Path, PathBuf};

use gander_core::protocol::host::OfficePreview;

/// An Office application Gander can automate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OfficeApp {
    /// `PowerPoint.Application`, for `.pptx`.
    PowerPoint,
    /// `Word.Application`, for `.docx`.
    Word,
    /// `Excel.Application`, for `.xlsx`.
    Excel,
}

impl OfficeApp {
    /// The application for an extension, for the plain OOXML formats only. Macro-enabled,
    /// legacy binary and ODF formats are never handed to Office.
    pub fn from_ext(ext: &str) -> Option<OfficeApp> {
        match ext {
            "pptx" => Some(OfficeApp::PowerPoint),
            "docx" => Some(OfficeApp::Word),
            "xlsx" => Some(OfficeApp::Excel),
            _ => None,
        }
    }

    /// The COM ProgID.
    pub const fn prog_id(self) -> &'static str {
        match self {
            OfficeApp::PowerPoint => "PowerPoint.Application",
            OfficeApp::Word => "Word.Application",
            OfficeApp::Excel => "Excel.Application",
        }
    }

    /// Image name of the application process.
    pub const fn exe(self) -> &'static str {
        match self {
            OfficeApp::PowerPoint => "POWERPNT.EXE",
            OfficeApp::Word => "WINWORD.EXE",
            OfficeApp::Excel => "EXCEL.EXE",
        }
    }

    /// The renderer name reported in `preview/info`.
    pub const fn renderer_name(self) -> &'static str {
        match self {
            OfficeApp::PowerPoint => "office_com_powerpoint",
            OfficeApp::Word => "office_com_word",
            OfficeApp::Excel => "office_com_excel",
        }
    }
}

/// How a document would be rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfficeBackend {
    /// Automate the installed application.
    Com(OfficeApp),
    /// Convert to PDF with the LibreOffice at this path (`soffice.exe`).
    LibreOffice(PathBuf),
}

impl OfficeBackend {
    /// The renderer name reported in `preview/info`.
    pub fn renderer_name(&self) -> &'static str {
        match self {
            OfficeBackend::Com(app) => app.renderer_name(),
            OfficeBackend::LibreOffice(_) => "libreoffice",
        }
    }
}

/// Extensions LibreOffice can convert (the OOXML trio plus the ODF family).
const LIBREOFFICE_EXTS: &[&str] = &["docx", "xlsx", "pptx", "odt", "ods", "odp"];

/// What is installed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OfficeEnv {
    /// PowerPoint is registered for automation and its executable exists.
    pub powerpoint: bool,
    /// Word is registered for automation and its executable exists.
    pub word: bool,
    /// Excel is registered for automation and its executable exists.
    pub excel: bool,
    /// `soffice.exe` of a LibreOffice installation, when found.
    pub libreoffice: Option<PathBuf>,
}

impl OfficeEnv {
    /// A machine with no Office renderer at all.
    pub fn none() -> Self {
        Self::default()
    }

    /// Looks at this computer.
    pub fn detect() -> Self {
        Self::detect_with(&SystemProbe)
    }

    /// Looks at a machine described by `probe`.
    pub fn detect_with(probe: &dyn Probe) -> Self {
        Self {
            powerpoint: com_server_installed(probe, OfficeApp::PowerPoint),
            word: com_server_installed(probe, OfficeApp::Word),
            excel: com_server_installed(probe, OfficeApp::Excel),
            libreoffice: find_libreoffice(probe),
        }
    }

    /// Whether `app` can be automated here.
    pub fn has_app(&self, app: OfficeApp) -> bool {
        match app {
            OfficeApp::PowerPoint => self.powerpoint,
            OfficeApp::Word => self.word,
            OfficeApp::Excel => self.excel,
        }
    }

    /// The renderer for a document with this extension (lower case, no dot): Office COM first,
    /// then LibreOffice, else `None`.
    pub fn backend_for(&self, ext: &str) -> Option<OfficeBackend> {
        if let Some(app) = OfficeApp::from_ext(ext).filter(|a| self.has_app(*a)) {
            return Some(OfficeBackend::Com(app));
        }
        if LIBREOFFICE_EXTS.contains(&ext) {
            return self.libreoffice.clone().map(OfficeBackend::LibreOffice);
        }
        None
    }

    /// `capabilities.preview_kinds.office`: `render` when any document type has a full renderer,
    /// else `thumbnail` (embedded thumbnails still serve the lists).
    pub fn capability(&self) -> OfficePreview {
        if self.powerpoint || self.word || self.excel || self.libreoffice.is_some() {
            OfficePreview::Render
        } else {
            OfficePreview::Thumbnail
        }
    }
}

/// Reads of the machine that detection needs.
pub trait Probe {
    /// The default value of `HKEY_CLASSES_ROOT\<subkey>`.
    fn classes_default(&self, subkey: &str) -> Option<String>;
    /// The default value of `HKEY_LOCAL_MACHINE\<subkey>` (64-bit view, then 32-bit view).
    fn local_machine_default(&self, subkey: &str) -> Option<String>;
    /// Whether a file exists.
    fn file_exists(&self, path: &Path) -> bool;
    /// An environment variable.
    fn env(&self, name: &str) -> Option<String>;
}

/// Pulls the executable path out of a `LocalServer32` value such as
/// `"C:\Office\POWERPNT.EXE" /AUTOMATION` or `C:\Office\WINWORD.EXE /Automation`.
pub fn server_exe_path(value: &str) -> Option<PathBuf> {
    let v = value.trim();
    if let Some(rest) = v.strip_prefix('"') {
        let end = rest.find('"')?;
        return Some(PathBuf::from(&rest[..end]));
    }
    let lower = v.to_ascii_lowercase();
    let end = lower.find(".exe")? + ".exe".len();
    Some(PathBuf::from(&v[..end]))
}

fn com_server_installed(probe: &dyn Probe, app: OfficeApp) -> bool {
    let Some(clsid) = probe.classes_default(&format!("{}\\CLSID", app.prog_id())) else {
        return false;
    };
    let Some(server) = probe.classes_default(&format!("CLSID\\{clsid}\\LocalServer32")) else {
        return false;
    };
    server_exe_path(&server).is_some_and(|p| probe.file_exists(&p))
}

fn find_libreoffice(probe: &dyn Probe) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    // The documented UNO registration: the default value is the `program` folder.
    if let Some(dir) = probe.local_machine_default("SOFTWARE\\LibreOffice\\UNO\\InstallPath") {
        candidates.push(Path::new(&dir).join("soffice.exe"));
    }
    for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(root) = probe.env(var) {
            candidates.push(Path::new(&root).join("LibreOffice/program/soffice.exe"));
        }
    }
    if let Some(local) = probe.env("LOCALAPPDATA") {
        candidates.push(Path::new(&local).join("Programs/LibreOffice/program/soffice.exe"));
    }
    if let Some(path) = probe.env("PATH") {
        for dir in path.split(';').filter(|d| !d.is_empty()) {
            candidates.push(Path::new(dir).join("soffice.exe"));
        }
    }
    candidates.into_iter().find(|c| probe.file_exists(c))
}

/// The real machine.
#[derive(Debug, Clone, Copy)]
pub struct SystemProbe;

impl Probe for SystemProbe {
    fn classes_default(&self, subkey: &str) -> Option<String> {
        registry::default_value(registry::Hive::Classes, subkey, 0)
    }

    fn local_machine_default(&self, subkey: &str) -> Option<String> {
        use registry::{Hive, VIEW_32, VIEW_64};
        registry::default_value(Hive::LocalMachine, subkey, VIEW_64)
            .or_else(|| registry::default_value(Hive::LocalMachine, subkey, VIEW_32))
    }

    fn file_exists(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }
}

mod registry {
    use std::ptr;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CLASSES_ROOT, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_WOW64_32KEY,
        KEY_WOW64_64KEY, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };

    #[derive(Clone, Copy)]
    pub enum Hive {
        Classes,
        LocalMachine,
    }

    /// Registry view flags (`HKLM` is redirected for 32-bit installers).
    pub const VIEW_64: u32 = KEY_WOW64_64KEY;
    pub const VIEW_32: u32 = KEY_WOW64_32KEY;

    /// The default (unnamed) value of `hive\subkey` when it is a string; `extra_access` adds
    /// registry-view flags.
    pub fn default_value(hive: Hive, subkey: &str, extra_access: u32) -> Option<String> {
        let root: HKEY = match hive {
            Hive::Classes => HKEY_CLASSES_ROOT,
            Hive::LocalMachine => HKEY_LOCAL_MACHINE,
        };
        let wide: Vec<u16> = subkey.encode_utf16().chain(Some(0)).collect();
        let mut key: HKEY = ptr::null_mut();
        // SAFETY: NUL-terminated subkey, valid out-pointer; the key is closed below.
        let status = unsafe {
            RegOpenKeyExW(
                root,
                wide.as_ptr(),
                0,
                KEY_QUERY_VALUE | extra_access,
                &mut key,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let mut bytes = 0u32;
        // SAFETY: size query with a null buffer; `key` is open.
        let sized = unsafe {
            RegQueryValueExW(
                key,
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut bytes,
            )
        };
        let mut out = None;
        if sized == ERROR_SUCCESS && (2..=64 * 1024).contains(&bytes) {
            let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
            let mut len = bytes;
            // SAFETY: `buf` holds `len` writable bytes; `key` is open.
            let read = unsafe {
                RegQueryValueExW(
                    key,
                    ptr::null(),
                    ptr::null(),
                    ptr::null_mut(),
                    buf.as_mut_ptr().cast(),
                    &mut len,
                )
            };
            if read == ERROR_SUCCESS {
                let units = (len as usize / 2).min(buf.len());
                let end = buf[..units].iter().position(|&c| c == 0).unwrap_or(units);
                out = Some(String::from_utf16_lossy(&buf[..end])).filter(|s| !s.is_empty());
            }
        }
        // SAFETY: `key` was opened by RegOpenKeyExW and is closed once.
        unsafe { RegCloseKey(key) };
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[derive(Default)]
    struct Fake {
        classes: HashMap<String, String>,
        hklm: HashMap<String, String>,
        files: HashSet<PathBuf>,
        env: HashMap<String, String>,
    }

    impl Probe for Fake {
        fn classes_default(&self, subkey: &str) -> Option<String> {
            self.classes.get(subkey).cloned()
        }
        fn local_machine_default(&self, subkey: &str) -> Option<String> {
            self.hklm.get(subkey).cloned()
        }
        fn file_exists(&self, path: &Path) -> bool {
            self.files.contains(path)
        }
        fn env(&self, name: &str) -> Option<String> {
            self.env.get(name).cloned()
        }
    }

    fn with_powerpoint(fake: &mut Fake) {
        let exe = r"C:\Office\POWERPNT.EXE";
        fake.classes
            .insert("PowerPoint.Application\\CLSID".into(), "{CLSID-PPT}".into());
        fake.classes.insert(
            "CLSID\\{CLSID-PPT}\\LocalServer32".into(),
            format!("\"{exe}\" /AUTOMATION"),
        );
        fake.files.insert(PathBuf::from(exe));
    }

    #[test]
    fn server_paths_are_parsed_from_every_registered_shape() {
        assert_eq!(
            server_exe_path(r#""C:\Program Files\Office\POWERPNT.EXE" /AUTOMATION"#),
            Some(PathBuf::from(r"C:\Program Files\Office\POWERPNT.EXE"))
        );
        assert_eq!(
            server_exe_path(r"C:\Office\WINWORD.EXE /Automation"),
            Some(PathBuf::from(r"C:\Office\WINWORD.EXE"))
        );
        assert_eq!(
            server_exe_path(r"C:\Program Files\Office\EXCEL.exe"),
            Some(PathBuf::from(r"C:\Program Files\Office\EXCEL.exe"))
        );
        assert_eq!(server_exe_path("not a server"), None);
    }

    #[test]
    fn empty_machine_reports_nothing_and_thumbnail_only() {
        let env = OfficeEnv::detect_with(&Fake::default());
        assert_eq!(env, OfficeEnv::none());
        assert_eq!(env.capability(), OfficePreview::Thumbnail);
        assert_eq!(env.backend_for("pptx"), None);
        assert_eq!(env.backend_for("odt"), None);
    }

    #[test]
    fn a_registration_without_the_executable_does_not_count() {
        let mut fake = Fake::default();
        with_powerpoint(&mut fake);
        fake.files.clear();
        assert!(!OfficeEnv::detect_with(&fake).powerpoint);
    }

    #[test]
    fn office_com_is_preferred_and_only_for_plain_ooxml() {
        let mut fake = Fake::default();
        with_powerpoint(&mut fake);
        let env = OfficeEnv::detect_with(&fake);
        assert!(env.powerpoint && !env.word && !env.excel);
        assert_eq!(env.capability(), OfficePreview::Render);
        assert_eq!(
            env.backend_for("pptx"),
            Some(OfficeBackend::Com(OfficeApp::PowerPoint))
        );
        // No Word here: docx has no renderer; macro-enabled and legacy files never get one.
        assert_eq!(env.backend_for("docx"), None);
        assert_eq!(env.backend_for("pptm"), None);
        assert_eq!(env.backend_for("ppt"), None);
    }

    #[test]
    fn libreoffice_is_found_by_registry_known_folder_or_path() {
        let soffice = PathBuf::from(r"D:\LO\program\soffice.exe");
        let mut by_registry = Fake::default();
        by_registry.hklm.insert(
            "SOFTWARE\\LibreOffice\\UNO\\InstallPath".into(),
            r"D:\LO\program".into(),
        );
        by_registry.files.insert(soffice.clone());
        assert_eq!(
            OfficeEnv::detect_with(&by_registry).libreoffice,
            Some(soffice.clone())
        );

        let mut by_folder = Fake::default();
        by_folder
            .env
            .insert("ProgramFiles".into(), r"C:\Program Files".into());
        let known = PathBuf::from(r"C:\Program Files").join("LibreOffice/program/soffice.exe");
        by_folder.files.insert(known.clone());
        assert_eq!(OfficeEnv::detect_with(&by_folder).libreoffice, Some(known));

        let mut by_path = Fake::default();
        by_path
            .env
            .insert("PATH".into(), r"C:\bin;D:\LO\program".into());
        by_path.files.insert(soffice.clone());
        let env = OfficeEnv::detect_with(&by_path);
        assert_eq!(env.libreoffice, Some(soffice.clone()));
        // LibreOffice takes the types Office does not have, including ODF.
        assert_eq!(env.capability(), OfficePreview::Render);
        assert_eq!(
            env.backend_for("odp"),
            Some(OfficeBackend::LibreOffice(soffice.clone()))
        );
        assert_eq!(
            env.backend_for("docx"),
            Some(OfficeBackend::LibreOffice(soffice))
        );
        assert_eq!(env.backend_for("txt"), None);
    }

    #[test]
    fn libreoffice_is_not_installed_on_this_machine() {
        // The development machine has no LibreOffice and must not get one: detection has to
        // report "none" here, so Office documents fall to COM or nothing. The assertion is
        // skipped (with a message) on a machine that does have one.
        let found = OfficeEnv::detect().libreoffice;
        let probe = SystemProbe;
        let registered = probe
            .local_machine_default(r"SOFTWARE\LibreOffice\UNO\InstallPath")
            .is_some();
        let on_path = probe.env("PATH").is_some_and(|p| {
            p.split(';')
                .any(|d| Path::new(d).join("soffice.exe").is_file())
        });
        let in_folder = ["ProgramFiles", "ProgramFiles(x86)"].iter().any(|v| {
            probe.env(v).is_some_and(|r| {
                Path::new(&r)
                    .join("LibreOffice/program/soffice.exe")
                    .is_file()
            })
        });
        if registered || on_path || in_folder {
            eprintln!("SKIPPED: this machine has a LibreOffice installation ({found:?})");
            assert!(found.is_some());
        } else {
            eprintln!("LibreOffice detection: none (registry, standard folders and PATH checked)");
            assert_eq!(found, None);
        }
    }

    #[test]
    fn this_machines_office_registration_is_read_from_the_registry() {
        // Facts about Office are machine dependent; only check the probe does not misbehave and
        // that every reported application really has a server executable on disk.
        let env = OfficeEnv::detect();
        eprintln!(
            "Office detection: powerpoint={} word={} excel={} libreoffice={:?}",
            env.powerpoint, env.word, env.excel, env.libreoffice
        );
        for app in [OfficeApp::PowerPoint, OfficeApp::Word, OfficeApp::Excel] {
            if env.has_app(app) {
                assert!(com_server_installed(&SystemProbe, app));
            }
        }
    }
}
