// SPDX-License-Identifier: Apache-2.0
//! The windows' contents. Each page is a `State` (what the window remembers between frames) and a
//! `show(ui, state, cx)` that draws from the polled [`UiData`] and calls the
//! [`HostController`] for actions. Decisions live in plain modules (`share_editor`, `toast`,
//! `format`), so a page only lays things out.

pub mod activity;
pub mod add_device;
pub mod devices;
pub mod network;
pub mod settings;
pub mod shares;

use std::sync::Arc;

use super::controller::{FolderPicker, HostController};
use super::model::UiData;

/// What a page needs to draw and act.
pub struct Cx<'a> {
    /// The latest poll.
    pub data: &'a UiData,
    /// The host.
    pub ctrl: &'a dyn HostController,
    /// The folder dialog.
    pub picker: &'a Arc<dyn FolderPicker>,
    /// What the page asks the application to do.
    pub out: &'a mut Outbox,
}

/// Requests from a page to the application (windows are opened and closed by the application).
#[derive(Debug, Default)]
pub struct Outbox {
    /// Open the Add device window.
    pub open_add_device: bool,
    /// Close the Add device window (the pairing window was closed from inside it).
    pub close_add_device: bool,
    /// Ask the controller for a fresh network check.
    pub refresh_network: bool,
    /// The theme choice changed: apply it now.
    pub theme: Option<crate::ui::theme::ThemeChoice>,
}

impl std::fmt::Debug for Cx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cx").finish_non_exhaustive()
    }
}
