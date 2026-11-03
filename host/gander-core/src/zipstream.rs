// SPDX-License-Identifier: Apache-2.0
//! A streaming ZIP writer for the folder download (WP-68, `GET /v1/zip`).
//!
//! The writer needs only [`Write`], never [`std::io::Seek`], and never holds more than one copy
//! buffer: every entry is written as it is read, so an archive of any size streams through a few
//! hundred KiB of memory.
//!
//! # Format
//!
//! - Entries are STORED (method 0, no compression). The files people fetch (photos, PDFs, Office
//!   documents, video) are already compressed, the link is a private network, and the host spends
//!   no CPU on it. Bytes in the archive are the bytes on disk.
//! - A file entry is written as: local header (flag bit 3, so no sizes or CRC yet), the bytes,
//!   then a data descriptor with the CRC-32 and the sizes. A directory entry has a trailing `/`
//!   and no data.
//! - The caller states each file's size up front (it is the size of the open handle) and the
//!   writer copies exactly that many bytes: a file that grew is cut at that size, a file that
//!   shrank is an error ([`io::ErrorKind::UnexpectedEof`]) and the archive cannot be completed.
//! - **ZIP64**: an entry whose size is `0xFFFFFFFF` or more is written ZIP64 (the local header
//!   carries a ZIP64 extra field and the data descriptor has 8-byte sizes); an entry that starts
//!   at an offset of `0xFFFFFFFF` or more, and an archive with 65 535 or more entries or a central
//!   directory at or past 4 GiB, get ZIP64 fields in the central directory and the ZIP64 end
//!   records. [`ZipStream::force_zip64`] turns all of it on for tests.
//! - Names are UTF-8 (flag bit 11) with `/` separators. The writer refuses an unsafe name (empty,
//!   NUL, backslash, a drive colon, a leading `/` or a `..` segment) with
//!   [`io::ErrorKind::InvalidInput`]: names normally come from validated path segments, this is
//!   the last line of defence against a "zip slip".
//! - Timestamps: the MS-DOS fields hold the calendar fields of the [`OffsetDateTime`] as given
//!   (the caller passes local time), clamped to 1980..=2107, and an extended-timestamp extra field
//!   (`UT`) carries the exact UTC modification time.
//!
//! A write error (for example the client went away) leaves the archive unusable; drop the writer.

use std::io::{self, Read, Write};

use time::OffsetDateTime;

const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_DESCRIPTOR: u32 = 0x0807_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_END: u32 = 0x0605_4b50;
const SIG_END64: u32 = 0x0606_4b50;
const SIG_LOCATOR64: u32 = 0x0706_4b50;

/// Bit 3: sizes and CRC follow the data in a data descriptor.
const FLAG_DESCRIPTOR: u16 = 1 << 3;
/// Bit 11: the name is UTF-8.
const FLAG_UTF8: u16 = 1 << 11;

/// "Needed to extract": 2.0 (data descriptor, directories).
const VERSION_BASE: u16 = 20;
/// "Needed to extract": 4.5 (ZIP64).
const VERSION_ZIP64: u16 = 45;

const U32_MAX: u64 = 0xFFFF_FFFF;
const U16_MAX: usize = 0xFFFF;

/// Extra field ids.
const EXTRA_ZIP64: u16 = 0x0001;
const EXTRA_UT: u16 = 0x5455;

/// MS-DOS attribute bits for the central directory.
const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_ARCHIVE: u32 = 0x20;

/// Copy buffer size.
const COPY_BUF: usize = 256 * 1024;

/// What the central directory needs to know about an entry already written.
#[derive(Debug)]
struct Central {
    name: String,
    crc: u32,
    size: u64,
    offset: u64,
    dos_time: u16,
    dos_date: u16,
    unix: u32,
    dir: bool,
    /// The entry's sizes are ZIP64 (its local header said so).
    zip64: bool,
}

/// The writer. See the module docs.
#[derive(Debug)]
pub struct ZipStream<W: Write> {
    out: W,
    offset: u64,
    central: Vec<Central>,
    always64: bool,
    buf: Vec<u8>,
}

