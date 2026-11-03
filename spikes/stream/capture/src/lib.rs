// SPDX-License-Identifier: Apache-2.0
//! WP-41 capture spike: Windows Graphics Capture (WGC) of one HWND into D3D11 textures that live
//! on a device the CALLER creates and owns.
//!
//! This is the path forward for the encode spike (WP-42): `windows-capture` 2.0.1 creates its own
//! device on the default adapter inside `GraphicsCaptureApiHandler::start`, without the
//! video-support flag or multithread protection, and lends each frame only for the duration of a
//! callback. A Media Foundation encoder needs a device it can register with an
//! `IMFDXGIDeviceManager` and a texture it can hold until the encoder is done, so this crate is a
//! thin wrapper over the `windows` crate instead. See
//! `docs/specs/2026-11-01-stream-spike-findings.md` for the measured facts.
//!
//! Usage (the API WP-42 calls):
//!
//! ```no_run
//! # #[cfg(windows)] fn demo(hwnd: windows::Win32::Foundation::HWND) -> anyhow::Result<()> {
//! use spike_capture::{d3d, wgc};
//! let adapters = d3d::adapters()?;                       // pick one, or pass None for default
//! let device = d3d::create_device(Some(&adapters[0].adapter))?; // video + multithread protected
//! let capture = wgc::Capture::start(hwnd, &device, &wgc::CaptureOptions::default())?;
//! while let Some(frame) = capture.recv_timeout(std::time::Duration::from_secs(1)) {
//!     let _texture = frame.texture();          // ID3D11Texture2D on `device`, BGRA8
//!     let _qpc_100ns = frame.system_relative_time_100ns(); // QPC-based presentation time
//!     // hand to the encoder; dropping `frame` returns its buffer to the pool
//! }
//! capture.stop();
//! # Ok(()) }
//! ```

#[cfg(windows)]
pub mod clock;
#[cfg(windows)]
pub mod d3d;
#[cfg(windows)]
pub mod readback;
#[cfg(windows)]
pub mod wgc;
