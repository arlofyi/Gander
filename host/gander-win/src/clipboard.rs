// SPDX-License-Identifier: Apache-2.0
//! The Windows text clipboard for the Stream clipboard bridge (WP-56): `OpenClipboard`,
//! `GetClipboardData(CF_UNICODETEXT)` and `SetClipboardData`, behind
//! [`gander_core::clipboard::ClipboardBackend`].
//!
//! - **Text only.** Only `CF_UNICODETEXT` is read or written. A clipboard that holds an image,
//!   files or anything else reads as "no text"; writing replaces it (the clipboard has one
//!   content, like any copy).
//! - **Busy clipboard.** Another application may hold the clipboard open for a moment, and
//!   `OpenClipboard` then fails with `ERROR_ACCESS_DENIED`. Opening is retried
//!   ([`SystemClipboard::attempts`] times, [`SystemClipboard::delay`] apart, about a quarter of a
//!   second by default) and then reported as [`BackendError::Busy`]; it never waits for ever.
//! - **A message-only owner window.** `OpenClipboard(NULL)` makes `EmptyClipboard` set the owner
//!   to nothing, which the documentation says makes `SetClipboardData` fail. Each operation
//!   therefore creates a hidden message-only window (class `STATIC`, no message pump needed
//!   because no delayed rendering is used), opens the clipboard for it and destroys it again.
//! - **Line ends.** The clipboard text uses `\r\n`; the bridge's text uses `\n`. Reading converts
//!   to `\n`, writing converts to `\r\n` ([`gander_core::clipboard::to_lf`] / `to_crlf`).
//! - **Size.** A clipboard text that cannot fit [`gander_core::clipboard::MAX_TEXT_BYTES`] is
//!   rejected from its allocation size before it is copied, so a huge clipboard costs nothing.
//! - Nothing here logs or returns clipboard content except the text handed back to the caller.

use std::ptr;
use std::thread;
use std::time::Duration;

use gander_core::clipboard::{BackendError, ClipboardBackend, MAX_TEXT_BYTES, to_crlf, to_lf};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, GetLastError, GlobalFree, HANDLE, HWND, SetLastError,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE};

/// `CF_TEXT`.
const CF_TEXT: u32 = 1;
/// `CF_OEMTEXT`.
const CF_OEMTEXT: u32 = 7;
/// `CF_UNICODETEXT`.
const CF_UNICODETEXT: u32 = 13;
/// `CF_LOCALE`: Windows adds it next to the text formats.
const CF_LOCALE: u32 = 16;

/// The most UTF-16 bytes a text of [`MAX_TEXT_BYTES`] UTF-8 bytes can need (ASCII: two bytes a
/// character), plus the terminator. A bigger allocation cannot hold an acceptable text (CRLF line
/// ends add up to one character per line, so the limit is generous).
const MAX_UTF16_BYTES: usize = 2 * MAX_TEXT_BYTES + 2 + 2 * 4096;

/// The system clipboard (module documentation). Cheap, holds no handle between calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemClipboard {
    /// How many times `OpenClipboard` is tried.
    pub attempts: u32,
    /// How long between two tries.
    pub delay: Duration,
}

impl Default for SystemClipboard {
    fn default() -> Self {
        Self {
            attempts: 10,
            delay: Duration::from_millis(25),
        }
    }
}

/// The clipboard as it was, for tests and diagnostics that must leave the user's clipboard as
/// they found it ([`SystemClipboard::snapshot`], [`SystemClipboard::restore`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Snapshot {
    /// Nothing on it.
    Empty,
    /// Only text: the exact UTF-16 units (line ends untouched, no terminator).
    Text(Vec<u16>),
    /// Anything this module cannot put back (an image, files, application formats). Restoring
    /// does nothing, so a caller should not have emptied the clipboard in the first place.
    Unrestorable,
}

/// A clipboard held open; closes it and destroys the owner window on drop.
struct Open {
    owner: HWND,
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: closes the clipboard this thread opened; harmless if it were not open.
        unsafe { CloseClipboard() };
        if !self.owner.is_null() {
            // SAFETY: the window was created by this thread in `create_owner` and is destroyed
            // exactly once, here.
            unsafe { DestroyWindow(self.owner) };
        }
    }
}

/// `GetLastError` of this thread.
fn last_error() -> u32 {
    // SAFETY: plain getter.
    unsafe { GetLastError() }
}

