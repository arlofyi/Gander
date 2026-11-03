// SPDX-License-Identifier: Apache-2.0
//! One host per data directory and Windows session.
//!
//! The Start menu entry and the sign-in task both start `gander-host.exe`. Without a guard a
//! second start would run a second tray icon and a second copy of the UI beside the first (its
//! listener would fail to bind and retry quietly, because the sockets are exclusive). So the host
//! takes a named mutex first; a host that finds it taken does not start. If it was started with
//! `--open` it first tells the running host to open its settings window, through a named event,
//! which is what makes "Gander" in the Start menu work while the tray application already runs.
//!
//! The names live in the session's `Local\` namespace and contain a hash of the data directory, so
//! hosts with different `--data-dir`s (tests, a second profile) do not see each other. Nothing is
//! written to disk and nothing is sent over the network.

use std::path::Path;

/// Whether this process is the host for its data directory.
#[derive(Debug)]
pub enum Instance {
    /// Nobody else is running: keep the guard for as long as the host runs.
    First(Guard),
    /// Another host holds the data directory.
    Running,
}

/// The name part shared by the mutex and the event: a short hash of the normalised data directory.
pub fn key(data_dir: &Path) -> String {
    let normalised = data_dir
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase();
    let hash = blake3::hash(normalised.as_bytes());
    hash.as_bytes()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(windows)]
pub use imp::{Guard, acquire, signal_open};

#[cfg(not(windows))]
pub use stub::{Guard, acquire, signal_open};

#[cfg(windows)]
mod imp {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
        WaitForSingleObject,
    };

    use super::{Instance, key};

    fn wide(name: &str) -> Vec<u16> {
        name.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn mutex_name(data_dir: &Path) -> Vec<u16> {
        wide(&format!("Local\\Gander.Host.{}", key(data_dir)))
    }

    fn event_name(data_dir: &Path) -> Vec<u16> {
        wide(&format!("Local\\Gander.Host.{}.Open", key(data_dir)))
    }

    /// A kernel handle that may be used from another thread (named events are thread safe).
    #[derive(Clone, Copy)]
    struct SendHandle(HANDLE);

    // SAFETY: a kernel object handle is a process-wide token; the calls made with it here
    // (`WaitForSingleObject`, `SetEvent`, `CloseHandle`) are safe from any thread.
    unsafe impl Send for SendHandle {}
    // SAFETY: as above; the handle is only read.
    unsafe impl Sync for SendHandle {}

    /// Holds the data directory for this process: the mutex, and the event other starts signal.
    /// Dropping it (the host exiting) releases both.
    #[derive(Debug)]
    pub struct Guard {
        mutex: SendHandle,
        event: SendHandle,
        stop: Arc<AtomicBool>,
    }

    impl SendHandle {
        /// The raw handle. Reading it through a method makes a closure capture the whole wrapper
        /// (and so its `Send`), not the raw pointer field.
        fn get(self) -> HANDLE {
            self.0
        }
    }

    impl std::fmt::Debug for SendHandle {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("HANDLE")
        }
    }

    impl Guard {
        /// Calls `on_open` (on a background thread) each time another start asks this host to open
        /// its window. The thread ends when the guard is dropped.
        pub fn on_open(&self, on_open: impl Fn() + Send + 'static) {
            let event = self.event;
            let stop = Arc::clone(&self.stop);
            std::thread::Builder::new()
                .name("gander-open-requests".to_owned())
                .spawn(move || {
                    loop {
                        // SAFETY: `event` stays open until the guard is dropped, which sets
                        // `stop` and signals the event before closing it; this thread then ends
                        // without touching the handle again.
                        let waited = unsafe { WaitForSingleObject(event.get(), INFINITE) };
                        if stop.load(Ordering::SeqCst) || waited != WAIT_OBJECT_0 {
                            return;
                        }
                        on_open();
                    }
                })
                .map(drop)
                .unwrap_or_else(|e| eprintln!("could not start the open-request thread: {e}"));
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // SAFETY: both handles were created by `acquire` and are closed exactly once, here.
            // Signalling first wakes the watcher thread so it sees `stop` and ends. The thread
            // may still be between its wait and its stop check; it never uses the handle after
            // that check, and a closed handle only makes the wait return an error.
            unsafe {
                SetEvent(self.event.0);
                CloseHandle(self.event.0);
                CloseHandle(self.mutex.0);
            }
        }
    }

    /// Takes the data directory, or reports that another host has it. If the mutex cannot be
    /// created at all (which does not happen in a normal session) the host starts anyway: not
    /// being able to guard against a second copy is no reason to refuse the first.
    pub fn acquire(data_dir: &Path) -> Instance {
        let name = mutex_name(data_dir);
        // SAFETY: `name` is a NUL-terminated wide string that outlives the call; a null security
        // attribute pointer means the default.
        let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        // SAFETY: reads the calling thread's last error, directly after the call above.
        let last_error = unsafe { GetLastError() };
        if mutex.is_null() {
            return Instance::First(Guard::detached());
        }
        if last_error == ERROR_ALREADY_EXISTS {
            // SAFETY: `mutex` is the handle just returned for the existing mutex.
            unsafe { CloseHandle(mutex) };
            return Instance::Running;
        }
        let event_name = event_name(data_dir);
        // SAFETY: as for the mutex; auto-reset, initially not signalled.
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, event_name.as_ptr()) };
        Instance::First(Guard {
            mutex: SendHandle(mutex),
            event: SendHandle(event),
            stop: Arc::new(AtomicBool::new(false)),
        })
    }

    impl Guard {
        /// A guard that holds nothing, for the case where the mutex could not be created.
        fn detached() -> Self {
            Self {
                mutex: SendHandle(std::ptr::null_mut()),
                // A null event makes `WaitForSingleObject` fail at once, so `on_open` ends.
                event: SendHandle(std::ptr::null_mut()),
                stop: Arc::new(AtomicBool::new(true)),
            }
        }
    }

    /// Asks the running host for this data directory to open its window. `false` if there is none.
    pub fn signal_open(data_dir: &Path) -> bool {
        let name = event_name(data_dir);
        // SAFETY: `name` is a NUL-terminated wide string that outlives the call.
        let event = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
        if event.is_null() {
            return false;
        }
        // SAFETY: `event` was just opened with the right to set it, and is closed once.
        let set = unsafe { SetEvent(event) };
        // SAFETY: see above.
        unsafe { CloseHandle(event) };
        set != 0
    }
}

