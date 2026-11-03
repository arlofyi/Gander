// SPDX-License-Identifier: Apache-2.0
//! Windows Search index queries (WP-66), the OS-independent half: the SQL, its escaping and the
//! reading of the paths the index returns. Running the SQL needs COM and lives in
//! `gander_win::search`; resolving what comes back is the host's job (`api/indexed.rs`).
//!
//! # Threat model
//!
//! Two things cross this boundary and neither is trusted:
//!
//! - **What the phone typed** (`q`) ends up inside a SQL string for the Windows Search OLE DB
//!   provider. The text is never concatenated as SQL: it is rejected when it holds control
//!   characters or is too long, every quote is doubled inside the one string literal it appears in,
//!   and the `LIKE` wildcards and the full-text `CONTAINS` operators are neutralised (see
//!   [`like_pattern`] and [`contains_expression`]). There is no `ESCAPE` clause in this dialect
//!   (measured: a syntax error), so `%`, `_` and `[` are escaped as one-character classes.
//! - **What the index returns** (`System.ItemUrl`) is a path somebody else wrote. [`relative_to`]
//!   only strips a known prefix and hands back the rest; the caller must then build the
//!   share-relative path and resolve it with the path resolver, segment by segment, like any
//!   phone-supplied path.
//!
//! `System.ItemPathDisplay` is deliberately not used: it is a DISPLAY value and for some item types
//! it is not a file path at all (measured 2026-11-03: `.eml` files show the mail subject).
//! `System.ItemUrl` is the indexer's own key, `file:` plus the real path with forward slashes and no
//! percent-encoding.

use std::fmt;

/// Longest search text sent to the index, in characters. Longer text is answered by the plain
/// walk instead.
pub const MAX_NEEDLE_CHARS: usize = 128;

/// Most words of a content search that are sent as terms; further words are ignored.
pub const MAX_CONTENT_TERMS: usize = 8;

/// What `q` is matched against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchScope {
    /// File and folder names (the default, and all the bounded walk can do).
    Name,
    /// Names and the text inside files, from the index's full-text catalogue.
    Content,
}

impl SearchScope {
    /// Parses the `scope` query value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "name" => Some(Self::Name),
            "content" => Some(Self::Content),
            _ => None,
        }
    }

    /// The query value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Content => "content",
        }
    }
}

/// Why a query is not sent to the index (the caller falls back to the walk).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryError {
    /// The search text is empty.
    Empty,
    /// The search text holds a control character (NUL, a newline, ...).
    ControlChar,
    /// The search text is longer than [`MAX_NEEDLE_CHARS`].
    TooLong,
    /// The folder is not a local drive path the index can be asked about.
    BadScope,
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "the search text is empty",
            Self::ControlChar => "the search text holds a control character",
            Self::TooLong => "the search text is too long for the index",
            Self::BadScope => "the folder is not a local drive path",
        })
    }
}

impl std::error::Error for QueryError {}

/// Checks the search text (already trimmed by the caller) for use in an index query.
pub fn check_needle(needle: &str) -> Result<&str, QueryError> {
    if needle.is_empty() {
        return Err(QueryError::Empty);
    }
    if needle.chars().any(char::is_control) {
        return Err(QueryError::ControlChar);
    }
    if needle.chars().count() > MAX_NEEDLE_CHARS {
        return Err(QueryError::TooLong);
    }
    Ok(needle)
}

/// `value` as one SQL string literal: single quotes, every `'` doubled. The only way text gets
/// into the statement.
pub fn sql_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push('\'');
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// The `LIKE` pattern (before quoting) that matches names containing `needle` literally:
/// `%needle%` with `%`, `_` and `[` written as one-character classes (`[%]`, `[_]`, `[[]`). The
/// dialect has no `ESCAPE` clause. The index compares case-insensitively.
pub fn like_pattern(needle: &str) -> String {
    let mut out = String::with_capacity(needle.len() + 2);
    out.push('%');
    for c in needle.chars() {
        match c {
            '%' => out.push_str("[%]"),
            '_' => out.push_str("[_]"),
            '[' => out.push_str("[[]"),
            other => out.push(other),
        }
    }
    out.push('%');
    out
}

