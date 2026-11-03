// SPDX-License-Identifier: Apache-2.0
//! The editable copy of the shares behind the Shares window.
//!
//! The window edits drafts and nothing reaches the host until Save: a half-typed name or a
//! just-ticked Stream box never changes what a phone can see. Every rule lives here, not in the
//! drawing code, so it is tested without a window:
//!
//! - **Deny by default**: a new share has no grants; a device sees it only after the owner ticks
//!   at least one permission for it.
//! - Removing the last permission removes the grant, so `config.toml` never carries empty grants.
//! - Hidden patterns are edited as text (one glob per line) and stored trimmed without blanks.
//! - [`ShareEditor::validate`] runs the same rules `config.toml` is checked with on load.

use std::path::{Path, PathBuf};

use gander_core::config::{ConfigFile as _, HostConfig, ShareConfig, ShareGrant};
use gander_core::protocol::files::{Permission, StreamDecision};
use gander_core::protocol::{DeviceId, ShareId};

/// The permissions in the order the grid shows them.
pub const PERMISSIONS: [Permission; 4] = [
    Permission::List,
    Permission::Preview,
    Permission::Fetch,
    Permission::Stream,
];

/// The drafts and what is saved.
#[derive(Debug, Clone, Default)]
pub struct ShareEditor {
    saved: Vec<ShareConfig>,
    drafts: Vec<ShareConfig>,
    /// Text of each share's hidden-patterns box, parallel to `drafts`.
    pattern_text: Vec<String>,
}

impl ShareEditor {
    /// An editor over the saved shares.
    pub fn new(saved: Vec<ShareConfig>) -> Self {
        let mut e = Self::default();
        e.reset_to(saved);
        e
    }

    fn reset_to(&mut self, saved: Vec<ShareConfig>) {
        self.pattern_text = saved.iter().map(|s| s.hidden_patterns.join("\n")).collect();
        self.drafts = saved.clone();
        self.saved = saved;
    }

    /// Adopts a changed saved state (another window, a reload) unless the owner has unsaved edits.
    pub fn sync(&mut self, saved: Vec<ShareConfig>) {
        if !self.is_dirty() && saved != self.saved {
            self.reset_to(saved);
        }
    }

    /// Whether the drafts differ from what is saved.
    pub fn is_dirty(&self) -> bool {
        self.drafts != self.saved
    }

    /// The drafts, in display order.
    pub fn drafts(&self) -> &[ShareConfig] {
        &self.drafts
    }

    /// Mutable access to one draft (the window edits name and flags in place).
    pub fn draft_mut(&mut self, index: usize) -> Option<&mut ShareConfig> {
        self.drafts.get_mut(index)
    }

    /// The hidden-patterns text box of one share.
    pub fn pattern_text_mut(&mut self, index: usize) -> Option<&mut String> {
        self.pattern_text.get_mut(index)
    }

    /// Re-reads the patterns from the text box into the draft (call after the box changed).
    pub fn commit_patterns(&mut self, index: usize) {
        if let (Some(text), Some(draft)) =
            (self.pattern_text.get(index), self.drafts.get_mut(index))
        {
            draft.hidden_patterns = parse_patterns(text);
        }
    }

    /// Adds a share for `root` with a fresh id, named after the folder, with no grants. Returns
    /// its index.
    pub fn add_share(&mut self, root: PathBuf) -> usize {
        let id = ShareId::from(format!("shr_{}", ulid::Ulid::new()));
        self.add_share_with_id(id, root)
    }

    /// [`ShareEditor::add_share`] with a chosen id (tests).
    pub fn add_share_with_id(&mut self, id: ShareId, root: PathBuf) -> usize {
        self.drafts.push(ShareConfig {
            id,
            name: default_name(&root),
            root,
            show_hidden: false,
            show_path: false,
            hidden_patterns: Vec::new(),
            stream_default: StreamDecision::Ask,
            grants: Vec::new(),
        });
        self.pattern_text.push(String::new());
        self.drafts.len() - 1
    }

    /// Points a share at another folder. The name is kept.
    pub fn set_root(&mut self, index: usize, root: PathBuf) {
        if let Some(d) = self.drafts.get_mut(index) {
            d.root = root;
        }
    }

    /// Removes a share (its grants go with it).
    pub fn remove(&mut self, index: usize) {
        if index < self.drafts.len() {
            self.drafts.remove(index);
            self.pattern_text.remove(index);
        }
    }

    /// Whether `device` holds `permission` in the share.
    pub fn has(&self, index: usize, device: &DeviceId, permission: Permission) -> bool {
        self.drafts
            .get(index)
            .is_some_and(|s| s.allows(device, permission))
    }

