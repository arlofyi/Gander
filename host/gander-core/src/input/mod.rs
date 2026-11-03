// SPDX-License-Identifier: Apache-2.0
//! Stream input, the platform-free half (WP-52): everything the Windows injector
//! (`gander_win::input`) decides without calling the OS, so it is unit-tested on any platform.
//!
//! - [`mapping`]: normalised picture coordinates -> screen pixels -> `SendInput` absolute
//!   coordinates over the virtual desktop (live window geometry, any monitor, any scale).
//! - [`keys`]: UI Events `code` -> scan code, the desktop-shortcut filter, text -> Unicode events.
//! - [`gate`]: when input may be injected: lock (fail closed), client pause, focus lost, local
//!   user activity, and the matching `stream.state` reasons.
//! - [`activity`]: telling the local user's input from our own injections.
//! - [`channel`]: decoding data-channel messages, which channel carries what, and the
//!   [`channel::InputSink`] trait the stream engine implements.

pub mod activity;
pub mod channel;
pub mod gate;
pub mod keys;
pub mod mapping;

pub use channel::{ChannelKind, InputSink, MessageKind, RecordingSink, Refusal, SinkRecord};
pub use gate::{Facts, GateConfig, GateState, InputGate, PauseCause};
pub use mapping::{OutOfRange, PictureArea, Point, WindowGeometry};
