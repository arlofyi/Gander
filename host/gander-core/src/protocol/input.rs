// SPDX-License-Identifier: Apache-2.0
//! Stream input messages on the WebRTC `input` and `control` data channels: compact JSON with a
//! `t` tag. Pointer coordinates are normalised to the captured window, 0..1.

use serde::{Deserialize, Serialize};

/// A message on the `input` (unordered, lossy) or `control` (ordered, reliable) channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum InputMessage {
    /// Pointer move.
    #[serde(rename = "move")]
    Move {
        /// Normalised x.
        x: f64,
        /// Normalised y.
        y: f64,
    },
    /// Button press.
    #[serde(rename = "down")]
    Down {
        /// Button.
        b: MouseButton,
        /// Normalised x.
        x: f64,
        /// Normalised y.
        y: f64,
    },
    /// Button release.
    #[serde(rename = "up")]
    Up {
        /// Button.
        b: MouseButton,
        /// Normalised x.
        x: f64,
        /// Normalised y.
        y: f64,
    },
    /// Wheel scroll in wheel-delta units (120 per notch), Windows wheel convention.
    #[serde(rename = "scroll")]
    Scroll {
        /// Horizontal delta: positive is a tilt to the right.
        dx: i32,
        /// Vertical delta: positive is the wheel turned away from the user (content scrolls up).
        dy: i32,
        /// Normalised x.
        x: f64,
        /// Normalised y.
        y: f64,
    },
    /// Physical key, by W3C UI Events `code` value (`KeyA`).
    #[serde(rename = "key")]
    Key {
        /// UI Events code.
        code: String,
        /// Pressed (`true`) or released.
        down: bool,
        /// Modifiers held.
        #[serde(default)]
        mods: Vec<Modifier>,
    },
    /// Text typed on the phone keyboard, injected as Unicode.
    #[serde(rename = "text")]
    Text {
        /// The text.
        s: String,
    },
    /// Ask for the host clipboard (text only, host-side opt-in).
    #[serde(rename = "clipboard.get")]
    ClipboardGet {},
    /// Clipboard text from the host.
    #[serde(rename = "clipboard")]
    Clipboard {
        /// The text.
        s: String,
    },
}

/// Mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    /// Primary.
    Left,
    /// Secondary.
    Right,
    /// Middle.
    Middle,
}

/// Keyboard modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    /// Control.
    Ctrl,
    /// Shift.
    Shift,
    /// Alt.
    Alt,
    /// Windows / Command key.
    Meta,
}
