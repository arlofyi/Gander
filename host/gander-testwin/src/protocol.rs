// SPDX-License-Identifier: Apache-2.0
//! The control pipe: name and command protocol.
//!
//! Each running `gander-testwin` serves one named pipe, `\\.\pipe\gander-testwin-<pid>`, where
//! `<pid>` is its own process id (also printed on stdout at start-up). Only the user that owns
//! the process can open it (protected DACL with a single allow entry for the user's SID) and
//! remote clients are rejected. One client is served at a time; a second `CreateFile` gets
//! `ERROR_PIPE_BUSY` until the first disconnects, so clients should retry briefly.
//!
//! The protocol is line based and strictly request/response over a byte-mode pipe:
//!
//! - A request is one UTF-8 line (`\n` or `\r\n` terminated, at most [`MAX_LINE`] bytes),
//!   a lower-case command word followed by space-separated arguments.
//! - Each request gets exactly one reply line: `ok` or `ok key=value key=value ...`, or
//!   `err <code> [detail]`. Values never contain spaces except the trailing `title=` value.
//! - A client may send any number of requests on one connection.
//!
//! | Request | Effect | Reply |
//! | --- | --- | --- |
//! | `ping` | nothing | `ok pong` |
//! | `state?` | read-only snapshot | `ok frame=N colour=RRGGBB hits=H clicks=C last_click=X,Y\|none windows=N minimised=0\|1 menu=0\|1 tooltip=0\|1 dialog=0\|1 box=X,Y,W,H` |
//! | `colour?` (or `color?`) | box colour and click counters | `ok colour=RRGGBB index=I hits=H clicks=C` |
//! | `typed?` | keyboard input received by the windows (WP-52) | `ok keydowns=K chars=N text=<hex of the UTF-8 of the last 256 characters>` |
//! | `frame?` | last painted frame number (the counter is `frame & 0xFFFFFF`) | `ok frame=N` |
//! | `layout?` | the pixel geometry of the main window, see [`crate::layout`] | `ok dpi=D client=WxH patch_a=X,Y,W,H patch_a_rgb=RRGGBB patch_b=... patch_b_rgb=RRGGBB strip=X,Y,W,H strip_cell=P strip_bits=24 arena=X,Y,W,H box_size=P counter_text=X,Y,SCALE clock_text=X,Y,SCALE palette=RRGGBB,...` |
//! | `windows?` | handles of all open windows, main first | `ok count=N hwnd=0xAAAA,0xBBBB` |
//! | `open-second [TITLE]` | opens a second top-level window in the same process (same class, same content); default title `<title> (second)` | `ok hwnd=0xBBBB` |
//! | `minimise` (or `minimize`) | minimises the main window | `ok` |
//! | `restore` | `SW_RESTORE` on the main window | `ok` |
//! | `popup` | same as Ctrl+P: tooltip and popup menu on the main window | `ok` |
//! | `dialog` | same as Ctrl+D: opens a modal message box owned by the main window | `ok` |
//! | `dismiss` | closes the popup menu, tooltip and any modal dialog | `ok` |
//! | `freeze` | stops the animation: no repaint, so the window content is static (WP-50 keepalive test) | `ok` |
//! | `park` | `freeze`, and every paint shows frame 0 from now on: the box sits at its start position (the arena's top-left corner) whatever the window size, so a harness knows where it is before it looks (WP-57) | `ok` |
//! | `animate` | restarts the animation after `freeze` or `park` | `ok` |
//! | `tone [HZ [MS]]` | plays a quiet sine (-30 dBFS, 5 ms fades) through this process's audio session, without blocking; HZ 20..=20000 (default 440), MS 1..=1000 (default 500; short, the speakers may be on). WP-55 process-loopback tests detect it | `ok`; `err busy` while a tone plays, `err tone <detail>` if it cannot start |
//! | `quit` | shuts the process down cleanly (exit code 0) | `ok` (sent before shutdown) |
//!
//! Errors: `err unknown-command`, `err bad-argument <detail>`, `err timeout` (the window thread
//! did not answer within 2 s, for example while hung), `err line-too-long` (the connection is
//! then closed), `err not-implemented`.
//!
//! Keyboard: besides the pipe, Ctrl+P (popup menu and tooltip) and Ctrl+D (modal dialog) work as
//! real keystrokes and as posted `WM_CHAR` messages with the control codes [`CTRL_P`] (0x10) and
//! [`CTRL_D`] (0x04), which needs no keyboard focus.
//!
//! The commands that open `popup`/`dialog` return immediately; the menu or dialog is modal on
//! the window thread and the animation keeps running underneath. Use `dismiss` to close them.

use std::fmt;