#[cfg(not(windows))]
mod stub {
    use std::path::Path;

    use super::Instance;

    /// There is no guard on this platform yet.
    #[derive(Debug)]
    pub struct Guard;

    impl Guard {
        /// Never called back: nothing can ask.
        pub fn on_open(&self, _on_open: impl Fn() + Send + 'static) {}
    }

    /// Always the first: the other hosts do not exist yet.
    pub fn acquire(_data_dir: &Path) -> Instance {
        Instance::First(Guard)
    }

    /// There is never another host to ask.
    pub fn signal_open(_data_dir: &Path) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A data directory name no other process uses, so the tests never meet a real host.
    fn unique_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        std::path::PathBuf::from(format!(
            "C:/gander-single-instance-test/{tag}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn the_key_ignores_case_slashes_and_a_trailing_separator() {
        assert_eq!(
            key(Path::new("C:\\Users\\A\\Gander\\")),
            key(Path::new("c:/users/a/gander"))
        );
        assert_ne!(key(Path::new("C:/a")), key(Path::new("C:/b")));
        assert_eq!(key(Path::new("C:/a")).len(), 16);
    }

    #[cfg(windows)]
    #[test]
    fn a_second_start_finds_the_first_and_a_free_directory_is_free() {
        let dir = unique_dir("second");
        let first = acquire(&dir);
        assert!(matches!(first, Instance::First(_)));
        assert!(matches!(acquire(&dir), Instance::Running));
        // Another directory is its own host.
        assert!(matches!(acquire(&unique_dir("other")), Instance::First(_)));
        // Releasing the first frees the directory.
        drop(first);
        assert!(matches!(acquire(&dir), Instance::First(_)));
    }

    #[cfg(windows)]
    #[test]
    fn asking_to_open_reaches_the_running_host() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Duration, Instant};

        let dir = unique_dir("open");
        assert!(!signal_open(&dir), "nobody to ask yet");
        let Instance::First(guard) = acquire(&dir) else {
            panic!("the directory is free");
        };
        let opened = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&opened);
        guard.on_open(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        assert!(signal_open(&dir));
        let deadline = Instant::now() + Duration::from_secs(5);
        while opened.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(opened.load(Ordering::SeqCst), 1);
        drop(guard);
        assert!(!signal_open(&dir), "the event is gone with the host");
    }
}
