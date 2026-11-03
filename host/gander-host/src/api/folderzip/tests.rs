// SPDX-License-Identifier: Apache-2.0
//! The walk, the limits, the skipped note and the writer against a real folder on disk.

use std::fs;
use std::io::{Cursor, Read as _};
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use gander_core::path::HiddenPatterns;

use super::*;

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("gander-wp68-{}", ulid::Ulid::new()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        // remove_dir_all does not follow junctions, so nothing outside is touched.
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn junction(link: &Path, target: &Path) -> bool {
    Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn limits() -> Limits {
    Limits {
        max_bytes: 1 << 30,
        max_entries: 1000,
        budget: Duration::from_secs(30),
    }
}

fn open_root(dir: &Path, show_hidden: bool, patterns: &[&str]) -> ShareRoot {
    let patterns: Vec<String> = patterns.iter().map(|p| (*p).to_owned()).collect();
    ShareRoot::open(dir, show_hidden, HiddenPatterns::new(&patterns).unwrap()).unwrap()
}

fn plan_at(root: &ShareRoot, path: &str, limits: Limits) -> Result<Plan, WalkError> {
    let start = root.resolve(path.as_bytes()).unwrap();
    walk(root, &start, path, limits, &AtomicBool::new(false))
}

/// `base/tree` plus an `outside` folder a junction points at (when `mklink /J` works).
struct Fixture {
    tmp: Tmp,
    junction: bool,
}

impl Fixture {
    fn new() -> Self {
        let tmp = Tmp::new();
        let tree = tmp.path("tree");
        let outside = tmp.path("outside");
        fs::create_dir_all(tree.join("empty")).unwrap();
        fs::create_dir_all(tree.join("sub/deeper")).unwrap();
        fs::create_dir_all(tree.join("private")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(tree.join("a.txt"), b"alpha").unwrap();
        fs::write(tree.join("B.txt"), b"bravo!").unwrap();
        fs::write(tree.join("sub/c.bin"), vec![7u8; 100]).unwrap();
        fs::write(tree.join("sub/deeper/d.txt"), b"d").unwrap();
        fs::write(tree.join("private/secret.txt"), b"hidden by pattern").unwrap();
        fs::write(tree.join("desktop.ini"), b"[.ShellClassInfo]").unwrap();
        // FILE_ATTRIBUTE_HIDDEN
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .attributes(0x2)
            .open(tree.join(".hidden.txt"))
            .unwrap()
            .set_len(0)
            .unwrap();
        fs::write(outside.join("secret.txt"), b"outside the share").unwrap();
        let junction = junction(&tree.join("link"), &outside);
        Self { tmp, junction }
    }

    fn tree(&self) -> PathBuf {
        self.tmp.path("tree")
    }

    fn root(&self) -> ShareRoot {
        open_root(&self.tree(), false, &["**/private"])
    }
}

fn rels(plan: &Plan) -> Vec<&str> {
    plan.entries.iter().map(|e| e.rel.as_str()).collect()
}

#[test]
fn walk_applies_the_listing_rules() {
    let fx = Fixture::new();
    let plan = plan_at(&fx.root(), "", limits()).unwrap();
    // Sorted case-insensitively, each folder before its contents; no hidden file, no
    // desktop.ini, nothing under the hidden pattern, nothing behind the junction.
    assert_eq!(
        rels(&plan),
        [
            "a.txt",
            "B.txt",
            "empty",
            "sub",
            "sub/c.bin",
            "sub/deeper",
            "sub/deeper/d.txt"
        ]
    );
    assert_eq!(plan.bytes, 5 + 6 + 100 + 1);
    assert_eq!(plan.cloud_only, 0);
    let dirs: Vec<bool> = plan.entries.iter().map(|e| e.dir).collect();
    assert_eq!(dirs, [false, false, true, true, false, true, false]);
    assert_eq!(plan.entries[4].path, "sub/c.bin");
    assert_eq!(plan.entries[4].size, 100);
    if fx.junction {
        assert_eq!(plan.skipped.list, [("link".to_owned(), Skip::Link)]);
        assert_eq!(plan.skipped.total, 1);
    } else {
        println!("SKIPPED  junction part: mklink /J failed");
        assert_eq!(plan.skipped.total, 0);
    }
}

#[test]
fn walk_of_a_subfolder_uses_paths_below_it() {
    let fx = Fixture::new();
    let plan = plan_at(&fx.root(), "sub", limits()).unwrap();
    assert_eq!(rels(&plan), ["c.bin", "deeper", "deeper/d.txt"]);
    assert_eq!(plan.entries[0].path, "sub/c.bin");
    assert_eq!(plan.skipped.total, 0);
}

#[test]
fn show_hidden_adds_hidden_files_but_not_always_omitted_ones() {
    let fx = Fixture::new();
    let root = open_root(&fx.tree(), true, &["**/private"]);
    let plan = plan_at(&root, "", limits()).unwrap();
    assert!(rels(&plan).contains(&".hidden.txt"));
    assert!(!rels(&plan).contains(&"desktop.ini"));
    assert!(!rels(&plan).iter().any(|r| r.starts_with("private")));
}

#[test]
fn limits_are_inclusive_and_checked_before_streaming() {
    let fx = Fixture::new();
    let root = fx.root();
    // 7 entries, 112 bytes: exactly at the limits is fine, one under is refused.
    let at = |max_bytes, max_entries| Limits {
        max_bytes,
        max_entries,
        budget: Duration::from_secs(30),
    };
    assert!(plan_at(&root, "", at(112, 7)).is_ok());
    assert_eq!(
        plan_at(&root, "", at(112, 6)).unwrap_err(),
        WalkError::Over(Over::Entries)
    );
    assert_eq!(
        plan_at(&root, "", at(111, 7)).unwrap_err(),
        WalkError::Over(Over::Bytes)
    );
    assert_eq!(
        plan_at(&root, "", at(0, 7)).unwrap_err(),
        WalkError::Over(Over::Bytes)
    );
}

#[test]
fn walk_stops_at_the_limit_without_reading_the_rest() {
    // A wide folder: the walk must give up at max_entries + 1, not enumerate and count it all.
    let tmp = Tmp::new();
    let dir = tmp.path("wide");
    fs::create_dir_all(&dir).unwrap();
    for i in 0..200 {
        fs::write(dir.join(format!("f{i:03}.txt")), b"x").unwrap();
    }
    let root = open_root(&dir, false, &[]);
    let at = |max_entries| Limits {
        max_bytes: 1 << 30,
        max_entries,
        budget: Duration::from_secs(30),
    };
    assert_eq!(
        plan_at(&root, "", at(199)).unwrap_err(),
        WalkError::Over(Over::Entries)
    );
    assert_eq!(plan_at(&root, "", at(200)).unwrap().entries.len(), 200);
}

#[test]
fn walk_budget_and_cancel() {
    let fx = Fixture::new();
    let root = fx.root();
    let no_time = Limits {
        budget: Duration::ZERO,
        ..limits()
    };
    assert_eq!(
        plan_at(&root, "", no_time).unwrap_err(),
        WalkError::Over(Over::Time)
    );
    let start = root.resolve(b"").unwrap();
    let cancel = AtomicBool::new(true);
    assert_eq!(
        walk(&root, &start, "", limits(), &cancel).unwrap_err(),
        WalkError::Cancelled
    );
}

struct Archive {
    bytes: Vec<u8>,
    written: Written,
}

fn archive(root: &ShareRoot, plan: &Plan, folder: &str) -> Archive {
    let mut written = Written::default();
    let bug = |_: &str, _: RejectReason| panic!("resolver bug reported");
    let cancel = AtomicBool::new(false);
    let job = ZipJob {
        root,
        plan,
        folder,
        folder_mtime: Timestamp::from_unix_timestamp(1_700_000_000).unwrap(),
        local: time::UtcOffset::UTC,
        cancel: &cancel,
        on_bug: &bug,
    };
    let bytes = job.write(Vec::new(), &mut written).unwrap();
    Archive { bytes, written }
}

fn read_all(bytes: Vec<u8>) -> std::collections::BTreeMap<String, Option<Vec<u8>>> {
    let mut ar = ::zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut out = std::collections::BTreeMap::new();
    for i in 0..ar.len() {
        let mut f = ar.by_index(i).unwrap();
        let name = f.name().unwrap().into_owned();
        if f.is_dir() {
            out.insert(name, None);
        } else {
            let mut v = Vec::new();
            f.read_to_end(&mut v).unwrap();
            out.insert(name, Some(v));
        }
    }
    out
}

#[test]
fn archive_matches_the_tree_and_names_skipped_items() {
    let fx = Fixture::new();
    let root = fx.root();
    let plan = plan_at(&root, "", limits()).unwrap();
    let a = archive(&root, &plan, "tree");
    assert_eq!(
        a.written,
        Written {
            entries: 7,
            bytes: 112
        }
    );
    let got = read_all(a.bytes);
    let mut want: Vec<(&str, Option<Vec<u8>>)> = vec![
        ("tree/", None),
        ("tree/a.txt", Some(b"alpha".to_vec())),
        ("tree/B.txt", Some(b"bravo!".to_vec())),
        ("tree/empty/", None),
        ("tree/sub/", None),
        ("tree/sub/c.bin", Some(vec![7u8; 100])),
        ("tree/sub/deeper/", None),
        ("tree/sub/deeper/d.txt", Some(b"d".to_vec())),
    ];
    if fx.junction {
        want.push(("tree/_gander-skipped.txt", None)); // content checked below
    }
    assert_eq!(
        got.len(),
        want.len(),
        "{:?}",
        got.keys().collect::<Vec<_>>()
    );
    for (name, content) in want {
        let have = got.get(name).unwrap_or_else(|| panic!("missing {name}"));
        if name != "tree/_gander-skipped.txt" {
            assert_eq!(*have, content, "{name}");
        }
    }
    if fx.junction {
        let note = String::from_utf8(got["tree/_gander-skipped.txt"].clone().unwrap()).unwrap();
        assert!(note.contains("link\t"), "{note}");
        assert!(note.contains("junction"), "{note}");
        // Nothing from behind the junction, and no file name other than the skipped one.
        assert!(!note.contains("secret"), "{note}");
        assert!(!got.keys().any(|k| k.contains("secret")));
    }
}

#[test]
fn no_note_when_nothing_was_skipped() {
    let fx = Fixture::new();
    let root = fx.root();
    let plan = plan_at(&root, "sub", limits()).unwrap();
    let a = archive(&root, &plan, "sub");
    let got = read_all(a.bytes);
    assert!(!got.keys().any(|k| k.contains("_gander-skipped")));
    assert_eq!(got["sub/c.bin"], Some(vec![7u8; 100]));
}

#[test]
fn files_that_change_between_the_walk_and_the_read() {
    let fx = Fixture::new();
    let root = fx.root();
    let plan = plan_at(&root, "", limits()).unwrap();
    // Gone, grown and shrunk since the walk.
    fs::remove_file(fx.tree().join("B.txt")).unwrap();
    fs::write(fx.tree().join("a.txt"), b"alpha plus a lot more").unwrap();
    fs::write(fx.tree().join("sub/c.bin"), vec![9u8; 40]).unwrap();
    let a = archive(&root, &plan, "tree");
    let got = read_all(a.bytes);
    assert!(
        !got.contains_key("tree/B.txt"),
        "a vanished file is skipped"
    );
    // Cut at the measured size, never longer than what the walk counted.
    assert_eq!(got["tree/a.txt"], Some(b"alpha".to_vec()));
    // Shorter now: what is there is sent.
    assert_eq!(got["tree/sub/c.bin"], Some(vec![9u8; 40]));
    let note = String::from_utf8(got["tree/_gander-skipped.txt"].clone().unwrap()).unwrap();
    assert!(note.contains("B.txt\twas removed"), "{note}");
    assert!(note.contains("a.txt\tgrew"), "{note}");
    assert_eq!(a.written.bytes, 5 + 40 + 1);
}

#[test]
fn the_note_does_not_overwrite_a_file_of_the_same_name() {
    let fx = Fixture::new();
    fs::write(fx.tree().join("_Gander-Skipped.TXT"), b"mine").unwrap();
    let root = fx.root();
    let plan = plan_at(&root, "", limits()).unwrap();
    assert_eq!(note_name(&plan), "_gander-skipped (2).txt");
    fs::remove_file(fx.tree().join("a.txt")).unwrap(); // now a vanished item exists as well
    let a = archive(&root, &plan, "tree");
    let got = read_all(a.bytes);
    assert_eq!(got["tree/_Gander-Skipped.TXT"], Some(b"mine".to_vec()));
    assert!(got.contains_key("tree/_gander-skipped (2).txt"));
}

#[test]
fn a_cancelled_write_stops() {
    let fx = Fixture::new();
    let root = fx.root();
    let plan = plan_at(&root, "", limits()).unwrap();
    let cancel = AtomicBool::new(true);
    let bug = |_: &str, _: RejectReason| {};
    let job = ZipJob {
        root: &root,
        plan: &plan,
        folder: "tree",
        folder_mtime: Timestamp::UNIX_EPOCH,
        local: time::UtcOffset::UTC,
        cancel: &cancel,
        on_bug: &bug,
    };
    let mut written = Written::default();
    let e = job.write(Vec::new(), &mut written).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(written.bytes, 0);
}

#[test]
fn a_closed_body_ends_the_writer() {
    let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(1);
    let mut w = ChannelWriter::new(tx, Arc::new(AtomicBool::new(false)));
    drop(rx);
    let big = vec![0u8; CHUNK + 1];
    let e = w.write(&big).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::BrokenPipe);
    // The cancel flag does the same without a send.
    let (tx, _rx) = mpsc::channel::<io::Result<Bytes>>(1);
    let cancel = Arc::new(AtomicBool::new(true));
    let mut w = ChannelWriter::new(tx, cancel);
    assert_eq!(w.write(b"x").unwrap_err().kind(), io::ErrorKind::BrokenPipe);
}

#[test]
fn folder_names_are_cleaned_for_the_archive() {
    assert_eq!(archive_folder_name("Projects"), "Projects");
    assert_eq!(archive_folder_name("Q1: plans/2026"), "Q1_ plans_2026");
    assert_eq!(archive_folder_name("..."), "folder");
    assert_eq!(archive_folder_name("  "), "folder");
    assert_eq!(archive_folder_name("\u{65e5}\u{672c}"), "\u{65e5}\u{672c}");
}

#[test]
fn size_limit_texts() {
    assert_eq!(human_bytes(4 * 1024 * 1024 * 1024), "4 GiB");
    assert_eq!(human_bytes(512 * 1024 * 1024), "512 MiB");
    assert_eq!(
        human_bytes(1024 * 1024 * 1024 + 512 * 1024 * 1024),
        "1.5 GiB"
    );
    let e = too_large(Over::Bytes, &limits());
    assert_eq!(e.code(), ErrorCode::TooLarge);
    assert!(e.0.error.message.contains("1 GiB"));
    assert_eq!(
        too_large(Over::Time, &limits()).0.error.message,
        "This folder took too long to measure for a zip download."
    );
}