impl<W: Write> ZipStream<W> {
    /// A writer that streams the archive into `out`.
    pub fn new(out: W) -> Self {
        Self {
            out,
            offset: 0,
            central: Vec::new(),
            always64: false,
            buf: Vec::new(),
        }
    }

    /// Writes every entry and the end of the archive in their ZIP64 form regardless of size (what
    /// an archive past 4 GiB looks like; for tests).
    #[must_use]
    pub fn force_zip64(mut self, on: bool) -> Self {
        self.always64 = on;
        self
    }

    /// Bytes handed to the output so far.
    pub fn bytes_written(&self) -> u64 {
        self.offset
    }

    /// Entries written so far.
    pub fn entries(&self) -> usize {
        self.central.len()
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.offset += bytes.len() as u64;
        Ok(())
    }

    /// Adds a directory entry. `name` is the path without the trailing slash (it is added).
    pub fn add_dir(&mut self, name: &str, mtime: OffsetDateTime) -> io::Result<()> {
        let name = format!("{}/", name.trim_end_matches('/'));
        check_name(&name)?;
        let offset = self.offset;
        let (dos_time, dos_date) = dos_datetime(mtime);
        let unix = unix_seconds(mtime);
        let mut head = local_header(&name, FLAG_UTF8, VERSION_BASE, dos_time, dos_date, false);
        head.extend_from_slice(&ut_extra(unix));
        self.put(&head)?;
        self.central.push(Central {
            name,
            crc: 0,
            size: 0,
            offset,
            dos_time,
            dos_date,
            unix,
            dir: true,
            zip64: false,
        });
        Ok(())
    }

