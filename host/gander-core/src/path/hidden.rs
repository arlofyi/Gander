// SPDX-License-Identifier: Apache-2.0
//! Share-level `hidden_patterns` and the names that are always omitted from listings.
//!
//! Pattern syntax (matched against the share-relative path, segment by segment):
//!
//! - `*` matches any run of characters within one segment, `?` exactly one character;
//! - a segment that is exactly `**` matches zero or more whole segments;
//! - matching is case-insensitive (the first targets are case-insensitive volumes, and a
//!   case-sensitive match would let `.GIT/config` slip past `.git/**`).
//!
//! A path is hidden when the path itself OR ANY OF ITS ANCESTORS matches a pattern, so hiding a
//! directory also hides and blocks everything below it (`**/.git` blocks `.git/config`).

/// Names omitted from every listing regardless of `show_hidden` (compared case-insensitively).
const ALWAYS_OMITTED: [&str; 5] = [
    "desktop.ini",
    "thumbs.db",
    ".ds_store",
    "$recycle.bin",
    "system volume information",
];

/// Whether `name` is one of the entries every listing omits (`desktop.ini`, `Thumbs.db`,
/// `.DS_Store`, `$RECYCLE.BIN`, `System Volume Information`).
pub fn is_always_omitted(name: &str) -> bool {
    ALWAYS_OMITTED.iter().any(|o| o.eq_ignore_ascii_case(name))
}

/// A pattern in `hidden_patterns` is malformed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid hidden pattern {pattern:?}: {why}")]
pub struct PatternError {
    /// The offending pattern.
    pub pattern: String,
    /// What is wrong with it.
    pub why: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    /// `**`
    AnyDepth,
    /// A glob over one segment, pre-folded to lower case.
    Glob(Vec<char>),
}

/// Compiled `hidden_patterns` of one share.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HiddenPatterns {
    patterns: Vec<Vec<Seg>>,
}

impl HiddenPatterns {
    /// Compiles the share's patterns. An empty list hides nothing.
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Result<Self, PatternError> {
        let mut compiled = Vec::with_capacity(patterns.len());
        for p in patterns {
            let p = p.as_ref();
            let err = |why| PatternError {
                pattern: p.to_owned(),
                why,
            };
            if p.is_empty() {
                return Err(err("empty pattern"));
            }
            if p.contains('\\') {
                return Err(err("use '/' as the separator"));
            }
            let body = p.strip_suffix('/').unwrap_or(p);
            let mut segs = Vec::new();
            for s in body.split('/') {
                if s.is_empty() {
                    return Err(err("empty segment (leading slash or '//')"));
                }
                if s == "**" {
                    segs.push(Seg::AnyDepth);
                } else if s.contains("**") {
                    return Err(err("'**' must be a whole segment"));
                } else {
                    segs.push(Seg::Glob(fold(s)));
                }
            }
            compiled.push(segs);
        }
        Ok(Self { patterns: compiled })
    }

    /// Whether no pattern is configured.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Whether the share-relative path `segments` (or one of its ancestors) is hidden.
    pub fn is_hidden(&self, segments: &[&str]) -> bool {
        if self.patterns.is_empty() || segments.is_empty() {
            return false;
        }
        let folded: Vec<Vec<char>> = segments.iter().map(|s| fold(s)).collect();
        (1..=folded.len()).any(|n| self.matches_exact(&folded[..n]))
    }

    /// Whether `name` inside the directory `dir` is hidden; avoids re-folding by the caller.
    pub fn is_child_hidden(&self, dir: &[&str], name: &str) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let mut path: Vec<&str> = dir.to_vec();
        path.push(name);
        self.is_hidden(&path)
    }

    fn matches_exact(&self, path: &[Vec<char>]) -> bool {
        self.patterns.iter().any(|p| match_segs(p, path))
    }
}

fn fold(s: &str) -> Vec<char> {
    s.chars().flat_map(char::to_lowercase).collect()
}

fn match_segs(pat: &[Seg], path: &[Vec<char>]) -> bool {
    match pat.split_first() {
        None => path.is_empty(),
        Some((Seg::AnyDepth, rest)) => (0..=path.len()).any(|skip| match_segs(rest, &path[skip..])),
        Some((Seg::Glob(g), rest)) => match path.split_first() {
            Some((first, tail)) => match_glob(g, first) && match_segs(rest, tail),
            None => false,
        },
    }
}

/// Iterative glob with `*` and `?` over one segment (linear backtracking, no recursion).
fn match_glob(pat: &[char], s: &[char]) -> bool {
    let (mut p, mut i) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while i < s.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == s[i]) {
            p += 1;
            i += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = Some((p, i));
            p += 1;
        } else if let Some((sp, si)) = star {
            p = sp + 1;
            i = si + 1;
            star = Some((sp, si + 1));
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hp(p: &[&str]) -> HiddenPatterns {
        HiddenPatterns::new(p).unwrap()
    }

    #[test]
    fn dir_double_star_blocks_contents_and_dir() {
        let h = hp(&[".git/**"]);
        assert!(h.is_hidden(&[".git", "config"]));
        assert!(h.is_hidden(&[".git"]));
        assert!(h.is_hidden(&[".GIT", "objects", "ab"]));
        assert!(!h.is_hidden(&["src", ".git"]));
        assert!(!h.is_hidden(&[".gitignore"]));
    }

    #[test]
    fn leading_double_star_matches_anywhere() {
        let h = hp(&["**/.git", "*.tmp", "build/out?"]);
        assert!(h.is_hidden(&["a", "b", ".git", "config"]));
        assert!(h.is_hidden(&[".git"]));
        assert!(h.is_hidden(&["x.TMP"]));
        assert!(!h.is_hidden(&["dir", "x.tmp"]));
        assert!(h.is_hidden(&["build", "out1", "f"]));
        assert!(!h.is_hidden(&["build", "out12"]));
        assert!(h.is_child_hidden(&["a"], ".git"));
    }

    #[test]
    fn glob_star_backtracks() {
        assert!(match_glob(&fold("*a*b"), &fold("xxaxxb")));
        assert!(!match_glob(&fold("*a*b"), &fold("xxaxxc")));
        assert!(match_glob(&fold("*"), &fold("")));
    }

    #[test]
    fn bad_patterns() {
        assert!(HiddenPatterns::new(&[""]).is_err());
        assert!(HiddenPatterns::new(&["/abs"]).is_err());
        assert!(HiddenPatterns::new(&["a\\b"]).is_err());
        assert!(HiddenPatterns::new(&["a**"]).is_err());
        assert!(HiddenPatterns::new(&["dir/"]).is_ok());
    }

    #[test]
    fn always_omitted() {
        assert!(is_always_omitted("Thumbs.db"));
        assert!(is_always_omitted("$RECYCLE.BIN"));
        assert!(is_always_omitted("desktop.INI"));
        assert!(!is_always_omitted("desktop.ini.bak"));
    }
}
