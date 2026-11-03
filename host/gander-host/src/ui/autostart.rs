// SPDX-License-Identifier: Apache-2.0
//! The Settings window's "Start Gander when I sign in" switch, behind a trait.
//!
//! The real implementation ([`TaskSchedulerAutostart`]) is a per-user Task Scheduler logon task
//! (ADR-0007): a `LogonTrigger` scoped to the signed-in user, `RunLevel LeastPrivilege` (never
//! "highest privileges"), started in that user's interactive session. The task definition is
//! `installer/autostart-task.xml`, embedded here as a template; it is registered with
//! `schtasks /Create /XML` as the current user, so no elevation is involved. The MSI runs the same
//! code through `gander-host --autostart on|off` at install and uninstall.
//!
//! Task Scheduler itself sits behind [`TaskScheduler`], so every test in this module (and every
//! other test in the crate) uses a fake or [`StubAutostart`]. Nothing here writes to Task
//! Scheduler, the registry or the Startup folder unless the application is started normally, and
//! tests must keep it that way. The one test that talks to the real Task Scheduler is
//! `schtasks_live_roundtrip` (`#[ignore]`, owner-run, own task name, documented cleanup).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::controller::{AutostartView, ControlError, ControlResult};

/// Reads and changes the logon autostart.
pub trait Autostart: Send + Sync + std::fmt::Debug + 'static {
    /// Whether autostart is on, with a caveat to show under the switch if there is one.
    fn status(&self) -> AutostartView;

    /// Turns autostart on or off.
    fn set_enabled(&self, enabled: bool) -> ControlResult<()>;
}

/// The task definition (`installer/autostart-task.xml`), with `@USER_SID@`, `@EXE_PATH@` and
/// `@WORKING_DIR@` placeholders.
pub const TASK_TEMPLATE: &str = include_str!("../../../../installer/autostart-task.xml");

/// Task Scheduler path of the task: folder `\Gander`, name `GanderHost`. `schtasks /TN` takes it
/// without the leading backslash.
pub const TASK_NAME: &str = "Gander\\GanderHost";

/// Why Task Scheduler could not be read or changed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SchedulerError(pub String);

impl SchedulerError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The three operations the autostart needs from Task Scheduler. The real one runs `schtasks`;
/// tests use a fake.
pub trait TaskScheduler: Send + Sync + std::fmt::Debug + 'static {
    /// The registered task's definition (XML), or `None` when it does not exist.
    fn query(&self) -> Result<Option<String>, SchedulerError>;

    /// Creates the task from `task_xml`, replacing an existing one of the same name.
    fn register(&self, task_xml: &str) -> Result<(), SchedulerError>;

    /// Deletes the task. Succeeds when it does not exist.
    fn unregister(&self) -> Result<(), SchedulerError>;
}

/// What the stored task definition says, as far as the switch cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFacts {
    /// `<Settings><Enabled>` is not false.
    pub enabled: bool,
    /// The `<Command>` path, quotes removed.
    pub command: Option<String>,
    /// `<RunLevel>`, if present.
    pub run_level: Option<String>,
    /// A `LogonTrigger` exists.
    pub has_logon_trigger: bool,
}

/// The text of the first `<tag>...</tag>` (or `<tag ...>...</tag>`) in `xml`, trimmed.
fn tag_text<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut from = 0;
    while let Some(pos) = xml[from..].find(&open) {
        let start = from + pos + open.len();
        let rest = &xml[start..];
        // `<Enabled` must not match `<EnabledFoo`: the next char ends the name.
        if rest.starts_with('>') || rest.starts_with(char::is_whitespace) {
            let body_start = start + rest.find('>')? + 1;
            let body_end = body_start + xml[body_start..].find(&close)?;
            return Some(xml[body_start..body_end].trim());
        }
        from = start;
    }
    None
}