    /// Grants or withdraws one permission. A grant with no permissions left is removed.
    pub fn set_permission(
        &mut self,
        index: usize,
        device: &DeviceId,
        permission: Permission,
        on: bool,
    ) {
        let Some(share) = self.drafts.get_mut(index) else {
            return;
        };
        let pos = share.grants.iter().position(|g| &g.device == device);
        match (pos, on) {
            (Some(p), true) => {
                let perms = &mut share.grants[p].permissions;
                if !perms.contains(&permission) {
                    perms.push(permission);
                    perms.sort();
                }
            }
            (Some(p), false) => {
                share.grants[p].permissions.retain(|x| *x != permission);
                if share.grants[p].permissions.is_empty() {
                    share.grants.remove(p);
                }
            }
            (None, true) => share.grants.push(ShareGrant {
                device: device.clone(),
                permissions: vec![permission],
            }),
            (None, false) => {}
        }
    }

    /// How many devices hold the Stream permission in the share.
    pub fn stream_grants(&self, index: usize) -> usize {
        self.drafts.get(index).map_or(0, |s| {
            s.grants
                .iter()
                .filter(|g| g.permissions.contains(&Permission::Stream))
                .count()
        })
    }

    /// Checks the drafts with the rules `config.toml` is loaded with.
    pub fn validate(&self) -> Result<(), String> {
        let candidate = HostConfig {
            shares: self.drafts.clone(),
            ..HostConfig::default()
        };
        candidate.validate().map_err(|e| match e {
            gander_core::config::ConfigError::Invalid { message, .. } => message,
            other => other.to_string(),
        })
    }

    /// The drafts to hand to `HostController::apply_shares`.
    pub fn to_save(&self) -> Vec<ShareConfig> {
        self.drafts.clone()
    }

    /// The host accepted the drafts.
    pub fn mark_saved(&mut self) {
        self.saved = self.drafts.clone();
    }

    /// Throws the edits away.
    pub fn revert(&mut self) {
        let saved = self.saved.clone();
        self.reset_to(saved);
    }
}

