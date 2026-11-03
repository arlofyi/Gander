// SPDX-License-Identifier: Apache-2.0
//! Directory change notifications (`ReadDirectoryChangesW`) for the events channel (WP-17).
//!
//! One [`Watcher`] owns an I/O completion port and one thread. [`Watcher::watch`] takes a handle
//! to an ALREADY RESOLVED directory (the host gets it from `path::ShareRoot::resolve`, so the
//! symlink/junction/short-name rules of the path resolver apply), opens it again for overlapped
//! directory listing (`NtCreateFile` relative to it with an empty name: same directory, no path
//! is walked again), and reports what changes inside that one directory (not recursive). The
//! callback receives a [`Batch`] per completed read; it runs on the watcher thread and must
//! return quickly (set a flag, send on a channel).
//!
//! # Lifetime rules (why the code looks the way it does)
//!
//! The kernel writes into the per-watch `OVERLAPPED` and buffer until the completion packet is
//! dequeued, even after `CancelIoEx`. Therefore:
//!
//! - a watch's memory is freed only after its completion was dequeued (or the read was never
//!   issued);
//! - dropping a [`WatchGuard`] while a read is pending cancels it and leaves the memory to the
//!   watcher thread, which frees it when the aborted completion arrives;
//! - the thread outlives every guard (guards hold the watcher) and drains cancelled reads before
//!   it exits; if a completion never arrives within 3 s the memory is leaked on purpose instead
//!   of being freed under the kernel.
//!
//! # Overflow and deletion
//!
//! A read that completes with zero bytes means the buffer overflowed and changes were lost: the
//! batch has `overflow: true` and no changes (treat the whole directory as changed). A failed
//! read (the directory was deleted or the volume went away) ends the watch: the callback gets one
//! final batch with `ended: true`.

use std::collections::HashMap;
use std::io;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_REPARSE_POINT,
    NtCreateFile,
};
use windows_sys::Win32::Foundation::{
    GetLastError, HANDLE, INVALID_HANDLE_VALUE, RtlNtStatusToDosError, UNICODE_STRING, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME,
    FILE_ACTION_RENAMED_OLD_NAME, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES,
    FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, ReadDirectoryChangesW,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, GetQueuedCompletionStatus, IO_STATUS_BLOCK, OVERLAPPED,
    PostQueuedCompletionStatus,
};
use windows_sys::Win32::System::Threading::INFINITE;

/// Bytes of the per-watch notification buffer. At about 80 bytes per record this holds hundreds
/// of changes; more than that is reported as an overflow.
const BUFFER_BYTES: usize = 64 * 1024;

/// What a listing shows: names, kinds, sizes, modification times and attributes.
const FILTER: u32 = FILE_NOTIFY_CHANGE_FILE_NAME
    | FILE_NOTIFY_CHANGE_DIR_NAME
    | FILE_NOTIFY_CHANGE_ATTRIBUTES
    | FILE_NOTIFY_CHANGE_SIZE
    | FILE_NOTIFY_CHANGE_LAST_WRITE
    | FILE_NOTIFY_CHANGE_CREATION;

/// Completion key that asks the thread to finish. Watch keys start at 1.
const SHUTDOWN_KEY: usize = 0;

/// How long the thread waits for cancelled reads to complete at shutdown.
const DRAIN_LIMIT: Duration = Duration::from_secs(3);

/// `FILE_NOTIFY_INFORMATION` header: `NextEntryOffset`, `Action`, `FileNameLength`.
const RECORD_HEADER: usize = 12;

/// What happened to an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Created.
    Added,
    /// Deleted.
    Removed,
    /// Content, size, timestamps or attributes changed.
    Modified,
    /// The old name of a renamed entry.
    RenamedFrom,
    /// The new name of a renamed entry.
    RenamedTo,
    /// An action this code does not know.
    Other(u32),
}

impl Action {
    fn from_raw(action: u32) -> Self {
        match action {
            FILE_ACTION_ADDED => Action::Added,
            FILE_ACTION_REMOVED => Action::Removed,
            FILE_ACTION_MODIFIED => Action::Modified,
            FILE_ACTION_RENAMED_OLD_NAME => Action::RenamedFrom,
            FILE_ACTION_RENAMED_NEW_NAME => Action::RenamedTo,
            other => Action::Other(other),
        }
    }
}

