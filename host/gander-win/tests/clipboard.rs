// SPDX-License-Identifier: Apache-2.0
//! The real Windows clipboard behind the Stream clipboard bridge (WP-56).
//!
//! These tests touch the user's actual clipboard, so they are `#[ignore]`d: run them by hand in
//! an unlocked session with
//!
//! ```text
//! cargo test -p gander-win --test clipboard -- --ignored --test-threads 1 --nocapture
//! ```
//!
//! Each test first saves the clipboard ([`SystemClipboard::snapshot`]) and puts it back when it
//! ends, also when an assertion fails. A clipboard holding anything but text (an image, files,
//! application formats) cannot be put back faithfully, so the test then does nothing at all and
//! says so. A locked or disconnected session skips the test as well: the clipboard is not
//! available there and the bridge would not touch it either.

#![cfg(windows)]

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gander_core::clipboard::{
    BackendError, ClipboardBackend, ClipboardMode, ClipboardService, ClipboardSetting,
    MAX_TEXT_BYTES, RecordingObserver, Request,
};
use gander_core::input::GateState;
use gander_core::protocol::{DeviceId, SessionId};
use gander_win::clipboard::{Snapshot, SystemClipboard};
use windows_sys::Win32::System::DataExchange::{CloseClipboard, OpenClipboard};

/// Puts the user's clipboard back when dropped.
struct Restore {
    clipboard: SystemClipboard,
    snapshot: Snapshot,
}

impl Drop for Restore {
    fn drop(&mut self) {
        if let Err(e) = self.clipboard.restore(&self.snapshot) {
            eprintln!("WARNING: the clipboard could not be restored: {e}");
        }
    }
}

/// The saved clipboard to restore, or `None` (with the reason printed) when the test must not
/// run.
fn guard() -> Option<Restore> {
    let state = gander_win::session::query_session_state();
    if state.as_ref().map_or(true, |s| s.is_locked()) {
        eprintln!("SKIPPED: the session is locked or not interactive ({state:?})");
        return None;
    }
    let clipboard = SystemClipboard::default();
    match clipboard.snapshot() {
        Ok(Snapshot::Unrestorable) => {
            eprintln!("SKIPPED: the clipboard holds more than text, which cannot be put back");
            None
        }
        Ok(snapshot) => Some(Restore {
            clipboard,
            snapshot,
        }),
        Err(e) => {
            eprintln!("SKIPPED: the clipboard could not be read ({e})");
            None
        }
    }
}

#[test]
#[ignore = "touches the real Windows clipboard; run with --ignored in an unlocked session"]
fn set_and_get_round_trip_with_line_ends_and_unicode() {
    let Some(saved) = guard() else { return };
    let cb = saved.clipboard;

    let text = "gander clipboard test\nsecond line\n\u{e9}\u{4e2d}\u{1F600}";
    cb.set_text(text).expect("set");
    assert_eq!(cb.get_text().expect("get").as_deref(), Some(text));
    // Windows stores `\r\n`: another application sees proper line ends.
    assert!(
        cb.formats().unwrap().contains(&13),
        "CF_UNICODETEXT is on it"
    );

    // Replacing, not appending.
    cb.set_text("short").expect("set");
    assert_eq!(cb.get_text().unwrap().as_deref(), Some("short"));

    // The cap: 64 KiB goes through whole.
    let big = "0123456789abcde\n".repeat(MAX_TEXT_BYTES / 16);
    cb.set_text(&big).expect("set big");
    assert_eq!(cb.get_text().unwrap().as_deref(), Some(big.as_str()));
}

#[test]
#[ignore = "touches the real Windows clipboard; run with --ignored in an unlocked session"]
fn an_empty_clipboard_reads_as_no_text() {
    let Some(saved) = guard() else { return };
    saved
        .clipboard
        .restore(&Snapshot::Empty)
        .expect("empty the clipboard");
    assert_eq!(saved.clipboard.get_text().unwrap(), None);
    assert!(saved.clipboard.formats().unwrap().is_empty());
}

#[test]
#[ignore = "touches the real Windows clipboard; run with --ignored in an unlocked session"]
fn a_clipboard_held_open_by_another_thread_is_retried_then_reported_busy() {
    let Some(saved) = guard() else { return };
    let cb = SystemClipboard {
        attempts: 4,
        delay: Duration::from_millis(25),
    };

    // Another thread (another "task" for the clipboard) holds it open.
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        // SAFETY: opens and closes the clipboard on this one thread.
        let opened = unsafe { OpenClipboard(std::ptr::null_mut()) } != 0;
        held_tx.send(opened).unwrap();
        let _ = release_rx.recv();
        if opened {
            // SAFETY: closes what was opened above.
            unsafe { CloseClipboard() };
        }
    });
    assert!(held_rx.recv().unwrap(), "the holder opened the clipboard");

    let started = Instant::now();
    assert_eq!(cb.get_text(), Err(BackendError::Busy));
    assert_eq!(cb.set_text("x"), Err(BackendError::Busy));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the retries are bounded: {:?}",
        started.elapsed()
    );

    // A short hold is waited out.
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    cb.set_text("after the hold").expect("free again");
    assert_eq!(cb.get_text().unwrap().as_deref(), Some("after the hold"));
    drop(saved);
}

#[test]
#[ignore = "touches the real Windows clipboard; run with --ignored in an unlocked session"]
fn the_service_over_the_real_clipboard_sets_gets_and_refuses() {
    let Some(saved) = guard() else { return };
    let observer = Arc::new(RecordingObserver::default());
    let setting = ClipboardSetting::new(ClipboardMode::Both);
    let service =
        ClipboardService::new(setting.clone(), Arc::new(saved.clipboard), observer.clone());
    let (s, d) = (SessionId::from("ses_real"), DeviceId::from("dev_real"));

    let ack = service.serve(
        &s,
        &d,
        GateState::Active,
        &Request::Set("from the phone\nsecond".into()),
    );
    assert_eq!(ack, r#"{"t":"clipboard.ack","bytes":21}"#);
    assert_eq!(
        saved.clipboard.get_text().unwrap().as_deref(),
        Some("from the phone\nsecond")
    );
    let got = service.serve(&s, &d, GateState::Active, &Request::Get);
    assert_eq!(got, r#"{"t":"clipboard","s":"from the phone\nsecond"}"#);

    // Off: refused, and the clipboard is left alone.
    setting.set(ClipboardMode::Off);
    let refused = service.serve(&s, &d, GateState::Active, &Request::Set("nope".into()));
    assert!(refused.contains("disabled"), "{refused}");
    assert_eq!(
        saved.clipboard.get_text().unwrap().as_deref(),
        Some("from the phone\nsecond")
    );
    assert_eq!(observer.events().len(), 3);
}
