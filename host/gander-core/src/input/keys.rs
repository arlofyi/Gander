// SPDX-License-Identifier: Apache-2.0
//! Keyboard input: W3C UI Events `code` values -> PC/AT set-1 scan codes (what `SendInput` with
//! `KEYEVENTF_SCANCODE` takes), the shortcut filter, and text -> Unicode key events.
//!
//! **Physical keys, not characters.** A `key` message names a physical key position (`KeyA` is
//! the key right of Caps Lock). The host injects its scan code and lets the host's keyboard
//! layout turn it into a virtual key and a character, exactly as a USB keyboard plugged into the
//! host would (stream-launch-policy spec: "`SendInput` scancodes for `key` messages"). The virtual
//! key is carried along for logging and for the platform layer's sanity checks. Characters typed
//! on the phone keyboard travel as `text` and are injected with `KEYEVENTF_UNICODE`, which is
//! layout independent ([`text_events`]).
//!
//! **Shortcut filter** ([`check_key`]). Input goes only to the streamed window (`security.md`
//! rule 12), but some keys act on the desktop rather than on the focused window: the Windows key
//! (alone it opens Start; with others it runs shell hotkeys such as Win+R, which starts arbitrary
//! programs), Alt+Tab / Alt+Esc (switch windows), Ctrl+Esc (Start) and Ctrl+Shift+Esc (Task
//! Manager). They are refused. Ctrl+Alt+Del cannot be injected by any user-mode program and is
//! not listed.

use std::collections::BTreeSet;

use crate::protocol::input::Modifier;

/// A key as `SendInput` needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeySpec {
    /// Set-1 make code (without the `E0` prefix).
    pub scan: u16,
    /// The key has the `E0` prefix (`KEYEVENTF_EXTENDEDKEY`).
    pub extended: bool,
    /// The US-layout virtual key (information; the layout decides the real one).
    pub vk: u16,
}

const fn k(scan: u16, vk: u16) -> KeySpec {
    KeySpec {
        scan,
        extended: false,
        vk,
    }
}

const fn e(scan: u16, vk: u16) -> KeySpec {
    KeySpec {
        scan,
        extended: true,
        vk,
    }
}

