// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask docs-check`: every relative link and image target in the repository's Markdown
//! must exist, and every `#heading` anchor into a Markdown file must match a heading there.
//!
//! The check is deliberately close to what GitHub does when it renders the files:
//!
//! - inline links and images (`[text](target "title")`, `![alt](target)`), reference definitions
//!   (`[label]: target`) and `href` / `src` attributes in raw HTML are read; fenced code blocks,
//!   inline code spans and HTML comments are not;
//! - a target is resolved against the folder of the file that contains it (a leading `/` means the
//!   repository root), `?query` is dropped and `%20`-style escapes are decoded;
//! - existence is checked case-sensitively, because GitHub serves from a case-sensitive file
//!   system and a link that only works on Windows is still broken;
//! - `#anchor` is checked against the target file's ATX headings using GitHub's slug rules
//!   (lower case, punctuation dropped, spaces become hyphens, repeats get `-1`, `-2`) and against
//!   `id="..."` / `name="..."` attributes. Anchors into files that are not Markdown (`#L12` in
//!   source files) are not checked.
//!
//! External links (`https:`, `mailto:` ...) are counted but never fetched. `target/`,
//! `.worktrees/`, `node_modules/`, `build/`, `.git/` and `.gradle/` are not walked.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use crate::proc::{display_path, workspace_root};

/// Directory names that are never walked (build output, other checkouts, VCS metadata).
const EXCLUDED_DIRS: &[&str] = &[
    "target",
    ".worktrees",
    "node_modules",
    "build",
    ".git",
    ".gradle",
];

/// Entry point for `cargo xtask docs-check [--root <dir>]`.
pub fn run(args: &[String]) -> Result<ExitCode> {
    let root = match args {
        [] => workspace_root(),
        [flag, dir] if flag == "--root" => PathBuf::from(dir),
        _ => bail!("usage: cargo xtask docs-check [--root <dir>]"),
    };
    let report = check_repository(&root)?;
    for broken in &report.broken {
        println!(
            "[BROKEN] {}:{}: {} -- {}",
            broken.file, broken.line, broken.link, broken.reason
        );
    }
    println!(
        "docs-check: {} Markdown files, {} relative links checked ({} with anchors), \
         {} external links not fetched, {} broken",
        report.files,
        report.checked,
        report.anchors,
        report.external,
        report.broken.len()
    );
    if report.broken.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

/// One link, image or reference definition found in a Markdown file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// 1-based line number.
    pub line: usize,
    /// Link text or image alt text (empty for definitions and HTML attributes).
    pub text: String,
    /// The destination exactly as written (angle brackets and title removed).
    pub target: String,
    pub kind: LinkKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Inline,
    Image,
    Definition,
    Html,
}

/// A link that does not resolve.
#[derive(Debug, PartialEq, Eq)]
pub struct Broken {
    pub file: String,
    pub line: usize,
    pub link: String,
    pub reason: String,
}

/// Result of checking a whole tree.
#[derive(Debug, Default)]
pub struct Report {
    pub files: usize,
    pub checked: usize,
    pub anchors: usize,
    pub external: usize,
    pub broken: Vec<Broken>,
}

/// Walk `root`, parse every Markdown file and check its relative links.
pub fn check_repository(root: &Path) -> Result<Report> {
    let mut files = Vec::new();
    collect_markdown(root, &mut files)?;
    files.sort();
    let mut report = Report {
        files: files.len(),
        ..Report::default()
    };
    let mut checker = Checker::new(root);
    for file in &files {
        let text = std::fs::read_to_string(file)
            .with_context(|| format!("reading {}", display_path(file)))?;
        let rel = relative_components(root, file)?;
        for link in parse_links(&text) {
            checker.check(&rel, &link, &mut report);
        }
    }
    Ok(report)
}

fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let name = entry.file_name();
            if EXCLUDED_DIRS.iter().any(|d| name.to_string_lossy() == *d) {
                continue;
            }
            collect_markdown(&path, out)?;
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            out.push(path);
        }
    }
    Ok(())
}

fn relative_components(root: &Path, file: &Path) -> Result<Vec<String>> {
    let rel = file
        .strip_prefix(root)
        .with_context(|| format!("{} is not under the root", display_path(file)))?;
    Ok(rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect())
}