/// The full-text condition for `needle`, or `None` when it has no searchable word.
///
/// Each whitespace-separated word becomes a quoted prefix term (`"budg*"`), and the terms are
/// joined with `AND`, so every word has to appear, in any order, and a half-typed last word still
/// matches. Quotes and `*` in the text are dropped (they are the grammar's own syntax), and a word
/// without a letter or digit is skipped: a term made of punctuation is a syntax error to the
/// provider. Inside a quoted term the rest is literal.
pub fn contains_expression(needle: &str) -> Option<String> {
    let terms: Vec<String> = needle
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| *c != '"' && *c != '*')
                .collect::<String>()
        })
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .take(MAX_CONTENT_TERMS)
        .map(|w| format!("\"{w}*\""))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" AND "))
}

/// The two spellings of a folder the index is asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexScope {
    /// For `SCOPE='...'` in a query: `file:C:/Users/me/Documents`.
    pub query_url: String,
    /// For `ISearchCrawlScopeManager::IncludedInCrawlScope`: `file:///C:\Users\me\Documents`.
    pub crawl_url: String,
    /// The folder with a trailing `/` in the form `System.ItemUrl` uses, for [`relative_to`].
    item_prefix: String,
}

impl IndexScope {
    /// For a plain absolute path with a drive letter (`C:\Users\me`), as
    /// `Resolved::final_path` reports it (no `\\?\` prefix). Network paths, device paths and
    /// anything with a character Windows forbids in a path are refused: the index is only used for
    /// local drives.
    pub fn for_dir(path: &str) -> Result<Self, QueryError> {
        let bytes = path.as_bytes();
        let drive = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\';
        if !drive {
            return Err(QueryError::BadScope);
        }
        if path
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '<' | '>' | '"' | '|' | '?' | '*'))
            || path[2..].contains(':')
        {
            return Err(QueryError::BadScope);
        }
        let trimmed = path.trim_end_matches('\\');
        let forward = trimmed.replace('\\', "/");
        Ok(Self {
            query_url: format!("file:{forward}"),
            crawl_url: format!("file:///{trimmed}"),
            item_prefix: format!("file:{forward}/"),
        })
    }
}

/// What the index returned for a row, relative to the folder that was searched: the part of
/// `item_url` after `scope` and a `/`, compared case-insensitively. `None` when the URL is not
/// below the folder (another folder, the folder itself, a different scheme). The result is
/// UNTRUSTED text: split it on `/` and resolve it.
pub fn relative_to<'a>(item_url: &'a str, scope: &IndexScope) -> Option<&'a str> {
    let prefix = scope.item_prefix.as_str();
    let head = item_url.get(..prefix.len())?;
    let same = head.eq_ignore_ascii_case(prefix) || head.to_lowercase() == prefix.to_lowercase();
    let rest = item_url.get(prefix.len()..)?;
    (same && !rest.is_empty()).then_some(rest)
}

/// How many index rows one query reads at most, counted from the cursor offset.
pub const MAX_SCAN_ROWS: u64 = 2_000;

/// Offsets past this are not served: a result list that deep is not what the phone is for.
pub const MAX_OFFSET: u64 = 20_000;

