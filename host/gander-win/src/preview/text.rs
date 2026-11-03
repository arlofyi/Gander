// SPDX-License-Identifier: Apache-2.0
//! Text previews: encoding detection, a binary sniff and bounded, offset-addressed reads.
//!
//! This runs in the host process, not in a render worker, on purpose: it is a bounded read of an
//! already-open handle plus a hand-written decoder in safe Rust (no third-party parser, no
//! `unsafe`), which is what the "untrusted bytes are parsed out of process" rule exists to keep
//! out of the listener. At most [`MAX_WINDOW`] source bytes are read for a non-UTF-8 file, and
//! one request returns at most [`MAX_LIMIT`] bytes.
//!
//! Detection order, on the first [`SNIFF_BYTES`] bytes: byte order mark (UTF-8, UTF-16 LE/BE;
//! UTF-32 is refused as binary), BOM-less UTF-16 (a clear alternating-NUL pattern that decodes
//! cleanly), then binary (a NUL byte, or mostly control characters), then valid UTF-8, else
//! Windows-1252.
//!
//! **Offsets.** `offset`, `limit` and `total` count bytes of the *decoded* UTF-8 text, never
//! splitting a character:
//!
//! - UTF-8 files are addressed directly in the file (after the BOM), so any offset is a seek and
//!   `total` is the file size minus the BOM. Invalid sequences inside the returned chunk become
//!   U+FFFD, so for such a file [`TextChunk::next_offset`] is the authoritative next offset.
//! - Other encodings decode the first [`MAX_WINDOW`] bytes of the file into one string and slice
//!   it, so `total` is the decoded length of that window and [`TextChunk::window_truncated`] says
//!   when the file is longer.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

/// Bytes sniffed to detect the encoding and to decide whether the file is binary.
pub const SNIFF_BYTES: usize = 8192;
/// Default `limit` of `GET /preview/text`.
pub const DEFAULT_LIMIT: u64 = 64 * 1024;
/// Hard cap on `limit` (1 MiB).
pub const MAX_LIMIT: u64 = 1024 * 1024;
/// Most source bytes decoded for a file that is not UTF-8.
pub const MAX_WINDOW: u64 = 8 * 1024 * 1024;

/// A detected text encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// UTF-8 (with or without a byte order mark).
    Utf8,
    /// UTF-16 little endian.
    Utf16Le,
    /// UTF-16 big endian.
    Utf16Be,
    /// Windows-1252, the fallback for 8-bit text.
    Windows1252,
}

impl Encoding {
    /// The label reported in `X-Gander-Text-Encoding`.
    pub const fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf16Le => "utf-16le",
            Encoding::Utf16Be => "utf-16be",
            Encoding::Windows1252 => "windows-1252",
        }
    }
}

/// The result of sniffing the start of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detected {
    /// How to decode.
    pub encoding: Encoding,
    /// Length of the byte order mark to skip (0 when there is none).
    pub bom_len: usize,
}

/// Why a file is not previewed as text.
#[derive(Debug)]
pub enum TextError {
    /// Binary content: a NUL byte, UTF-32, or mostly control characters.
    Binary,
    /// The path is a directory.
    IsDirectory,
    /// A read failed.
    Io(io::Error),
}

impl From<io::Error> for TextError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::IsADirectory {
            TextError::IsDirectory
        } else {
            TextError::Io(e)
        }
    }
}

/// One slice of decoded text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk {
    /// UTF-8 bytes of the slice.
    pub bytes: Vec<u8>,
    /// The detected encoding.
    pub encoding: Encoding,
    /// Where the slice really starts (the requested offset moved forward to a character
    /// boundary, and clamped to `total`).
    pub offset: u64,
    /// Where the next slice starts; equals `offset + bytes.len()` unless invalid UTF-8 was
    /// replaced.
    pub next_offset: u64,
    /// Total size of the decoded text (see the module docs for what that means per encoding).
    pub total: u64,
    /// The file is longer than what was decoded (non-UTF-8 files over [`MAX_WINDOW`]).
    pub window_truncated: bool,
}

impl TextChunk {
    /// Whether text remains after this slice.
    pub fn has_more(&self) -> bool {
        self.next_offset < self.total
    }
}

