// SPDX-License-Identifier: Apache-2.0
//! Out-of-process rendering: the job description shared with `gander-render.exe` ([`job`]) and,
//! on Windows, the spawner that runs one job under a job object ([`worker`]).
//!
//! The host process never decodes untrusted bytes itself (security.md, item 11): every
//! thumbnail, preview and metadata read is a [`job::RenderJob`] executed by a fresh worker.

pub mod job;
#[cfg(windows)]
pub mod worker;