/// Every `code` value the host injects. Keys missing here (media keys, Power, Sleep, Print
/// Screen, Pause, IME keys) are refused as unknown.
const KEYS: &[(&str, KeySpec)] = &[
    ("KeyA", k(0x1E, 0x41)),
    ("KeyB", k(0x30, 0x42)),
    ("KeyC", k(0x2E, 0x43)),
    ("KeyD", k(0x20, 0x44)),
    ("KeyE", k(0x12, 0x45)),
    ("KeyF", k(0x21, 0x46)),
    ("KeyG", k(0x22, 0x47)),
    ("KeyH", k(0x23, 0x48)),
    ("KeyI", k(0x17, 0x49)),
    ("KeyJ", k(0x24, 0x4A)),
    ("KeyK", k(0x25, 0x4B)),
    ("KeyL", k(0x26, 0x4C)),
    ("KeyM", k(0x32, 0x4D)),
    ("KeyN", k(0x31, 0x4E)),
    ("KeyO", k(0x18, 0x4F)),
    ("KeyP", k(0x19, 0x50)),
    ("KeyQ", k(0x10, 0x51)),
    ("KeyR", k(0x13, 0x52)),
    ("KeyS", k(0x1F, 0x53)),
    ("KeyT", k(0x14, 0x54)),
    ("KeyU", k(0x16, 0x55)),
    ("KeyV", k(0x2F, 0x56)),
    ("KeyW", k(0x11, 0x57)),
    ("KeyX", k(0x2D, 0x58)),
    ("KeyY", k(0x15, 0x59)),
    ("KeyZ", k(0x2C, 0x5A)),
    ("Digit1", k(0x02, 0x31)),
    ("Digit2", k(0x03, 0x32)),
    ("Digit3", k(0x04, 0x33)),
    ("Digit4", k(0x05, 0x34)),
    ("Digit5", k(0x06, 0x35)),
    ("Digit6", k(0x07, 0x36)),
    ("Digit7", k(0x08, 0x37)),
    ("Digit8", k(0x09, 0x38)),
    ("Digit9", k(0x0A, 0x39)),
    ("Digit0", k(0x0B, 0x30)),
    ("Enter", k(0x1C, 0x0D)),
    ("Escape", k(0x01, 0x1B)),
    ("Backspace", k(0x0E, 0x08)),
    ("Tab", k(0x0F, 0x09)),
    ("Space", k(0x39, 0x20)),
    ("Minus", k(0x0C, 0xBD)),
    ("Equal", k(0x0D, 0xBB)),
    ("BracketLeft", k(0x1A, 0xDB)),
    ("BracketRight", k(0x1B, 0xDD)),
    ("Backslash", k(0x2B, 0xDC)),
    ("Semicolon", k(0x27, 0xBA)),
    ("Quote", k(0x28, 0xDE)),
    ("Backquote", k(0x29, 0xC0)),
    ("Comma", k(0x33, 0xBC)),
    ("Period", k(0x34, 0xBE)),
    ("Slash", k(0x35, 0xBF)),
    ("IntlBackslash", k(0x56, 0xE2)),
    ("CapsLock", k(0x3A, 0x14)),
    ("F1", k(0x3B, 0x70)),
    ("F2", k(0x3C, 0x71)),
    ("F3", k(0x3D, 0x72)),
    ("F4", k(0x3E, 0x73)),
    ("F5", k(0x3F, 0x74)),
    ("F6", k(0x40, 0x75)),
    ("F7", k(0x41, 0x76)),
    ("F8", k(0x42, 0x77)),
    ("F9", k(0x43, 0x78)),
    ("F10", k(0x44, 0x79)),
    ("F11", k(0x57, 0x7A)),
    ("F12", k(0x58, 0x7B)),
    ("ScrollLock", k(0x46, 0x91)),
    ("Insert", e(0x52, 0x2D)),
    ("Home", e(0x47, 0x24)),
    ("PageUp", e(0x49, 0x21)),
    ("Delete", e(0x53, 0x2E)),
    ("End", e(0x4F, 0x23)),
    ("PageDown", e(0x51, 0x22)),
    ("ArrowRight", e(0x4D, 0x27)),
    ("ArrowLeft", e(0x4B, 0x25)),
    ("ArrowDown", e(0x50, 0x28)),
    ("ArrowUp", e(0x48, 0x26)),
    ("NumLock", k(0x45, 0x90)),
    ("NumpadDivide", e(0x35, 0x6F)),
    ("NumpadMultiply", k(0x37, 0x6A)),
    ("NumpadSubtract", k(0x4A, 0x6D)),
    ("NumpadAdd", k(0x4E, 0x6B)),
    ("NumpadEnter", e(0x1C, 0x0D)),
    ("Numpad1", k(0x4F, 0x61)),
    ("Numpad2", k(0x50, 0x62)),
    ("Numpad3", k(0x51, 0x63)),
    ("Numpad4", k(0x4B, 0x64)),
    ("Numpad5", k(0x4C, 0x65)),
    ("Numpad6", k(0x4D, 0x66)),
    ("Numpad7", k(0x47, 0x67)),
    ("Numpad8", k(0x48, 0x68)),
    ("Numpad9", k(0x49, 0x69)),
    ("Numpad0", k(0x52, 0x60)),
    ("NumpadDecimal", k(0x53, 0x6E)),
    ("ContextMenu", e(0x5D, 0x5D)),
    ("ShiftLeft", k(0x2A, 0xA0)),
    ("ShiftRight", k(0x36, 0xA1)),
    ("ControlLeft", k(0x1D, 0xA2)),
    ("ControlRight", e(0x1D, 0xA3)),
    ("AltLeft", k(0x38, 0xA4)),
    ("AltRight", e(0x38, 0xA5)),
    ("MetaLeft", e(0x5B, 0x5B)),
    ("MetaRight", e(0x5C, 0x5C)),
];

