// SPDX-License-Identifier: Apache-2.0
//! Gander core: protocol types, policy, path resolution and the pairing state machine.
//!
//! This crate performs no OS calls (no filesystem handles, no sockets, no registry) so that its
//! logic is testable on any platform. OS-specific behaviour lives in `gander-win`.
//!
//! Modules:
//!
//! - [`tiles`]: deep-zoom tile pyramid geometry shared by the host, the render worker and the
//!   Android client (WP-67).
//! - [`protocol`]: serde DTOs for every request, response, event and input message of protocol
//!   v1 (`docs/specs/2026-11-01-protocol-v1.md`). The hand-written OpenAPI document
//!   `protocol/openapi.yaml` describes the same shapes; a test keeps the two in step.
//! - [`error`]: the machine-readable error codes of the protocol and their HTTP status.
//! - [`clipboard`]: the Stream clipboard text bridge (WP-56): the setting, caps, policy, message
//!   codec and the service that applies them over an abstract clipboard.
//! - [`config`]: the host's on-disk files (`config.toml`, `devices.toml`, `apps.toml`), parsing,
//!   validation and load/save through the [`config::SecureFileStore`] trait, which the platform
//!   crate implements with an owner-only ACL.
//! - [`path`]: path-resolution policy (segment validation, reparse-tag policy, hidden
//!   patterns) and listing filter/sort/paging; the platform crate does the handle walk.
//! - [`launch`]: Stream launch command, window discovery and fit-to-phone math (WP-51).
//! - [`input`]: Stream input mapping, key table, pause rules and the data-channel boundary
//!   (WP-52).
//! - [`policy`]: the Stream launch policy (`allowed | refused | ask`) over an abstract handler
//!   lookup, the listing view of it, and the Phase 1 `POST /stream` service function.
//! - [`net`]: listener plan, Tailscale identity semantics, LocalAPI framing, firewall rule
//!   matching (WP-12).
//! - [`search`]: the Windows Search index queries behind `search: "indexed"` (WP-66): the SQL, its
//!   escaping and the reading of the paths the index returns.
//! - [`zipstream`]: the streaming ZIP writer behind `GET /v1/zip` (WP-68).
//! - [`pairing`]: SAS derivation, token hashing and lookup, the QR payload and the pairing window
//!   state machine (WP-13).

#![forbid(unsafe_code)]

pub mod clipboard;
pub mod config;
pub mod error;
pub mod input;
pub mod launch;
pub mod lifecycle;
pub mod net;
pub mod pairing;
pub mod path;
pub mod policy;
pub mod protocol;
pub mod search;
pub mod tiles;
pub mod zipstream;
