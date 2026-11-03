// SPDX-License-Identifier: Apache-2.0
//! `apps.toml`: the Stream app catalogue (stream-launch-policy spec, step 3).
//!
//! An entry without `approved_by_owner` is a suggestion: it carries the right launch and window
//! strategy but streams nothing until the owner approves it on first use.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{ConfigError, ConfigFile, default_version, find_duplicate};
use crate::protocol::{AppId, Timestamp};

const FILE: &str = "apps.toml";

/// Contents of `apps.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppCatalogue {
    /// Schema version, always 1.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Catalogue entries.
    #[serde(default, rename = "app", skip_serializing_if = "Vec::is_empty")]
    pub apps: Vec<AppEntry>,
}

impl Default for AppCatalogue {
    fn default() -> Self {
        Self {
            version: super::CONFIG_VERSION,
            apps: Vec::new(),
        }
    }
}

/// `[[app]]`: one desktop application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppEntry {
    /// Catalogue id, for example `powerpoint`.
    pub id: AppId,
    /// Display name.
    pub name: String,
    /// Canonical path of the handler executable.
    pub exe: PathBuf,
    /// Authenticode publisher at approval time; `None` if unsigned or not yet read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    /// Lower-case extensions without the dot that this app may open.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// When the owner approved the app; `None` means suggested, not approved.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub approved_by_owner: Option<Timestamp>,
    /// How to launch.
    #[serde(default)]
    pub launch: LaunchSpec,
    /// How to find the window after launch.
    #[serde(default)]
    pub window_match: WindowMatch,
    /// What to do with the app when the session ends.
    #[serde(default)]
    pub close_on_end: CloseOnEnd,
    /// Allow 30 s instead of 15 s for the window to appear.
    #[serde(default)]
    pub slow_start: bool,
    /// Package family name for packaged (MSIX) apps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_family_name: Option<String>,
}

impl AppEntry {
    /// Approved by the owner.
    pub fn is_approved(&self) -> bool {
        self.approved_by_owner.is_some()
    }

    /// Whether `ext` (any case, with or without a leading dot) is in `extensions`.
    pub fn handles_extension(&self, ext: &str) -> bool {
        let ext = ext.strip_prefix('.').unwrap_or(ext);
        self.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext))
    }
}

/// `launch`: `"default"` (the handler's own command string with the file as one quoted argument)
/// or a template such as `"{exe} --new-window {file}"`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum LaunchSpec {
    /// The handler's registered command.
    #[default]
    Default,
    /// A template; `{exe}` and `{file}` are substituted, `{file}` is mandatory.
    Template(String),
}

impl LaunchSpec {
    /// Placeholders a template may contain.
    pub const PLACEHOLDERS: [&'static str; 2] = ["{exe}", "{file}"];
}

impl fmt::Display for LaunchSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchSpec::Default => f.write_str("default"),
            LaunchSpec::Template(t) => f.write_str(t),
        }
    }
}

impl Serialize for LaunchSpec {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for LaunchSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(if s == "default" {
            LaunchSpec::Default
        } else {
            LaunchSpec::Template(s)
        })
    }
}

/// Window discovery strategy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowMatch {
    /// A new visible window whose title contains the file name (single-instance apps).
    #[default]
    TitleContainsFilename,
    /// A new visible window of the launched process tree.
    NewWindowOfProcess,
    /// Always let the user pick.
    Pick,
}

/// What happens to the app when the session ends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseOnEnd {
    /// Ask on the phone.
    #[default]
    Ask,
    /// Leave it open.
    Never,
    /// Close only if Gander started it and it has no other windows (`WM_CLOSE`).
    IfLaunchedByUs,
}

impl ConfigFile for AppCatalogue {
    const FILE_NAME: &'static str = FILE;