/// The scan code of a UI Events `code`, `None` when the host does not inject that key.
pub fn key_for_code(code: &str) -> Option<KeySpec> {
    KEYS.iter().find(|(c, _)| *c == code).map(|(_, spec)| *spec)
}

/// The left-hand key a protocol modifier presses.
pub fn modifier_code(m: Modifier) -> &'static str {
    match m {
        Modifier::Ctrl => "ControlLeft",
        Modifier::Shift => "ShiftLeft",
        Modifier::Alt => "AltLeft",
        Modifier::Meta => "MetaLeft",
    }
}

/// The modifier a key code is, if it is one.
pub fn modifier_of(code: &str) -> Option<Modifier> {
    match code {
        "ControlLeft" | "ControlRight" => Some(Modifier::Ctrl),
        "ShiftLeft" | "ShiftRight" => Some(Modifier::Shift),
        "AltLeft" | "AltRight" => Some(Modifier::Alt),
        "MetaLeft" | "MetaRight" => Some(Modifier::Meta),
        _ => None,
    }
}

/// Why a key is not injected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyRefusal {
    /// The code is not in the host's table.
    #[error("unknown key code {0:?}")]
    Unknown(String),
    /// The key or combination acts on the desktop, not the streamed window.
    #[error("{0} acts outside the streamed window")]
    Shortcut(&'static str),
}

/// Checks a key press against the shortcut filter. `mods` are the modifiers the message carries,
/// `held` the modifiers already held down by earlier `key` messages.
pub fn check_key(
    code: &str,
    mods: &[Modifier],
    held: &BTreeSet<Modifier>,
) -> Result<KeySpec, KeyRefusal> {
    let spec = key_for_code(code).ok_or_else(|| KeyRefusal::Unknown(code.to_owned()))?;
    let active = |m: Modifier| mods.contains(&m) || held.contains(&m);
    if modifier_of(code) == Some(Modifier::Meta) || active(Modifier::Meta) {
        return Err(KeyRefusal::Shortcut("the Windows key"));
    }
    match code {
        "Tab" if active(Modifier::Alt) => Err(KeyRefusal::Shortcut("Alt+Tab")),
        "Escape" if active(Modifier::Alt) => Err(KeyRefusal::Shortcut("Alt+Esc")),
        "Escape" if active(Modifier::Ctrl) && active(Modifier::Shift) => {
            Err(KeyRefusal::Shortcut("Ctrl+Shift+Esc"))
        }
        "Escape" if active(Modifier::Ctrl) => Err(KeyRefusal::Shortcut("Ctrl+Esc")),
        _ => Ok(spec),
    }
}

/// Longest `text` message, in UTF-16 code units (each becomes a down and an up event).
pub const MAX_TEXT_UNITS: usize = 2048;

/// One keyboard event of a `text` message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEvent {
    /// A UTF-16 code unit with `KEYEVENTF_UNICODE` (surrogate pairs are two units, in order).
    Unicode(u16),
    /// A physical key (Enter for a line break, Tab), pressed and released.
    Key(KeySpec),
}

/// Why a `text` message is not injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TextRefusal {
    /// Longer than [`MAX_TEXT_UNITS`].
    #[error("the text is too long ({0} UTF-16 units)")]
    TooLong(usize),
}

