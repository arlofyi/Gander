// SPDX-License-Identifier: Apache-2.0
//! The Stream launch policy (`docs/specs/2026-11-01-stream-launch-policy.md`, "Policy: who may
//! open what"; ADR-0006). Pure logic: the handler lookup is behind [`AssociationResolver`], which
//! tests fill with a fake table ([`StaticAssociations`]) and production implements with
//! `AssocQueryStringW` plus a WinVerifyTrust signature read (`gander_win::assoc`).
//!
//! Decision order for one file, for one device:
//!
//! 0. the device lacks the share's `stream` permission -> refused (`permission_denied`);
//! 1. no extension, or a hard-refused final extension ([`rules::HARD_REFUSED_EXTENSIONS`]) ->
//!    refused;
//! 2. resolve the handler without executing anything; none -> refused (`app_unavailable`); the
//!    handler on the never list or under `%SystemRoot%\System32` (except notepad/mspaint) ->
//!    refused;
//! 3. an approved catalogue entry for the handler that lists the extension -> allowed;
//! 4. an approved entry without the extension, or whose recorded publisher no longer matches the
//!    signature -> ask;
//! 5. an unknown (or only suggested) handler -> ask, or refused when the share's
//!    `stream_default` is `refused`;
//! 6. a browser handler keeps the outcome of 3-5 but is launched only through
//!    [`IsolatedBrowserLaunch`].
//!
//! `list` reports the outcome without prompting ([`ListingPolicy`]); `POST /stream` goes through
//! [`handle_stream_request`], which answers `stream_unavailable` in Phase 1 once the policy has
//! passed; with the Phase 2 lifecycle (WP-53) the host calls [`evaluate_stream_request`] and
//! starts a session instead.

mod assoc;
mod catalogue;
pub mod rules;
mod service;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

pub use assoc::{AssocError, AssociationResolver, HandlerInfo, Signature, StaticAssociations};
pub use catalogue::{ApprovalError, approve_extension, approve_handler};
pub use rules::{Browser, BrowserFamily, IsolatedBrowserLaunch};
pub use service::{
    ListingPolicy, evaluate_stream_request, handle_stream_request, phase1_stream_capabilities,
};

use crate::config::{AppCatalogue, AppEntry};
use crate::error::{ErrorBody, ErrorCode};
use crate::protocol::AppId;
use crate::protocol::files::StreamDecision;
use crate::protocol::stream::AppRef;

/// Host facts the rules need. Filled by the platform crate (`GetSystemWindowsDirectoryW`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyEnvironment {
    /// `%SystemRoot%`, for example `C:\Windows`.
    pub system_root: PathBuf,
}

impl Default for PolicyEnvironment {
    fn default() -> Self {
        Self {
            system_root: PathBuf::from("C:\\Windows"),
        }
    }
}

/// One policy question: may this device stream this file?
#[derive(Debug, Clone, Copy)]
pub struct PolicyInput<'a> {
    /// File name (last path segment) as it is on disk.
    pub file_name: &'a str,
    /// The device holds the share's `stream` permission.
    pub stream_permitted: bool,
    /// The share's `stream_default` (applies to unknown handlers).
    pub share_default: StreamDecision,
    /// A catalogue app the client asked for (`POST /stream` `app`); `None` lets the policy pick
    /// the file's registered handler.
    pub requested_app: Option<&'a AppId>,
}

/// Why the policy decided as it did. The wire name ([`DecisionReason::as_str`]) is used in the
/// policy test vectors and the audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecisionReason {
    /// Refused: the device lacks the share's `stream` permission.
    NoStreamPermission,
    /// Refused: the file has no extension.
    NoExtension,
    /// Refused: the final extension is on the step-1 list.
    HardRefusedExtension,
    /// Refused: nothing is registered to open this type (or the file would run itself).
    NoHandler,
    /// Refused: the association lookup or signature read failed (fail closed).
    HandlerLookupFailed,
    /// Refused: the handler is on the never list.
    NeverListedHandler,
    /// Refused: the handler lives under `%SystemRoot%\System32` and is not allowlisted.
    SystemHandler,
    /// Refused: the client named a catalogue app that does not exist.
    UnknownApp,
    /// Refused: the handler is unknown and the share's `stream_default` is `refused`.
    ShareDefaultRefused,
    /// Allowed: approved catalogue entry that lists the extension.
    CatalogueApproved,
    /// Ask: approved catalogue entry, but the extension is new for it.
    NewExtensionForApprovedApp,
    /// Ask: approved catalogue entry, but the signature publisher changed since approval.
    PublisherChanged,
    /// Ask: the handler is not (or only suggested) in the catalogue.
    UnknownHandler,
}

