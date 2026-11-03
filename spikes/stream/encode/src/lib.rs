// SPDX-License-Identifier: Apache-2.0
//! WP-42 encode spike: a Media Foundation hardware encoder (H.264, HEVC) fed with NV12 textures
//! converted from Windows Graphics Capture frames on the same D3D11 device.
//!
//! Pattern (the usual one for MF hardware encoders, as also used by `moq-video` and Sunshine;
//! no code copied): enumerate hardware encoder MFTs per adapter with `MFTEnum2` and
//! `MFT_ENUM_ADAPTER_LUID`, unlock the asynchronous MFT, hand it an `IMFDXGIDeviceManager` that
//! wraps the capture device, configure low-latency CBR with a fixed GOP and no B-frames through
//! `ICodecAPI`, feed `IMFSample`s that wrap D3D11 textures, and force IDR pictures on demand with
//! `CODECAPI_AVEncVideoForceKeyFrame`. Output is Annex-B (start codes, parameter sets in band).
//!
//! ```no_run
//! # #[cfg(windows)] fn demo(device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
//! #     luid: i64, nv12: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D) -> anyhow::Result<()> {
//! use spike_encode::{encoder::{Config, Encoder, Event}, mft::{self, Codec}};
//! mft::startup()?;
//! let found = mft::hardware_encoders(Codec::H264, Some(luid))?;
//! let cfg = Config::new(Codec::H264, 1080, 2280, 30, 6_000_000);
//! let mut enc = Encoder::new(&found[0].activate, device, &cfg)?;
//! loop {
//!     match enc.next_event(std::time::Duration::from_secs(1)) {
//!         Some(Event::NeedInput(_)) => enc.feed(nv12, 0, 333_333)?,
//!         Some(Event::HaveOutput(_)) => { let _pkt = enc.take_output()?; break }
//!         _ => break,
//!     }
//! }
//! # Ok(()) }
//! ```

pub mod annexb;
#[cfg(windows)]
pub mod convert;
#[cfg(windows)]
pub mod encoder;
#[cfg(windows)]
pub mod mft;
#[cfg(windows)]
pub mod xadapter;
