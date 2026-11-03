// SPDX-License-Identifier: Apache-2.0
//! WebRTC session engine for Stream mode (Phase 2, WP-50): capture -> encode -> WebRTC track,
//! with keepalive, keyframes on demand, a delay-based bandwidth estimator, the battery cap,
//! host-only ICE inside the firewall's UDP range, liveness and pause over a data channel, and the
//! session-lock guard.
//!
//! Productionised from the stream spike (`spikes/stream/*`, findings in
//! `docs/specs/2026-11-01-stream-spike-findings.md`); the spikes stay as they were.
//!
//! - [`engine`]: [`StreamEngine`] and [`Session`], the API WP-51..54 build on.
//! - [`estimator`]: the delay-based [`estimator::DelayEstimator`] (D12 condition a).
//! - [`interceptor`]: PLI/FIR delivery to the application (condition b).
//! - [`control`]: liveness and pause messages on the `control` data channel (condition c).
//! - [`sdp`], [`ports`]: host-only candidates, empty-candidate answers, trickle (condition d),
//!   ports inside `[firewall] udp_ports`.
//! - [`source`]: the capture + encode contract; [`win`] the Windows implementation (WGC, D3D11,
//!   Media Foundation hardware H.264) and the lock and power adapters.
//! - [`input`]: the data channels wired to `gander_win::input` (WP-52).
//! - [`audio`]: the application's sound (WP-55): the [`audio::AudioSource`] contract, Opus
//!   framing, the gate; [`win::audio`] the WASAPI process-loopback capture.
//! - [`testing`]: the loopback viewer, the impairment relay and a synthetic source (test support,
//!   used by this crate's tests and by later packages' tests; not used in production).

pub mod audio;
pub mod control;
pub mod deps;
pub mod engine;
pub mod error;
pub mod estimator;
pub mod h264;
#[cfg(windows)]
pub mod input;
pub mod interceptor;
pub mod peer;
pub mod ports;
pub mod rate;
pub mod sdp;
pub mod source;
pub mod testing;
#[cfg(windows)]
pub mod win;

pub use deps::{
    BindAddrs, FakeLock, FixedPower, LockSource, LockStatus, PowerSource, SignalSink, StaticAddrs,
};
pub use engine::{
    Channel, EndReason, EngineConfig, EngineDeps, InputHandler, KeyframeRequests, PauseReason,
    Session, SessionState, SessionStats, StartRequest, StreamEngine, WindowTarget,
};
pub use error::EngineError;
