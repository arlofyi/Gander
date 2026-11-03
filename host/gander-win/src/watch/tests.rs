// SPDX-License-Identifier: Apache-2.0
//! Tests for [`super`]: record parsing with hostile buffers, then the real thing on a temp dir.

use std::fs::{self, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

use super::*;

// ---------- parsing ----------

fn record(next: u32, action: u32, name: &str) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut v = Vec::new();
    v.extend_from_slice(&next.to_le_bytes());
    v.extend_from_slice(&action.to_le_bytes());
    v.extend_from_slice(&((units.len() * 2) as u32).to_le_bytes());
    for u in units {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

/// Two records, the first padded to a 4-byte boundary like the OS does.
fn two_records() -> Vec<u8> {
    let mut first = record(0, 1, "a.txt");
    while !first.len().is_multiple_of(4) {
        first.push(0);
    }
    let next = first.len() as u32;
    first[0..4].copy_from_slice(&next.to_le_bytes());
    let mut buf = first;
    buf.extend(record(0, 3, "b ü.txt"));
    buf
}

#[test]
fn parses_chained_records() {
    let changes = parse_notifications(&two_records());
    assert_eq!(
        changes,
        vec![
            Change {
                action: Action::Added,
                name: "a.txt".into()
            },
            Change {
                action: Action::Modified,
                name: "b ü.txt".into()
            },
        ]
    );
}

#[test]
fn rename_pair_and_unknown_action() {
    let mut a = record(0, 4, "old.txt");
    while !a.len().is_multiple_of(4) {
        a.push(0);
    }
    let n = a.len() as u32;
    a[0..4].copy_from_slice(&n.to_le_bytes());
    a.extend(record(0, 5, "new.txt"));
    let changes = parse_notifications(&a);
    assert_eq!(changes[0].action, Action::RenamedFrom);
    assert_eq!(changes[1].action, Action::RenamedTo);
    assert_eq!(
        parse_notifications(&record(0, 99, "x"))[0].action,
        Action::Other(99)
    );
}

#[test]
fn hostile_buffers_never_panic_or_loop() {
    // Empty, shorter than a header, header only.
    assert!(parse_notifications(&[]).is_empty());
    assert!(parse_notifications(&[1, 2, 3]).is_empty());
    assert_eq!(parse_notifications(&record(0, 1, "")).len(), 1);
    // Name length beyond the buffer.
    let mut b = record(0, 1, "abc");
    b[8..12].copy_from_slice(&1000u32.to_le_bytes());
    assert!(parse_notifications(&b).is_empty());
    // Name length that overflows usize arithmetic.
    let mut b = record(0, 1, "abc");
    b[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(parse_notifications(&b).is_empty());
    // Odd name length.
    let mut b = record(0, 1, "abc");
    b[8..12].copy_from_slice(&5u32.to_le_bytes());
    assert!(parse_notifications(&b).is_empty());
    // NextEntryOffset pointing at itself or inside the header would loop forever.
    for next in [1u32, 4, 11] {
        let mut b = record(next, 1, "abc");
        b.extend(vec![0u8; 64]);
        assert_eq!(parse_notifications(&b).len(), 1, "next={next}");
    }
    // NextEntryOffset far past the end.
    let b = record(u32::MAX, 1, "abc");
    assert_eq!(parse_notifications(&b).len(), 1);
    // Unpaired surrogate becomes U+FFFD instead of failing.
    let mut b = record(0, 1, "x");
    b[8..12].copy_from_slice(&2u32.to_le_bytes());
    b.truncate(RECORD_HEADER);
    b.extend_from_slice(&0xD800u16.to_le_bytes());
    assert_eq!(parse_notifications(&b)[0].name, "\u{FFFD}");
}

// ---------- the real thing ----------

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        // The process id alone is not unique across runs: a folder an earlier process with a
        // recycled id could not delete (a watch handle was still closing) would be reused, with
        // its old contents. The start time makes the name fresh; `create_dir` proves it.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let p = std::env::temp_dir().join(format!(
            "gander-wp17-watch-{}-{}-{nanos:x}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A directory handle like the resolver's: list access, backup semantics, synchronous I/O.
fn open_dir(path: &Path) -> fs::File {
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .expect("open directory handle")
}

fn collect(watcher: &Watcher, dir: &Path) -> (WatchGuard, Receiver<Batch>) {
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    let file = open_dir(dir);
    let guard = watcher
        .watch(std::os::windows::io::AsHandle::as_handle(&file), move |b| {
            let _ = tx.lock().unwrap().send(b.clone());
        })
        .expect("watch");
    (guard, rx)
}

fn wait_for(rx: &Receiver<Batch>, what: impl Fn(&Change) -> bool) -> Batch {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(b) if b.changes.iter().any(&what) => return b,
            Ok(_) => {}
            Err(_) => panic!("expected change not reported within 2 s"),
        }
    }
}

#[test]
fn reports_create_modify_rename_and_delete() {
    let tmp = TempDir::new();
    let watcher = Watcher::new().unwrap();
    let (_guard, rx) = collect(&watcher, &tmp.0);
    let file = tmp.0.join("a.txt");

    let t0 = Instant::now();
    fs::write(&file, b"one").unwrap();
    wait_for(&rx, |c| c.action == Action::Added && c.name == "a.txt");
    assert!(t0.elapsed() < Duration::from_secs(2));

    fs::write(&file, b"two, longer").unwrap();
    wait_for(&rx, |c| c.action == Action::Modified && c.name == "a.txt");

    fs::rename(&file, tmp.0.join("b.txt")).unwrap();
    let b = wait_for(&rx, |c| c.action == Action::RenamedTo && c.name == "b.txt");
    let from = |c: &Change| c.action == Action::RenamedFrom && c.name == "a.txt";
    if !b.changes.iter().any(from) {
        wait_for(&rx, from);
    }

    fs::remove_file(tmp.0.join("b.txt")).unwrap();
    wait_for(&rx, |c| c.action == Action::Removed && c.name == "b.txt");

    fs::create_dir(tmp.0.join("sub")).unwrap();
    wait_for(&rx, |c| c.action == Action::Added && c.name == "sub");
}

#[test]
fn is_not_recursive_and_directories_are_independent() {
    let tmp = TempDir::new();
    fs::create_dir(tmp.0.join("one")).unwrap();
    fs::create_dir(tmp.0.join("two")).unwrap();
    let watcher = Watcher::new().unwrap();
    let (_g1, rx1) = collect(&watcher, &tmp.0.join("one"));
    let (_g2, rx2) = collect(&watcher, &tmp.0.join("two"));
    assert_eq!(watcher.watch_count(), 2);

    fs::create_dir(tmp.0.join("one").join("deep")).unwrap();
    wait_for(&rx1, |c| c.name == "deep");
    // A change two levels down is not reported to the parent's watch.
    fs::write(tmp.0.join("one").join("deep").join("f.txt"), b"x").unwrap();
    fs::write(tmp.0.join("two").join("t.txt"), b"x").unwrap();
    wait_for(&rx2, |c| c.name == "t.txt");
    std::thread::sleep(Duration::from_millis(200));
    while let Ok(b) = rx1.try_recv() {
        assert!(
            !b.changes.iter().any(|c| c.name.contains("f.txt")),
            "subtree change leaked into a non-recursive watch: {b:?}"
        );
    }
    assert!(
        rx2.try_iter()
            .all(|b| b.changes.iter().all(|c| c.name != "deep")),
        "directories must not see each other's changes"
    );
}

#[test]
fn no_callbacks_after_the_guard_is_dropped_and_memory_is_drained() {
    let tmp = TempDir::new();
    let watcher = Watcher::new().unwrap();
    let (guard, rx) = collect(&watcher, &tmp.0);
    fs::write(tmp.0.join("a.txt"), b"x").unwrap();
    wait_for(&rx, |c| c.name == "a.txt");
    drop(guard);
    // The cancelled read completes on the watcher thread, which then frees the watch.
    let deadline = Instant::now() + Duration::from_secs(2);
    while watcher.watch_count() != 0 {
        assert!(Instant::now() < deadline, "cancelled watch never drained");
        std::thread::sleep(Duration::from_millis(5));
    }
    while rx.try_recv().is_ok() {}
    fs::write(tmp.0.join("late.txt"), b"x").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        rx.try_recv().is_err(),
        "callback after the guard was dropped"
    );
}

