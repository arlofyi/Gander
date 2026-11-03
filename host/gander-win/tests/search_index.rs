// SPDX-License-Identifier: Apache-2.0
//! Live tests of the Windows Search index (WP-66). They read the real index of the machine they
//! run on and change nothing: no crawl scope is added, no setting is touched, no file is created.
//! Every test is `#[ignore]`d because it needs the Windows Search service and a folder that is in
//! its scope (a temporary folder usually is not); run them by hand:
//!
//! ```text
//! cargo test -p gander-win --test search_index -- --ignored --nocapture --test-threads 1
//! ```
//!
//! The folder is the first of `GANDER_INDEX_TEST_DIR` (an absolute path, to pick one), then
//! Documents, Downloads, Pictures, Desktop of the signed-in user, that the indexer reports as
//! crawled AND that holds at least one indexed file. With none, a test prints `SKIP` and passes.

#![cfg(windows)]

use std::ops::ControlFlow;
use std::path::PathBuf;

use gander_core::search::{IndexScope, SearchScope, build_sql};
use gander_win::search::IndexSession;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Pictures, KF_FLAG_DEFAULT,
    SHGetKnownFolderPath,
};
use windows::core::GUID;

fn known_folder(id: &GUID) -> Option<PathBuf> {
    // SAFETY: a valid known-folder id and no token (the current user); the returned string is
    // freed with `CoTaskMemFree`.
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const std::ffi::c_void));
        s.map(PathBuf::from)
    }
}

/// `(folder, scope)` of the first candidate that is crawled and has an indexed file in it.
fn indexed_folder(session: &IndexSession) -> Option<(PathBuf, IndexScope)> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("GANDER_INDEX_TEST_DIR") {
        candidates.push(PathBuf::from(p));
    }
    for id in [
        &FOLDERID_Documents,
        &FOLDERID_Downloads,
        &FOLDERID_Pictures,
        &FOLDERID_Desktop,
    ] {
        candidates.extend(known_folder(id));
    }
    for dir in candidates {
        let Some(text) = dir.to_str() else { continue };
        let Ok(scope) = IndexScope::for_dir(text) else {
            continue;
        };
        if !session.is_in_scope(&scope).unwrap_or(false) {
            continue;
        }
        let sql = build_sql(&scope, ".", SearchScope::Name, 50).expect("sql");
        let mut any = false;
        let _ = session.query(&sql, 0, &mut |_| {
            any = true;
            ControlFlow::Break(())
        });
        if any {
            return Some((dir, scope));
        }
    }
    None
}

#[test]
#[ignore = "needs the Windows Search service and an indexed folder"]
fn scope_and_backlog_are_readable() {
    let Ok(session) = IndexSession::open() else {
        eprintln!("SKIP: the Windows Search service is not available");
        return;
    };
    let nowhere = IndexScope::for_dir(r"C:\gander-no-such-folder-wp66").unwrap();
    assert!(
        !session.is_in_scope(&nowhere).unwrap(),
        "a folder that does not exist is not crawled"
    );
    let backlog = session.backlog();
    eprintln!("backlog: {backlog:?}");
    assert!(backlog.is_some(), "the queue sizes can be read");
    match indexed_folder(&session) {
        Some((_, scope)) => eprintln!(
            "indexed folder found (path withheld), crawl url form: {}",
            scope.crawl_url.replace(|c: char| c.is_alphanumeric(), "x")
        ),
        None => eprintln!("SKIP: no indexed folder with files found"),
    }
}

/// All `System.ItemUrl`s of `sql` after `skip` rows, panicking with the statement on an error.
fn urls(session: &IndexSession, sql: &str, skip: u64) -> Vec<String> {
    let mut out = Vec::new();
    session
        .query(sql, skip, &mut |u| {
            out.push(u.to_owned());
            ControlFlow::Continue(())
        })
        .unwrap_or_else(|e| panic!("{e}\n{sql}"));
    out
}

fn file_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