/// Existence and anchor lookups with per-directory and per-file caches.
struct Checker {
    root: PathBuf,
    dirs: HashMap<PathBuf, Vec<String>>,
    anchors: HashMap<PathBuf, HashSet<String>>,
}

impl Checker {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            dirs: HashMap::new(),
            anchors: HashMap::new(),
        }
    }

    /// Check `link`, found in the file at `source` (components relative to the root).
    fn check(&mut self, source: &[String], link: &Link, report: &mut Report) {
        let target = link.target.trim();
        if target.is_empty() {
            return;
        }
        if is_external(target) {
            report.external += 1;
            return;
        }
        report.checked += 1;
        if let Err(reason) = self.resolve(source, target, report) {
            report.broken.push(Broken {
                file: source.join("/"),
                line: link.line,
                link: describe(link),
                reason,
            });
        }
    }

    fn resolve(
        &mut self,
        source: &[String],
        target: &str,
        report: &mut Report,
    ) -> std::result::Result<(), String> {
        if is_windows_drive_path(target) {
            return Err("absolute Windows path, not a repository link".into());
        }
        let (path_part, fragment) = match target.split_once('#') {
            Some((p, f)) => (p, Some(f)),
            None => (target, None),
        };
        let path_part = path_part.split('?').next().unwrap_or("");
        let path_part = percent_decode(path_part);
        let components = if path_part.is_empty() {
            source.to_vec()
        } else {
            join_relative(source, &path_part)?
        };
        let is_directory = self.lookup(&components)?;
        if let Some(fragment) = fragment.filter(|f| !f.is_empty()) {
            let is_markdown = !is_directory
                && components
                    .last()
                    .is_some_and(|n| n.to_ascii_lowercase().ends_with(".md"));
            if is_markdown {
                report.anchors += 1;
                let fragment = percent_decode(fragment).to_lowercase();
                let anchors = self.anchors_of(&components)?;
                if !anchors.contains(&fragment) {
                    return Err(format!(
                        "no heading or id `{fragment}` in {}",
                        components.join("/")
                    ));
                }
            }
        }
        Ok(())
    }

    /// Check that `components` exists under the root with exactly this spelling. Returns whether
    /// it is a directory.
    fn lookup(&mut self, components: &[String]) -> std::result::Result<bool, String> {
        let mut current = self.root.clone();
        for (index, name) in components.iter().enumerate() {
            let names = self.list(&current)?;
            if !names.contains(name) {
                let shown = components[..=index].join("/");
                return Err(match names.iter().find(|n| n.eq_ignore_ascii_case(name)) {
                    Some(actual) => format!(
                        "case mismatch: `{shown}` is `{actual}` on disk (GitHub is case-sensitive)"
                    ),
                    None => format!("not found: `{shown}`"),
                });
            }
            current.push(name);
        }
        Ok(current.is_dir())
    }

    fn list(&mut self, dir: &Path) -> std::result::Result<&Vec<String>, String> {
        if !self.dirs.contains_key(dir) {
            let entries = std::fs::read_dir(dir).map_err(|_| {
                format!(
                    "not found: `{}` is not a directory",
                    display_path(dir.strip_prefix(&self.root).unwrap_or(dir))
                )
            })?;
            let names = entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            self.dirs.insert(dir.to_path_buf(), names);
        }
        Ok(&self.dirs[dir])
    }

    fn anchors_of(
        &mut self,
        components: &[String],
    ) -> std::result::Result<&HashSet<String>, String> {
        let path = components
            .iter()
            .fold(self.root.clone(), |acc, c| acc.join(c));
        if !self.anchors.contains_key(&path) {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", components.join("/")))?;
            self.anchors.insert(path.clone(), anchors_in(&text));
        }
        Ok(&self.anchors[&path])
    }
}

fn describe(link: &Link) -> String {
    match link.kind {
        LinkKind::Inline => format!("[{}]({})", link.text, link.target),
        LinkKind::Image => format!("![{}]({})", link.text, link.target),
        LinkKind::Definition => format!("[{}]: {}", link.text, link.target),
        LinkKind::Html => format!("<... \"{}\">", link.target),
    }
}

/// Resolve `target` against the folder of `source`. A leading `/` is the repository root.
/// Lexical normalisation only; `..` past the root is an error.
fn join_relative(source: &[String], target: &str) -> std::result::Result<Vec<String>, String> {
    let (mut components, rest) = match target.strip_prefix('/') {
        Some(rest) => (Vec::new(), rest),
        None => (source[..source.len().saturating_sub(1)].to_vec(), target),
    };
    for part in rest.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err("points outside the repository".into());
                }
            }
            name => components.push(name.to_string()),
        }
    }
    Ok(components)
}