/// Detects the encoding of a file from its first bytes (at most [`SNIFF_BYTES`]), or reports it
/// as binary.
pub fn detect(sample: &[u8]) -> Result<Detected, TextError> {
    if sample.starts_with(&[0xFF, 0xFE, 0x00, 0x00])
        || sample.starts_with(&[0x00, 0x00, 0xFE, 0xFF])
    {
        return Err(TextError::Binary); // UTF-32 is not a text preview in v1.
    }
    if sample.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Ok(Detected {
            encoding: Encoding::Utf8,
            bom_len: 3,
        });
    }
    if sample.starts_with(&[0xFF, 0xFE]) {
        return Ok(Detected {
            encoding: Encoding::Utf16Le,
            bom_len: 2,
        });
    }
    if sample.starts_with(&[0xFE, 0xFF]) {
        return Ok(Detected {
            encoding: Encoding::Utf16Be,
            bom_len: 2,
        });
    }
    if let Some(encoding) = sniff_utf16_without_bom(sample) {
        return Ok(Detected {
            encoding,
            bom_len: 0,
        });
    }
    if sample.contains(&0) || mostly_control(sample) {
        return Err(TextError::Binary);
    }
    // A character cut by the end of the sample is not an error (the next bytes may complete it).
    let utf8 = match std::str::from_utf8(sample) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    };
    Ok(Detected {
        encoding: if utf8 {
            Encoding::Utf8
        } else {
            Encoding::Windows1252
        },
        bom_len: 0,
    })
}

/// BOM-less UTF-16: ASCII-heavy text has a zero in every other byte. Accepted only when the
/// pattern is clear on one parity, absent on the other, and the sample decodes without errors
/// or NUL characters.
fn sniff_utf16_without_bom(sample: &[u8]) -> Option<Encoding> {
    let even_len = sample.len() & !1;
    if even_len < 4 {
        return None;
    }
    let units = even_len / 2;
    let (mut zero_even, mut zero_odd) = (0usize, 0usize);
    for pair in sample[..even_len].chunks_exact(2) {
        zero_even += usize::from(pair[0] == 0);
        zero_odd += usize::from(pair[1] == 0);
    }
    let clear = |hi: usize, lo: usize| hi * 10 >= units * 3 && lo * 50 <= units;
    let encoding = if clear(zero_odd, zero_even) {
        Encoding::Utf16Le
    } else if clear(zero_even, zero_odd) {
        Encoding::Utf16Be
    } else {
        return None;
    };
    let decoded = decode_utf16(&sample[..even_len], encoding == Encoding::Utf16Le);
    let bad = decoded
        .chars()
        .filter(|&c| c == '\u{FFFD}' || c == '\0')
        .count();
    (bad * 100 <= decoded.chars().count()).then_some(encoding)
}

/// More than 10 % control characters other than the usual whitespace and ESC.
fn mostly_control(sample: &[u8]) -> bool {
    if sample.is_empty() {
        return false;
    }
    let control = sample
        .iter()
        .filter(|&&b| {
            (b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 0x08 | 0x1B)) || b == 0x7F
        })
        .count();
    control * 10 > sample.len()
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let units = bytes.chunks_exact(2).map(|p| {
        if little_endian {
            u16::from_le_bytes([p[0], p[1]])
        } else {
            u16::from_be_bytes([p[0], p[1]])
        }
    });
    char::decode_utf16(units)
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect()
}

/// Code points of Windows-1252 bytes 0x80 to 0x9F (the rest of the range is Latin-1).
const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
];

fn decode_windows_1252(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => {
                char::from_u32(u32::from(CP1252_HIGH[usize::from(b - 0x80)])).unwrap_or('\u{FFFD}')
            }
            _ => char::from(b),
        })
        .collect()
}

/// Decodes a whole window of a non-UTF-8 file.
fn decode_window(bytes: &[u8], encoding: Encoding) -> String {
    match encoding {
        Encoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        Encoding::Utf16Le => decode_utf16(&bytes[..bytes.len() & !1], true),
        Encoding::Utf16Be => decode_utf16(&bytes[..bytes.len() & !1], false),
        Encoding::Windows1252 => decode_windows_1252(bytes),
    }
}

