// SPDX-License-Identifier: Apache-2.0
//! Network trust logic without OS calls (WP-12).
//!
//! - [`plan`]: the listener plan (which addresses to bind) from the trust adapter; never a
//!   wildcard address.
//! - [`tailscale`]: WhoIs / status / prefs semantics, the [`tailscale::TailscaleLocal`] backend
//!   trait, fallback, the 60 s WhoIs cache and a fake for tests.
//! - [`localapi`]: HTTP/1.1 framing for the Tailscale LocalAPI.
//! - [`firewall`]: firewall rule parsing and the self-check's Block-rule matching.
//!
//! The platform side (interface enumeration, address-change notifications, the LocalAPI pipe, the
//! CLI, the firewall rule store, DPAPI) lives in `gander_win::net`; the listener itself in
//! `gander_host::server`.

pub mod firewall;
pub mod localapi;
pub mod plan;
pub mod tailscale;