/// `https:`, `mailto:`, `//host/...` and any other scheme of two or more letters.
fn is_external(target: &str) -> bool {
    if target.starts_with("//") {
        return true;
    }
    match target.split_once(':') {
        Some((scheme, _)) => {
            scheme.len() >= 2
                && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
        None => false,
    }
}

/// `C:/Users/...` or `C:\...`: a single-letter scheme is a drive, not a URL.
fn is_windows_drive_path(target: &str) -> bool {
    let mut chars = target.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(c), Some(':')) if c.is_ascii_alphabetic()
    )
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            out.push(hi * 16 + lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Line classification shared by the link parser and the heading reader
// ---------------------------------------------------------------------------------------------

/// One non-fence line, in the two forms the readers need.
struct Scanned {
    /// HTML comments blanked, code left alone (what a heading reader wants).
    text: String,
    /// Comments and inline code spans blanked (what a link reader wants).
    plain: String,
}

/// Tracks fenced code blocks and HTML comments across lines.
#[derive(Default)]
struct Scanner {
    fence: Option<(char, usize)>,
    in_comment: bool,
}

impl Scanner {
    /// Feed one line. Returns `None` for a line that belongs to (or opens or closes) a fenced
    /// code block.
    fn line(&mut self, raw: &str) -> Option<Scanned> {
        if let Some((marker, len)) = self.fence {
            if closes_fence(raw, marker, len) {
                self.fence = None;
            }
            return None;
        }
        if let Some(opened) = opens_fence(raw) {
            self.fence = Some(opened);
            return None;
        }
        let text = blank_comments(raw, &mut self.in_comment);
        let plain = blank_code_spans(&text);
        Some(Scanned { text, plain })
    }
}

/// A fence opener: up to three spaces, then three or more backticks or tildes.
fn opens_fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = trimmed.chars().take_while(|c| *c == marker).count();
    if len < 3 {
        return None;
    }
    // A backtick fence's info string may not contain a backtick (that would be a code span).
    if marker == '`' && trimmed[len..].contains('`') {
        return None;
    }
    Some((marker, len))
}

fn closes_fence(line: &str, marker: char, min_len: usize) -> bool {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return false;
    }
    let len = trimmed.chars().take_while(|c| *c == marker).count();
    len >= min_len && trimmed[len..].trim().is_empty()
}

/// Replace the inside of `<!-- ... -->` (possibly spanning lines) with spaces.
fn blank_comments(line: &str, in_comment: &mut bool) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    loop {
        if *in_comment {
            match rest.find("-->") {
                Some(end) => {
                    out.extend(std::iter::repeat_n(' ', rest[..end + 3].chars().count()));
                    rest = &rest[end + 3..];
                    *in_comment = false;
                }
                None => {
                    out.extend(std::iter::repeat_n(' ', rest.chars().count()));
                    return out;
                }
            }
        } else {
            match rest.find("<!--") {
                Some(start) => {
                    out.push_str(&rest[..start]);
                    rest = &rest[start..];
                    *in_comment = true;
                }
                None => {
                    out.push_str(rest);
                    return out;
                }
            }
        }
    }
}

/// Replace inline code spans (matching backtick runs of equal length) with spaces, so brackets
/// and parentheses inside code are never read as links.
fn blank_code_spans(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = chars.clone();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i] != '`' {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i] == '`' {
            i += 1;
        }
        let run = i - start;
        // Find the closing run of the same length.
        let mut j = i;
        let mut close = None;
        while j < chars.len() {
            if chars[j] == '`' {
                let from = j;
                while j < chars.len() && chars[j] == '`' {
                    j += 1;
                }
                if j - from == run {
                    close = Some(j);
                    break;
                }
            } else {
                j += 1;
            }
        }
        if let Some(end) = close {
            for c in &mut out[start..end] {
                *c = ' ';
            }
            i = end;
        }
    }
    out.into_iter().collect()
}

// ---------------------------------------------------------------------------------------------
// Link parser
// ---------------------------------------------------------------------------------------------