fn sniff(file: &File) -> Result<(Vec<u8>, u64), TextError> {
    let len = file.metadata()?.len();
    let mut f = file;
    f.seek(SeekFrom::Start(0))?;
    let mut sample = Vec::new();
    f.take(SNIFF_BYTES as u64).read_to_end(&mut sample)?;
    Ok((sample, len))
}

/// Detects the encoding of an open file.
pub fn probe(file: &File) -> Result<(Detected, u64), TextError> {
    let (sample, len) = sniff(file)?;
    Ok((detect(&sample)?, len))
}

/// Total size of the decoded text of a file (what `text_bytes` in `preview/info` reports).
pub fn decoded_total(file: &File) -> Result<(Encoding, u64), TextError> {
    let (det, len) = probe(file)?;
    if det.encoding == Encoding::Utf8 {
        return Ok((det.encoding, len.saturating_sub(det.bom_len as u64)));
    }
    let window = read_window(file, det, len)?;
    Ok((det.encoding, window.0.len() as u64))
}

fn read_window(file: &File, det: Detected, len: u64) -> Result<(String, bool), TextError> {
    let mut f = file;
    f.seek(SeekFrom::Start(det.bom_len as u64))?;
    let want = len.saturating_sub(det.bom_len as u64).min(MAX_WINDOW);
    let mut raw = Vec::with_capacity(want as usize);
    f.take(want).read_to_end(&mut raw)?;
    let truncated = len.saturating_sub(det.bom_len as u64) > MAX_WINDOW;
    Ok((decode_window(&raw, det.encoding), truncated))
}

/// Reads `limit` bytes of decoded text starting at `offset` (see the module docs).
pub fn read_chunk(file: &File, offset: u64, limit: u64) -> Result<TextChunk, TextError> {
    let limit = limit.clamp(1, MAX_LIMIT);
    let (sample, len) = sniff(file)?;
    let det = detect(&sample)?;
    if det.encoding == Encoding::Utf8 {
        return read_utf8(file, det, len, offset, limit);
    }
    let (text, window_truncated) = read_window(file, det, len)?;
    let total = text.len() as u64;
    let mut start = usize::try_from(offset.min(total)).unwrap_or(text.len());
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let mut end = start
        .saturating_add(usize::try_from(limit).unwrap_or(usize::MAX))
        .min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == start && start < text.len() {
        // The limit is smaller than one character: return that one character so a client that
        // advances by what it received still makes progress.
        end = start + text[start..].chars().next().map_or(0, char::len_utf8);
    }
    Ok(TextChunk {
        bytes: text.as_bytes()[start..end].to_vec(),
        encoding: det.encoding,
        offset: start as u64,
        next_offset: end as u64,
        total,
        window_truncated,
    })
}