/// Frees a global block that was never handed to the clipboard (a failed `SetClipboardData`, or
/// an error before it): it is still ours.
fn free(block: HANDLE) {
    // SAFETY: `block` came from `GlobalAlloc` in this module, is unlocked and is freed once.
    unsafe { GlobalFree(block) };
}

/// A hidden message-only window to own the clipboard, or null when none could be made (the
/// clipboard then opens with no owner, which still reads and, in practice, writes).
fn create_owner() -> HWND {
    let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
    // SAFETY: a predefined class, no parent but HWND_MESSAGE, no instance or parameter; the
    // class name is a NUL-terminated UTF-16 string that outlives the call.
    unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        )
    }
}

impl SystemClipboard {
    fn open(&self) -> Result<Open, BackendError> {
        let owner = create_owner();
        let mut last = 0;
        for attempt in 0..self.attempts.max(1) {
            if attempt > 0 {
                thread::sleep(self.delay);
            }
            // SAFETY: `owner` is a window of this thread or null.
            if unsafe { OpenClipboard(owner) } != 0 {
                return Ok(Open { owner });
            }
            // SAFETY: plain getter.
            last = unsafe { GetLastError() };
        }
        if !owner.is_null() {
            // SAFETY: created above and not used by any open clipboard.
            unsafe { DestroyWindow(owner) };
        }
        Err(if last == ERROR_ACCESS_DENIED || last == 0 {
            BackendError::Busy
        } else {
            BackendError::Failed(last)
        })
    }

    /// The clipboard's `CF_UNICODETEXT` as UTF-16 units up to the first NUL (line ends untouched),
    /// `None` when it holds none.
    fn read_units(&self) -> Result<Option<Vec<u16>>, BackendError> {
        let _open = self.open()?;
        // SAFETY: the clipboard is open on this thread.
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) } == 0 {
            return Ok(None);
        }
        // SAFETY: as above; the handle belongs to the clipboard and is only read while open.
        let handle: HANDLE = unsafe { GetClipboardData(CF_UNICODETEXT) };
        if handle.is_null() {
            // Available but not deliverable (a delayed render that failed).
            // SAFETY: plain getter.
            return Err(BackendError::Failed(unsafe { GetLastError() }));
        }
        // SAFETY: `handle` is the clipboard's own global memory block.
        let size = unsafe { GlobalSize(handle) };
        if size > MAX_UTF16_BYTES {
            return Err(BackendError::TooLarge);
        }
        // SAFETY: as above.
        let ptr = unsafe { GlobalLock(handle) }.cast::<u16>();
        if ptr.is_null() {
            // SAFETY: plain getter.
            return Err(BackendError::Failed(unsafe { GetLastError() }));
        }
        // SAFETY: the block is locked and `size` bytes long; `u16` alignment holds for global
        // memory (at least 8 bytes aligned). The copy is made before the unlock.
        let units = unsafe { std::slice::from_raw_parts(ptr, size / 2) };
        let end = units.iter().position(|u| *u == 0).unwrap_or(units.len());
        let copy = units[..end].to_vec();
        // SAFETY: balances the lock above.
        unsafe { GlobalUnlock(handle) };
        Ok(Some(copy))
    }

    /// Replaces the clipboard with `units` (UTF-16, no terminator needed).
    fn write_units(&self, units: &[u16]) -> Result<(), BackendError> {
        let bytes = (units.len() + 1) * 2;
        if bytes > MAX_UTF16_BYTES {
            return Err(BackendError::TooLarge);
        }
        // SAFETY: allocates a movable block; null on failure.
        let block = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
        if block.is_null() {
            // SAFETY: plain getter.
            return Err(BackendError::Failed(unsafe { GetLastError() }));
        }
        // SAFETY: `block` is ours.
        let ptr = unsafe { GlobalLock(block) }.cast::<u16>();
        if ptr.is_null() {
            let e = last_error();
            free(block);
            return Err(BackendError::Failed(e));
        }
        // SAFETY: the block is locked and `units.len() + 1` units long.
        unsafe {
            ptr::copy_nonoverlapping(units.as_ptr(), ptr, units.len());
            ptr.add(units.len()).write(0);
            GlobalUnlock(block);
        }
        let open = match self.open() {
            Ok(o) => o,
            Err(e) => {
                free(block);
                return Err(e);
            }
        };
        // SAFETY: the clipboard is open on this thread.
        if unsafe { EmptyClipboard() } == 0 {
            let e = last_error();
            free(block);
            drop(open);
            return Err(BackendError::Failed(e));
        }
        // SAFETY: on success the system owns `block`; on failure it is still ours to free.
        let set = unsafe { SetClipboardData(CF_UNICODETEXT, block) };
        if set.is_null() {
            let e = last_error();
            free(block);
            drop(open);
            return Err(BackendError::Failed(e));
        }
        drop(open);
        Ok(())
    }

    /// The formats on the clipboard now (diagnostics and [`SystemClipboard::snapshot`]).
    pub fn formats(&self) -> Result<Vec<u32>, BackendError> {
        let _open = self.open()?;
        let mut out = Vec::new();
        let mut format = 0;
        loop {
            // SAFETY: clears this thread's last error so the end of the list can be told from a
            // failure.
            unsafe { SetLastError(0) };
            // SAFETY: as above.
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                // SAFETY: plain getter.
                let e = unsafe { GetLastError() };
                return if e == 0 {
                    Ok(out)
                } else {
                    Err(BackendError::Failed(e))
                };
            }
            out.push(format);
        }
    }

    /// Reads the clipboard so it can be put back with [`SystemClipboard::restore`].
    pub fn snapshot(&self) -> Result<Snapshot, BackendError> {
        let formats = self.formats()?;
        if formats.is_empty() {
            return Ok(Snapshot::Empty);
        }
        if formats
            .iter()
            .any(|f| !matches!(*f, CF_TEXT | CF_OEMTEXT | CF_UNICODETEXT | CF_LOCALE))
        {
            return Ok(Snapshot::Unrestorable);
        }
        Ok(match self.read_units()? {
            Some(units) => Snapshot::Text(units),
            None => Snapshot::Unrestorable,
        })
    }

    /// Puts a [`Snapshot`] back; an [`Snapshot::Unrestorable`] one is left alone.
    pub fn restore(&self, snapshot: &Snapshot) -> Result<(), BackendError> {
        match snapshot {
            Snapshot::Unrestorable => Ok(()),
            Snapshot::Text(units) => self.write_units(units),
            Snapshot::Empty => {
                let _open = self.open()?;
                // SAFETY: the clipboard is open on this thread.
                if unsafe { EmptyClipboard() } == 0 {
                    // SAFETY: plain getter.
                    return Err(BackendError::Failed(unsafe { GetLastError() }));
                }
                Ok(())
            }
        }
    }
}