/// Every link, image, reference definition and HTML `href` / `src` in `text`.
pub fn parse_links(text: &str) -> Vec<Link> {
    let mut scanner = Scanner::default();
    let mut links = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let Some(scanned) = scanner.line(raw) else {
            continue;
        };
        let line = scanned.plain;
        let line_number = index + 1;
        if let Some(link) = parse_definition(&line, line_number) {
            links.push(link);
            continue;
        }
        parse_inline(&line, line_number, &mut links);
        parse_html_attributes(&line, line_number, &mut links);
    }
    links
}

/// `[label]: target "title"` at the start of a line (up to three spaces of indent).
fn parse_definition(line: &str, line_number: usize) -> Option<Link> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let rest = trimmed.strip_prefix('[')?;
    let close = rest.find("]:")?;
    let label = &rest[..close];
    if label.is_empty() || label.starts_with('^') || label.contains('[') || label.contains(']') {
        return None;
    }
    let after = rest[close + 2..].trim_start();
    let target = if let Some(inner) = after.strip_prefix('<') {
        inner.split_once('>')?.0.to_string()
    } else {
        after.split_whitespace().next()?.to_string()
    };
    Some(Link {
        line: line_number,
        text: label.to_string(),
        target,
        kind: LinkKind::Definition,
    })
}