/// One changed entry of the watched directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// What happened.
    pub action: Action,
    /// The entry's name inside the watched directory.
    pub name: String,
}

/// What one completed read reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Batch {
    /// The changes, in the order the OS reported them.
    pub changes: Vec<Change>,
    /// The OS buffer overflowed: changes were lost, treat the whole directory as changed.
    pub overflow: bool,
    /// The watch stopped for good (directory deleted, volume gone); no more batches follow.
    pub ended: bool,
}

type Callback = Box<dyn Fn(&Batch) + Send + Sync + 'static>;

/// Parses the `FILE_NOTIFY_INFORMATION` records `ReadDirectoryChangesW` wrote into `buf`.
///
/// The OS output is trusted no further than its bounds: a record that does not fit, has an odd
/// name length or a non-advancing `NextEntryOffset` ends the parse with what was read so far.
pub fn parse_notifications(buf: &[u8]) -> Vec<Change> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while let Some(header) = offset
        .checked_add(RECORD_HEADER)
        .and_then(|end| buf.get(offset..end))
    {
        let next = u32::from_le_bytes(header[0..4].try_into().expect("4 bytes")) as usize;
        let action = u32::from_le_bytes(header[4..8].try_into().expect("4 bytes"));
        let name_len = u32::from_le_bytes(header[8..12].try_into().expect("4 bytes")) as usize;
        let start = offset + RECORD_HEADER; // in bounds: the header was just read
        let Some(end) = start.checked_add(name_len) else {
            break;
        };
        let Some(raw_name) = buf.get(start..end) else {
            break;
        };
        if !name_len.is_multiple_of(2) {
            break;
        }
        let units: Vec<u16> = raw_name
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        out.push(Change {
            action: Action::from_raw(action),
            name: String::from_utf16_lossy(&units),
        });
        if next == 0 {
            break;
        }
        // The next record must start after this one's header, or the list would not advance.
        if next < RECORD_HEADER {
            break;
        }
        let Some(following) = offset.checked_add(next) else {
            break;
        };
        offset = following;
    }
    out
}

/// The per-watch `OVERLAPPED` and notification buffer. Boxed so the address is stable while the
/// kernel owns it; `u64` elements give the 8-byte alignment `FILE_NOTIFY_INFORMATION` needs.
#[repr(C)]
struct Op {
    overlapped: OVERLAPPED,
    buffer: [u64; BUFFER_BYTES / 8],
}

/// Owning pointer to an [`Op`].
struct OpPtr(NonNull<Op>);

// SAFETY: the pointee is plain data accessed only through the owning `WatchState`: by the thread
// that issued a read (before the kernel owns it), by the kernel while a read is pending, and by
// the watcher thread after the completion. `WatchState::flags` serialises these phases.
unsafe impl Send for OpPtr {}
// SAFETY: as above.
unsafe impl Sync for OpPtr {}

impl OpPtr {
    fn new() -> Self {
        // SAFETY: `Op` is plain data for which all-zero bytes are valid.
        let op: Box<Op> = Box::new(unsafe { std::mem::zeroed() });
        Self(NonNull::from(Box::leak(op)))
    }
}