/// `text` as the clipboard stores it: `\r\n` line ends, UTF-16, no terminator.
pub(crate) fn encode(text: &str) -> Vec<u16> {
    to_crlf(text).encode_utf16().collect()
}

/// UTF-16 units from the clipboard as the bridge's text: `\n` line ends, lone surrogates
/// replaced.
pub(crate) fn decode(units: &[u16]) -> String {
    to_lf(&String::from_utf16_lossy(units)).into_owned()
}

impl ClipboardBackend for SystemClipboard {
    fn get_text(&self) -> Result<Option<String>, BackendError> {
        Ok(self.read_units()?.map(|u| decode(&u)))
    }

    fn set_text(&self, text: &str) -> Result<(), BackendError> {
        self.write_units(&encode(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_stored_with_windows_line_ends_and_read_back_with_unix_ones() {
        let units = encode("one\ntwo\r\nthree");
        assert_eq!(String::from_utf16(&units).unwrap(), "one\r\ntwo\r\nthree");
        assert_eq!(decode(&units), "one\ntwo\nthree");
        assert_eq!(
            decode(&encode("\u{1F600} caf\u{e9}")),
            "\u{1F600} caf\u{e9}"
        );
    }

    #[test]
    fn a_lone_surrogate_does_not_fail_the_read() {
        assert_eq!(decode(&[0x61, 0xD800, 0x62]), "a\u{FFFD}b");
    }

    #[test]
    fn the_allocation_limit_admits_a_full_cap_text_with_line_ends() {
        // 64 KiB of ASCII, every 20th byte a newline.
        let text: String = (0..MAX_TEXT_BYTES)
            .map(|i| if i % 20 == 19 { '\n' } else { 'a' })
            .collect();
        let bytes = (encode(&text).len() + 1) * 2;
        assert!(bytes <= MAX_UTF16_BYTES, "{bytes} > {MAX_UTF16_BYTES}");
    }
}