fn parse_inline(line: &str, line_number: usize, links: &mut Vec<Link>) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                i += 2;
                continue;
            }
            ']' if chars.get(i + 1) == Some(&'(') => {
                if let (Some(open), Some((target, end))) = (
                    matching_open_bracket(&chars, i),
                    parse_destination(&chars, i + 2),
                ) {
                    let image = open > 0 && chars[open - 1] == '!';
                    links.push(Link {
                        line: line_number,
                        text: chars[open + 1..i]
                            .iter()
                            .collect::<String>()
                            .trim()
                            .to_string(),
                        target,
                        kind: if image {
                            LinkKind::Image
                        } else {
                            LinkKind::Inline
                        },
                    });
                    i = end;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// Index of the `[` that pairs with the `]` at `close`, on the same line.
fn matching_open_bracket(chars: &[char], close: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = close;
    while i > 0 {
        i -= 1;
        // An escaped bracket does not count.
        let escaped = i > 0 && chars[i - 1] == '\\';
        match chars[i] {
            ']' if !escaped => depth += 1,
            '[' if !escaped => {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// Parse `target "title")` starting just after the `(`. Returns the target and the index just
/// after the closing `)`.
fn parse_destination(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut i = start;
    while chars.get(i).is_some_and(|c| *c == ' ' || *c == '\t') {
        i += 1;
    }
    let mut target = String::new();
    if chars.get(i) == Some(&'<') {
        i += 1;
        loop {
            match chars.get(i)? {
                '>' => {
                    i += 1;
                    break;
                }
                '\\' if chars.get(i + 1).is_some() => {
                    target.push(chars[i + 1]);
                    i += 2;
                }
                c => {
                    target.push(*c);
                    i += 1;
                }
            }
        }
    } else {
        let mut depth = 0usize;
        while let Some(&c) = chars.get(i) {
            match c {
                c if c.is_whitespace() => break,
                '\\' if chars.get(i + 1).is_some_and(|n| n.is_ascii_punctuation()) => {
                    target.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                '(' => depth += 1,
                ')' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                _ => {}
            }
            target.push(c);
            i += 1;
        }
    }
    while chars.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    // Optional title.
    if let Some(&open) = chars.get(i)
        && matches!(open, '"' | '\'' | '(')
    {
        let close = if open == '(' { ')' } else { open };
        i += 1;
        loop {
            match chars.get(i)? {
                '\\' => i += 2,
                c if *c == close => {
                    i += 1;
                    break;
                }
                _ => i += 1,
            }
        }
        while chars.get(i).is_some_and(|c| c.is_whitespace()) {
            i += 1;
        }
    }
    if chars.get(i) == Some(&')') {
        Some((target, i + 1))
    } else {
        None
    }
}

/// `href="..."` and `src="..."` in raw HTML (single or double quotes).
fn parse_html_attributes(line: &str, line_number: usize, links: &mut Vec<Link>) {
    for attribute in ["href", "src"] {
        let mut from = 0;
        while let Some(found) = line[from..].find(attribute) {
            let at = from + found;
            from = at + attribute.len();
            let before_ok = line[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace());
            if !before_ok {
                continue;
            }
            let rest = line[from..].trim_start();
            let Some(rest) = rest.strip_prefix('=') else {
                continue;
            };
            let rest = rest.trim_start();
            let Some(quote) = rest.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
                continue;
            };
            if let Some(end) = rest[1..].find(quote) {
                links.push(Link {
                    line: line_number,
                    text: String::new(),
                    target: rest[1..1 + end].to_string(),
                    kind: LinkKind::Html,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Heading anchors (GitHub slugs)
// ---------------------------------------------------------------------------------------------

/// Every anchor a link into this Markdown text may use: heading slugs (with `-1`, `-2` suffixes
/// for repeats) and explicit `id` / `name` attributes, all lower case.
pub fn anchors_in(text: &str) -> HashSet<String> {
    let mut scanner = Scanner::default();
    let mut anchors = HashSet::new();
    for raw in text.lines() {
        let Some(scanned) = scanner.line(raw) else {
            continue;
        };
        if let Some(heading) = atx_heading(&scanned.text) {
            let base = slugify(&heading_plain_text(heading));
            let mut slug = base.clone();
            let mut n = 0;
            while anchors.contains(&slug) {
                n += 1;
                slug = format!("{base}-{n}");
            }
            anchors.insert(slug);
        }
        for attribute in ["id", "name"] {
            collect_attribute_values(&scanned.plain, attribute, &mut anchors);
        }
    }
    anchors
}

fn collect_attribute_values(line: &str, attribute: &str, out: &mut HashSet<String>) {
    let mut from = 0;
    while let Some(found) = line[from..].find(attribute) {
        let at = from + found;
        from = at + attribute.len();
        if !line[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_whitespace())
        {
            continue;
        }
        let Some(rest) = line[from..].trim_start().strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(quote) = rest.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
            continue;
        };
        if let Some(end) = rest[1..].find(quote) {
            out.insert(rest[1..1 + end].to_lowercase());
        }
    }
}

/// The text of an ATX heading line (`## Title ##` gives `Title`), or `None`.
fn atx_heading(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let mut text = rest.trim();
    // Optional closing sequence: spaces, then hashes, then nothing.
    let without_hashes = text.trim_end_matches('#');
    if without_hashes.len() != text.len()
        && (without_hashes.is_empty() || without_hashes.ends_with([' ', '\t']))
    {
        text = without_hashes.trim_end();
    }
    Some(text)
}

/// A heading's rendered text: code spans keep their content, links and images give their text,
/// emphasis markers and HTML tags disappear.
fn heading_plain_text(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    // (character, literal): literal characters are never treated as emphasis markers.
    let mut out: Vec<(char, bool)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '`' => {
                let start = i;
                while i < chars.len() && chars[i] == '`' {
                    i += 1;
                }
                let run = i - start;
                let mut j = i;
                let mut close = None;
                while j < chars.len() {
                    if chars[j] == '`' {
                        let from = j;
                        while j < chars.len() && chars[j] == '`' {
                            j += 1;
                        }
                        if j - from == run {
                            close = Some(from);
                            break;
                        }
                    } else {
                        j += 1;
                    }
                }
                match close {
                    Some(end) => {
                        for &ch in &chars[i..end] {
                            out.push((ch, true));
                        }
                        i = end + run;
                    }
                    None => out.extend(std::iter::repeat_n(('`', true), run)),
                }
            }
            '\\' if chars.get(i + 1).is_some_and(|n| n.is_ascii_punctuation()) => {
                out.push((chars[i + 1], true));
                i += 2;
            }
            ']' if chars.get(i + 1) == Some(&'(') => {
                // Drop the destination of an inline link; the text was already emitted.
                match parse_destination(&chars, i + 2) {
                    Some((_, end)) => i = end,
                    None => {
                        out.push((c, false));
                        i += 1;
                    }
                }
            }
            '<' if chars
                .get(i + 1)
                .is_some_and(|n| n.is_ascii_alphabetic() || *n == '/') =>
            {
                match chars[i..].iter().position(|ch| *ch == '>') {
                    Some(offset) => i += offset + 1,
                    None => {
                        out.push((c, false));
                        i += 1;
                    }
                }
            }
            '*' => i += 1,
            _ => {
                out.push((c, false));
                i += 1;
            }
        }
    }
    // Underscores that are not inside a word are emphasis markers; those inside a word are text.
    let mut text = String::new();
    for (index, &(c, literal)) in out.iter().enumerate() {
        if c == '_' && !literal {
            let before = index
                .checked_sub(1)
                .is_some_and(|p| out[p].0.is_alphanumeric());
            let after = out.get(index + 1).is_some_and(|n| n.0.is_alphanumeric());
            if !(before && after) {
                continue;
            }
        }
        text.push(c);
    }
    text
}

/// GitHub's heading anchor: lower case, keep letters, digits, `-` and `_`, spaces become `-`,
/// everything else is dropped.
pub fn slugify(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            '-' | '_' => Some(c),
            c if c.is_alphanumeric() => Some(c),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets(text: &str) -> Vec<String> {
        parse_links(text).into_iter().map(|l| l.target).collect()
    }

    // -- link parser ----------------------------------------------------------------------

    #[test]
    fn inline_links_and_images_are_found_with_line_numbers() {
        let links = parse_links("intro\nsee [the spec](docs/spec.md) and ![pic](img/a.png)\n");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].line, 2);
        assert_eq!(links[0].text, "the spec");
        assert_eq!(links[0].target, "docs/spec.md");
        assert_eq!(links[0].kind, LinkKind::Inline);
        assert_eq!(links[1].target, "img/a.png");
        assert_eq!(links[1].kind, LinkKind::Image);
    }

    #[test]
    fn titles_angle_brackets_and_nested_parentheses() {
        assert_eq!(targets(r#"[a](x.md "A title")"#), ["x.md"]);
        assert_eq!(targets("[a](x.md 'single')"), ["x.md"]);
        assert_eq!(targets("[a](x.md (paren title))"), ["x.md"]);
        assert_eq!(targets("[a](<my file.md>)"), ["my file.md"]);
        assert_eq!(targets("[a](dir/f(1).md)"), ["dir/f(1).md"]);
        assert_eq!(targets("([a](x.md))"), ["x.md"]);
        assert_eq!(targets("[a](  spaced.md  )"), ["spaced.md"]);
        assert_eq!(targets(r"[a](x\(1\).md)"), ["x(1).md"]);
    }

    #[test]
    fn link_text_may_hold_code_and_brackets() {
        let links = parse_links("[`cargo xtask` [docs]](a.md) and [![badge](b.svg)](c.md)");
        assert_eq!(
            links.iter().map(|l| l.target.as_str()).collect::<Vec<_>>(),
            ["a.md", "b.svg", "c.md"]
        );
        assert_eq!(links[0].kind, LinkKind::Inline);
        assert_eq!(links[1].kind, LinkKind::Image);
    }

    #[test]
    fn code_is_not_parsed() {
        assert!(targets("run `[a](b.md)` now").is_empty());
        assert!(targets("``[a](`b.md`)``").is_empty());
        assert!(targets("```\n[a](b.md)\n```\n").is_empty());
        assert!(targets("~~~md\n[a](b.md)\n~~~\n").is_empty());
        // A longer fence is not closed by a shorter one.
        assert!(targets("````\n```\n[a](b.md)\n```\n````\n").is_empty());
        // After the fence closes, links count again.
        assert_eq!(targets("```\n[a](b.md)\n```\n[c](d.md)\n"), ["d.md"]);
    }

    #[test]
    fn unclosed_backtick_does_not_swallow_the_line() {
        assert_eq!(targets("a ` stray [b](c.md)"), ["c.md"]);
    }

    #[test]
    fn html_comments_are_not_parsed() {
        assert!(targets("<!-- [a](b.md) -->").is_empty());
        assert!(targets("<!--\n[a](b.md)\n-->\n").is_empty());
        assert_eq!(targets("<!-- x --> [a](b.md)"), ["b.md"]);
    }

    #[test]
    fn escaped_and_unpaired_brackets_are_not_links() {
        assert!(targets(r"\[a](b.md)").is_empty());
        assert!(targets("a](b.md)").is_empty());
        assert!(targets("[a](b.md").is_empty());
        assert!(targets("x[0](1)").iter().all(|t| t == "1"));
    }

    #[test]
    fn reference_definitions_are_found() {
        let links =
            parse_links("[spec]: docs/spec.md \"title\"\n  [other]: <a b.md>\n[^1]: note\n");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "docs/spec.md");
        assert_eq!(links[0].kind, LinkKind::Definition);
        assert_eq!(links[1].target, "a b.md");
        // Four spaces of indent is a code block, not a definition.
        assert!(targets("    [a]: b.md").is_empty());
    }

    #[test]
    fn html_attributes_are_found() {
        let links = parse_links(r#"<a href="x.md#top">x</a> <img src='y.png' alt="z">"#);
        assert_eq!(
            links.iter().map(|l| l.target.as_str()).collect::<Vec<_>>(),
            ["x.md#top", "y.png"]
        );
        assert!(links.iter().all(|l| l.kind == LinkKind::Html));
        // `data-src` is not `src`.
        assert!(targets(r#"<div data-src="x.png">"#).is_empty());
    }

    #[test]
    fn external_links_are_classified() {
        assert!(is_external("https://example.com/a"));
        assert!(is_external("mailto:a@b.c"));
        assert!(is_external("//cdn.example.com/x"));
        assert!(!is_external("docs/a.md"));
        assert!(!is_external("#anchor"));
        assert!(!is_external("a:b")); // one letter is a drive, not a scheme
        assert!(is_windows_drive_path("C:/Users/x"));
        assert!(is_windows_drive_path(r"c:\x"));
        assert!(!is_windows_drive_path("docs/c:x"));
    }

    // -- slugs and anchors ----------------------------------------------------------------

    fn slug_of(heading: &str) -> String {
        slugify(&heading_plain_text(heading))
    }

    #[test]
    fn slugs_follow_github() {
        assert_eq!(slug_of("What this is"), "what-this-is");
        assert_eq!(
            slug_of("Non-negotiables (security)"),
            "non-negotiables-security"
        );
        assert_eq!(slug_of("Commands & `--ui-smoke`"), "commands----ui-smoke");
        assert_eq!(slug_of("Plan A -- the fast one"), "plan-a----the-fast-one");
        assert_eq!(slug_of("WP-1C: Docs sync"), "wp-1c-docs-sync");
        assert_eq!(slug_of("Day 1 / Night 2"), "day-1--night-2");
        assert_eq!(slug_of("Ünïcode Heading"), "ünïcode-heading");
    }

    #[test]
    fn heading_markup_is_stripped_before_slugging() {
        assert_eq!(slug_of("The [Gander](https://x.y/z) app"), "the-gander-app");
        assert_eq!(slug_of("**Bold** and *italic*"), "bold-and-italic");
        assert_eq!(slug_of("_emphasis_ here"), "emphasis-here");
        assert_eq!(slug_of("snake_case stays"), "snake_case-stays");
        assert_eq!(slug_of("`__init__` is code"), "__init__-is-code");
        assert_eq!(slug_of("a <b>tag</b> inside"), "a-tag-inside");
        assert_eq!(slug_of("![logo](a.png) Title"), "logo-title");
    }

    #[test]
    fn anchors_cover_atx_headings_duplicates_and_html_ids() {
        let text = "\
# Title

## Setup ##

## Setup

## Setup

```sh
# not a heading
```

####### seven hashes is not a heading
#nospace is not a heading

<a name=\"Legacy-Anchor\"></a>
<h2 id='custom'>x</h2>
";
        let anchors = anchors_in(text);
        for expected in [
            "title",
            "setup",
            "setup-1",
            "setup-2",
            "legacy-anchor",
            "custom",
        ] {
            assert!(
                anchors.contains(expected),
                "missing {expected}: {anchors:?}"
            );
        }
        assert!(!anchors.contains("not-a-heading"));
        assert!(!anchors.contains("nospace-is-not-a-heading"));
    }

    #[test]
    fn closing_hashes_need_a_space() {
        assert_eq!(atx_heading("## C# language"), Some("C# language"));
        assert_eq!(atx_heading("## Done ##"), Some("Done"));
        assert_eq!(atx_heading("##"), Some(""));
        assert_eq!(atx_heading("#hashtag"), None);
        assert_eq!(atx_heading("    # indented code"), None);
    }

    // -- resolution -----------------------------------------------------------------------

    fn source(path: &str) -> Vec<String> {
        path.split('/').map(String::from).collect()
    }

    #[test]
    fn relative_targets_resolve_against_the_source_folder() {
        let s = source("docs/specs/a.md");
        assert_eq!(
            join_relative(&s, "b.md").unwrap(),
            ["docs", "specs", "b.md"]
        );
        assert_eq!(
            join_relative(&s, "./b.md").unwrap(),
            ["docs", "specs", "b.md"]
        );
        assert_eq!(
            join_relative(&s, "../vision.md").unwrap(),
            ["docs", "vision.md"]
        );
        assert_eq!(join_relative(&s, "../../README.md").unwrap(), ["README.md"]);
        assert_eq!(join_relative(&s, "/CLAUDE.md").unwrap(), ["CLAUDE.md"]);
        assert_eq!(join_relative(&s, "dir/").unwrap(), ["docs", "specs", "dir"]);
        assert!(join_relative(&s, "../../../x.md").is_err());
        assert!(join_relative(&source("a.md"), "../x.md").is_err());
    }

    #[test]
    fn percent_escapes_decode() {
        assert_eq!(percent_decode("my%20file.md"), "my file.md");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("caf%C3%A9"), "caf\u{e9}");
    }

    // -- whole-tree behaviour -------------------------------------------------------------

    fn temp_tree(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let root =
            std::env::temp_dir().join(format!("gander-docs-{tag}-{}-{nanos}", std::process::id()));
        for (path, content) in files {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        }
        root
    }

    #[test]
    fn a_tree_with_good_and_bad_links_is_reported() {
        let root = temp_tree(
            "tree",
            &[
                (
                    "README.md",
                    "# Top
[ok](docs/a.md) [anchor](docs/a.md#setup) [gone](docs/missing.md)\n\
                     [bad anchor](docs/a.md#nope) [web](https://example.com) [dir](docs/)\n\
                     ![img](docs/pic.png) [escape](../outside.md) [drive](C:/x/y.md)\n\
                     [case](Docs/a.md) [query](docs/a.md?plain=1#setup) [self](#top)\n",
                ),
                (
                    "docs/a.md",
                    "# Top\n\n## Setup\n\n[up](../README.md) [sib](b.md#only)\n",
                ),
                ("docs/b.md", "## Only\n"),
                ("docs/pic.png", "x"),
                // Excluded trees are neither read nor linked to successfully.
                ("target/skip.md", "[broken](nowhere.md)\n"),
                (".worktrees/w/skip.md", "[broken](nowhere.md)\n"),
                ("node_modules/p/skip.md", "[broken](nowhere.md)\n"),
                ("client/build/skip.md", "[broken](nowhere.md)\n"),
            ],
        );
        let report = check_repository(&root).unwrap();
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(report.files, 3, "excluded folders must not be walked");
        assert_eq!(report.external, 1);
        let reasons: Vec<(usize, &str)> = report
            .broken
            .iter()
            .map(|b| (b.line, b.reason.as_str()))
            .collect();
        assert_eq!(report.broken.len(), 5, "{reasons:#?}");
        assert!(report.broken.iter().all(|b| b.file == "README.md"));
        let reason = |needle: &str| {
            report
                .broken
                .iter()
                .find(|b| b.link.contains(needle))
                .unwrap_or_else(|| panic!("no broken link mentioning {needle}: {reasons:#?}"))
                .reason
                .clone()
        };
        assert!(reason("missing.md").starts_with("not found"));
        assert!(reason("#nope").contains("no heading or id `nope`"));
        assert!(reason("../outside.md").contains("outside the repository"));
        assert!(reason("C:/x/y.md").contains("Windows path"));
        let case = reason("Docs/a.md");
        // On a case-insensitive file system the directory list still says `docs`.
        assert!(case.contains("case mismatch"), "{case}");
        assert_eq!(report.anchors, 5);
    }

    #[test]
    fn a_clean_tree_passes() {
        let root = temp_tree(
            "clean",
            &[
                ("README.md", "[a](docs/a.md#title)\n"),
                ("docs/a.md", "# Title\n"),
            ],
        );
        let report = check_repository(&root).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(report.broken.is_empty(), "{:?}", report.broken);
        assert_eq!((report.files, report.checked, report.anchors), (2, 1, 1));
    }

    #[test]
    fn the_repository_docs_have_no_broken_links() {
        let report = check_repository(&workspace_root()).unwrap();
        assert!(
            report.files > 10,
            "expected the repository's Markdown files"
        );
        assert!(
            report.broken.is_empty(),
            "broken links (run `cargo xtask docs-check`):\n{:#?}",
            report.broken
        );
    }
}