impl Drop for OpPtr {
    fn drop(&mut self) {
        // SAFETY: allocated by `Box` in `new` and dropped exactly once. The protocol in the module
        // docs guarantees no read is pending when the last reference to the watch goes away
        // (otherwise the watch is leaked, never dropped).
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

#[derive(Debug)]
struct Flags {
    /// The watch should keep re-arming. Cleared by the guard and when the watch ends.
    active: bool,
    /// A `ReadDirectoryChangesW` is outstanding (its completion has not been dequeued).
    pending: bool,
    /// The guard was dropped: no callback may start from now on.
    dropped: bool,
}

struct WatchState {
    handle: OwnedHandle,
    op: OpPtr,
    flags: Mutex<Flags>,
    callback: Callback,
    /// Held while the callback runs, so dropping the guard can wait for a call in flight.
    calling: Mutex<()>,
}

impl WatchState {
    fn flags(&self) -> MutexGuard<'_, Flags> {
        self.flags.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle() as HANDLE
    }

    /// Arms one read. Called with the flags lock held so the completion handler cannot observe
    /// `pending == false` for a read that was just issued.
    fn issue(&self, flags: &mut Flags) -> io::Result<()> {
        let op = self.op.0.as_ptr();
        // SAFETY: no read is pending (callers check), so this thread owns the Op; the pointers are
        // to a live heap allocation that stays valid until the completion is dequeued (module
        // docs). A zeroed OVERLAPPED with a null event completes through the port.
        let ok = unsafe {
            std::ptr::write_bytes(std::ptr::addr_of_mut!((*op).overlapped), 0, 1);
            ReadDirectoryChangesW(
                self.raw(),
                std::ptr::addr_of_mut!((*op).buffer).cast(),
                BUFFER_BYTES as u32,
                0,
                FILTER,
                std::ptr::null_mut(),
                std::ptr::addr_of_mut!((*op).overlapped),
                None,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        flags.pending = true;
        Ok(())
    }

    fn cancel(&self) {
        // SAFETY: a valid handle; a null OVERLAPPED cancels every read on it.
        unsafe { CancelIoEx(self.raw(), std::ptr::null()) };
    }
}

struct Shared {
    port: OwnedHandle,
    watches: Mutex<HashMap<usize, Arc<WatchState>>>,
    next_key: AtomicUsize,
}

impl Shared {
    fn watches(&self) -> MutexGuard<'_, HashMap<usize, Arc<WatchState>>> {
        self.watches.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn port(&self) -> HANDLE {
        self.port.as_raw_handle() as HANDLE
    }

    fn remove(&self, key: usize) {
        self.watches().remove(&key);
    }
}

/// Opens a second handle to the directory `dir` refers to, for ASYNCHRONOUS I/O.
///
/// `ReOpenFile` cannot be used: measured on Windows 11 (build 26200) it fails with
/// `ERROR_ACCESS_DENIED` for every directory handle whatever the access and flags (files work).
/// The same effect comes from `NtCreateFile` with the directory as `RootDirectory` and an EMPTY
/// name, which is how the path resolver hands out a fresh handle to a share root: no path is
/// walked and nothing is followed, so the new handle is the very object that was resolved. The
/// absence of `FILE_SYNCHRONOUS_IO_*` makes it an overlapped handle that can join the port.
fn open_same_directory(dir: BorrowedHandle<'_>) -> io::Result<OwnedHandle> {
    let name = UNICODE_STRING {
        Length: 0,
        MaximumLength: 0,
        Buffer: std::ptr::null_mut(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: dir.as_raw_handle() as HANDLE,
        ObjectName: &name,
        Attributes: 0,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut handle: HANDLE = std::ptr::null_mut();
    let mut iosb = IO_STATUS_BLOCK::default();
    // SAFETY: every pointer refers to a live local that outlives the call; no EA buffer.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
            &attributes,
            &mut iosb,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_OPEN_FOR_BACKUP_INTENT | FILE_OPEN_REPARSE_POINT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        // SAFETY: pure conversion without pointers.
        let code = unsafe { RtlNtStatusToDosError(status) };
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    // SAFETY: NtCreateFile succeeded, so `handle` is a valid handle we now own.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// What a completed read means.
enum Outcome {
    Changes(Vec<Change>),
    Overflow,
    Ended,
}

fn call(callback: &Callback, batch: &Batch) {
    // A panic in a user callback must not kill the thread that serves every watch.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(batch)));
}

/// Runs the callback unless the guard has been dropped. The `calling` lock is taken before the
/// check, so a guard dropped while this runs waits for it (see `WatchGuard::drop`) and a guard
/// dropped before it is seen here: no callback starts after the guard's drop returned.
fn deliver(state: &WatchState, batch: &Batch) {
    let _calling = state.calling.lock().unwrap_or_else(|e| e.into_inner());
    if state.flags().dropped {
        return;
    }
    call(&state.callback, batch);
}

/// Handles one dequeued completion packet.
fn complete(shared: &Shared, key: usize, bytes: u32, error: u32) {
    let Some(state) = shared.watches().get(&key).cloned() else {
        return;
    };
    let outcome = {
        let mut flags = state.flags();
        flags.pending = false;
        if !flags.active {
            // Cancelled by the guard: the aborted completion is the signal that the memory is free.
            drop(flags);
            shared.remove(key);
            return;
        }
        if error != 0 {
            flags.active = false;
            Outcome::Ended
        } else if bytes == 0 {
            Outcome::Overflow
        } else {
            let len = (bytes as usize).min(BUFFER_BYTES);
            // SAFETY: the read completed (`pending` was true until just now), so the kernel no
            // longer writes the buffer; `len` is within it.
            let buf = unsafe {
                std::slice::from_raw_parts(
                    std::ptr::addr_of!((*state.op.0.as_ptr()).buffer).cast::<u8>(),
                    len,
                )
            };
            Outcome::Changes(parse_notifications(buf))
        }
    };
    let ended = matches!(outcome, Outcome::Ended);
    let batch = match outcome {
        Outcome::Changes(changes) => Batch {
            changes,
            ..Batch::default()
        },
        Outcome::Overflow => Batch {
            overflow: true,
            ..Batch::default()
        },
        Outcome::Ended => Batch {
            ended: true,
            ..Batch::default()
        },
    };
    deliver(&state, &batch);
    if ended {
        shared.remove(key);
        return;
    }
    // Re-arm unless the guard was dropped while the callback ran (its drop saw `pending == false`
    // and already removed the watch).
    let rearm_failed = {
        let mut flags = state.flags();
        if !flags.active {
            return;
        }
        match state.issue(&mut flags) {
            Ok(()) => false,
            Err(_) => {
                flags.active = false;
                true
            }
        }
    };
    if rearm_failed {
        deliver(
            &state,
            &Batch {
                ended: true,
                ..Batch::default()
            },
        );
        shared.remove(key);
    }
}

fn run(shared: Arc<Shared>) {
    let mut closing_since: Option<Instant> = None;
    loop {
        let timeout = if closing_since.is_some() {
            100
        } else {
            INFINITE
        };
        let mut bytes = 0u32;
        let mut key = 0usize;
        let mut overlapped: *mut OVERLAPPED = std::ptr::null_mut();
        // SAFETY: valid out-pointers and a live port handle (owned by `shared`).
        let ok = unsafe {
            GetQueuedCompletionStatus(
                shared.port(),
                &mut bytes,
                &mut key,
                &mut overlapped,
                timeout,
            )
        };
        if overlapped.is_null() {
            if ok == 0 {
                // SAFETY: reads the calling thread's last error.
                if unsafe { GetLastError() } != WAIT_TIMEOUT {
                    break; // the port itself failed: give up (leaks below)
                }
            } else if key == SHUTDOWN_KEY {
                closing_since.get_or_insert_with(Instant::now);
            }
        } else {
            let error = if ok == 0 {
                // SAFETY: reads the calling thread's last error.
                unsafe { GetLastError() }
            } else {
                0
            };
            complete(&shared, key, bytes, error);
        }
        if let Some(since) = closing_since
            && (shared.watches().is_empty() || since.elapsed() > DRAIN_LIMIT)
        {
            break;
        }
    }
    // Anything still registered has a read the kernel may yet complete into its buffer: leak it.
    let leftovers: Vec<_> = shared.watches().drain().map(|(_, s)| s).collect();
    for state in leftovers {
        std::mem::forget(state);
    }
}

struct WatcherInner {
    shared: Arc<Shared>,
    /// The watcher thread, on which every callback runs.
    thread_id: std::thread::ThreadId,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for WatcherInner {
    fn drop(&mut self) {
        // SAFETY: a live port; posts the shutdown packet behind any pending completions.
        unsafe {
            PostQueuedCompletionStatus(self.shared.port(), 0, SHUTDOWN_KEY, std::ptr::null());
        }
        let thread = self
            .thread
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(t) = thread
            // Joining oneself (a callback that held the last reference) would deadlock.
            && t.thread().id() != std::thread::current().id()
        {
            let _ = t.join();
        }
    }
}

/// Owns the completion port and the thread that serves every [`WatchGuard`] created from it.
/// Cheap to clone; the thread stops when the last clone and the last guard are gone.
#[derive(Clone)]
pub struct Watcher {
    inner: Arc<WatcherInner>,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher")
            .field("watches", &self.inner.shared.watches().len())
            .finish_non_exhaustive()
    }
}

impl Watcher {
    /// Creates the completion port and starts the thread.
    pub fn new() -> io::Result<Self> {
        // SAFETY: creates a new port (no file handle, no existing port).
        let port =
            unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) };
        if port.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `port` is a fresh handle owned by nobody else.
        let port = unsafe { OwnedHandle::from_raw_handle(port as _) };
        let shared = Arc::new(Shared {
            port,
            watches: Mutex::new(HashMap::new()),
            next_key: AtomicUsize::new(1),
        });
        let thread = std::thread::Builder::new()
            .name("gander-dir-watch".to_owned())
            .spawn({
                let shared = Arc::clone(&shared);
                move || run(shared)
            })?;
        Ok(Self {
            inner: Arc::new(WatcherInner {
                shared,
                thread_id: thread.thread().id(),
                thread: Mutex::new(Some(thread)),
            }),
        })
    }

    /// Watches the directory behind `dir` (a handle opened with `FILE_LIST_DIRECTORY` access, for
    /// example a resolved share directory). `on_batch` runs on the watcher thread for every
    /// completed read; dropping the returned guard stops the watch.
    pub fn watch(
        &self,
        dir: BorrowedHandle<'_>,
        on_batch: impl Fn(&Batch) + Send + Sync + 'static,
    ) -> io::Result<WatchGuard> {
        let shared = &self.inner.shared;
        let handle = open_same_directory(dir)?;
        let raw = handle.as_raw_handle() as HANDLE;
        let key = shared.next_key.fetch_add(1, Ordering::Relaxed);
        // SAFETY: associates the new overlapped handle with the port under `key`.
        let associated = unsafe { CreateIoCompletionPort(raw, shared.port(), key, 0) };
        if associated.is_null() {
            return Err(io::Error::last_os_error());
        }
        let state = Arc::new(WatchState {
            handle,
            op: OpPtr::new(),
            flags: Mutex::new(Flags {
                active: true,
                pending: false,
                dropped: false,
            }),
            callback: Box::new(on_batch),
            calling: Mutex::new(()),
        });
        // Registered before the read is issued so the completion always finds its state.
        shared.watches().insert(key, Arc::clone(&state));
        let issued = {
            let mut flags = state.flags();
            state.issue(&mut flags)
        };
        if let Err(e) = issued {
            shared.remove(key);
            return Err(e);
        }
        Ok(WatchGuard {
            state,
            key,
            watcher: Arc::clone(&self.inner),
        })
    }

    /// Number of watches currently registered (guards alive, or cancelled and not yet drained).
    pub fn watch_count(&self) -> usize {
        self.inner.shared.watches().len()
    }
}

/// A running watch; dropping it stops the watch.
pub struct WatchGuard {
    state: Arc<WatchState>,
    key: usize,
    watcher: Arc<WatcherInner>,
}

impl std::fmt::Debug for WatchGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchGuard")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        {
            let mut flags = self.state.flags();
            flags.dropped = true;
            if flags.active {
                flags.active = false;
                if flags.pending {
                    // The aborted completion tells the watcher thread to free the watch.
                    self.state.cancel();
                } else {
                    drop(flags);
                    self.watcher.shared.remove(self.key);
                }
            }
        }
        // A callback may have passed its `dropped` check just before this drop: wait for it to
        // return, so none runs after the guard is gone. A guard dropped from inside its own
        // callback runs on the watcher thread, which already holds the lock: skip the wait.
        if std::thread::current().id() != self.watcher.thread_id {
            drop(self.state.calling.lock().unwrap_or_else(|e| e.into_inner()));
        }
    }
}

#[cfg(test)]
mod tests;
