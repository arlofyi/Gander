// SPDX-License-Identifier: Apache-2.0
//! Stream launch, window discovery and fit-to-phone: the pure logic
//! (`docs/specs/2026-11-01-stream-launch-policy.md`, "Launch and window discovery", WP-51).
//!
//! The Win32 side (`gander_win::launch`) takes window snapshots, starts the process and moves the
//! window; everything that decides something lives here and is unit-tested without a desktop:
//!
//! - [`command`]: the launch command from a catalogue template, the handler's registered command,
//!   or the isolated browser template; quoting for `CreateProcessW`.
//! - [`plan`]: from a [`PolicyDecision`](crate::policy::PolicyDecision) and the catalogue entry to
//!   a [`plan::LaunchPlan`] (how to start, how to find the window, how long to wait). The policy
//!   is the gate: a refused decision, or an `ask` without the owner's answer, plans nothing.
//! - [`discovery`]: window snapshots, the before/after diff, title and process-tree matching,
//!   the `ApplicationFrameHost` mapping for packaged apps, the splash-class skip list, settle and
//!   timeout, and the `pick` fallback.
//! - [`fit`]: the fit-to-phone size and position.

pub mod command;
pub mod discovery;
pub mod fit;
pub mod plan;