#[test]
#[ignore = "needs the Windows Search service and an indexed folder"]
fn name_search_matches_what_was_typed_and_only_that() {
    let Ok(session) = IndexSession::open() else {
        eprintln!("SKIP: the Windows Search service is not available");
        return;
    };
    let Some((_, scope)) = indexed_folder(&session) else {
        eprintln!("SKIP: no indexed folder with files found");
        return;
    };
    // A real name: found by itself, below the folder, and the statement is accepted.
    let first = urls(
        &session,
        &build_sql(&scope, "a", SearchScope::Name, 40).unwrap(),
        0,
    );
    assert!(!first.is_empty());
    let target = first
        .iter()
        .map(|u| file_name(u).to_owned())
        .find(|n| !n.contains('\''))
        .expect("a name without a quote");
    let rows = urls(
        &session,
        &build_sql(&scope, &target, SearchScope::Name, 100).unwrap(),
        0,
    );
    assert!(
        rows.iter().any(|u| file_name(u) == target),
        "a file is found by its own full name"
    );
    for u in &rows {
        assert!(
            u.to_lowercase()
                .starts_with(&format!("file:{}/", scope.query_url[5..].to_lowercase())),
            "every row is below the folder"
        );
        assert!(file_name(u).to_lowercase().contains(&target.to_lowercase()));
    }
    // Upper case finds it too (the index compares case-insensitively).
    let upper = urls(
        &session,
        &build_sql(&scope, &target.to_uppercase(), SearchScope::Name, 100).unwrap(),
        0,
    );
    assert!(upper.iter().any(|u| file_name(u) == target));

    // Text full of SQL and LIKE syntax is accepted by the real provider and means itself: no
    // syntax error, and every hit has exactly that text in its file name.
    for needle in [
        "%",
        "_",
        "[",
        "[a]",
        "[^a]",
        "a%b",
        "a_b",
        "100%",
        "it's",
        "'",
        "''",
        "*",
        "?",
        "\"quoted\"",
        "x' OR 1=1 --",
        "'; DROP TABLE SystemIndex; --",
        r"a\b",
        "(a)",
        "a;b",
        "a=b",
        "\u{e9}t\u{e9}",
    ] {
        let sql = build_sql(&scope, needle, SearchScope::Name, 300).unwrap();
        for u in urls(&session, &sql, 0) {
            assert!(
                file_name(&u)
                    .to_lowercase()
                    .contains(&needle.to_lowercase()),
                "{needle:?} matched {} (statement: {sql})",
                file_name(&u)
            );
        }
    }
    // The injection attempts matched nothing at all.
    for needle in ["x' OR 1=1 --", "'; DROP TABLE SystemIndex; --"] {
        let sql = build_sql(&scope, needle, SearchScope::Name, 300).unwrap();
        assert!(urls(&session, &sql, 0).is_empty(), "{needle}");
    }
}

#[test]
#[ignore = "needs the Windows Search service and an indexed folder"]
fn content_search_is_accepted_and_ranked() {
    let Ok(session) = IndexSession::open() else {
        eprintln!("SKIP: the Windows Search service is not available");
        return;
    };
    let Some((_, scope)) = indexed_folder(&session) else {
        eprintln!("SKIP: no indexed folder with files found");
        return;
    };
    // Syntax the provider must take whatever the user typed (hits depend on the folder).
    for needle in [
        "the",
        "the and",
        "budget 2024",
        "it's",
        "\"quoted\"",
        "x' OR 1=1 --",
        "a*b",
        "OR",
        "NEAR",
        "AND NOT",
        "-- ;",
        "100%",
        "[a]",
        "\u{e9}t\u{e9}",
        "a b c d e f g h i j k l",
    ] {
        let sql = build_sql(&scope, needle, SearchScope::Content, 100).unwrap();
        let _ = urls(&session, &sql, 0);
    }
    let rows = urls(
        &session,
        &build_sql(&scope, "the", SearchScope::Content, 200).unwrap(),
        0,
    );
    eprintln!("content hits for `the`: {}", rows.len());
    // A word that is in no file is not found; the injected text is not special.
    let none = urls(
        &session,
        &build_sql(&scope, "zzqxwp66nosuchword", SearchScope::Content, 50).unwrap(),
        0,
    );
    assert!(none.is_empty());
}

#[test]
#[ignore = "needs the Windows Search service and an indexed folder"]
fn skipping_rows_continues_the_same_list() {
    let Ok(session) = IndexSession::open() else {
        eprintln!("SKIP: the Windows Search service is not available");
        return;
    };
    let Some((_, scope)) = indexed_folder(&session) else {
        eprintln!("SKIP: no indexed folder with files found");
        return;
    };
    let all = urls(
        &session,
        &build_sql(&scope, "a", SearchScope::Name, 300).unwrap(),
        0,
    );
    if all.len() < 150 {
        eprintln!("SKIP: fewer than 150 rows to page through");
        return;
    }
    // The same statement twice gives the same list, and `skip` is an offset into it.
    let again = urls(
        &session,
        &build_sql(&scope, "a", SearchScope::Name, 300).unwrap(),
        0,
    );
    assert_eq!(all, again, "the order is stable");
    for skip in [1u64, 63, 64, 65, 100] {
        let tail = urls(
            &session,
            &build_sql(&scope, "a", SearchScope::Name, 300).unwrap(),
            skip,
        );
        assert_eq!(tail, all[skip as usize..], "skip {skip}");
    }
    // Stopping the visitor leaves the row it refused unconsumed.
    let mut taken = 0u64;
    let end = session
        .query(
            &build_sql(&scope, "a", SearchScope::Name, 300).unwrap(),
            10,
            &mut |_| {
                if taken == 5 {
                    return ControlFlow::Break(());
                }
                taken += 1;
                ControlFlow::Continue(())
            },
        )
        .unwrap();
    assert_eq!((end.consumed, end.stopped), (5, true));
    // Skipping past the end is an empty answer, not an error.
    let past = urls(
        &session,
        &build_sql(&scope, "a", SearchScope::Name, 300).unwrap(),
        100_000,
    );
    assert!(past.is_empty());
}
