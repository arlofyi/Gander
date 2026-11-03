// SPDX-License-Identifier: Apache-2.0
//! Tests of the Windows resolver: every vector of `protocol/test-vectors/paths.json`, the step-7
//! check against a deliberately broken resolver, listing semantics, and a proptest fuzz
//! invariant. Fixtures live in fresh directories under the system temp dir and are removed on
//! drop; junctions are created with `mklink /J` (no privilege needed), symlinks only when the
//! environment allows (Developer Mode or admin), otherwise the vector prints `SKIPPED`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::AsHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use gander_core::path::{HiddenPatterns, RejectReason, ResolveError};
use gander_core::protocol::files::{EntryFlag, EntryKind, ListQuery};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use serde_json::Value;

use super::{ShareRoot, nt};

// ----------------------------------------------------------------------------------------------
// Fixture helpers
// ----------------------------------------------------------------------------------------------

/// A temp directory `base` holding the share `root` and an `outside` directory with a secret.
struct TempShare {
    base: PathBuf,
    root: PathBuf,
    outside: PathBuf,
}

impl TempShare {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base =
            std::env::temp_dir().join(format!("gander-wp11-{}-{}-{tag}", std::process::id(), n));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("root");
        let outside = base.join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"SECRET OUTSIDE THE SHARE").unwrap();
        Self {
            base,
            root,
            outside,
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.root.clone(), |p, s| p.join(s))
    }

    fn file(&self, rel: &str, content: &[u8]) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn share<S: AsRef<str>>(&self, show_hidden: bool, patterns: &[S]) -> ShareRoot {
        ShareRoot::open(
            &self.root,
            show_hidden,
            HiddenPatterns::new(patterns).unwrap(),
        )
        .unwrap()
    }
}