/// Reads the facts the switch needs out of a task definition as `schtasks /Query /XML` prints it.
pub fn inspect_task_xml(xml: &str) -> TaskFacts {
    let settings = tag_text(xml, "Settings").unwrap_or("");
    TaskFacts {
        enabled: !tag_text(settings, "Enabled").is_some_and(|v| v.eq_ignore_ascii_case("false")),
        command: tag_text(xml, "Command")
            .map(|c| unescape(c.trim().trim_matches('"')).into_owned())
            .filter(|c| !c.is_empty()),
        run_level: tag_text(xml, "RunLevel").map(str::to_owned),
        has_logon_trigger: xml.contains("<LogonTrigger"),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn unescape(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('&') {
        return text.into();
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
        .into()
}

/// Fills the template for `user_sid` and `exe`.
///
/// The leading explanatory comment is dropped, every placeholder must be filled, and the values
/// are XML-escaped. Refuses anything but a SID in the user slot (the task must stay scoped to one
/// user) and a relative executable path.
pub fn render_task_xml(template: &str, user_sid: &str, exe: &Path) -> Result<String, String> {
    if !user_sid.starts_with("S-1-")
        || !user_sid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(format!("not a user SID: {user_sid:?}"));
    }
    if !exe.is_absolute() {
        return Err(format!(
            "the executable path is not absolute: {}",
            exe.display()
        ));
    }
    let dir = exe
        .parent()
        .ok_or_else(|| format!("the executable has no folder: {}", exe.display()))?;
    let body = match (template.find("<!--"), template.find("-->")) {
        (Some(a), Some(b)) if a < b => format!("{}{}", &template[..a], &template[b + 3..]),
        _ => template.to_owned(),
    };
    let xml = body
        .replace("@USER_SID@", &escape(user_sid))
        .replace("@EXE_PATH@", &escape(&exe.display().to_string()))
        .replace("@WORKING_DIR@", &escape(&dir.display().to_string()));
    if let Some(left) = unfilled_placeholder(&xml) {
        return Err(format!(
            "the task template has an unfilled placeholder {left}"
        ));
    }
    Ok(xml.trim_start().to_owned())
}

/// The first `@NAME@` placeholder still in `xml`, if any.
fn unfilled_placeholder(xml: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(pos) = xml[from..].find('@') {
        let start = from + pos;
        let rest = &xml[start + 1..];
        let len = rest
            .find(|c: char| !(c.is_ascii_uppercase() || c == '_'))
            .unwrap_or(rest.len());
        if len > 0 && rest[len..].starts_with('@') {
            return Some(&xml[start..start + len + 2]);
        }
        from = start + 1;
    }
    None
}

/// Compares two Windows paths the way the file system does (separators and ASCII case).
fn same_path(a: &str, b: &str) -> bool {
    let norm = |p: &str| {
        p.replace('/', "\\")
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

/// The Task Scheduler logon task behind the Settings switch.
#[derive(Debug)]
pub struct TaskSchedulerAutostart {
    scheduler: Arc<dyn TaskScheduler>,
    exe: PathBuf,
    user_sid: String,
}

impl TaskSchedulerAutostart {
    /// An autostart that starts `exe` for the user with SID `user_sid` through `scheduler`.
    pub fn new(
        scheduler: Arc<dyn TaskScheduler>,
        exe: impl Into<PathBuf>,
        user_sid: impl Into<String>,
    ) -> Self {
        Self {
            scheduler,
            exe: exe.into(),
            user_sid: user_sid.into(),
        }
    }

    /// The real thing: `schtasks.exe` for the current user and the running executable.
    #[cfg(windows)]
    pub fn for_current_exe() -> Result<Self, SchedulerError> {
        let exe = std::env::current_exe()
            .map_err(|e| SchedulerError::new(format!("cannot find gander-host.exe: {e}")))?;
        let sid = gander_win::config_store::current_user_sid_string()
            .map_err(|e| SchedulerError::new(format!("cannot read the user's SID: {e}")))?;
        Ok(Self::new(Arc::new(SchtasksScheduler::new()), exe, sid))
    }

    fn describe(&self, facts: &TaskFacts) -> AutostartView {
        let mut notes = Vec::new();
        if facts
            .run_level
            .as_deref()
            .is_some_and(|r| r != "LeastPrivilege")
        {
            notes.push(
                "The sign-in task was changed to run with elevated rights. Turn the switch off \
                 and on to restore the safe setting."
                    .to_owned(),
            );
        }
        if !facts.has_logon_trigger {
            notes.push("The sign-in task has no sign-in trigger.".to_owned());
        }
        if let Some(command) = &facts.command {
            let comparable = command.is_ascii() && self.exe.display().to_string().is_ascii();
            if comparable && !same_path(command, &self.exe.display().to_string()) {
                notes.push(format!(
                    "The sign-in task starts {command}, not this copy. Turn the switch off and on \
                     to point it here."
                ));
            }
        }
        AutostartView {
            enabled: facts.enabled && facts.has_logon_trigger,
            note: (!notes.is_empty()).then(|| notes.join(" ")),
        }
    }
}

impl Autostart for TaskSchedulerAutostart {
    fn status(&self) -> AutostartView {
        match self.scheduler.query() {
            Ok(None) => AutostartView {
                enabled: false,
                note: None,
            },
            Ok(Some(xml)) => self.describe(&inspect_task_xml(&xml)),
            Err(e) => AutostartView {
                enabled: false,
                note: Some(format!("Could not read Task Scheduler: {e}")),
            },
        }
    }

    fn set_enabled(&self, enabled: bool) -> ControlResult<()> {
        let result = if enabled {
            render_task_xml(TASK_TEMPLATE, &self.user_sid, &self.exe)
                .map_err(SchedulerError)
                .and_then(|xml| self.scheduler.register(&xml))
        } else {
            self.scheduler.unregister()
        };
        result.map_err(|e| {
            let what = if enabled {
                "turn on starting at sign-in"
            } else {
                "turn off starting at sign-in"
            };
            ControlError::new(format!("Could not {what}: {e}"))
        })
    }
}

/// An autostart for a computer where the real one cannot be built (no SID, no executable path).
/// It says why under the switch and refuses changes.
#[derive(Debug)]
pub struct UnavailableAutostart(pub String);

impl Autostart for UnavailableAutostart {
    fn status(&self) -> AutostartView {
        AutostartView {
            enabled: false,
            note: Some(format!("Starting at sign-in is unavailable: {}", self.0)),
        }
    }

    fn set_enabled(&self, _enabled: bool) -> ControlResult<()> {
        Err(ControlError::new(format!(
            "Starting at sign-in is unavailable: {}",
            self.0
        )))
    }
}

/// The autostart the application uses: Task Scheduler on Windows.
#[cfg(windows)]
pub fn system_autostart() -> Arc<dyn Autostart> {
    match TaskSchedulerAutostart::for_current_exe() {
        Ok(a) => Arc::new(a),
        Err(e) => Arc::new(UnavailableAutostart(e.to_string())),
    }
}

/// The autostart the application uses on a computer without Task Scheduler: none.
#[cfg(not(windows))]
pub fn system_autostart() -> Arc<dyn Autostart> {
    Arc::new(UnavailableAutostart(
        "this computer has no Task Scheduler".to_owned(),
    ))
}

/// Task Scheduler through `schtasks.exe`, as the current user (no elevation).
#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct SchtasksScheduler {
    schtasks: PathBuf,
    task_name: String,
}

#[cfg(windows)]
impl SchtasksScheduler {
    /// The Gander task, `System32\schtasks.exe` (never found through `PATH`).
    pub fn new() -> Self {
        Self::with_task_name(TASK_NAME)
    }

    /// A different task name (the live test uses its own so it never touches the real task).
    pub fn with_task_name(name: impl Into<String>) -> Self {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        Self {
            schtasks: PathBuf::from(root).join("System32").join("schtasks.exe"),
            task_name: name.into(),
        }
    }

    fn run(&self, args: &[&std::ffi::OsStr]) -> Result<std::process::Output, SchedulerError> {
        use std::os::windows::process::CommandExt as _;
        /// Do not flash a console window when the host has none.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new(&self.schtasks)
            .args(args)
            .stdin(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| SchedulerError::new(format!("cannot run schtasks.exe: {e}")))
    }

    fn failure(what: &str, out: &std::process::Output) -> SchedulerError {
        let text = String::from_utf8_lossy(&out.stderr);
        let text = text.trim();
        let text = if text.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        } else {
            text.to_owned()
        };
        SchedulerError::new(format!("schtasks {what} failed ({}): {text}", out.status))
    }
}

#[cfg(windows)]
impl Default for SchtasksScheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
impl TaskScheduler for SchtasksScheduler {
    fn query(&self) -> Result<Option<String>, SchedulerError> {
        use std::ffi::OsStr;
        let out = self.run(&[
            OsStr::new("/Query"),
            OsStr::new("/TN"),
            OsStr::new(&self.task_name),
            OsStr::new("/XML"),
        ])?;
        // schtasks exits 1 for a missing task and for refused access alike, and its message is
        // localised, so a non-zero exit reads as "no such task". Turning the switch on then shows
        // the real error if there is one.
        if !out.status.success() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
    }

    fn register(&self, task_xml: &str) -> Result<(), SchedulerError> {
        use std::ffi::OsStr;
        use std::io::Write as _;
        // schtasks reads the definition from a UTF-16 (little endian, with a byte-order mark)
        // file; the template declares encoding="UTF-16".
        let mut bytes = vec![0xFF, 0xFE];
        for unit in task_xml.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "gander-autostart-{}-{nanos}.xml",
            std::process::id()
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| SchedulerError::new(format!("cannot write the task file: {e}")))?;
        let written = file.write_all(&bytes).and_then(|()| file.flush());
        drop(file);
        let result = written
            .map_err(|e| SchedulerError::new(format!("cannot write the task file: {e}")))
            .and_then(|()| {
                self.run(&[
                    OsStr::new("/Create"),
                    OsStr::new("/TN"),
                    OsStr::new(&self.task_name),
                    OsStr::new("/XML"),
                    path.as_os_str(),
                    OsStr::new("/F"),
                ])
            });
        let _ = std::fs::remove_file(&path);
        let out = result?;
        if out.status.success() {
            Ok(())
        } else {
            Err(Self::failure("/Create", &out))
        }
    }

    fn unregister(&self) -> Result<(), SchedulerError> {
        use std::ffi::OsStr;
        let out = self.run(&[
            OsStr::new("/Delete"),
            OsStr::new("/TN"),
            OsStr::new(&self.task_name),
            OsStr::new("/F"),
        ])?;
        if out.status.success() {
            return Ok(());
        }
        // Already gone is success; anything else is the delete failing.
        if self.query()?.is_none() {
            return Ok(());
        }
        Err(Self::failure("/Delete", &out))
    }
}

/// An in-memory switch. Does NOT register anything with Windows. A test double, kept for the
/// tests that need an [`Autostart`] and for the UI smoke run.
#[derive(Debug, Default)]
pub struct StubAutostart {
    enabled: AtomicBool,
}

impl StubAutostart {
    /// A switch that is off.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Autostart for StubAutostart {
    fn status(&self) -> AutostartView {
        AutostartView {
            enabled: self.enabled.load(Ordering::SeqCst),
            note: Some(
                "Test switch, not connected to Windows. This switch is not saved.".to_owned(),
            ),
        }
    }

    fn set_enabled(&self, enabled: bool) -> ControlResult<()> {
        self.enabled.store(enabled, Ordering::SeqCst);
        Ok(())
    }
}

/// A switch whose changes fail, for tests of the error path.
#[derive(Debug, Default)]
pub struct FailingAutostart;

impl Autostart for FailingAutostart {
    fn status(&self) -> AutostartView {
        AutostartView {
            enabled: false,
            note: None,
        }
    }

    fn set_enabled(&self, _enabled: bool) -> ControlResult<()> {
        Err(ControlError::new("Task Scheduler refused the change"))
    }
}

/// An in-memory [`TaskScheduler`] for tests: remembers the registered XML and counts calls.
#[derive(Debug, Default)]
pub struct FakeScheduler {
    state: Mutex<FakeState>,
}

#[derive(Debug, Default)]
struct FakeState {
    task: Option<String>,
    registers: u32,
    unregisters: u32,
    fail_with: Option<String>,
}

impl FakeScheduler {
    /// An empty scheduler.
    pub fn new() -> Self {
        Self::default()
    }

    /// The registered task definition, if any.
    pub fn task(&self) -> Option<String> {
        self.state.lock().unwrap().task.clone()
    }

    /// Puts a definition in place as if someone had registered it by hand.
    pub fn preload(&self, xml: &str) {
        self.state.lock().unwrap().task = Some(xml.to_owned());
    }

    /// Makes every following call fail with `message`.
    pub fn fail_with(&self, message: &str) {
        self.state.lock().unwrap().fail_with = Some(message.to_owned());
    }

    /// How many times `register` and `unregister` were called.
    pub fn calls(&self) -> (u32, u32) {
        let s = self.state.lock().unwrap();
        (s.registers, s.unregisters)
    }
}

impl TaskScheduler for FakeScheduler {
    fn query(&self) -> Result<Option<String>, SchedulerError> {
        let s = self.state.lock().unwrap();
        match &s.fail_with {
            Some(m) => Err(SchedulerError::new(m.clone())),
            None => Ok(s.task.clone()),
        }
    }

    fn register(&self, task_xml: &str) -> Result<(), SchedulerError> {
        let mut s = self.state.lock().unwrap();
        s.registers += 1;
        if let Some(m) = &s.fail_with {
            return Err(SchedulerError::new(m.clone()));
        }
        s.task = Some(task_xml.to_owned());
        Ok(())
    }

    fn unregister(&self) -> Result<(), SchedulerError> {
        let mut s = self.state.lock().unwrap();
        s.unregisters += 1;
        if let Some(m) = &s.fail_with {
            return Err(SchedulerError::new(m.clone()));
        }
        s.task = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";

    fn exe() -> PathBuf {
        PathBuf::from(r"C:\Users\Owner\AppData\Local\Programs\Gander\gander-host.exe")
    }

    fn autostart(fake: &Arc<FakeScheduler>) -> TaskSchedulerAutostart {
        TaskSchedulerAutostart::new(fake.clone(), exe(), SID)
    }

    #[test]
    fn the_stub_toggles_in_memory_and_admits_it() {
        let a = StubAutostart::new();
        assert!(!a.status().enabled);
        a.set_enabled(true).unwrap();
        let s = a.status();
        assert!(s.enabled);
        assert!(s.note.unwrap().contains("not saved"));
        a.set_enabled(false).unwrap();
        assert!(!a.status().enabled);
    }

    #[test]
    fn a_failing_switch_reports_why() {
        let err = FailingAutostart.set_enabled(true).unwrap_err();
        assert!(err.to_string().contains("Task Scheduler"));
    }

    #[test]
    fn the_template_is_a_least_privilege_logon_task() {
        let xml = render_task_xml(TASK_TEMPLATE, SID, &exe()).unwrap();
        let facts = inspect_task_xml(&xml);
        assert_eq!(facts.run_level.as_deref(), Some("LeastPrivilege"));
        assert!(facts.has_logon_trigger);
        assert!(facts.enabled);
        assert_eq!(facts.command.as_deref(), Some(exe().to_str().unwrap()));
        assert!(!xml.contains("HighestAvailable"));
        assert!(!xml.contains('@'), "every placeholder is filled: {xml}");
        assert!(!xml.contains("<!--"), "the explanatory comment is dropped");
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert_eq!(xml.matches(SID).count(), 2, "trigger and principal");
        assert!(xml.contains(
            r"<WorkingDirectory>C:\Users\Owner\AppData\Local\Programs\Gander</WorkingDirectory>"
        ));
    }

    #[test]
    fn values_are_xml_escaped_and_bad_input_is_refused() {
        let tricky = PathBuf::from(r"C:\Tom & Jerry\<x>\gander-host.exe");
        let xml = render_task_xml(TASK_TEMPLATE, SID, &tricky).unwrap();
        assert!(xml.contains("Tom &amp; Jerry"));
        assert!(xml.contains("&lt;x&gt;"));
        // The facts read it back unescaped.
        assert_eq!(
            inspect_task_xml(&xml).command.as_deref(),
            Some(tricky.to_str().unwrap())
        );
        assert!(render_task_xml(TASK_TEMPLATE, "Owner", &exe()).is_err());
        assert!(render_task_xml(TASK_TEMPLATE, "S-1-5-1</UserId>", &exe()).is_err());
        assert!(render_task_xml(TASK_TEMPLATE, SID, Path::new("gander-host.exe")).is_err());
        assert!(render_task_xml("<Task>@EXE_PATH@ @NOPE@</Task>", SID, &exe()).is_err());
        assert!(
            render_task_xml("<Task>@USER_SID@ mail me@example.com</Task>", SID, &exe()).is_ok()
        );
    }

    #[test]
    fn turning_it_on_registers_the_task_and_off_removes_it() {
        let fake = Arc::new(FakeScheduler::new());
        let a = autostart(&fake);
        assert!(!a.status().enabled);
        assert!(a.status().note.is_none());

        a.set_enabled(true).unwrap();
        assert_eq!(fake.calls(), (1, 0));
        let stored = fake.task().unwrap();
        assert!(stored.contains("<LogonTrigger>") && stored.contains(SID));
        let status = a.status();
        assert!(status.enabled);
        assert!(status.note.is_none(), "{:?}", status.note);

        // Turning it on again replaces the task rather than failing.
        a.set_enabled(true).unwrap();
        assert_eq!(fake.calls(), (2, 0));

        a.set_enabled(false).unwrap();
        assert_eq!(fake.calls(), (2, 1));
        assert!(fake.task().is_none());
        assert!(!a.status().enabled);
        // Off when already off is fine.
        a.set_enabled(false).unwrap();
    }

    #[test]
    fn scheduler_errors_surface_in_the_switch() {
        let fake = Arc::new(FakeScheduler::new());
        fake.fail_with("Access is denied");
        let a = autostart(&fake);
        let err = a.set_enabled(true).unwrap_err().to_string();
        assert!(
            err.contains("turn on") && err.contains("Access is denied"),
            "{err}"
        );
        let err = a.set_enabled(false).unwrap_err().to_string();
        assert!(err.contains("turn off"), "{err}");
        let status = a.status();
        assert!(!status.enabled);
        assert!(status.note.unwrap().contains("Access is denied"));
    }

    #[test]
    fn a_task_edited_by_hand_is_reported() {
        let fake = Arc::new(FakeScheduler::new());
        let a = autostart(&fake);
        a.set_enabled(true).unwrap();
        let good = fake.task().unwrap();

        // Elevated run level.
        fake.preload(&good.replace("LeastPrivilege", "HighestAvailable"));
        let s = a.status();
        assert!(s.enabled);
        assert!(s.note.unwrap().contains("elevated"));

        // Disabled in Task Scheduler.
        fake.preload(&good.replace("<Enabled>true</Enabled>", "<Enabled>false</Enabled>"));
        assert!(!a.status().enabled);

        // Points at another copy of the program (case and separators do not matter).
        fake.preload(&good.replace("Programs\\Gander", "Dev\\gander\\target"));
        let s = a.status();
        assert!(s.enabled);
        assert!(s.note.unwrap().contains("not this copy"));
        fake.preload(&good.replace("C:\\Users", "c:/users"));
        assert!(a.status().note.is_none());

        // Turning it on restores the safe definition.
        a.set_enabled(true).unwrap();
        assert_eq!(fake.task().unwrap(), good);
    }

    #[test]
    fn inspecting_real_schtasks_output() {
        // `schtasks /Query /XML` returns the stored definition with its own formatting.
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n\
            <Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\r\n\
            <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>S-1-5-21-1</UserId></LogonTrigger></Triggers>\r\n\
            <Principals><Principal id=\"Author\"><UserId>S-1-5-21-1</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\r\n\
            <Settings><Enabled>true</Enabled><EnabledAfter>x</EnabledAfter></Settings>\r\n\
            <Actions Context=\"Author\"><Exec><Command>\"C:\\Gander\\gander-host.exe\"</Command></Exec></Actions></Task>";
        let facts = inspect_task_xml(xml);
        assert!(facts.enabled && facts.has_logon_trigger);
        assert_eq!(facts.command.as_deref(), Some(r"C:\Gander\gander-host.exe"));
        assert_eq!(facts.run_level.as_deref(), Some("LeastPrivilege"));
        // A task with no settings or trigger reads as on-but-triggerless.
        let bare = inspect_task_xml("<Task/>");
        assert!(bare.enabled && !bare.has_logon_trigger && bare.command.is_none());
    }

    #[test]
    fn an_unavailable_autostart_says_why_and_refuses() {
        let a = UnavailableAutostart("no SID".to_owned());
        assert!(a.status().note.unwrap().contains("no SID"));
        assert!(a.set_enabled(true).is_err());
    }

    /// Owner-run check against the REAL Task Scheduler, as the current user (no elevation).
    ///
    /// It registers a throwaway task named `Gander\GanderHostLiveTest` (never the real
    /// `Gander\GanderHost`), points it at this test executable, reads it back, and deletes it.
    /// It never starts the task. Run it with:
    ///
    /// ```text
    /// cargo test -p gander-host --lib schtasks_live_roundtrip -- --ignored --nocapture
    /// ```
    ///
    /// Cleanup if it is interrupted before the end, from any prompt:
    ///
    /// ```text
    /// schtasks /Delete /TN "Gander\GanderHostLiveTest" /F
    /// ```
    #[cfg(windows)]
    #[test]
    #[ignore = "registers a real (throwaway) scheduled task; owner-run"]
    fn schtasks_live_roundtrip() {
        struct Cleanup(SchtasksScheduler);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.unregister();
            }
        }

        let scheduler = SchtasksScheduler::with_task_name("Gander\\GanderHostLiveTest");
        let _cleanup = Cleanup(scheduler.clone());
        let sid = gander_win::config_store::current_user_sid_string().expect("user SID");
        let exe = std::env::current_exe().unwrap();
        let a = TaskSchedulerAutostart::new(Arc::new(scheduler.clone()), &exe, sid);

        scheduler.unregister().expect("clean slate");
        assert!(!a.status().enabled);
        a.set_enabled(true)
            .expect("register as the current user, unelevated");
        let status = a.status();
        println!("status after on: {status:?}");
        assert!(status.enabled && status.note.is_none(), "{status:?}");
        let xml = scheduler.query().unwrap().expect("task exists");
        let facts = inspect_task_xml(&xml);
        assert_eq!(facts.run_level.as_deref(), Some("LeastPrivilege"));
        assert!(facts.has_logon_trigger);
        a.set_enabled(false).expect("remove");
        assert!(scheduler.query().unwrap().is_none());
        a.set_enabled(false).expect("removing twice is fine");
    }
}