/// `WM_CHAR` code of the Ctrl+P keystroke (DLE): opens the popup menu and the tooltip.
pub const CTRL_P: u32 = 0x10;
/// `WM_CHAR` code of the Ctrl+D keystroke (EOT): opens the modal dialog.
pub const CTRL_D: u32 = 0x04;

/// Longest accepted request line in bytes, newline excluded.
pub const MAX_LINE: usize = 256;

/// Default `tone` frequency in hertz.
pub const TONE_DEFAULT_HZ: u32 = 440;
/// Default `tone` duration in milliseconds.
pub const TONE_DEFAULT_MS: u32 = 500;
/// Longest `tone`, milliseconds.
pub const TONE_MAX_MS: u32 = 1000;

/// The pipe name for process `pid`: `\\.\pipe\gander-testwin-<pid>`.
pub fn pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\gander-testwin-{pid}")
}

/// The single-instance pipe of `--reuse <key>`: `\\.\pipe\gander-testwin-reuse-<key>`. It
/// speaks the same line protocol as [`pipe_name`]; a second launch with the same key sends
/// `open-second <title>` to it and exits.
pub fn reuse_pipe_name(key: &str) -> String {
    format!(r"\\.\pipe\gander-testwin-reuse-{key}")
}

/// A parsed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `ping`
    Ping,
    /// `state?`
    State,
    /// `colour?`
    Colour,
    /// `frame?`
    Frame,
    /// `typed?`: `WM_KEYDOWN`/`WM_SYSKEYDOWN` count and the characters received (`WM_CHAR`).
    Typed,
    /// `layout?`
    Layout,
    /// `windows?`
    Windows,
    /// `open-second [TITLE]`
    OpenSecond {
        /// Title for the new window, `None` for the default.
        title: Option<String>,
    },
    /// `minimise`
    Minimise,
    /// `restore`
    Restore,
    /// `popup`
    Popup,
    /// `dialog`
    Dialog,
    /// `dismiss`
    Dismiss,
    /// `freeze`: stop repainting (static content).
    Freeze,
    /// `park`: freeze with the box at its frame-0 position.
    Park,
    /// `animate`: repaint every frame again.
    Animate,
    /// `tone [HZ [MS]]`: hook for the audio spike.
    Tone {
        /// Frequency in hertz.
        hz: u32,
        /// Duration in milliseconds.
        ms: u32,
    },
    /// `quit`
    Quit,
}