    /// Adds a file entry of exactly `size` bytes read from `src`.
    ///
    /// If `src` ends early the call fails with [`io::ErrorKind::UnexpectedEof`] and the archive
    /// cannot be completed; if it holds more, only `size` bytes are taken.
    pub fn add_file<R: Read>(
        &mut self,
        name: &str,
        mtime: OffsetDateTime,
        size: u64,
        src: &mut R,
    ) -> io::Result<()> {
        check_name(name)?;
        if name.ends_with('/') {
            return Err(invalid("a file name must not end with a slash"));
        }
        let zip64 = self.always64 || size >= U32_MAX;
        let offset = self.offset;
        let (dos_time, dos_date) = dos_datetime(mtime);
        let unix = unix_seconds(mtime);
        let version = if zip64 { VERSION_ZIP64 } else { VERSION_BASE };
        let mut head = local_header(
            name,
            FLAG_UTF8 | FLAG_DESCRIPTOR,
            version,
            dos_time,
            dos_date,
            zip64,
        );
        head.extend_from_slice(&ut_extra(unix));
        self.put(&head)?;

        let mut buf = std::mem::take(&mut self.buf);
        if buf.is_empty() {
            buf = vec![0u8; COPY_BUF];
        }
        let mut hasher = crc32fast::Hasher::new();
        let mut remaining = size;
        while remaining > 0 {
            let want = usize::try_from(remaining)
                .unwrap_or(usize::MAX)
                .min(buf.len());
            let n = match src.read(&mut buf[..want]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the file ended before its measured size",
                    ));
                }
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            hasher.update(&buf[..n]);
            self.out.write_all(&buf[..n])?;
            self.offset += n as u64;
            remaining -= n as u64;
        }
        self.buf = buf;
        let crc = hasher.finalize();

        let mut desc = Vec::with_capacity(24);
        desc.extend_from_slice(&SIG_DESCRIPTOR.to_le_bytes());
        desc.extend_from_slice(&crc.to_le_bytes());
        if zip64 {
            desc.extend_from_slice(&size.to_le_bytes());
            desc.extend_from_slice(&size.to_le_bytes());
        } else {
            let s = u32::try_from(size).unwrap_or(u32::MAX);
            desc.extend_from_slice(&s.to_le_bytes());
            desc.extend_from_slice(&s.to_le_bytes());
        }
        self.put(&desc)?;
        self.central.push(Central {
            name: name.to_owned(),
            crc,
            size,
            offset,
            dos_time,
            dos_date,
            unix,
            dir: false,
            zip64,
        });
        Ok(())
    }

    /// Adds a small in-memory file.
    pub fn add_bytes(&mut self, name: &str, mtime: OffsetDateTime, data: &[u8]) -> io::Result<()> {
        let mut src = data;
        self.add_file(name, mtime, data.len() as u64, &mut src)
    }

    /// Writes the central directory and the end records, flushes and returns the output.
    pub fn finish(mut self) -> io::Result<W> {
        let cd_start = self.offset;
        let central = std::mem::take(&mut self.central);
        let mut any_zip64 = self.always64;
        for c in &central {
            let (record, needs64) = central_record(c, self.always64);
            any_zip64 |= needs64;
            self.put(&record)?;
        }
        let cd_end = self.offset;
        let cd_size = cd_end - cd_start;
        let count = central.len();
        let end64 = any_zip64 || count >= U16_MAX || cd_start >= U32_MAX || cd_size >= U32_MAX;
        if end64 {
            let mut rec = Vec::with_capacity(56 + 20);
            rec.extend_from_slice(&SIG_END64.to_le_bytes());
            rec.extend_from_slice(&44u64.to_le_bytes()); // size of the rest of this record
            rec.extend_from_slice(&VERSION_ZIP64.to_le_bytes()); // made by
            rec.extend_from_slice(&VERSION_ZIP64.to_le_bytes()); // needed
            rec.extend_from_slice(&0u32.to_le_bytes()); // this disk
            rec.extend_from_slice(&0u32.to_le_bytes()); // disk with the central directory
            rec.extend_from_slice(&(count as u64).to_le_bytes());
            rec.extend_from_slice(&(count as u64).to_le_bytes());
            rec.extend_from_slice(&cd_size.to_le_bytes());
            rec.extend_from_slice(&cd_start.to_le_bytes());
            rec.extend_from_slice(&SIG_LOCATOR64.to_le_bytes());
            rec.extend_from_slice(&0u32.to_le_bytes()); // disk with the ZIP64 end record
            rec.extend_from_slice(&cd_end.to_le_bytes()); // where it starts
            rec.extend_from_slice(&1u32.to_le_bytes()); // total disks
            self.put(&rec)?;
        }
        let mut end = Vec::with_capacity(22);
        end.extend_from_slice(&SIG_END.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        let n16 = if end64 {
            0xFFFF
        } else {
            u16::try_from(count).unwrap_or(0xFFFF)
        };
        end.extend_from_slice(&n16.to_le_bytes());
        end.extend_from_slice(&n16.to_le_bytes());
        let size32 = if end64 {
            0xFFFF_FFFF
        } else {
            u32::try_from(cd_size).unwrap_or(u32::MAX)
        };
        let start32 = if end64 {
            0xFFFF_FFFF
        } else {
            u32::try_from(cd_start).unwrap_or(u32::MAX)
        };
        end.extend_from_slice(&size32.to_le_bytes());
        end.extend_from_slice(&start32.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes()); // comment length
        self.put(&end)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// The zip-slip guard: refuses names that could escape the extraction folder.
fn check_name(name: &str) -> io::Result<()> {
    if name.is_empty() || name.len() > U16_MAX {
        return Err(invalid("a zip entry name must be 1 to 65535 bytes"));
    }
    if name.starts_with('/') || name.contains(['\0', '\\']) {
        return Err(invalid("unsafe zip entry name"));
    }
    if name.split('/').any(|s| s == ".." || s.contains(':')) {
        return Err(invalid("unsafe zip entry name"));
    }
    Ok(())
}

/// A local file header up to and including the name; the caller appends the extra fields.
///
/// The extra length written here counts only the ZIP64 field (when `zip64`) plus the 9-byte `UT`
/// field every entry carries, matching what the callers append.
fn local_header(
    name: &str,
    flags: u16,
    version: u16,
    dos_time: u16,
    dos_date: u16,
    zip64: bool,
) -> Vec<u8> {
    let extra_len: u16 = 9 + if zip64 { 20 } else { 0 };
    let mut h = Vec::with_capacity(30 + name.len() + usize::from(extra_len));
    h.extend_from_slice(&SIG_LOCAL.to_le_bytes());
    h.extend_from_slice(&version.to_le_bytes());
    h.extend_from_slice(&flags.to_le_bytes());
    h.extend_from_slice(&0u16.to_le_bytes()); // method: stored
    h.extend_from_slice(&dos_time.to_le_bytes());
    h.extend_from_slice(&dos_date.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes()); // CRC-32: in the descriptor
    let size_field = if zip64 { 0xFFFF_FFFFu32 } else { 0 };
    h.extend_from_slice(&size_field.to_le_bytes()); // compressed size
    h.extend_from_slice(&size_field.to_le_bytes()); // uncompressed size
    h.extend_from_slice(&(name.len() as u16).to_le_bytes());
    h.extend_from_slice(&extra_len.to_le_bytes());
    h.extend_from_slice(name.as_bytes());
    if zip64 {
        h.extend_from_slice(&EXTRA_ZIP64.to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(&0u64.to_le_bytes()); // uncompressed size: in the descriptor
        h.extend_from_slice(&0u64.to_le_bytes()); // compressed size: in the descriptor
    }
    h
}

/// The `UT` extended-timestamp extra field with a modification time (9 bytes).
fn ut_extra(unix: u32) -> [u8; 9] {
    let mut f = [0u8; 9];
    f[..2].copy_from_slice(&EXTRA_UT.to_le_bytes());
    f[2..4].copy_from_slice(&5u16.to_le_bytes());
    f[4] = 1; // modification time present
    f[5..].copy_from_slice(&unix.to_le_bytes());
    f
}

/// One central directory record and whether it used a ZIP64 extra field.
fn central_record(c: &Central, always64: bool) -> (Vec<u8>, bool) {
    let size64 = c.zip64 && !c.dir;
    let off64 = always64 || c.offset >= U32_MAX;
    let mut extra: Vec<u8> = Vec::new();
    if size64 || off64 {
        let mut body: Vec<u8> = Vec::new();
        if size64 {
            body.extend_from_slice(&c.size.to_le_bytes()); // uncompressed
            body.extend_from_slice(&c.size.to_le_bytes()); // compressed
        }
        if off64 {
            body.extend_from_slice(&c.offset.to_le_bytes());
        }
        extra.extend_from_slice(&EXTRA_ZIP64.to_le_bytes());
        extra.extend_from_slice(&(body.len() as u16).to_le_bytes());
        extra.extend_from_slice(&body);
    }
    extra.extend_from_slice(&ut_extra(c.unix));
    let needs64 = size64 || off64;
    let version = if needs64 { VERSION_ZIP64 } else { VERSION_BASE };
    let flags = if c.dir {
        FLAG_UTF8
    } else {
        FLAG_UTF8 | FLAG_DESCRIPTOR
    };
    let mut r = Vec::with_capacity(46 + c.name.len() + extra.len());
    r.extend_from_slice(&SIG_CENTRAL.to_le_bytes());
    r.extend_from_slice(&version.to_le_bytes()); // made by (MS-DOS host)
    r.extend_from_slice(&version.to_le_bytes()); // needed
    r.extend_from_slice(&flags.to_le_bytes());
    r.extend_from_slice(&0u16.to_le_bytes()); // method: stored
    r.extend_from_slice(&c.dos_time.to_le_bytes());
    r.extend_from_slice(&c.dos_date.to_le_bytes());
    r.extend_from_slice(&c.crc.to_le_bytes());
    let size32 = if size64 {
        0xFFFF_FFFF
    } else {
        u32::try_from(c.size).unwrap_or(u32::MAX)
    };
    r.extend_from_slice(&size32.to_le_bytes()); // compressed
    r.extend_from_slice(&size32.to_le_bytes()); // uncompressed
    r.extend_from_slice(&(c.name.len() as u16).to_le_bytes());
    r.extend_from_slice(&(extra.len() as u16).to_le_bytes());
    r.extend_from_slice(&0u16.to_le_bytes()); // comment length
    r.extend_from_slice(&0u16.to_le_bytes()); // disk number start
    r.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
    let attrs = if c.dir { ATTR_DIRECTORY } else { ATTR_ARCHIVE };
    r.extend_from_slice(&attrs.to_le_bytes());
    let off32 = if off64 {
        0xFFFF_FFFF
    } else {
        u32::try_from(c.offset).unwrap_or(u32::MAX)
    };
    r.extend_from_slice(&off32.to_le_bytes());
    r.extend_from_slice(c.name.as_bytes());
    r.extend_from_slice(&extra);
    (r, needs64)
}

/// MS-DOS time and date of `t`'s calendar fields, clamped to 1980-01-01 .. 2107-12-31.
pub fn dos_datetime(t: OffsetDateTime) -> (u16, u16) {
    if t.year() < 1980 {
        return (0, (1 << 5) | 1);
    }
    if t.year() > 2107 {
        return ((23 << 11) | (59 << 5) | 29, (127 << 9) | (12 << 5) | 31);
    }
    let date = (((t.year() - 1980) as u16) << 9)
        | (u16::from(u8::from(t.month())) << 5)
        | u16::from(t.day());
    let time =
        (u16::from(t.hour()) << 11) | (u16::from(t.minute()) << 5) | u16::from(t.second() / 2);
    (time, date)
}

/// Seconds since the Unix epoch, clamped into a `u32`.
fn unix_seconds(t: OffsetDateTime) -> u32 {
    u32::try_from(t.unix_timestamp().max(0)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn t(secs: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(secs).unwrap()
    }

    /// A reader that hands out the same byte `n` times without storing it.
    struct Repeat {
        left: u64,
        byte: u8,
    }

    impl Read for Repeat {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = usize::try_from(self.left)
                .unwrap_or(usize::MAX)
                .min(buf.len());
            buf[..n].fill(self.byte);
            self.left -= n as u64;
            Ok(n)
        }
    }

    fn build(force64: bool) -> Vec<u8> {
        let mut z = ZipStream::new(Vec::new()).force_zip64(force64);
        z.add_dir("top", t(1_700_000_000)).unwrap();
        z.add_bytes("top/a.txt", t(1_700_000_000), b"hello")
            .unwrap();
        z.add_bytes("top/empty.bin", t(1_700_000_000), b"").unwrap();
        z.add_dir("top/sub", t(1_700_000_000)).unwrap();
        let big: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
        z.add_bytes("top/sub/\u{65e5}\u{672c}.dat", t(1_700_000_000), &big)
            .unwrap();
        z.finish().unwrap()
    }

    fn check(bytes: Vec<u8>) {
        let mut ar = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(ar.len(), 5);
        let mut names: Vec<String> = ar.file_names().map(|n| n.unwrap().into_owned()).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "top/",
                "top/a.txt",
                "top/empty.bin",
                "top/sub/",
                "top/sub/\u{65e5}\u{672c}.dat"
            ]
        );
        let mut s = String::new();
        ar.by_name("top/a.txt")
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert_eq!(s, "hello");
        assert!(ar.by_name("top/sub/").unwrap().is_dir());
        let mut v = Vec::new();
        // The reader verifies the CRC-32 once the entry is read to its end.
        ar.by_name("top/sub/\u{65e5}\u{672c}.dat")
            .unwrap()
            .read_to_end(&mut v)
            .unwrap();
        assert_eq!(v.len(), 700_000);
        assert_eq!(v[250], 250);
        assert_eq!(ar.by_name("top/empty.bin").unwrap().size(), 0);
    }

    #[test]
    fn small_archive_reads_back() {
        check(build(false));
    }

    #[test]
    fn forced_zip64_archive_reads_back() {
        let plain = build(false);
        let z64 = build(true);
        assert!(z64.len() > plain.len(), "ZIP64 adds fields");
        assert!(
            z64.windows(4).any(|w| w == SIG_END64.to_le_bytes()),
            "ZIP64 end record present"
        );
        check(z64);
    }

    #[test]
    fn empty_archive_is_valid() {
        let bytes = ZipStream::new(Vec::new()).finish().unwrap();
        assert_eq!(bytes.len(), 22);
        assert_eq!(zip::ZipArchive::new(Cursor::new(bytes)).unwrap().len(), 0);
    }

    #[test]
    fn many_entries_use_the_zip64_end_records() {
        let mut z = ZipStream::new(Vec::new());
        for i in 0..70_000u32 {
            z.add_bytes(&format!("d/{i}"), t(0), b"").unwrap();
        }
        let bytes = z.finish().unwrap();
        assert!(bytes.windows(4).any(|w| w == SIG_END64.to_le_bytes()));
        let ar = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(ar.len(), 70_000);
    }

    #[test]
    fn short_source_is_an_error_and_long_source_is_cut() {
        let mut z = ZipStream::new(Vec::new());
        let mut short = Repeat { left: 10, byte: 1 };
        let e = z
            .add_file("short", t(0), 20, &mut short)
            .expect_err("a source shorter than its size must fail");
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);

        let mut z = ZipStream::new(Vec::new());
        let mut long = Repeat { left: 100, byte: 7 };
        z.add_file("long", t(0), 40, &mut long).unwrap();
        assert_eq!(long.left, 60, "only the measured size is read");
        let bytes = z.finish().unwrap();
        let mut ar = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(ar.by_name("long").unwrap().size(), 40);
    }

    #[test]
    fn unsafe_names_are_refused() {
        let mut z = ZipStream::new(Vec::new());
        for bad in [
            "", "/abs", "a/../b", "..", "a\\b", "c:/x", "a/b:c", "nul\0", "dir/",
        ] {
            let e = z.add_bytes(bad, t(0), b"x");
            assert!(e.is_err(), "{bad:?} must be refused");
        }
        assert!(z.add_dir("a/../b", t(0)).is_err());
        assert_eq!(z.entries(), 0);
        assert!(z.add_bytes("a/..b/c..", t(0), b"x").is_ok());
    }

    #[test]
    fn dos_time_is_clamped() {
        assert_eq!(dos_datetime(t(0)), (0, (1 << 5) | 1)); // 1970 -> 1980-01-01
        // 2001-09-09 01:46:40 UTC
        let (time, date) = dos_datetime(t(1_000_000_000));
        assert_eq!(date >> 9, 2001 - 1980);
        assert_eq!((date >> 5) & 0xF, 9);
        assert_eq!(date & 0x1F, 9);
        assert_eq!(time >> 11, 1);
        assert_eq!((time >> 5) & 0x3F, 46);
        assert_eq!(time & 0x1F, 20);
    }

    /// An entry that is really past 4 GiB (a zero-filled source, nothing kept in memory): the
    /// central directory must carry the ZIP64 sizes and offsets of an archive that large. Slow
    /// and large on disk, so it is ignored by default:
    /// `cargo test -p gander-core --lib zipstream -- --ignored`.
    #[test]
    #[ignore = "writes a 4.5 GiB archive to the temp folder"]
    fn archive_past_4_gib_reads_back() {
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let path = std::env::temp_dir().join(format!("gander-zip64-{}.zip", std::process::id()));
        let _cleanup = Cleanup(path.clone());
        let file = std::fs::File::create(&path).unwrap();
        let mut z = ZipStream::new(io::BufWriter::new(file));
        let big: u64 = 4 * 1024 * 1024 * 1024 + 123;
        z.add_file(
            "a.bin",
            t(1_700_000_000),
            big,
            &mut Repeat { left: big, byte: 0 },
        )
        .unwrap();
        // This entry starts past 4 GiB: its offset needs the ZIP64 extra field.
        z.add_bytes("b.txt", t(1_700_000_000), b"after the big one")
            .unwrap();
        z.finish().unwrap();
        {
            let file = std::fs::File::open(&path).unwrap();
            let mut ar = zip::ZipArchive::new(io::BufReader::new(file)).unwrap();
            assert_eq!(ar.len(), 2);
            assert_eq!(ar.by_name("a.bin").unwrap().size(), big);
            let mut s = String::new();
            ar.by_name("b.txt").unwrap().read_to_string(&mut s).unwrap();
            assert_eq!(s, "after the big one");
            let mut n = 0u64;
            let mut f = ar.by_name("a.bin").unwrap();
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let k = f.read(&mut buf).unwrap();
                if k == 0 {
                    break;
                }
                n += k as u64;
            }
            assert_eq!(n, big);
        }
    }
}