/// The statement for one search: the rows of `scope` whose name contains `needle` (and, for
/// [`SearchScope::Content`], whose text does), shallow-first by URL (names) or by relevance
/// (content), with `top` rows at most.
pub fn build_sql(
    scope: &IndexScope,
    needle: &str,
    kind: SearchScope,
    top: u64,
) -> Result<String, QueryError> {
    let needle = check_needle(needle)?;
    let name = format!(
        "System.FileName LIKE {}",
        sql_literal(&like_pattern(needle))
    );
    let contains = match kind {
        SearchScope::Name => None,
        SearchScope::Content => contains_expression(needle),
    };
    let (predicate, order) = match &contains {
        Some(expr) => (
            format!(
                "(CONTAINS(System.Search.Contents, {}) OR {name})",
                sql_literal(expr)
            ),
            "System.Search.Rank DESC, System.ItemUrl",
        ),
        None => (name, "System.ItemUrl"),
    };
    Ok(format!(
        "SELECT TOP {top} System.ItemUrl FROM SystemIndex WHERE SCOPE={} AND {predicate} ORDER BY {order}",
        sql_literal(&scope.query_url)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(path: &str) -> IndexScope {
        IndexScope::for_dir(path).unwrap()
    }

    #[test]
    fn literal_doubles_quotes_and_nothing_else() {
        assert_eq!(sql_literal("abc"), "'abc'");
        assert_eq!(sql_literal("it's"), "'it''s'");
        assert_eq!(sql_literal("'"), "''''");
        assert_eq!(sql_literal("a''b"), "'a''''b'");
        assert_eq!(sql_literal(""), "''");
        // Everything else is data inside the literal.
        assert_eq!(sql_literal("a;--b"), "'a;--b'");
        assert_eq!(sql_literal("\"x\""), "'\"x\"'");
    }

    /// The text of a literal back from its quoted form (what the provider's parser sees).
    fn unquote(lit: &str) -> String {
        let inner = lit.strip_prefix('\'').unwrap().strip_suffix('\'').unwrap();
        inner.replace("''", "'")
    }

    #[test]
    fn a_literal_never_ends_early() {
        // Whatever the text, the quoted form has no lone quote inside: the statement cannot be
        // closed by it.
        for text in [
            "x' OR 1=1 --",
            "'; DROP TABLE SystemIndex; --",
            "''",
            "'''",
            "\\' OR '1'='1",
            "a'b'c'd",
            "\u{2019}\u{2018}",
        ] {
            let lit = sql_literal(text);
            let inner = &lit[1..lit.len() - 1];
            assert!(!inner.replace("''", "").contains('\''), "{text}: {lit}");
            assert_eq!(unquote(&lit), text);
        }
    }

    #[test]
    fn like_escapes_the_wildcards() {
        assert_eq!(like_pattern("budget"), "%budget%");
        assert_eq!(like_pattern("100%"), "%100[%]%");
        assert_eq!(like_pattern("a_b"), "%a[_]b%");
        assert_eq!(like_pattern("[x]"), "%[[]x]%");
        assert_eq!(like_pattern("[^a]"), "%[[]^a]%");
        assert_eq!(like_pattern("%_["), "%[%][_][[]%");
        assert_eq!(like_pattern("\u{e9}t\u{e9}"), "%\u{e9}t\u{e9}%");
    }

    #[test]
    fn contains_terms_are_quoted_prefixes() {
        assert_eq!(
            contains_expression("budget").as_deref(),
            Some("\"budget*\"")
        );
        assert_eq!(
            contains_expression("budget plan 2024").as_deref(),
            Some("\"budget*\" AND \"plan*\" AND \"2024*\"")
        );
        assert_eq!(
            contains_expression("  a   b ").as_deref(),
            Some("\"a*\" AND \"b*\"")
        );
    }

    #[test]
    fn contains_drops_the_grammar() {
        // Quotes and stars are the grammar's own characters: they never reach it.
        assert_eq!(contains_expression("a\"b").as_deref(), Some("\"ab*\""));
        assert_eq!(contains_expression("x*").as_deref(), Some("\"x*\""));
        assert_eq!(contains_expression("\"*\""), None);
        // Operators are plain words inside their quotes.
        assert_eq!(
            contains_expression("this OR that NEAR other").as_deref(),
            Some("\"this*\" AND \"OR*\" AND \"that*\" AND \"NEAR*\" AND \"other*\"")
        );
        // A term of punctuation is skipped, not sent.
        assert_eq!(contains_expression("-- ; ()"), None);
        assert_eq!(
            contains_expression("a -- b").as_deref(),
            Some("\"a*\" AND \"b*\"")
        );
        // The word count is bounded.
        let many = (0..30)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let e = contains_expression(&many).unwrap();
        assert_eq!(e.matches(" AND ").count(), MAX_CONTENT_TERMS - 1);
    }

    #[test]
    fn needle_checks() {
        assert_eq!(check_needle(""), Err(QueryError::Empty));
        assert_eq!(check_needle("a\0b"), Err(QueryError::ControlChar));
        assert_eq!(check_needle("a\nb"), Err(QueryError::ControlChar));
        assert_eq!(check_needle("a\u{7f}"), Err(QueryError::ControlChar));
        assert_eq!(
            check_needle(&"x".repeat(MAX_NEEDLE_CHARS + 1)),
            Err(QueryError::TooLong)
        );
        assert!(check_needle(&"x".repeat(MAX_NEEDLE_CHARS)).is_ok());
        assert!(check_needle("it's \"fine\" [1]").is_ok());
    }

    #[test]
    fn scope_spellings() {
        let s = scope(r"C:\Users\me\My Docs");
        assert_eq!(s.query_url, "file:C:/Users/me/My Docs");
        assert_eq!(s.crawl_url, r"file:///C:\Users\me\My Docs");
        // A drive root keeps its own form.
        let root = scope(r"D:\");
        assert_eq!(root.query_url, "file:D:");
        assert_eq!(root.crawl_url, "file:///D:");
        // Trailing separators do not matter.
        assert_eq!(scope(r"C:\a\").query_url, "file:C:/a");
    }

    #[test]
    fn scope_refuses_what_is_not_a_local_drive_path() {
        for bad in [
            r"\\server\share\x",
            r"\\.\C:\x",
            r"\\?\C:\x",
            "C:relative",
            "C:",
            "relative\\x",
            "",
            "C:/forward/slashes",
            "C:\\a\u{0}b",
            "C:\\a\nb",
            r"C:\a:stream",
            r"C:\a*b",
            r"C:\a?b",
            "C:\\a\"b",
            r"C:\a|b",
            r"C:\a<b",
        ] {
            assert_eq!(IndexScope::for_dir(bad), Err(QueryError::BadScope), "{bad}");
        }
    }

    #[test]
    fn relative_paths_come_from_the_prefix_only() {
        let s = scope(r"C:\Users\me\Docs");
        assert_eq!(
            relative_to("file:C:/Users/me/Docs/a/b.txt", &s),
            Some("a/b.txt")
        );
        assert_eq!(
            relative_to("file:c:/users/ME/docs/a", &s),
            Some("a"),
            "case-insensitive"
        );
        // Not below the folder.
        assert_eq!(relative_to("file:C:/Users/me/Docs", &s), None);
        assert_eq!(relative_to("file:C:/Users/me/Docs/", &s), None);
        assert_eq!(relative_to("file:C:/Users/me/Docs2/a", &s), None);
        assert_eq!(relative_to("file:C:/Users/me/Doc", &s), None);
        assert_eq!(relative_to("file:C:/Users/other/Docs/a", &s), None);
        assert_eq!(relative_to("file:D:/Users/me/Docs/a", &s), None);
        assert_eq!(relative_to("C:/Users/me/Docs/a", &s), None);
        assert_eq!(relative_to("", &s), None);
        // A multi-byte character at the cut never panics.
        assert_eq!(relative_to("file:C:/Users/me/Do\u{e9}s/a", &s), None);
        assert_eq!(relative_to(&"\u{e9}".repeat(40), &s), None);
        // The rest is returned verbatim, hostile or not: the resolver refuses it later.
        assert_eq!(
            relative_to("file:C:/Users/me/Docs/../../x", &s),
            Some("../../x")
        );
        // A drive root.
        let root = scope(r"D:\");
        assert_eq!(relative_to("file:D:/a/b", &root), Some("a/b"));
        assert_eq!(relative_to("file:E:/a/b", &root), None);
    }

    #[test]
    fn name_sql() {
        let s = scope(r"C:\Users\me\Docs");
        let sql = build_sql(&s, "budget", SearchScope::Name, 100).unwrap();
        assert_eq!(
            sql,
            "SELECT TOP 100 System.ItemUrl FROM SystemIndex WHERE SCOPE='file:C:/Users/me/Docs' \
             AND System.FileName LIKE '%budget%' ORDER BY System.ItemUrl"
        );
    }

    #[test]
    fn content_sql() {
        let s = scope(r"C:\Users\me\Docs");
        let sql = build_sql(&s, "budget 2024", SearchScope::Content, 50).unwrap();
        assert_eq!(
            sql,
            "SELECT TOP 50 System.ItemUrl FROM SystemIndex WHERE SCOPE='file:C:/Users/me/Docs' \
             AND (CONTAINS(System.Search.Contents, '\"budget*\" AND \"2024*\"') \
             OR System.FileName LIKE '%budget 2024%') \
             ORDER BY System.Search.Rank DESC, System.ItemUrl"
        );
        // Nothing searchable inside: the names are still searched, in name order.
        let sql = build_sql(&s, "--", SearchScope::Content, 50).unwrap();
        assert!(!sql.contains("CONTAINS"), "{sql}");
        assert!(sql.ends_with("ORDER BY System.ItemUrl"), "{sql}");
    }

    #[test]
    fn hostile_text_stays_inside_its_literals() {
        let s = scope(r"C:\Users\me\Docs");
        for kind in [SearchScope::Name, SearchScope::Content] {
            for text in [
                "x' OR 1=1 --",
                "'; DROP TABLE SystemIndex; --",
                "x') OR SCOPE='file:C:/' --",
                "\" OR \"\"=\"",
                "a%b_c[d",
                "'",
                "''",
                "x\\' OR '",
            ] {
                let sql = build_sql(&s, text, kind, 10).unwrap();
                // Strip every complete literal; what is left is the statement skeleton, which
                // must be exactly what we wrote (no text of the query survived outside a
                // literal).
                let skeleton = strip_literals(&sql);
                let expected = match kind {
                    SearchScope::Name => {
                        "SELECT TOP 10 System.ItemUrl FROM SystemIndex WHERE SCOPE=<> AND \
                         System.FileName LIKE <> ORDER BY System.ItemUrl"
                    }
                    SearchScope::Content => {
                        // The text may or may not hold a searchable word.
                        if sql.contains("CONTAINS") {
                            "SELECT TOP 10 System.ItemUrl FROM SystemIndex WHERE SCOPE=<> AND \
                             (CONTAINS(System.Search.Contents, <>) OR System.FileName LIKE <>) \
                             ORDER BY System.Search.Rank DESC, System.ItemUrl"
                        } else {
                            "SELECT TOP 10 System.ItemUrl FROM SystemIndex WHERE SCOPE=<> AND \
                             System.FileName LIKE <> ORDER BY System.ItemUrl"
                        }
                    }
                };
                assert_eq!(skeleton, expected, "{text:?} -> {sql}");
            }
        }
    }

    /// Replaces every SQL string literal (with `''` as an escaped quote) by `<>`.
    fn strip_literals(sql: &str) -> String {
        let mut out = String::new();
        let mut chars = sql.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\'' {
                out.push(c);
                continue;
            }
            loop {
                match chars.next() {
                    Some('\'') if chars.peek() == Some(&'\'') => {
                        chars.next();
                    }
                    Some('\'') => break,
                    Some(_) => {}
                    None => panic!("unterminated literal in {sql}"),
                }
            }
            out.push_str("<>");
        }
        out
    }

    #[test]
    fn the_scope_literal_is_escaped_too() {
        let s = scope(r"C:\Users\o'neil\Docs");
        let sql = build_sql(&s, "a", SearchScope::Name, 1).unwrap();
        assert!(sql.contains("SCOPE='file:C:/Users/o''neil/Docs'"), "{sql}");
        assert_eq!(strip_literals(&sql).matches("<>").count(), 2);
    }

    #[test]
    fn bad_text_is_not_sent() {
        let s = scope(r"C:\a");
        assert_eq!(
            build_sql(&s, "", SearchScope::Name, 1),
            Err(QueryError::Empty)
        );
        assert_eq!(
            build_sql(&s, "a\0b", SearchScope::Content, 1),
            Err(QueryError::ControlChar)
        );
    }

    #[test]
    fn scope_parameter() {
        assert_eq!(SearchScope::parse("name"), Some(SearchScope::Name));
        assert_eq!(SearchScope::parse("content"), Some(SearchScope::Content));
        for bad in ["", "Name", "all", "contents"] {
            assert_eq!(SearchScope::parse(bad), None);
        }
        assert_eq!(SearchScope::Content.as_str(), "content");
    }
}
