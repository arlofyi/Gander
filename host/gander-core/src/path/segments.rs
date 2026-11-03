// SPDX-License-Identifier: Apache-2.0
//! Algorithm steps 1-4: split a protocol path and validate every segment before any filesystem
//! access.

use super::RejectReason;

/// Step 3: maximum number of segments.
pub const MAX_SEGMENTS: usize = 128;
/// Step 3: maximum total length of the protocol path in bytes.
pub const MAX_PATH_BYTES: usize = 4096;

/// A protocol path that passed steps 1-4: zero or more segments, each non-empty, valid UTF-8 and
/// free of separators, dot segments, reserved names and characters Windows would reinterpret.
///
/// The root is the empty path. This type only proves the shape is safe; the platform resolver
/// still has to walk it handle by handle.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ValidatedPath {
    segments: Vec<String>,
}

impl ValidatedPath {
    /// The share root (`""` or `"/"`).
    pub fn root() -> Self {
        Self::default()
    }

    /// Steps 1-4 on raw bytes. The input has been percent-decoded exactly once by the HTTP layer
    /// and is NOT decoded again; bytes are accepted so invalid UTF-8 is reported as
    /// [`RejectReason::InvalidUtf8`] instead of being lost in a lossy conversion.
    pub fn parse(input: &[u8]) -> Result<Self, RejectReason> {
        // "" and "/" are the root; any other leading slash yields an empty first segment.
        if input.is_empty() || input == b"/" {
            return Ok(Self::root());
        }
        // Step 2: split, drop one trailing empty segment, refuse other empty segments.
        let body = input.strip_suffix(b"/").unwrap_or(input);
        let mut count = 0usize;
        for part in body.split(|&b| b == b'/') {
            if part.is_empty() {
                return Err(RejectReason::EmptySegment);
            }
            count += 1;
        }
        // Step 3.
        if count > MAX_SEGMENTS {
            return Err(RejectReason::TooDeep);
        }
        if input.len() > MAX_PATH_BYTES {
            return Err(RejectReason::TooLong);
        }
        // Step 4, per segment in order.
        let segments = body
            .split(|&b| b == b'/')
            .map(validate_segment)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { segments })
    }

    /// Steps 1-4 on a string (a Rust `str` is always valid UTF-8).
    pub fn parse_str(input: &str) -> Result<Self, RejectReason> {
        Self::parse(input.as_bytes())
    }

    /// Whether this is the share root.
    pub fn is_root(&self) -> bool {
        self.segments.is_empty()
    }

    /// The validated segments, root first.
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// The segments as `&str`, for pattern matching.
    pub fn segment_strs(&self) -> Vec<&str> {
        self.segments.iter().map(String::as_str).collect()
    }

    /// The canonical protocol form: segments joined with `/`, no leading or trailing slash.
    pub fn to_protocol_string(&self) -> String {
        self.segments.join("/")
    }

    /// The last segment, or `None` for the root.
    pub fn file_name(&self) -> Option<&str> {
        self.segments.last().map(String::as_str)
    }
}

/// Step 4a-4e for one segment, in the order the spec gives.
fn validate_segment(seg: &[u8]) -> Result<String, RejectReason> {
    // 4a
    if seg == b"." || seg == b".." {
        return Err(RejectReason::DotSegment);
    }
    // 4b: NUL and 0x01..0x1F, plus the characters Windows reinterprets. Backslash and ':' are
    // rejected on every platform so behaviour is identical everywhere.
    if seg
        .iter()
        .any(|&b| b < 0x20 || matches!(b, b'\\' | b':' | b'<' | b'>' | b'"' | b'|' | b'?' | b'*'))
    {
        return Err(RejectReason::InvalidChar);
    }
    // 4c
    if matches!(seg.last(), Some(b'.' | b' ')) {
        return Err(RejectReason::TrailingDotOrSpace);
    }
    // 4d
    if is_reserved_name(seg) {
        return Err(RejectReason::ReservedName);
    }
    // 4e
    match std::str::from_utf8(seg) {
        Ok(s) => Ok(s.to_owned()),
        Err(_) => Err(RejectReason::InvalidUtf8),
    }
}

/// Superscript one, two and three in UTF-8 (`COM¹`, `LPT²`, ...).
const SUPERSCRIPTS: [&[u8]; 3] = [b"\xC2\xB9", b"\xC2\xB2", b"\xC2\xB3"];

/// Step 4d: the base name (before the first '.') is a DOS device name, case-insensitively.
///
/// Trailing spaces of the base name are ignored too (`"con .txt"`): Win32 path parsing strips
/// them, so this is slightly stricter than the spec text and never weaker.
fn is_reserved_name(seg: &[u8]) -> bool {
    let base = seg.split(|&b| b == b'.').next().unwrap_or(seg);
    let base = trim_trailing_spaces(base);
    let upper: Vec<u8> = base.to_ascii_uppercase();
    match upper.len() {
        3 => matches!(&upper[..], b"CON" | b"PRN" | b"AUX" | b"NUL"),
        4 => (upper.starts_with(b"COM") || upper.starts_with(b"LPT")) && upper[3].is_ascii_digit(),
        5 => {
            (upper.starts_with(b"COM") || upper.starts_with(b"LPT"))
                && SUPERSCRIPTS.contains(&&upper[3..])
        }
        _ => false,
    }
}