/// Why a request line was refused. `Display` gives the text that follows `err ` on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Empty line or a command word that does not exist.
    UnknownCommand,
    /// Known command, unusable arguments.
    BadArgument(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::UnknownCommand => f.write_str("unknown-command"),
            ParseError::BadArgument(d) => write!(f, "bad-argument {d}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parses one request line (without its line terminator).
pub fn parse(line: &str) -> Result<Command, ParseError> {
    let line = line.trim();
    let (word, rest) = match line.split_once(char::is_whitespace) {
        Some((w, r)) => (w, r.trim()),
        None => (line, ""),
    };
    let no_args = |c: Command| {
        if rest.is_empty() {
            Ok(c)
        } else {
            Err(ParseError::BadArgument(format!(
                "{word} takes no arguments"
            )))
        }
    };
    match word {
        "ping" => no_args(Command::Ping),
        "state?" => no_args(Command::State),
        "colour?" | "color?" => no_args(Command::Colour),
        "frame?" => no_args(Command::Frame),
        "typed?" => no_args(Command::Typed),
        "layout?" => no_args(Command::Layout),
        "windows?" => no_args(Command::Windows),
        "open-second" => {
            let title = (!rest.is_empty()).then(|| rest.to_owned());
            if title
                .as_deref()
                .is_some_and(|t| t.chars().any(char::is_control))
            {
                return Err(ParseError::BadArgument(
                    "title has control characters".into(),
                ));
            }
            Ok(Command::OpenSecond { title })
        }
        "minimise" | "minimize" => no_args(Command::Minimise),
        "restore" => no_args(Command::Restore),
        "popup" => no_args(Command::Popup),
        "dialog" => no_args(Command::Dialog),
        "dismiss" => no_args(Command::Dismiss),
        "freeze" => no_args(Command::Freeze),
        "park" => no_args(Command::Park),
        "animate" => no_args(Command::Animate),
        "quit" => no_args(Command::Quit),
        "tone" => {
            let mut parts = rest.split_whitespace();
            let hz = parse_ranged(parts.next(), TONE_DEFAULT_HZ, 20..=20_000, "hz")?;
            let ms = parse_ranged(parts.next(), TONE_DEFAULT_MS, 1..=TONE_MAX_MS, "ms")?;
            if parts.next().is_some() {
                return Err(ParseError::BadArgument(
                    "tone takes at most 2 arguments".into(),
                ));
            }
            Ok(Command::Tone { hz, ms })
        }
        _ => Err(ParseError::UnknownCommand),
    }
}

fn parse_ranged(
    text: Option<&str>,
    default: u32,
    range: std::ops::RangeInclusive<u32>,
    name: &str,
) -> Result<u32, ParseError> {
    let Some(text) = text else { return Ok(default) };
    let v: u32 = text
        .parse()
        .map_err(|_| ParseError::BadArgument(format!("{name} is not a number: {text}")))?;
    if range.contains(&v) {
        Ok(v)
    } else {
        Err(ParseError::BadArgument(format!(
            "{name} {v} is outside {}..={}",
            range.start(),
            range.end()
        )))
    }
}

/// Formats an error reply line (no terminator).
pub fn err_reply(error: &ParseError) -> String {
    format!("err {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_contains_the_pid() {
        assert_eq!(pipe_name(4242), r"\\.\pipe\gander-testwin-4242");
    }

    #[test]
    fn parses_every_simple_command() {
        assert_eq!(parse("freeze"), Ok(Command::Freeze));
        assert_eq!(parse("animate"), Ok(Command::Animate));
        assert_eq!(parse("park"), Ok(Command::Park));
        assert!(parse("park now").is_err());
        let cases = [
            ("ping", Command::Ping),
            ("state?", Command::State),
            ("colour?", Command::Colour),
            ("color?", Command::Colour),
            ("frame?", Command::Frame),
            ("typed?", Command::Typed),
            ("layout?", Command::Layout),
            ("windows?", Command::Windows),
            ("minimise", Command::Minimise),
            ("minimize", Command::Minimise),
            ("restore", Command::Restore),
            ("popup", Command::Popup),
            ("dialog", Command::Dialog),
            ("dismiss", Command::Dismiss),
            ("quit", Command::Quit),
        ];
        for (line, want) in cases {
            assert_eq!(parse(line), Ok(want), "{line}");
        }
    }

    #[test]
    fn tolerates_whitespace_and_crlf() {
        assert_eq!(parse("  ping \r"), Ok(Command::Ping));
        assert_eq!(parse("ping\r\n"), Ok(Command::Ping));
    }

    #[test]
    fn open_second_takes_the_rest_of_the_line_as_title() {
        assert_eq!(
            parse("open-second"),
            Ok(Command::OpenSecond { title: None })
        );
        assert_eq!(
            parse("open-second  My second  window "),
            Ok(Command::OpenSecond {
                title: Some("My second  window".into())
            })
        );
        assert!(parse("open-second bad\u{7}title").is_err());
    }

    #[test]
    fn tone_defaults_and_bounds() {
        assert_eq!(parse("tone"), Ok(Command::Tone { hz: 440, ms: 500 }));
        assert_eq!(parse("tone 1000"), Ok(Command::Tone { hz: 1000, ms: 500 }));
        assert_eq!(
            parse("tone 1000 250"),
            Ok(Command::Tone { hz: 1000, ms: 250 })
        );
        assert!(matches!(parse("tone 19"), Err(ParseError::BadArgument(_))));
        assert!(matches!(
            parse("tone 20001"),
            Err(ParseError::BadArgument(_))
        ));
        assert!(matches!(
            parse("tone 440 0"),
            Err(ParseError::BadArgument(_))
        ));
        assert!(matches!(
            parse("tone 440 1001"),
            Err(ParseError::BadArgument(_))
        ));
        assert!(matches!(parse("tone x"), Err(ParseError::BadArgument(_))));
        assert!(matches!(
            parse("tone 1 2 3"),
            Err(ParseError::BadArgument(_))
        ));
    }

    #[test]
    fn rejects_unknown_empty_and_extra_arguments() {
        assert_eq!(parse(""), Err(ParseError::UnknownCommand));
        assert_eq!(parse("   "), Err(ParseError::UnknownCommand));
        assert_eq!(parse("PING"), Err(ParseError::UnknownCommand));
        assert_eq!(parse("format c:"), Err(ParseError::UnknownCommand));
        assert!(matches!(parse("ping now"), Err(ParseError::BadArgument(_))));
        assert!(matches!(parse("quit now"), Err(ParseError::BadArgument(_))));
    }

    #[test]
    fn error_replies() {
        assert_eq!(
            err_reply(&ParseError::UnknownCommand),
            "err unknown-command"
        );
        assert_eq!(
            err_reply(&ParseError::BadArgument("hz 5 is outside".into())),
            "err bad-argument hz 5 is outside"
        );
    }
}
