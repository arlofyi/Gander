// SPDX-License-Identifier: Apache-2.0
//! Windows integration for the Gander host.
//!
//! Window capture (WGC), Media Foundation encoding, `SendInput`, WinRT PDF rendering, shell
//! thumbnails, handle-relative path opens and the Tailscale LocalAPI pipe live here. Platform
//! specific code is gated with `#[cfg(windows)]` and must be built and tested on Windows in CI.

#[cfg(windows)]
pub mod assoc;
#[cfg(windows)]
pub mod clipboard;
#[cfg(windows)]
mod com;
#[cfg(windows)]
pub mod config_store;
pub mod dpapi;
#[cfg(windows)]
pub mod input;
#[cfg(windows)]
pub mod launch;
#[cfg(windows)]
pub mod net;
#[cfg(windows)]
pub mod path;
pub mod preview;
pub mod render;
#[cfg(windows)]
pub mod search;
#[cfg(windows)]
pub mod session;
#[cfg(windows)]
pub mod sysinfo;
pub mod thumbs;
#[cfg(windows)]
pub mod watch;