fn trim_trailing_spaces(mut s: &[u8]) -> &[u8] {
    while let [rest @ .., b' '] = s {
        s = rest;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(input: &str) -> Option<RejectReason> {
        ValidatedPath::parse_str(input).err()
    }

    #[test]
    fn root_forms() {
        assert!(ValidatedPath::parse_str("").unwrap().is_root());
        assert!(ValidatedPath::parse_str("/").unwrap().is_root());
        assert_eq!(reason("//"), Some(RejectReason::EmptySegment));
        assert_eq!(reason("/a"), Some(RejectReason::EmptySegment));
        assert_eq!(reason("a//b"), Some(RejectReason::EmptySegment));
        assert_eq!(reason("a//"), Some(RejectReason::EmptySegment));
    }

    #[test]
    fn trailing_slash_is_dropped_once() {
        let p = ValidatedPath::parse_str("dir/sub/").unwrap();
        assert_eq!(p.segments(), ["dir", "sub"]);
        assert_eq!(p.to_protocol_string(), "dir/sub");
        assert_eq!(p.file_name(), Some("sub"));
    }

    #[test]
    fn limits() {
        let deep = "d/".repeat(128) + "a";
        assert_eq!(reason(&deep), Some(RejectReason::TooDeep));
        let ok_deep = "d/".repeat(127) + "a";
        assert!(reason(&ok_deep).is_none());
        assert_eq!(reason(&"a".repeat(4097)), Some(RejectReason::TooLong));
    }

    #[test]
    fn per_segment_checks() {
        assert_eq!(reason(".."), Some(RejectReason::DotSegment));
        assert_eq!(reason("a/./b"), Some(RejectReason::DotSegment));
        assert_eq!(reason("..."), Some(RejectReason::TrailingDotOrSpace));
        assert_eq!(reason("a\u{7}"), Some(RejectReason::InvalidChar));
        assert_eq!(reason("a:b"), Some(RejectReason::InvalidChar));
        assert_eq!(reason("a "), Some(RejectReason::TrailingDotOrSpace));
        assert_eq!(reason("Con"), Some(RejectReason::ReservedName));
        assert_eq!(reason("lpt0.tar.gz"), Some(RejectReason::ReservedName));
        assert_eq!(reason("LPT\u{b3}"), Some(RejectReason::ReservedName));
        assert_eq!(reason("con .txt"), Some(RejectReason::ReservedName));
        assert_eq!(reason("COM\u{b4}"), None);
        assert_eq!(reason("COM10"), None);
        assert_eq!(reason("console.log"), None);
        assert_eq!(reason(".gitignore"), None);
        assert_eq!(
            ValidatedPath::parse(b"\xC0\xAE\xC0\xAE").err(),
            Some(RejectReason::InvalidUtf8)
        );
    }

    #[test]
    fn checks_run_in_spec_order() {
        // invalid_char (4b) wins over invalid_utf8 (4e) within a segment.
        assert_eq!(
            ValidatedPath::parse(b"\xC3*").err(),
            Some(RejectReason::InvalidChar)
        );
        // Earlier segments are checked first.
        assert_eq!(reason("../a*"), Some(RejectReason::DotSegment));
    }

    mod prop {
        use super::super::*;
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(2048))]

            /// Fuzz invariant: arbitrary bytes never panic, and anything accepted is safe to hand
            /// to a handle-relative open one segment at a time.
            #[test]
            fn random_bytes_never_panic_and_accepted_segments_are_safe(
                input in proptest::collection::vec(any::<u8>(), 0..300)
            ) {
                if let Ok(path) = ValidatedPath::parse(&input) {
                    prop_assert!(path.segments().len() <= MAX_SEGMENTS);
                    for seg in path.segments() {
                        prop_assert!(!seg.is_empty());
                        prop_assert!(seg != "." && seg != "..");
                        prop_assert!(!seg.contains(['/', '\\', ':', '\0']));
                        prop_assert!(!seg.ends_with(['.', ' ']));
                    }
                }
            }

            /// Valid-looking segment lists round-trip through the protocol form.
            #[test]
            fn safe_segments_round_trip(
                segs in proptest::collection::vec("[a-zA-Z0-9_ ~%-]{0,6}[a-zA-Z0-9_~%-]", 1..8)
            ) {
                let joined = segs.join("/");
                match ValidatedPath::parse_str(&joined) {
                    Ok(p) => prop_assert_eq!(p.to_protocol_string(), joined),
                    Err(r) => prop_assert_eq!(r, RejectReason::ReservedName),
                }
            }
        }
    }
}
