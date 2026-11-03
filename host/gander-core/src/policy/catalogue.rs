// SPDX-License-Identifier: Apache-2.0
//! Owner approvals that change `apps.toml` (spec step 3, prompt answer "Always", and step 4,
//! "owner approves the extension once"). Loading and saving the file is
//! [`crate::config::ConfigFile::load`] / [`crate::config::ConfigFile::save`].

use super::assoc::HandlerInfo;
use super::{PolicyEnvironment, entry_matches, rules};
use crate::config::{AppCatalogue, AppEntry, CloseOnEnd, LaunchSpec, WindowMatch};
use crate::protocol::{AppId, Timestamp};

/// Why an approval was not recorded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ApprovalError {
    /// The handler is on the never list or a refused system binary; no approval can allow it.
    #[error("this app can never be approved for streaming")]
    HandlerNotApprovable,
    /// The extension is hard-refused (or empty / not lower-case letters and digits).
    #[error("this kind of file can never be streamed")]
    ExtensionNotApprovable,
    /// No catalogue entry with that id.
    #[error("no such app in the catalogue")]
    UnknownApp,
}

fn normalise_extension(ext: &str) -> Result<String, ApprovalError> {
    let ext = ext.strip_prefix('.').unwrap_or(ext).to_ascii_lowercase();
    let well_formed = !ext.is_empty()
        && ext
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if !well_formed || rules::is_hard_refused_extension(&ext) {
        return Err(ApprovalError::ExtensionNotApprovable);
    }
    Ok(ext)
}

/// Records "Always" for `handler` opening `ext`: approves the matching entry (a suggestion or an
/// approved app gaining an extension) or creates a new approved entry. Returns the entry id. The
/// caller saves the catalogue.
pub fn approve_handler(
    catalogue: &mut AppCatalogue,
    environment: &PolicyEnvironment,
    handler: &HandlerInfo,
    display_name: &str,
    ext: &str,
    now: Timestamp,
) -> Result<AppId, ApprovalError> {
    if rules::is_never_listed_name(&handler.exe_name())
        || rules::is_refused_system_binary(&handler.exe, &environment.system_root)
    {
        return Err(ApprovalError::HandlerNotApprovable);
    }
    let ext = normalise_extension(ext)?;
    let publisher = handler.signature.publisher().map(str::to_owned);

    if let Some(entry) = catalogue
        .apps
        .iter_mut()
        .find(|e| entry_matches(e, handler))
    {
        if entry.approved_by_owner.is_none() {
            entry.approved_by_owner = Some(now);
        }
        // The publisher recorded is the one the owner saw in this prompt.
        entry.publisher = publisher;
        if !entry.handles_extension(&ext) {
            entry.extensions.push(ext);
        }
        return Ok(entry.id.clone());
    }

    let id = unique_id(catalogue, &handler.exe_name());
    catalogue.apps.push(AppEntry {
        id: id.clone(),
        name: display_name.to_owned(),
        exe: handler.exe.clone(),
        publisher,
        extensions: vec![ext],
        approved_by_owner: Some(now),
        launch: LaunchSpec::Default,
        window_match: WindowMatch::default(),
        close_on_end: CloseOnEnd::default(),
        slow_start: false,
        package_family_name: handler.package_family_name.clone(),
    });
    Ok(id)
}

/// Adds `ext` to the approved entry `app` (spec step 4: approve the extension once).
pub fn approve_extension(
    catalogue: &mut AppCatalogue,
    app: &AppId,
    ext: &str,
) -> Result<(), ApprovalError> {
    let ext = normalise_extension(ext)?;
    let entry = catalogue
        .apps
        .iter_mut()
        .find(|e| &e.id == app)
        .ok_or(ApprovalError::UnknownApp)?;
    if !entry.handles_extension(&ext) {
        entry.extensions.push(ext);
    }
    Ok(())
}

/// `POWERPNT.EXE` -> `powerpnt`, `powerpnt-2`, ...
fn unique_id(catalogue: &AppCatalogue, exe_name: &str) -> AppId {
    let stem = exe_name
        .rsplit_once('.')
        .map_or(exe_name, |(stem, _)| stem)
        .to_ascii_lowercase();
    let base: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let base = if base.is_empty() {
        "app".to_owned()
    } else {
        base
    };
    let taken = |id: &str| catalogue.apps.iter().any(|a| a.id.as_str() == id);
    if !taken(&base) {
        return AppId::from(base);
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{base}-{n}");
        if !taken(&candidate) {
            return AppId::from(candidate);
        }
        n += 1;
    }
}
