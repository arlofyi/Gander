// SPDX-License-Identifier: Apache-2.0
//! `spike_webrtc` (WP-43, shared with WP-44): the building blocks of the stream spike. The
//! `spike-webrtc` binary (WP-43) and the `spike-input` binary (WP-44, `spikes/stream/input`) are
//! both thin drivers over this library.
//!
//! - [`source`]: `gander-testwin` captured with WGC, converted and encoded by a hardware MFT.
//! - [`peer`]: the host peer (offers the track and the `input` channel) and the loopback viewer.
//! - [`signal`]: the plain WebSocket signalling messages.
//! - [`relay`]: the in-process UDP shaper and the SDP candidate rewriting.
//! - [`metrics`], [`au`], [`decode`], [`testwin`]: measurement state, H.264 helpers, the ffmpeg
//!   decode harness and the test-window launcher.
#![cfg_attr(not(windows), allow(dead_code))]

pub mod au;
pub mod decode;
pub mod metrics;
pub mod peer;
pub mod relay;
pub mod signal;
#[cfg(windows)]
pub mod source;
#[cfg(windows)]
pub mod testwin;
