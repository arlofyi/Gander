// SPDX-License-Identifier: Apache-2.0
//! The Gander host library: everything the tray binary runs, as a library so integration tests
//! and later packages can drive it.
//!
//! - [`server`]: TLS listener, trust adapters, request gate and the route extension point (WP-12).
//! - [`pairing`]: pairing window and endpoints, device registry, token authenticator (WP-13).
//! - [`audit`]: the audit log (`audit.jsonl`, rotation, redaction) (WP-13).
//! - [`api`]: Browse, Fetch, thumbnails, previews, `/me`, and [`api::host_routes`], the route
//!   assembly every caller uses (WP-14; Windows only until the other hosts exist).
//! - [`events`]: the `/v1/events` WebSocket: host state, share changes, revocation (WP-17).
//! - [`stream`]: the stream engine glue: signalling over `/v1/events`, ICE addresses, the port
//!   range (WP-50).
//! - [`lifecycle`]: the Stream session lifecycle: `POST`/`DELETE /v1/stream`, prompts, the
//!   resume grace, `close_on_end`, keep-awake, the tray state and the audit entries (WP-53).
//! - [`diagnostics`]: where console output goes when the release build has no console (WP-1A).
//! - [`single_instance`]: one host per data directory, and how a second start asks the first to open
//!   its window (the Start menu entry).

#[cfg(windows)]
pub mod api;
pub mod audit;
pub mod clipboard;
pub mod diagnostics;
pub mod events;
pub mod lifecycle;
pub mod pairing;
pub mod server;
pub mod single_instance;
pub mod stream;
#[cfg(feature = "ui")]
pub mod ui;