fn read_utf8(
    file: &File,
    det: Detected,
    len: u64,
    offset: u64,
    limit: u64,
) -> Result<TextChunk, TextError> {
    let bom = det.bom_len as u64;
    let total = len.saturating_sub(bom);
    let mut start = offset.min(total);
    // Read a little before and after the slice so character boundaries can be found: up to 3
    // continuation bytes belong to a character that began before `start`.
    let lead = start.min(3);
    let want = (limit + lead + 4).min(total + lead - start);
    let mut f = file;
    f.seek(SeekFrom::Start(bom + start - lead))?;
    let mut raw = Vec::with_capacity(want as usize);
    f.take(want).read_to_end(&mut raw)?;
    let is_continuation = |b: u8| b & 0xC0 == 0x80;

    // Move `start` forward off a continuation byte (the middle of a character).
    let mut begin = lead as usize;
    while begin < raw.len() && begin < lead as usize + 3 && is_continuation(raw[begin]) {
        begin += 1;
    }
    start += (begin - lead as usize) as u64;
    let at_file_end = start >= total;

    // End: `limit` bytes on, moved back to a character boundary unless the file ends there.
    let mut end = (begin + usize::try_from(limit).unwrap_or(usize::MAX)).min(raw.len());
    let reaches_eof = start + (end - begin) as u64 >= total;
    if !reaches_eof {
        while end > begin && end < raw.len() && is_continuation(raw[end]) {
            end -= 1;
        }
        // `end` now sits on the first byte of a character (or at `begin`); keep whole characters.
        if end == begin {
            // One character longer than the limit: return it whole.
            end = (begin + 1).min(raw.len());
            while end < raw.len() && is_continuation(raw[end]) {
                end += 1;
            }
        }
    }
    let slice = if at_file_end {
        &raw[0..0]
    } else {
        &raw[begin..end]
    };
    let consumed = slice.len() as u64;
    Ok(TextChunk {
        bytes: String::from_utf8_lossy(slice).into_owned().into_bytes(),
        encoding: Encoding::Utf8,
        offset: start,
        next_offset: start + consumed,
        total,
        window_truncated: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn file_with(bytes: &[u8]) -> (File, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "gander-text-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        File::create(&path).unwrap().write_all(bytes).unwrap();
        (File::open(&path).unwrap(), path)
    }

    fn utf16(text: &str, le: bool, bom: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if bom {
            out.extend(if le { [0xFF, 0xFE] } else { [0xFE, 0xFF] });
        }
        for u in text.encode_utf16() {
            out.extend(if le { u.to_le_bytes() } else { u.to_be_bytes() });
        }
        out
    }

    fn whole(bytes: &[u8]) -> TextChunk {
        let (f, p) = file_with(bytes);
        let chunk = read_chunk(&f, 0, MAX_LIMIT).unwrap();
        drop(f);
        let _ = std::fs::remove_file(p);
        chunk
    }

    #[test]
    fn utf16_with_bom_is_decoded_in_both_byte_orders() {
        let text = "Grüße, 世界 \u{1F980}\r\nzweite Zeile";
        for le in [true, false] {
            let chunk = whole(&utf16(text, le, true));
            assert_eq!(std::str::from_utf8(&chunk.bytes).unwrap(), text);
            assert_eq!(
                chunk.encoding,
                if le {
                    Encoding::Utf16Le
                } else {
                    Encoding::Utf16Be
                }
            );
            assert_eq!(chunk.total, text.len() as u64);
            assert!(!chunk.has_more());
        }
    }

    #[test]
    fn utf16_without_bom_is_recognised_for_ascii_heavy_text() {
        let text = "hello world, this is a log line\nanother line\n".repeat(20);
        let chunk = whole(&utf16(&text, true, false));
        assert_eq!(chunk.encoding, Encoding::Utf16Le);
        assert_eq!(String::from_utf8(chunk.bytes).unwrap(), text);
        let chunk = whole(&utf16(&text, false, false));
        assert_eq!(chunk.encoding, Encoding::Utf16Be);
    }

    #[test]
    fn utf8_bom_is_skipped_and_windows_1252_is_the_fallback() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend("héllo".as_bytes());
        let chunk = whole(&bytes);
        assert_eq!(chunk.encoding, Encoding::Utf8);
        assert_eq!(String::from_utf8(chunk.bytes).unwrap(), "héllo");
        assert_eq!(chunk.total, "héllo".len() as u64);

        // 0xE9 is e-acute in Windows-1252 and an invalid lone byte in UTF-8; 0x93/0x94 are curly
        // quotes and 0x80 the euro sign.
        let chunk = whole(b"caf\xE9 \x93quoted\x94 \x80 5");
        assert_eq!(chunk.encoding, Encoding::Windows1252);
        assert_eq!(
            String::from_utf8(chunk.bytes).unwrap(),
            "caf\u{e9} \u{201c}quoted\u{201d} \u{20ac} 5"
        );
    }

    #[test]
    fn binary_files_are_refused() {
        for bytes in [
            &b"PK\x03\x04\x00\x00\x00binary"[..],
            &[0x7F, b'E', b'L', b'F', 0, 1, 2, 3][..],
            &[0x01, 0x02, 0x03, 0x04, b'a'][..],
            &[0xFF, 0xFE, 0x00, 0x00, b'a', 0, 0, 0][..],
        ] {
            let (f, p) = file_with(bytes);
            assert!(
                matches!(read_chunk(&f, 0, 100), Err(TextError::Binary)),
                "{bytes:?}"
            );
            drop(f);
            let _ = std::fs::remove_file(p);
        }
        // A NUL after the first 8 KiB is past the sniff window: the file is text to us.
        let mut late = vec![b'a'; SNIFF_BYTES + 10];
        late.push(0);
        assert!(whole(&late).bytes.len() > SNIFF_BYTES);
    }

    #[test]
    fn empty_and_tiny_files_are_text() {
        let chunk = whole(b"");
        assert!(chunk.bytes.is_empty());
        assert_eq!((chunk.total, chunk.offset, chunk.next_offset), (0, 0, 0));
        assert_eq!(whole(b"x").bytes, b"x");
    }

    #[test]
    fn utf8_chunks_never_split_a_character_and_cover_the_file() {
        let text = "aé世\u{1F980}".repeat(500);
        let (f, p) = file_with(text.as_bytes());
        for limit in [1u64, 2, 3, 5, 7, 64, 1000] {
            let mut got = String::new();
            let mut offset = 0;
            let mut guard = 0;
            loop {
                let chunk = read_chunk(&f, offset, limit).unwrap();
                assert_eq!(chunk.total, text.len() as u64);
                got.push_str(std::str::from_utf8(&chunk.bytes).expect("whole characters only"));
                assert_eq!(chunk.next_offset, chunk.offset + chunk.bytes.len() as u64);
                offset = chunk.next_offset;
                if !chunk.has_more() {
                    break;
                }
                guard += 1;
                assert!(guard < 10_000, "no progress at limit {limit}");
            }
            assert_eq!(got, text, "limit {limit}");
        }
        // An offset inside a character moves forward to the next boundary.
        let mid = text.find('é').unwrap() + 1; // inside the two-byte e-acute
        let chunk = read_chunk(&f, mid as u64, 10).unwrap();
        assert_eq!(chunk.offset as usize, mid + 1);
        assert!(std::str::from_utf8(&chunk.bytes).is_ok());
        // Past the end: empty, at the end.
        let chunk = read_chunk(&f, text.len() as u64 + 99, 10).unwrap();
        assert!(chunk.bytes.is_empty());
        assert_eq!(chunk.offset, text.len() as u64);
        assert!(!chunk.has_more());
        drop(f);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn non_utf8_chunks_page_through_the_decoded_text() {
        let text = "Zeile eins\nZeile zwei: 世界\nZeile drei\n".repeat(40);
        let (f, p) = file_with(&utf16(&text, true, true));
        let mut got = String::new();
        let mut offset = 0;
        loop {
            let chunk = read_chunk(&f, offset, 37).unwrap();
            assert_eq!(chunk.total, text.len() as u64);
            got.push_str(std::str::from_utf8(&chunk.bytes).unwrap());
            offset = chunk.next_offset;
            if !chunk.has_more() {
                break;
            }
        }
        assert_eq!(got, text);
        assert_eq!(
            decoded_total(&f).unwrap(),
            (Encoding::Utf16Le, text.len() as u64)
        );
        drop(f);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn invalid_utf8_after_the_sniff_window_is_replaced_not_fatal() {
        let mut bytes = vec![b'a'; SNIFF_BYTES + 50];
        bytes.extend([0xFF, 0xFE, b'z']);
        let (f, p) = file_with(&bytes);
        let chunk = read_chunk(&f, SNIFF_BYTES as u64 + 40, 100).unwrap();
        assert_eq!(chunk.encoding, Encoding::Utf8);
        let text = String::from_utf8(chunk.bytes).unwrap();
        assert!(text.contains('\u{FFFD}') && text.ends_with('z'));
        assert_eq!(
            chunk.next_offset,
            bytes.len() as u64,
            "next offset counts source bytes"
        );
        drop(f);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn limits_are_clamped() {
        let (f, p) = file_with(&vec![b'x'; 3 * 1024 * 1024]);
        let chunk = read_chunk(&f, 0, u64::MAX).unwrap();
        assert_eq!(chunk.bytes.len() as u64, MAX_LIMIT);
        assert!(chunk.has_more());
        let chunk = read_chunk(&f, 0, 0).unwrap();
        assert_eq!(chunk.bytes.len(), 1);
        drop(f);
        let _ = std::fs::remove_file(p);
    }
}
