// SPDX-License-Identifier: Apache-2.0
//! Windows networking for the listener and the trust adapters (WP-12).
//!
//! - [`interfaces`]: `GetAdaptersAddresses` snapshot and `NotifyUnicastIpAddressChange` watcher.
//! - [`socket`]: `SO_EXCLUSIVEADDRUSE` and the OS table of this process's listening sockets.
//! - [`tailscale`]: LocalAPI pipe and CLI backends for `gander_core::net::tailscale`.
//! - [`firewall`]: read-only firewall rule store access for the connectivity self-check.

pub mod firewall;
pub mod interfaces;
pub mod socket;
pub mod tailscale;