impl DecisionReason {
    /// The outcome this reason implies.
    pub const fn outcome(self) -> StreamDecision {
        match self {
            DecisionReason::CatalogueApproved => StreamDecision::Allowed,
            DecisionReason::NewExtensionForApprovedApp
            | DecisionReason::PublisherChanged
            | DecisionReason::UnknownHandler => StreamDecision::Ask,
            _ => StreamDecision::Refused,
        }
    }

    /// Snake-case name, for example `hard_refused_extension`.
    pub const fn as_str(self) -> &'static str {
        match self {
            DecisionReason::NoStreamPermission => "no_stream_permission",
            DecisionReason::NoExtension => "no_extension",
            DecisionReason::HardRefusedExtension => "hard_refused_extension",
            DecisionReason::NoHandler => "no_handler",
            DecisionReason::HandlerLookupFailed => "handler_lookup_failed",
            DecisionReason::NeverListedHandler => "never_listed_handler",
            DecisionReason::SystemHandler => "system_handler",
            DecisionReason::UnknownApp => "unknown_app",
            DecisionReason::ShareDefaultRefused => "share_default_refused",
            DecisionReason::CatalogueApproved => "catalogue_approved",
            DecisionReason::NewExtensionForApprovedApp => "new_extension_for_approved_app",
            DecisionReason::PublisherChanged => "publisher_changed",
            DecisionReason::UnknownHandler => "unknown_handler",
        }
    }

    /// The protocol error a refusal is sent as (`None` for allowed and ask). Messages are in user
    /// terms and contain no paths.
    pub fn error_body(self) -> Option<ErrorBody> {
        let (code, message) = match self {
            DecisionReason::NoStreamPermission => (
                ErrorCode::PermissionDenied,
                "This device does not have the stream permission for this share.",
            ),
            DecisionReason::NoExtension | DecisionReason::HardRefusedExtension => (
                ErrorCode::StreamRefused,
                "This kind of file cannot be streamed.",
            ),
            DecisionReason::NeverListedHandler | DecisionReason::SystemHandler => (
                ErrorCode::StreamRefused,
                "The app that opens this kind of file cannot be streamed.",
            ),
            DecisionReason::ShareDefaultRefused => (
                ErrorCode::StreamRefused,
                "Streaming with apps the owner has not approved is turned off for this share.",
            ),
            DecisionReason::NoHandler => (
                ErrorCode::AppUnavailable,
                "No app on this computer opens this kind of file.",
            ),
            DecisionReason::HandlerLookupFailed => (
                ErrorCode::AppUnavailable,
                "The app for this kind of file could not be checked.",
            ),
            DecisionReason::UnknownApp => (
                ErrorCode::AppUnavailable,
                "That app is not in this computer's app list.",
            ),
            DecisionReason::CatalogueApproved
            | DecisionReason::NewExtensionForApprovedApp
            | DecisionReason::PublisherChanged
            | DecisionReason::UnknownHandler => return None,
        };
        Some(ErrorBody::new(code, message))
    }
}

/// The policy's answer for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecision {
    /// `allowed | refused | ask`.
    pub outcome: StreamDecision,
    /// The decision path (audit log, refusal message).
    pub reason: DecisionReason,
    /// Final extension, lower case.
    pub extension: Option<String>,
    /// The resolved handler, when resolution got that far.
    pub handler: Option<HandlerInfo>,
    /// The catalogue entry involved, if any.
    pub app: Option<AppRef>,
    /// Set when the handler is a browser: the only way it may be launched.
    pub browser: Option<IsolatedBrowserLaunch>,
}

impl PolicyDecision {
    fn new(reason: DecisionReason, extension: Option<String>) -> Self {
        Self {
            outcome: reason.outcome(),
            reason,
            extension,
            handler: None,
            app: None,
            browser: None,
        }
    }

    /// The refusal as a protocol error body; `None` unless refused.
    pub fn error_body(&self) -> Option<ErrorBody> {
        self.reason.error_body()
    }
}

/// The policy engine: catalogue + association lookup + host facts.
#[derive(Clone, Copy)]
pub struct StreamPolicy<'a> {
    /// The app catalogue (`apps.toml`).
    pub catalogue: &'a AppCatalogue,
    /// Handler lookup.
    pub resolver: &'a dyn AssociationResolver,
    /// Host facts.
    pub environment: &'a PolicyEnvironment,
}

impl std::fmt::Debug for StreamPolicy<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamPolicy")
            .field("catalogue", &self.catalogue.apps.len())
            .field("environment", self.environment)
            .finish_non_exhaustive()
    }
}

impl<'a> StreamPolicy<'a> {
    /// Builds an engine.
    pub fn new(
        catalogue: &'a AppCatalogue,
        resolver: &'a dyn AssociationResolver,
        environment: &'a PolicyEnvironment,
    ) -> Self {
        Self {
            catalogue,
            resolver,
            environment,
        }
    }