impl Drop for TempShare {
    fn drop(&mut self) {
        // remove_dir_all removes junctions and symlinks without following them.
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// `mklink /J link target` (junctions need no privilege).
fn junction(link: &Path, target: &Path) -> Result<(), String> {
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let out = Command::new("cmd")
        .raw_arg(format!(
            "/C mklink /J \"{}\" \"{}\"",
            link.display(),
            target.display()
        ))
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

fn set_attrs(path: &Path, flags: &str) {
    let out = Command::new("attrib")
        .raw_arg(format!("{flags} \"{}\"", path.display()))
        .output()
        .unwrap();
    assert!(out.status.success(), "attrib {flags} failed");
}

/// Expands the template tokens of `protocol/test-vectors/README.md` into bytes.
fn expand(s: &str, ts: &TempShare, same: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        out.extend_from_slice(&rest.as_bytes()[..start]);
        let end = rest[start..].find("}}").expect("unterminated token") + start;
        let token = &rest[start + 2..end];
        out.extend(expand_token(token, ts, same));
        rest = &rest[end + 2..];
    }
    out.extend_from_slice(rest.as_bytes());
    out
}

fn quoted(t: &str) -> (&str, &str) {
    let a = t.find('\'').unwrap();
    let b = t[a + 1..].find('\'').unwrap() + a + 1;
    (&t[a + 1..b], &t[b + 1..])
}

fn expand_token(token: &str, ts: &TempShare, same: Option<&[u8]>) -> Vec<u8> {
    if let Some(t) = token.strip_prefix("segment:") {
        let (ch, tail) = quoted(t);
        let n: usize = tail.trim_start_matches('*').parse().unwrap();
        return ch.repeat(n).into_bytes();
    }
    if let Some(t) = token.strip_prefix("repeat:") {
        let (s, tail) = quoted(t);
        let n: usize = tail.trim_start_matches(',').parse().unwrap();
        return s.repeat(n).into_bytes();
    }
    if let Some(t) = token.strip_prefix("bytes:") {
        return t
            .split(',')
            .map(|b| u8::from_str_radix(b.trim().trim_start_matches("0x"), 16).unwrap())
            .collect();
    }
    match token {
        "outside" => ts.outside.display().to_string().into_bytes(),
        "root" => ts.root.display().to_string().into_bytes(),
        "same" => same.expect("{{same}} outside a fixture").to_vec(),
        other => panic!("unknown template token {other}"),
    }
}

fn expand_str(s: &str, ts: &TempShare, same: &[u8]) -> String {
    String::from_utf8(expand(s, ts, Some(same))).unwrap()
}

fn host_path(s: &str) -> PathBuf {
    PathBuf::from(s.replace('/', "\\"))
}

fn strs(v: Option<&Value>) -> Vec<&str> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

enum Outcome {
    Pass,
    Skip(String),
    Fail(String),
}

/// What the fixture builder produced, for the post-resolve checks.
#[derive(Default)]
struct Built {
    /// Share-relative path -> content of each created file.
    files: BTreeMap<String, Vec<u8>>,
    patterns: Vec<String>,
}

fn build_fixture(v: &Value, ts: &TempShare, input: &[u8]) -> Result<Built, String> {
    let fx = &v["fixture"];
    let mut built = Built::default();
    for d in strs(fx.get("dirs")) {
        fs::create_dir_all(ts.path(&expand_str(d, ts, input))).map_err(|e| e.to_string())?;
    }
    for f in strs(fx.get("files")) {
        let rel = expand_str(f, ts, input);
        let content = format!("content of {rel}").into_bytes();
        let p = ts.path(&rel);
        fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        fs::write(&p, &content).map_err(|e| format!("create {rel}: {e}"))?;
        built.files.insert(rel, content);
    }
    for s in fx
        .get("symlinks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = s["name"].as_str().unwrap();
        let target = host_path(&expand_str(s["target"].as_str().unwrap(), ts, input));
        let link = ts.path(name);
        let r = if s.get("dir").and_then(Value::as_bool).unwrap_or(false) {
            std::os::windows::fs::symlink_dir(&target, &link)
        } else {
            std::os::windows::fs::symlink_file(&target, &link)
        };
        if let Err(e) = r {
            return Err(format!(
                "SKIP:needs privilege (symlink creation failed: {e})"
            ));
        }
    }
    for j in fx
        .get("junctions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = j["name"].as_str().unwrap();
        let target = host_path(&expand_str(j["target"].as_str().unwrap(), ts, input));
        junction(&ts.path(name), &target)?;
    }
    for r in fx
        .get("reparse")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = r["name"].as_str().unwrap();
        match r["tag"].as_str().unwrap() {
            "IO_REPARSE_TAG_WOF" => {
                let content = vec![b'A'; 256 * 1024];
                built.files.insert(name.to_owned(), content.clone());
                ts.file(name, &content);
                make_wof(ts, name)?;
            }
            other => {
                return Err(format!(
                    "SKIP:fixture unavailable ({other} cannot be created without its filter \
                     driver or provider registration)"
                ));
            }
        }
    }
    if let Some(attrs) = fx.get("attrs").and_then(Value::as_object) {
        for (name, list) in attrs {
            for a in strs(Some(list)) {
                let flag = match a {
                    "hidden" => "+h",
                    "system" => "+s",
                    "readonly" => "+r",
                    other => return Err(format!("unknown attr {other}")),
                };
                set_attrs(&ts.path(name), flag);
            }
        }
    }
    if let Some(share) = fx.get("share") {
        built.patterns = strs(share.get("hidden_patterns"))
            .into_iter()
            .map(str::to_owned)
            .collect();
    }
    Ok(built)
}

/// Compresses `name` with `compact /c /exe:xpress4k` and checks it now carries the WOF tag.
fn make_wof(ts: &TempShare, name: &str) -> Result<(), String> {
    let p = ts.path(name);
    let out = Command::new("compact")
        .raw_arg(format!("/c /exe:xpress4k \"{}\"", p.display()))
        .output()
        .map_err(|e| format!("SKIP:fixture unavailable (compact.exe: {e})"))?;
    if !out.status.success() {
        return Err("SKIP:fixture unavailable (compact.exe failed)".into());
    }
    let share = ts.share::<&str>(false, &[]);
    let wide: Vec<u16> = name.encode_utf16().collect();
    let h = nt::open_relative(share.handle.as_handle(), &wide)
        .map_err(|s| format!("open after compact: {s:#x}"))?;
    let (_, tag) = nt::attribute_tag(h.as_handle()).map_err(|e| e.to_string())?;
    // compact.exe reported success. The WOF filter usually hides its reparse point from user
    // mode (measured on Windows 11 26200: tag reads 0, fsutil says "not a reparse point"), so
    // either the WOF tag or no tag is accepted; both must resolve and read back decompressed.
    if tag == gander_core::path::IO_REPARSE_TAG_WOF || tag == 0 {
        Ok(())
    } else {
        Err(format!("unexpected reparse tag {tag:#x} after compact"))
    }
}

fn run_vector(v: &Value) -> Outcome {
    let id = v["id"].as_str().unwrap();
    let platform = v["platform"].as_str().unwrap_or("all");
    if platform == "posix" {
        return Outcome::Skip("platform posix".into());
    }
    let ts = TempShare::new(id);
    let input = expand(v["input"].as_str().unwrap(), &ts, None);
    let built = match build_fixture(v, &ts, &input) {
        Ok(b) => b,
        Err(e) => {
            return match e.strip_prefix("SKIP:") {
                Some(why) => Outcome::Skip(why.to_owned()),
                None => Outcome::Fail(format!("fixture: {e}")),
            };
        }
    };
    if id == "short-name-8dot3" && fs::metadata(ts.root.join("PROGRA~1")).is_err() {
        return Outcome::Skip("fixture unavailable (8.3 names disabled on this volume)".into());
    }
    let share = ts.share(false, &built.patterns);
    let result = share.resolve(&input);
    let expect = v["expect"].as_str().unwrap();
    match (expect, result) {
        ("ok", Ok(resolved)) => check_ok(v, &ts, &built, &input, resolved),
        ("ok", Err(e)) => Outcome::Fail(format!("expected ok, got {e}")),
        ("reject", Ok(_)) => Outcome::Fail("expected reject, got ok".into()),
        ("reject", Err(e)) => {
            let want = v["reason"].as_str().unwrap();
            match e.reason() {
                Some(got) if got.as_str() == want => Outcome::Pass,
                _ => Outcome::Fail(format!("expected {want}, got {e}")),
            }
        }
        (other, _) => Outcome::Fail(format!("unknown expect {other}")),
    }
}

fn check_ok(
    v: &Value,
    ts: &TempShare,
    built: &Built,
    input: &[u8],
    resolved: super::Resolved,
) -> Outcome {
    let input = String::from_utf8_lossy(input);
    let rel = input.trim_end_matches('/');
    if rel.is_empty() {
        return if resolved.kind() == EntryKind::Dir {
            Outcome::Pass
        } else {
            Outcome::Fail("root is not a dir".into())
        };
    }
    let Some((stored, content)) = built
        .files
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(rel))
    else {
        // A directory vector.
        return if resolved.kind() == EntryKind::Dir {
            Outcome::Pass
        } else {
            Outcome::Fail("expected a directory".into())
        };
    };
    let stored_name = stored.rsplit('/').next().unwrap();
    if resolved.name() != stored_name {
        return Outcome::Fail(format!(
            "name {:?} is not the stored {stored_name:?}",
            resolved.name()
        ));
    }
    if v["fixture"].get("race").is_some() {
        // Swap the parent for a junction to the outside after resolve returned, then read via
        // the handle: it must still be the original file.
        fs::write(ts.outside.join("a.txt"), b"OUTSIDE").unwrap();
        let dir = ts.root.join("dir");
        let moved = fs::rename(&dir, ts.root.join("dir_old"));
        let swapped = moved.is_ok() && junction(&dir, &ts.outside).is_ok();
        if !swapped {
            // Windows refuses to rename a directory with an open child handle; replace the file
            // itself instead (the handle was opened with FILE_SHARE_DELETE).
            let f = ts.path("dir/a.txt");
            if fs::rename(&f, ts.path("dir/a_old.txt")).is_err() {
                return Outcome::Fail("could not mutate the fixture".into());
            }
            fs::write(&f, b"REPLACED").unwrap();
        }
    }
    let mut got = Vec::new();
    if let Err(e) = resolved.into_file().read_to_end(&mut got) {
        return Outcome::Fail(format!("read via handle: {e}"));
    }
    if &got != content {
        return Outcome::Fail("content read via the handle differs".into());
    }
    Outcome::Pass
}

