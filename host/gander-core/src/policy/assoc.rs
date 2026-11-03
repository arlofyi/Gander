// SPDX-License-Identifier: Apache-2.0
//! Handler resolution (spec step 2) as a trait, so the policy runs against a fake association
//! table in tests and against `AssocQueryStringW` in production (`gander_win::assoc`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::rules::same_windows_path;

/// The Authenticode signature of a handler executable, read without executing it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Signature {
    /// A valid signature chaining to a trusted root; the leaf certificate's simple display name.
    Signed {
        /// Publisher, for example `Microsoft Corporation`.
        publisher: String,
    },
    /// No embedded signature and no catalogue signature.
    Unsigned,
    /// A signature is present but does not verify (tampered file, untrusted root, expired
    /// without timestamp, and so on).
    Untrusted,
    /// A packaged (MSIX/Store) app activated by `AppUserModelID` with no executable registered
    /// for the type. Windows verified the package signature at install time; the publisher is
    /// not read here.
    Package,
}

impl Signature {
    /// The publisher if the signature verified.
    pub fn publisher(&self) -> Option<&str> {
        match self {
            Signature::Signed { publisher } => Some(publisher),
            _ => None,
        }
    }

    /// What the host prompt shows next to the executable name.
    pub fn display(&self) -> &str {
        match self {
            Signature::Signed { publisher } => publisher,
            Signature::Unsigned => "unsigned",
            Signature::Untrusted => "signature not trusted",
            Signature::Package => "packaged app",
        }
    }
}

/// The resolved handler for an extension's `open` verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerInfo {
    /// Canonical path of the handler executable (`GetFinalPathNameByHandle`). For a packaged app
    /// without a registered executable: `shell:AppsFolder\<AppUserModelID>`.
    pub exe: PathBuf,
    /// The registered command template (`ASSOCSTR_COMMAND`), if any.
    pub command: Option<String>,
    /// The handler's signature.
    pub signature: Signature,
    /// `AppUserModelID` for packaged apps (`ASSOCSTR_APPID`).
    pub app_user_model_id: Option<String>,
    /// Package family name for packaged apps (the part of the AUMID before `!`).
    pub package_family_name: Option<String>,
}

impl HandlerInfo {
    /// A plain (unpackaged) handler.
    pub fn new(exe: impl Into<PathBuf>, signature: Signature) -> Self {
        Self {
            exe: exe.into(),
            command: None,
            signature,
            app_user_model_id: None,
            package_family_name: None,
        }
    }

    /// The executable's file name, for example `POWERPNT.EXE`.
    pub fn exe_name(&self) -> String {
        super::rules::windows_file_name(&self.exe)
    }
}

/// Why resolution failed. Never sent to a client (it may contain host paths).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AssocError(pub String);

/// Resolves handlers without executing anything.
pub trait AssociationResolver {
    /// The handler of `ext` (lower case, no dot). `Ok(None)` when nothing is registered, or when
    /// the "handler" is the file itself (a `"%1"` command).
    fn resolve(&self, ext: &str) -> Result<Option<HandlerInfo>, AssocError>;

    /// Canonicalises `exe` and reads its signature (used when a client names a catalogue app).
    fn inspect(&self, exe: &Path) -> Result<HandlerInfo, AssocError>;
}

/// A fixed association table: the fake used by tests and the policy vectors.
#[derive(Debug, Clone, Default)]
pub struct StaticAssociations {
    table: BTreeMap<String, HandlerInfo>,
    extra: Vec<HandlerInfo>,
}

impl StaticAssociations {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Associates `ext` (any case, optional dot) with `handler`.
    pub fn with(mut self, ext: &str, handler: HandlerInfo) -> Self {
        self.insert(ext, handler);
        self
    }

    /// Associates `ext` with `handler`.
    pub fn insert(&mut self, ext: &str, handler: HandlerInfo) {
        let ext = ext.strip_prefix('.').unwrap_or(ext).to_ascii_lowercase();
        self.table.insert(ext, handler);
    }

    /// Makes `handler` known to [`AssociationResolver::inspect`] without associating it.
    pub fn insert_executable(&mut self, handler: HandlerInfo) {
        self.extra.push(handler);
    }
}

impl AssociationResolver for StaticAssociations {
    fn resolve(&self, ext: &str) -> Result<Option<HandlerInfo>, AssocError> {
        Ok(self.table.get(&ext.to_ascii_lowercase()).cloned())
    }

    fn inspect(&self, exe: &Path) -> Result<HandlerInfo, AssocError> {
        self.table
            .values()
            .chain(self.extra.iter())
            .find(|h| same_windows_path(&h.exe, exe))
            .cloned()
            .ok_or_else(|| AssocError("executable not found".to_owned()))
    }
}