    /// Runs steps 1-6 for one file. Never prompts and never executes anything.
    pub fn evaluate(&self, input: &PolicyInput<'_>) -> PolicyDecision {
        let extension = rules::final_extension(input.file_name);
        if !input.stream_permitted {
            return PolicyDecision::new(DecisionReason::NoStreamPermission, extension);
        }
        let Some(ext) = extension.clone() else {
            return PolicyDecision::new(DecisionReason::NoExtension, None);
        };
        if rules::is_hard_refused_extension(&ext) {
            return PolicyDecision::new(DecisionReason::HardRefusedExtension, extension);
        }

        // Step 2: the handler.
        let (handler, requested_entry) = match input.requested_app {
            Some(id) => {
                let Some(entry) = self.catalogue.apps.iter().find(|a| &a.id == id) else {
                    return PolicyDecision::new(DecisionReason::UnknownApp, extension);
                };
                match self.resolver.inspect(&entry.exe) {
                    Ok(h) => (h, Some(entry)),
                    Err(_) => {
                        return PolicyDecision::new(DecisionReason::HandlerLookupFailed, extension);
                    }
                }
            }
            None => match self.resolver.resolve(&ext) {
                Ok(Some(h)) => (h, None),
                Ok(None) => return PolicyDecision::new(DecisionReason::NoHandler, extension),
                Err(_) => {
                    return PolicyDecision::new(DecisionReason::HandlerLookupFailed, extension);
                }
            },
        };

        let mut decision = PolicyDecision::new(DecisionReason::UnknownHandler, extension);
        if rules::is_never_listed_name(&handler.exe_name()) {
            decision = PolicyDecision::new(DecisionReason::NeverListedHandler, decision.extension);
            decision.handler = Some(handler);
            return decision;
        }
        if rules::is_refused_system_binary(&handler.exe, &self.environment.system_root) {
            decision = PolicyDecision::new(DecisionReason::SystemHandler, decision.extension);
            decision.handler = Some(handler);
            return decision;
        }

        // Steps 3-5: the catalogue.
        let entry = match requested_entry {
            Some(entry) => Some(entry),
            None => best_entry(self.catalogue, &handler, &ext),
        };
        let reason = match entry {
            Some(entry) if entry.is_approved() => {
                let publisher_ok = match &entry.publisher {
                    Some(p) => handler.signature.publisher() == Some(p.as_str()),
                    None => true,
                };
                if !publisher_ok {
                    DecisionReason::PublisherChanged
                } else if entry.handles_extension(&ext) {
                    DecisionReason::CatalogueApproved
                } else {
                    DecisionReason::NewExtensionForApprovedApp
                }
            }
            _ => match input.share_default {
                StreamDecision::Refused => DecisionReason::ShareDefaultRefused,
                // An unapproved app is never streamed without the owner's prompt, so a share
                // default of `allowed` still asks.
                StreamDecision::Ask | StreamDecision::Allowed => DecisionReason::UnknownHandler,
            },
        };
        decision.reason = reason;
        decision.outcome = reason.outcome();
        decision.app = entry.map(|e| AppRef {
            id: e.id.clone(),
            name: e.name.clone(),
        });

        // Step 6: browsers only through an isolated window.
        if decision.outcome != StreamDecision::Refused {
            decision.browser = Browser::detect(&handler.exe_name(), handler.signature.publisher())
                .map(|browser| IsolatedBrowserLaunch { browser });
        }
        decision.handler = Some(handler);
        decision
    }
}

/// Whether a catalogue entry describes this handler: same package family for packaged apps,
/// otherwise the same executable path (case-insensitive, `\\?\` ignored).
pub fn entry_matches(entry: &AppEntry, handler: &HandlerInfo) -> bool {
    if let (Some(a), Some(b)) = (&entry.package_family_name, &handler.package_family_name) {
        return a.eq_ignore_ascii_case(b);
    }
    rules::same_windows_path(&entry.exe, &handler.exe)
}

/// Prefers an approved entry listing `ext`, then any approved entry, then a suggestion.
fn best_entry<'c>(
    catalogue: &'c AppCatalogue,
    handler: &HandlerInfo,
    ext: &str,
) -> Option<&'c AppEntry> {
    let matching: Vec<&AppEntry> = catalogue
        .apps
        .iter()
        .filter(|e| entry_matches(e, handler))
        .collect();
    matching
        .iter()
        .find(|e| e.is_approved() && e.handles_extension(ext))
        .or_else(|| matching.iter().find(|e| e.is_approved()))
        .or_else(|| matching.first())
        .copied()
}