#[test]
fn churn_of_create_and_cancel_is_safe() {
    let tmp = TempDir::new();
    let watcher = Watcher::new().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    for i in 0..60 {
        let file = open_dir(&tmp.0);
        let hits = Arc::clone(&hits);
        let guard = watcher
            .watch(
                std::os::windows::io::AsHandle::as_handle(&file),
                move |_| {
                    hits.fetch_add(1, Ordering::Relaxed);
                },
            )
            .unwrap();
        if i % 3 == 0 {
            let _ = fs::write(tmp.0.join(format!("f{i}.txt")), b"x");
        }
        drop(guard);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while watcher.watch_count() != 0 {
        assert!(
            Instant::now() < deadline,
            "watches left: {}",
            watcher.watch_count()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn guards_keep_the_watcher_alive_and_dropping_everything_joins_the_thread() {
    let tmp = TempDir::new();
    let (guard, rx) = {
        let watcher = Watcher::new().unwrap();
        collect(&watcher, &tmp.0)
        // `watcher` dropped here; the guard still holds the thread alive.
    };
    fs::write(tmp.0.join("a.txt"), b"x").unwrap();
    wait_for(&rx, |c| c.name == "a.txt");
    let t0 = Instant::now();
    drop(guard); // last reference: cancels, drains, joins
    assert!(t0.elapsed() < Duration::from_secs(2));
}

#[test]
fn deleting_the_watched_directory_does_not_hang_and_cleans_up() {
    let tmp = TempDir::new();
    let dir = tmp.0.join("victim");
    fs::create_dir(&dir).unwrap();
    let watcher = Watcher::new().unwrap();
    let (guard, rx) = collect(&watcher, &dir);
    fs::write(dir.join("f.txt"), b"x").unwrap();
    wait_for(&rx, |c| c.name == "f.txt");
    fs::remove_file(dir.join("f.txt")).unwrap();
    let _ = fs::remove_dir(&dir);
    // Whatever the OS reports (an ended watch or nothing), dropping the guard must finish and
    // nothing may be left registered.
    let ended = rx.try_iter().any(|b| b.ended)
        || matches!(rx.recv_timeout(Duration::from_millis(500)), Ok(b) if b.ended);
    eprintln!("WP-17 watched directory deleted: ended batch observed = {ended}");
    drop(guard);
    let deadline = Instant::now() + Duration::from_secs(2);
    while watcher.watch_count() != 0 {
        assert!(Instant::now() < deadline, "watch left registered");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_panicking_callback_does_not_stop_other_watches() {
    let tmp = TempDir::new();
    fs::create_dir(tmp.0.join("bad")).unwrap();
    fs::create_dir(tmp.0.join("good")).unwrap();
    let watcher = Watcher::new().unwrap();
    let bad = open_dir(&tmp.0.join("bad"));
    let _bad_guard = watcher
        .watch(std::os::windows::io::AsHandle::as_handle(&bad), |_| {
            panic!("callback bug")
        })
        .unwrap();
    let (_good_guard, rx) = collect(&watcher, &tmp.0.join("good"));
    fs::write(tmp.0.join("bad").join("x.txt"), b"x").unwrap();
    fs::write(tmp.0.join("good").join("y.txt"), b"x").unwrap();
    wait_for(&rx, |c| c.name == "y.txt");
    // And the panicking watch keeps working (re-armed after the caught panic).
    fs::write(tmp.0.join("good").join("z.txt"), b"x").unwrap();
    wait_for(&rx, |c| c.name == "z.txt");
}

#[allow(dead_code)]
fn _types(_: Sender<()>) {}