/// `ERROR_UNTRUSTED_MOUNT_POINT`: Windows 11 RedirectionGuard refused to follow a junction.
const ERROR_UNTRUSTED_MOUNT_POINT: i32 = 448;

/// The resolver the spec forbids: join the protocol path onto the root as a string and open it
/// with a normal (link-following) open.
fn broken_resolve(root: &Path, protocol_path: &str) -> std::io::Result<fs::File> {
    let joined = format!(r"{}\{}", root.display(), protocol_path.replace('/', r"\"));
    fs::File::open(joined)
}

// ----------------------------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------------------------

#[test]
fn vectors_from_paths_json() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../protocol/test-vectors/paths.json");
    let doc: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let vectors = doc["vectors"].as_array().unwrap();
    let (mut pass, mut skip, mut fail) = (0, 0, Vec::new());
    for v in vectors {
        let id = v["id"].as_str().unwrap();
        match run_vector(v) {
            Outcome::Pass => {
                pass += 1;
                println!("PASS     {id}");
            }
            Outcome::Skip(why) => {
                skip += 1;
                println!("SKIPPED  {id}: {why}");
            }
            Outcome::Fail(why) => {
                println!("FAIL     {id}: {why}");
                fail.push(format!("{id}: {why}"));
            }
        }
    }
    println!(
        "paths.json: {} vectors, {pass} passed, {skip} skipped, {} failed",
        vectors.len(),
        fail.len()
    );
    assert!(fail.is_empty(), "failed vectors:\n{}", fail.join("\n"));
    // Only vectors whose fixture genuinely needs a privilege or an unavailable driver may skip.
    for v in vectors {
        let id = v["id"].as_str().unwrap();
        let may_skip = v.get("privilege").is_some()
            || v["fixture"].get("reparse").is_some()
            || id == "short-name-8dot3";
        if !may_skip {
            assert!(
                matches!(run_vector(v), Outcome::Pass),
                "{id} must not be skipped"
            );
        }
    }
}

#[test]
fn escaped_check_catches_broken_resolver() {
    let ts = TempShare::new("escaped");
    junction(&ts.root.join("junc"), &ts.outside).unwrap();
    ts.file("inside.txt", b"in");
    let share = ts.share::<&str>(false, &[]);

    // The real resolver refuses the junction at step 6c.
    let err = share.resolve(b"junc/secret.txt").unwrap_err();
    assert_eq!(err.reason(), Some(RejectReason::ReparsePoint));

    // A deliberately broken resolver: string join + a following open. Step 7 must catch it.
    // Windows 11 RedirectionGuard refuses to follow junctions created by a non-admin
    // (ERROR_UNTRUSTED_MOUNT_POINT, 448); then the broken resolver escapes the other classic
    // way, a joined `..`, and step 7 must catch that too.
    let broken = match broken_resolve(&ts.root, "junc/secret.txt") {
        Err(e) if e.raw_os_error() == Some(ERROR_UNTRUSTED_MOUNT_POINT) => {
            println!(
                "NOTE     escaped_check: OS refused to follow the junction (RedirectionGuard); broken resolver uses a joined '..' instead"
            );
            broken_resolve(&ts.root, "../outside/secret.txt").unwrap()
        }
        other => other.unwrap(),
    };
    let err = share.verify_within_root(broken.as_handle()).unwrap_err();
    assert_eq!(err.reason(), Some(RejectReason::Escaped));
    assert!(RejectReason::Escaped.is_bug());
    // The broken resolver did read outside data: the check is what stopped it.
    let mut leaked = String::new();
    (&broken).read_to_string(&mut leaked).unwrap();
    assert_eq!(leaked, "SECRET OUTSIDE THE SHARE");

    // And a legitimate handle passes, as does the root itself.
    let ok = fs::File::open(ts.root.join("inside.txt")).unwrap();
    share.verify_within_root(ok.as_handle()).unwrap();
    let root = share.resolve(b"").unwrap();
    share.verify_within_root(root.handle()).unwrap();

    // A sibling whose name extends the root's name is outside (prefix + separator rule).
    let sibling_dir = ts.base.join("root2");
    fs::create_dir_all(&sibling_dir).unwrap();
    fs::write(sibling_dir.join("x.txt"), b"x").unwrap();
    let sibling = fs::File::open(sibling_dir.join("x.txt")).unwrap();
    assert_eq!(
        share
            .verify_within_root(sibling.as_handle())
            .unwrap_err()
            .reason(),
        Some(RejectReason::Escaped)
    );
}

#[test]
fn root_may_be_a_junction_resolved_once() {
    let ts = TempShare::new("rootlink");
    ts.file("a.txt", b"a");
    let link = ts.base.join("rootlink");
    junction(&link, &ts.root).unwrap();
    let share = match ShareRoot::open(&link, false, HiddenPatterns::default()) {
        Err(e) if e.raw_os_error() == Some(ERROR_UNTRUSTED_MOUNT_POINT) => {
            println!(
                "SKIPPED  root_may_be_a_junction: OS refuses to follow a junction created by a non-admin (RedirectionGuard, error 448)"
            );
            return;
        }
        other => other.unwrap(),
    };
    let canonical = share.canonical_root();
    assert!(canonical.to_string_lossy().starts_with(r"\\?\"));
    assert!(canonical.ends_with("root"), "{}", canonical.display());
    assert_eq!(share.resolve(b"a.txt").unwrap().name(), "a.txt");
}

#[test]
fn listing_semantics() {
    let ts = TempShare::new("listing");
    ts.file("b.txt", b"bb");
    ts.file("A.txt", b"a");
    ts.file("hidden.txt", b"h");
    ts.file("sys.txt", b"s");
    ts.file("desktop.ini", b"[x]");
    ts.file("Thumbs.db", b"t");
    ts.file(".git/config", b"c");
    ts.file("r\u{e9}sum\u{e9}.txt", b"nfc");
    ts.file("re\u{301}sume\u{301}.txt", b"nfd");
    ts.file("sub/inner.txt", b"i");
    set_attrs(&ts.path("hidden.txt"), "+h");
    set_attrs(&ts.path("sys.txt"), "+s");
    junction(&ts.root.join("junc"), &ts.outside).unwrap();
    // A name with an unpaired surrogate cannot be listed in UTF-8.
    let bad: Vec<u16> = [0xD800].into_iter().chain("x.txt".encode_utf16()).collect();
    fs::write(ts.root.join(OsString::from_wide(&bad)), b"?").unwrap();

    let share = ts.share(false, &[".git/**"]);
    let root = share.resolve(b"").unwrap();
    let listing = share.list(&root, &ListQuery::default(), 1000).unwrap();
    let names: Vec<&str> = listing
        .page
        .entries
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "junc",
            "sub",
            "A.txt",
            "b.txt",
            "re\u{301}sume\u{301}.txt",
            "r\u{e9}sum\u{e9}.txt"
        ]
    );
    assert_eq!(listing.unlistable, 1);
    assert!(!listing.truncated);
    // Names come back byte-identical (no normalisation).
    assert!(names.contains(&"re\u{301}sume\u{301}.txt"));
    let junc = &listing.page.entries[0];
    assert_eq!(junc.kind, EntryKind::Dir);
    assert_eq!(junc.attrs.flags(), [EntryFlag::Link]);
    let b = listing
        .page
        .entries
        .iter()
        .find(|e| e.name == "b.txt")
        .unwrap();
    assert_eq!(b.size, 2);
    assert!(b.ctime.is_some());

    // hidden_patterns hide AND block, case-insensitively.
    for p in [".git/config", ".GIT/config", ".git"] {
        assert_eq!(
            share.resolve(p.as_bytes()).unwrap_err().reason(),
            Some(RejectReason::NotFound),
            "{p}"
        );
    }
    // Hidden-attribute files are omitted from listings but still resolvable directly.
    assert_eq!(share.resolve(b"hidden.txt").unwrap().name(), "hidden.txt");

    // show_hidden lists hidden and system entries, never the always-omitted ones.
    let shown = ts.share::<&str>(true, &[]);
    let root = shown.resolve(b"/").unwrap();
    let all = shown.enumerate(&root).unwrap();
    let names: Vec<&str> = all.entries.iter().map(|e| e.name.as_str()).collect();
    for n in ["hidden.txt", "sys.txt", ".git"] {
        assert!(names.contains(&n), "{n} missing");
    }
    for n in ["desktop.ini", "Thumbs.db"] {
        assert!(!names.contains(&n), "{n} listed");
    }
    let h = all.entries.iter().find(|e| e.name == "hidden.txt").unwrap();
    assert!(h.attrs.hidden);

    // Sub-directory listing, paging, and a file is not listable.
    let sub = share.resolve(b"sub").unwrap();
    let page = share
        .list(
            &sub,
            &ListQuery {
                limit: Some(1),
                ..Default::default()
            },
            1000,
        )
        .unwrap();
    assert_eq!(page.page.entries[0].name, "inner.txt");
    assert!(page.page.next_cursor.is_none());
    let file = share.resolve(b"b.txt").unwrap();
    assert!(matches!(
        share.list(&file, &ListQuery::default(), 1000),
        Err(super::ListError::NotADirectory)
    ));
    let stat = file.stat().unwrap();
    assert_eq!((stat.name.as_str(), stat.size), ("b.txt", 2));
}

#[test]
fn proptest_resolver_never_escapes_or_panics() {
    let ts = TempShare::new("fuzz");
    ts.file("dir/sub/a.txt", b"a");
    ts.file("Program Files Test/x.txt", b"x");
    ts.file("notes~1.txt", b"n");
    junction(&ts.root.join("junc"), &ts.outside).unwrap();
    junction(
        &ts.root.join("dir").join("jin"),
        &ts.root.join("dir").join("sub"),
    )
    .unwrap();
    let share = ts.share(false, &["**/hidden"]);

    let vocab = prop_oneof![
        Just("dir".to_owned()),
        Just("DIR".to_owned()),
        Just("sub".to_owned()),
        Just("a.txt".to_owned()),
        Just("junc".to_owned()),
        Just("jin".to_owned()),
        Just("secret.txt".to_owned()),
        Just("..".to_owned()),
        Just(".".to_owned()),
        Just("PROGRA~1".to_owned()),
        Just("notes~1.txt".to_owned()),
        Just("a.txt.".to_owned()),
        Just("con".to_owned()),
        Just("hidden".to_owned()),
        Just(String::new()),
        "[a-zA-Z0-9.~ :$]{1,8}",
    ];
    let segments = proptest::collection::vec(vocab, 0..7).prop_map(|s| s.join("/").into_bytes());
    let bytes = proptest::collection::vec(any::<u8>(), 0..64);
    let strategy = prop_oneof![segments, bytes];

    let mut runner = TestRunner::new(Config {
        cases: 512,
        failure_persistence: None,
        ..Config::default()
    });
    runner
        .run(&strategy, |input| {
            match share.resolve(&input) {
                Ok(r) => {
                    // Anything returned is inside the root (independent re-check).
                    prop_assert!(share.verify_within_root(r.handle()).is_ok());
                }
                Err(ResolveError::Rejected(reason)) => {
                    prop_assert_ne!(reason, RejectReason::Escaped, "step 7 fired");
                }
                Err(ResolveError::Io(e)) => prop_assert!(false, "unexpected I/O error {e}"),
            }
            Ok(())
        })
        .unwrap();
}
