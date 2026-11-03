// SPDX-License-Identifier: Apache-2.0
//! A deterministic Win32 test window (frame counter, moving box, clock, colour patches) used by
//! the stream spike and the Phase 2 end-to-end tests.
//!
//! The crate is a library plus the `gander-testwin` binary. The library holds everything that is
//! pure and portable, so a spike or test can reuse it without launching the window:
//!
//! - [`layout`]: where each element is, for any client size and dpi, and the fixed colours
//!   (patch A `RGB(255,0,255)` top-left, patch B `RGB(0,255,0)` bottom-right).
//! - [`render`]: the software renderer and the readers that recover the frame counter and check
//!   patches from captured pixels.
//! - [`protocol`]: the per-process control pipe name and its line protocol.
//! - [`cli`]: the command line.
//! - `sec` (Windows only): the current-user-only security descriptor of the control pipe.
//!
//! The Win32 runtime (window, ticker, pipe server) lives in the binary and is compiled only on
//! Windows. See `host/gander-testwin/README.md` for the one-page reference.

pub mod cli;
pub mod layout;
pub mod protocol;
pub mod render;
#[cfg(windows)]
pub mod sec;