/// One glob per line, trimmed, blank lines dropped.
pub fn parse_patterns(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The folder's own name; for a drive root (`D:\`) the drive letter.
pub fn default_name(root: &Path) -> String {
    if let Some(name) = root.file_name() {
        return name.to_string_lossy().into_owned();
    }
    let text = root.to_string_lossy();
    let trimmed = text.trim_end_matches(['\\', '/', ':']);
    if trimmed.is_empty() {
        "Shared folder".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("D:\\Data\\{name}"))
        } else {
            PathBuf::from(format!("/srv/{name}"))
        }
    }

    fn phone() -> DeviceId {
        DeviceId::from("dev_phone")
    }

    fn tablet() -> DeviceId {
        DeviceId::from("dev_tablet")
    }

    #[test]
    fn a_new_share_denies_everything_until_a_permission_is_ticked() {
        let mut e = ShareEditor::new(Vec::new());
        assert!(!e.is_dirty());
        let i = e.add_share(root("Projects"));
        assert!(e.is_dirty());
        let share = &e.drafts()[i];
        assert_eq!(share.name, "Projects");
        assert!(share.id.as_str().starts_with("shr_"));
        assert!(share.grants.is_empty(), "deny by default");
        assert!(!share.show_hidden && !share.show_path);
        assert_eq!(share.stream_default, StreamDecision::Ask);
        for p in PERMISSIONS {
            assert!(!e.has(i, &phone(), p));
        }
        assert!(e.validate().is_ok());
    }

    #[test]
    fn permissions_are_per_device_and_empty_grants_disappear() {
        let mut e = ShareEditor::new(Vec::new());
        let i = e.add_share_with_id(ShareId::from("shr_a"), root("A"));
        e.set_permission(i, &phone(), Permission::Fetch, true);
        e.set_permission(i, &phone(), Permission::List, true);
        e.set_permission(i, &tablet(), Permission::List, true);
        assert!(e.has(i, &phone(), Permission::Fetch));
        assert!(!e.has(i, &tablet(), Permission::Fetch));
        // Kept sorted, no duplicates.
        e.set_permission(i, &phone(), Permission::List, true);
        assert_eq!(
            e.drafts()[i].permissions_for(&phone()),
            &[Permission::List, Permission::Fetch]
        );
        e.set_permission(i, &phone(), Permission::List, false);
        e.set_permission(i, &phone(), Permission::Fetch, false);
        assert_eq!(e.drafts()[i].grants.len(), 1, "phone's grant is gone");
        assert!(e.drafts()[i].permissions_for(&phone()).is_empty());
        // Withdrawing something never granted is a no-op.
        e.set_permission(i, &phone(), Permission::Stream, false);
        assert_eq!(e.drafts()[i].grants.len(), 1);
    }

    #[test]
    fn stream_grants_are_counted_for_the_warning() {
        let mut e = ShareEditor::new(Vec::new());
        let i = e.add_share(root("A"));
        assert_eq!(e.stream_grants(i), 0);
        e.set_permission(i, &phone(), Permission::Stream, true);
        e.set_permission(i, &tablet(), Permission::Fetch, true);
        assert_eq!(e.stream_grants(i), 1);
    }

    #[test]
    fn hidden_patterns_are_edited_as_lines() {
        assert_eq!(
            parse_patterns("**/.git\n  \n  node_modules  \r\n"),
            vec!["**/.git".to_owned(), "node_modules".to_owned()]
        );
        let mut e = ShareEditor::new(Vec::new());
        let i = e.add_share(root("A"));
        *e.pattern_text_mut(i).unwrap() = "**/.git\n\n*.tmp\n".to_owned();
        e.commit_patterns(i);
        assert_eq!(e.drafts()[i].hidden_patterns, vec!["**/.git", "*.tmp"]);
        // The text box keeps what the owner typed, blank lines included.
        assert_eq!(e.pattern_text_mut(i).unwrap(), "**/.git\n\n*.tmp\n");
    }

    #[test]
    fn dirty_tracking_revert_and_save() {
        let mut saved = ShareEditor::new(Vec::new());
        let i = saved.add_share_with_id(ShareId::from("shr_a"), root("A"));
        saved.mark_saved();
        assert!(!saved.is_dirty());
        let mut e = ShareEditor::new(saved.to_save());
        e.draft_mut(i).unwrap().name = "Renamed".into();
        assert!(e.is_dirty());
        e.revert();
        assert!(!e.is_dirty());
        assert_eq!(e.drafts()[i].name, "A");
        e.draft_mut(i).unwrap().show_hidden = true;
        e.mark_saved();
        assert!(!e.is_dirty());
    }

    #[test]
    fn sync_adopts_outside_changes_only_when_clean() {
        let mut other = ShareEditor::new(Vec::new());
        other.add_share_with_id(ShareId::from("shr_a"), root("A"));
        let changed = other.to_save();

        let mut clean = ShareEditor::new(Vec::new());
        clean.sync(changed.clone());
        assert_eq!(clean.drafts().len(), 1);

        let mut dirty = ShareEditor::new(Vec::new());
        dirty.add_share_with_id(ShareId::from("shr_b"), root("B"));
        dirty.sync(changed);
        assert_eq!(
            dirty.drafts()[0].id,
            ShareId::from("shr_b"),
            "edits are kept"
        );
    }

    #[test]
    fn validation_uses_the_config_rules() {
        let mut e = ShareEditor::new(Vec::new());
        let i = e.add_share(root("A"));
        e.draft_mut(i).unwrap().name = "   ".into();
        let err = e.validate().unwrap_err();
        assert!(err.contains("empty name"), "{err}");
        e.draft_mut(i).unwrap().name = "A".into();
        e.draft_mut(i).unwrap().root = PathBuf::from("relative/path");
        assert!(e.validate().unwrap_err().contains("absolute"));
        let j = e.add_share_with_id(e.drafts()[i].id.clone(), root("B"));
        e.draft_mut(i).unwrap().root = root("A");
        assert!(
            e.validate().unwrap_err().contains("duplicate"),
            "same id twice"
        );
        e.remove(j);
        assert!(e.validate().is_ok());
    }

    #[test]
    fn remove_keeps_the_pattern_boxes_in_step() {
        let mut e = ShareEditor::new(Vec::new());
        let a = e.add_share(root("A"));
        let b = e.add_share(root("B"));
        *e.pattern_text_mut(b).unwrap() = "keep-me".into();
        e.commit_patterns(b);
        e.remove(a);
        assert_eq!(e.drafts().len(), 1);
        assert_eq!(e.pattern_text_mut(0).unwrap(), "keep-me");
        assert_eq!(e.drafts()[0].hidden_patterns, vec!["keep-me"]);
    }

    #[test]
    fn default_names() {
        assert_eq!(default_name(&root("Projects")), "Projects");
        if cfg!(windows) {
            assert_eq!(default_name(Path::new("D:\\")), "D");
        }
    }
}