    fn version(&self) -> u32 {
        self.version
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if let Some(dup) = find_duplicate(self.apps.iter().map(|a| &a.id)) {
            return Err(ConfigError::invalid(
                FILE,
                format!("duplicate app id {dup}"),
            ));
        }
        for app in &self.apps {
            if app.id.as_str().is_empty() {
                return Err(ConfigError::invalid(FILE, "app id must not be empty"));
            }
            if app.exe.as_os_str().is_empty() {
                return Err(ConfigError::invalid(
                    FILE,
                    format!("app {} has no exe", app.id),
                ));
            }
            for ext in &app.extensions {
                let ok = !ext.is_empty()
                    && ext
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
                if !ok {
                    return Err(ConfigError::invalid(
                        FILE,
                        format!(
                            "app {}: extension {ext:?} must be lower-case letters and digits without a dot",
                            app.id
                        ),
                    ));
                }
            }
            if let LaunchSpec::Template(t) = &app.launch {
                if !t.contains("{file}") {
                    return Err(ConfigError::invalid(
                        FILE,
                        format!("app {}: launch template must contain {{file}}", app.id),
                    ));
                }
                let mut rest = t.as_str();
                while let Some(start) = rest.find('{') {
                    let tail = &rest[start..];
                    let Some(p) = LaunchSpec::PLACEHOLDERS
                        .iter()
                        .find(|p| tail.starts_with(**p))
                    else {
                        return Err(ConfigError::invalid(
                            FILE,
                            format!("app {}: launch template has an unknown placeholder", app.id),
                        ));
                    };
                    rest = &tail[p.len()..];
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example from `docs/specs/2026-11-01-stream-launch-policy.md`, step 3, verbatim.
    const SPEC_EXAMPLE: &str = r#"
[[app]]
id = "powerpoint"
name = "Microsoft PowerPoint"
exe = "C:\\Program Files\\Microsoft Office\\root\\Office16\\POWERPNT.EXE"
publisher = "Microsoft Corporation"       # from the Authenticode signature at approval time
extensions = ["pptx", "ppsx", "potx", "odp"]
approved_by_owner = "2026-11-02T20:11:03Z"
launch = "default"                        # or a template: "{exe} --new-window {file}"
window_match = "title_contains_filename"  # or "new_window_of_process" | "pick"
close_on_end = "ask"                      # ask | never | if_launched_by_us
"#;

    #[test]
    fn spec_example_parses_and_roundtrips() {
        let cat = AppCatalogue::from_toml_str(SPEC_EXAMPLE).unwrap();
        let app = &cat.apps[0];
        assert!(app.is_approved());
        assert!(app.handles_extension(".PPTX"));
        assert!(!app.handles_extension("pptm"));
        assert_eq!(app.launch, LaunchSpec::Default);
        assert_eq!(app.window_match, WindowMatch::TitleContainsFilename);
        assert_eq!(app.close_on_end, CloseOnEnd::Ask);
        let back = AppCatalogue::from_toml_str(&cat.to_toml_string().unwrap()).unwrap();
        assert_eq!(back, cat);
    }

    #[test]
    fn templates_are_checked() {
        let mut cat = AppCatalogue::from_toml_str(SPEC_EXAMPLE).unwrap();
        cat.apps[0].launch = LaunchSpec::Template("{exe} --new-window {file}".into());
        assert!(cat.validate().is_ok());
        cat.apps[0].launch = LaunchSpec::Template("{exe} --new-window".into());
        assert!(cat.validate().is_err());
        cat.apps[0].launch = LaunchSpec::Template("{exe} {file} {cmd}".into());
        assert!(cat.validate().is_err());
    }

    #[test]
    fn extensions_are_normalised_form_only() {
        let mut cat = AppCatalogue::from_toml_str(SPEC_EXAMPLE).unwrap();
        cat.apps[0].extensions = vec![".pptx".into()];
        assert!(cat.validate().is_err());
        cat.apps[0].extensions = vec!["PPTX".into()];
        assert!(cat.validate().is_err());
    }
}