/// The keyboard events that type `s`: every character as Unicode, `\r\n`, `\r` and `\n` as one
/// Enter, `\t` as Tab, other control characters dropped (they would arrive as shortcuts such as
/// Ctrl+C in some applications).
pub fn text_events(s: &str) -> Result<Vec<TextEvent>, TextRefusal> {
    let units = s.encode_utf16().count();
    if units > MAX_TEXT_UNITS {
        return Err(TextRefusal::TooLong(units));
    }
    let enter = key_for_code("Enter").map(TextEvent::Key);
    let tab = key_for_code("Tab").map(TextEvent::Key);
    let mut out = Vec::with_capacity(units);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.extend(enter);
            }
            '\n' => out.extend(enter),
            '\t' => out.extend(tab),
            c if c.is_control() => {}
            c => {
                let mut buf = [0u16; 2];
                out.extend(
                    c.encode_utf16(&mut buf)
                        .iter()
                        .map(|u| TextEvent::Unicode(*u)),
                );
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_unique_codes_and_scan_codes() {
        let mut codes = BTreeSet::new();
        let mut scans = BTreeSet::new();
        for (code, spec) in KEYS {
            assert!(codes.insert(*code), "duplicate {code}");
            assert!(
                scans.insert((spec.scan, spec.extended)),
                "duplicate scan of {code}"
            );
            assert!(spec.scan > 0 && spec.scan < 0x80, "{code}: make code range");
        }
    }

    #[test]
    fn known_scan_codes() {
        assert_eq!(key_for_code("KeyA"), Some(k(0x1E, 0x41)));
        assert_eq!(key_for_code("Enter").unwrap().scan, 0x1C);
        assert!(key_for_code("NumpadEnter").unwrap().extended);
        assert!(key_for_code("ArrowUp").unwrap().extended);
        assert!(!key_for_code("Numpad8").unwrap().extended);
        assert_eq!(key_for_code("Power"), None);
        assert_eq!(key_for_code("keya"), None, "codes are case sensitive");
    }

    #[test]
    fn shortcut_filter() {
        let none = BTreeSet::new();
        assert!(check_key("KeyA", &[Modifier::Ctrl], &none).is_ok());
        assert!(
            check_key("F4", &[Modifier::Alt], &none).is_ok(),
            "Alt+F4 stays in the app"
        );
        assert!(matches!(
            check_key("MetaLeft", &[], &none),
            Err(KeyRefusal::Shortcut(_))
        ));
        assert!(matches!(
            check_key("KeyR", &[Modifier::Meta], &none),
            Err(KeyRefusal::Shortcut(_))
        ));
        assert_eq!(
            check_key("Tab", &[Modifier::Alt], &none),
            Err(KeyRefusal::Shortcut("Alt+Tab"))
        );
        let alt_held: BTreeSet<_> = [Modifier::Alt].into();
        assert_eq!(
            check_key("Tab", &[], &alt_held),
            Err(KeyRefusal::Shortcut("Alt+Tab"))
        );
        assert_eq!(
            check_key("Escape", &[Modifier::Ctrl, Modifier::Shift], &none),
            Err(KeyRefusal::Shortcut("Ctrl+Shift+Esc"))
        );
        assert_eq!(
            check_key("Escape", &[Modifier::Ctrl], &none),
            Err(KeyRefusal::Shortcut("Ctrl+Esc"))
        );
        assert!(check_key("Escape", &[], &none).is_ok());
        assert_eq!(
            check_key("LaunchApp1", &[], &none),
            Err(KeyRefusal::Unknown("LaunchApp1".into()))
        );
    }

    #[test]
    fn text_to_events() {
        let ev = text_events("Hé\r\nx\ty\u{7}😀").unwrap();
        let enter = TextEvent::Key(key_for_code("Enter").unwrap());
        let tab = TextEvent::Key(key_for_code("Tab").unwrap());
        assert_eq!(
            ev,
            vec![
                TextEvent::Unicode(u16::from(b'H')),
                TextEvent::Unicode(0xE9),
                enter,
                TextEvent::Unicode(u16::from(b'x')),
                tab,
                TextEvent::Unicode(u16::from(b'y')),
                TextEvent::Unicode(0xD83D),
                TextEvent::Unicode(0xDE00),
            ]
        );
        assert_eq!(text_events("\r\r").unwrap(), vec![enter, enter]);
        assert_eq!(
            text_events(&"a".repeat(MAX_TEXT_UNITS + 1)),
            Err(TextRefusal::TooLong(MAX_TEXT_UNITS + 1))
        );
        assert_eq!(
            text_events(&"a".repeat(MAX_TEXT_UNITS)).unwrap().len(),
            MAX_TEXT_UNITS
        );
    }
}
