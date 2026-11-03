// SPDX-License-Identifier: Apache-2.0
//! Command line of `gander-testwin`.

use std::fmt;
use std::time::Duration;

/// Window title when none is given.
pub const DEFAULT_TITLE: &str = "Gander Test Window";
/// How long `--self-test` keeps the window open.
pub const DEFAULT_SELF_TEST_SECS: u64 = 20;
/// Upper bound for `--self-test-secs`, so a typo cannot leave a window running for days.
pub const MAX_SELF_TEST_SECS: u64 = 3600;
/// Upper bound for `--splash`, in milliseconds.
pub const MAX_SPLASH_MS: u64 = 10_000;
/// Longest `--reuse` key.
pub const MAX_REUSE_KEY: usize = 32;

/// Usage text printed for `--help` and after a usage error.
pub const USAGE: &str = "\
gander-testwin: deterministic Win32 test window (60 Hz frame counter, moving box, clock)

USAGE:
    gander-testwin [OPTIONS] [TITLE...]

OPTIONS:
    --title <TEXT>        Window title (default: \"Gander Test Window\"). Bare words are joined
                          with spaces and used as the title too.
    --self-test           Exit with code 0 after 20 s (never leave a window running)
    --self-test-secs <N>  Like --self-test with N seconds (1..=3600)
    --no-focus            Show the window without activating it

SCENARIOS (window discovery tests, WP-51):
    --splash <MS>         First show a splash window (class \"MsoSplash\", same title) for MS
                          milliseconds (1..=10000), then the main window
    --reuse <KEY>         Single instance per KEY ([A-Za-z0-9_-], at most 32): if an instance
                          with this KEY runs, ask it over \\\\.\\pipe\\gander-testwin-reuse-<KEY> to
                          open a window titled TITLE and exit 0; otherwise become that instance
    --twin                Also open a second window, \"<TITLE> (second)\", at start
    --dpi-unaware         Run DPI-unaware: the system bitmap-scales the window on a scaled
                          monitor (input mapping tests, WP-52)
    -h, --help            Print this help

Prints one line on stdout when ready:
    gander-testwin ready pid=<pid> pipe=<pipe name> hwnd=0x<hex> title=<title>
The control pipe protocol is documented in host/gander-testwin/README.md.";

/// Parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// Window title.
    pub title: String,
    /// `Some(d)` when the process must exit by itself after `d`.
    pub self_test: Option<Duration>,
    /// Show without activating (`SW_SHOWNOACTIVATE`).
    pub no_focus: bool,
    /// `--help` was requested.
    pub help: bool,
    /// `--splash <MS>`: a splash window first, for this long.
    pub splash: Option<Duration>,
    /// `--reuse <KEY>`: single-instance key.
    pub reuse: Option<String>,
    /// `--twin`: a second window at start.
    pub twin: bool,
    /// `--dpi-unaware`: no per-monitor awareness (system bitmap scaling).
    pub dpi_unaware: bool,
}

/// A command line the tool refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgError(pub String);

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ArgError {}

/// Parses the arguments after the program name.
pub fn parse<I>(args: I) -> Result<Args, ArgError>
where
    I: IntoIterator<Item = String>,
{
    let mut out = Args {
        title: DEFAULT_TITLE.to_owned(),
        self_test: None,
        no_focus: false,
        help: false,
        splash: None,
        reuse: None,
        twin: false,
        dpi_unaware: false,
    };
    let mut title_words: Vec<String> = Vec::new();
    let mut explicit_title: Option<String> = None;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => out.help = true,
            "--self-test" => {
                out.self_test
                    .get_or_insert(Duration::from_secs(DEFAULT_SELF_TEST_SECS));
            }
            "--self-test-secs" => {
                let v = it
                    .next()
                    .ok_or_else(|| ArgError("--self-test-secs needs a value".into()))?;
                let secs: u64 = v
                    .parse()
                    .map_err(|_| ArgError(format!("--self-test-secs: not a number: {v}")))?;
                if !(1..=MAX_SELF_TEST_SECS).contains(&secs) {
                    return Err(ArgError(format!(
                        "--self-test-secs: {secs} is outside 1..={MAX_SELF_TEST_SECS}"
                    )));
                }
                out.self_test = Some(Duration::from_secs(secs));
            }
            "--no-focus" => out.no_focus = true,
            "--twin" => out.twin = true,
            "--dpi-unaware" => out.dpi_unaware = true,
            "--splash" => {
                let v = it
                    .next()
                    .ok_or_else(|| ArgError("--splash needs a value".into()))?;
                let ms: u64 = v
                    .parse()
                    .map_err(|_| ArgError(format!("--splash: not a number: {v}")))?;
                if !(1..=MAX_SPLASH_MS).contains(&ms) {
                    return Err(ArgError(format!(
                        "--splash: {ms} is outside 1..={MAX_SPLASH_MS}"
                    )));
                }
                out.splash = Some(Duration::from_millis(ms));
            }
            "--reuse" => {
                let v = it
                    .next()
                    .ok_or_else(|| ArgError("--reuse needs a value".into()))?;
                let ok = !v.is_empty()
                    && v.len() <= MAX_REUSE_KEY
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
                if !ok {
                    return Err(ArgError(format!(
                        "--reuse: the key must be 1..={MAX_REUSE_KEY} of A-Z a-z 0-9 - _"
                    )));
                }
                out.reuse = Some(v);
            }
            "--title" => {
                explicit_title = Some(
                    it.next()
                        .ok_or_else(|| ArgError("--title needs a value".into()))?,
                );
            }
            "--" => title_words.extend(it.by_ref()),
            flag if flag.starts_with('-') && flag.len() > 1 => {
                return Err(ArgError(format!("unknown option: {flag}")));
            }
            _ => title_words.push(arg),
        }
    }
    if let Some(t) = explicit_title {
        out.title = t;
    } else if !title_words.is_empty() {
        out.title = title_words.join(" ");
    }
    if out.title.trim().is_empty() {
        return Err(ArgError("the title must not be empty".into()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Args, ArgError> {
        parse(args.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn defaults() {
        let a = p(&[]).unwrap();
        assert_eq!(a.title, DEFAULT_TITLE);
        assert_eq!(a.self_test, None);
        assert!(!a.no_focus && !a.help);
    }

    #[test]
    fn self_test_defaults_to_twenty_seconds() {
        assert_eq!(
            p(&["--self-test"]).unwrap().self_test,
            Some(Duration::from_secs(20))
        );
    }

    #[test]
    fn self_test_secs_overrides_in_either_order() {
        assert_eq!(
            p(&["--self-test-secs", "3"]).unwrap().self_test,
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            p(&["--self-test-secs", "3", "--self-test"])
                .unwrap()
                .self_test,
            Some(Duration::from_secs(3))
        );
    }

    #[test]
    fn self_test_secs_is_bounded() {
        assert!(p(&["--self-test-secs", "0"]).is_err());
        assert!(p(&["--self-test-secs", "3601"]).is_err());
        assert!(p(&["--self-test-secs", "abc"]).is_err());
        assert!(p(&["--self-test-secs"]).is_err());
    }

    #[test]
    fn title_from_flag_or_bare_words() {
        assert_eq!(p(&["--title", "Spike A"]).unwrap().title, "Spike A");
        assert_eq!(p(&["Spike", "B"]).unwrap().title, "Spike B");
        assert_eq!(p(&["--self-test", "Spike", "C"]).unwrap().title, "Spike C");
        assert_eq!(
            p(&["--", "--weird", "title"]).unwrap().title,
            "--weird title"
        );
        assert_eq!(p(&["--title", "Flag", "words"]).unwrap().title, "Flag");
    }

    #[test]
    fn rejects_unknown_options_and_empty_titles() {
        assert!(p(&["--nope"]).is_err());
        assert!(p(&["--title"]).is_err());
        assert!(p(&["--title", "  "]).is_err());
    }

    #[test]
    fn scenario_flags() {
        let a = p(&["--splash", "1500", "--reuse", "k-1", "--twin", "T"]).unwrap();
        assert_eq!(a.splash, Some(Duration::from_millis(1500)));
        assert_eq!(a.reuse.as_deref(), Some("k-1"));
        assert!(a.twin);
        assert_eq!(a.title, "T");
        let d = p(&[]).unwrap();
        assert_eq!((d.splash, d.reuse, d.twin), (None, None, false));
        assert!(p(&["--splash", "0"]).is_err());
        assert!(p(&["--splash", "10001"]).is_err());
        assert!(p(&["--splash"]).is_err());
        assert!(p(&["--reuse", "a/b"]).is_err());
        assert!(p(&["--reuse", ""]).is_err());
        assert!(p(&["--reuse", &"k".repeat(33)]).is_err());
        assert!(p(&["--reuse"]).is_err());
    }

    #[test]
    fn flags() {
        let a = p(&["--no-focus", "-h"]).unwrap();
        assert!(a.no_focus && a.help);
    }
}
