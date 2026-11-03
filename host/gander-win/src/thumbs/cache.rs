// SPDX-License-Identifier: Apache-2.0
//! Thumbnail cache keyed by content identity (renderer spec, "Cache").
//!
//! - Files: `<root>/thumbs/<2 hex>/<key>.<ext>`, written to a temporary name and renamed.
//! - Index: `<root>/thumbs/index.redb` with two tables: `entries` (key -> bytes, access sequence)
//!   and `lru` ((access sequence, key) -> bytes), so eviction pops the least recently used entry
//!   without scanning.
//! - Cap: 2 GiB by default; when the total passes 95 % of the cap, entries are evicted, oldest
//!   access first, until it is at or below 80 %.
//! - Key: (volume serial, file id, size, last-write time, renderer version, output size, format).
//!   Renaming or moving a file within its volume keeps the key; writing to it changes it.
//!
//! Recency is tracked with a monotonic access sequence rather than wall-clock time, so a clock
//! change cannot reorder the LRU. Hits update recency in memory and are written to the index in
//! batches (on the next insert, eviction, every [`TOUCH_FLUSH_BATCH`] hits, and on drop).
//!
//! A cache hit is never proof that the file still exists: the caller resolves and opens the file
//! first and derives the key from the open handle.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use crate::render::job::OutputFormat;

/// Default total cap: 2 GiB.
pub const DEFAULT_CAP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Pending recency updates written to the index in one transaction.
pub const TOUCH_FLUSH_BATCH: usize = 256;

const ENTRIES: TableDefinition<'_, &str, (u64, u64)> = TableDefinition::new("entries");
const LRU: TableDefinition<'_, (u64, &str), u64> = TableDefinition::new("lru");

/// What identifies a file's content without trusting its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    /// Volume serial number.
    pub volume: u64,
    /// 128-bit file id (NTFS/ReFS `FILE_ID_INFO`).
    pub file_id: u128,
    /// File size in bytes.
    pub size: u64,
    /// Last-write time (FILETIME ticks).
    pub mtime: i64,
}

/// A cache key: one rendered output of one version of one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentKey {
    /// The source file's identity.
    pub identity: FileIdentity,
    /// [`crate::render::job::RENDERER_VERSION`] at render time.
    pub renderer_version: u32,
    /// Longest side requested.
    pub size_px: u32,
    /// Output encoding.
    pub format: OutputFormat,
}

impl ContentKey {
    /// The key as stored in the index and used as the file name: lower-case hex fields.
    pub fn as_string(&self) -> String {
        let id = &self.identity;
        format!(
            "{:016x}-{:032x}-{:x}-{:x}-r{}-s{}-{}",
            id.volume,
            id.file_id,
            id.size,
            id.mtime,
            self.renderer_version,
            self.size_px,
            self.format.extension()
        )
    }

    /// Strong ETag for the rendered output (protocol spec: derived from file id, size, mtime and
    /// generator version). Hashed so the volume serial and file id are not exposed verbatim.
    pub fn etag(&self) -> String {
        format!("\"t-{:016x}\"", fnv1a64(self.as_string().as_bytes()))
    }
}

/// 64-bit FNV-1a; a stable, dependency-free hash for ETags and directory fan-out (not security
/// relevant: nothing trusts it).
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn db_err(e: impl Into<redb::Error>) -> io::Error {
    io::Error::other(e.into())
}

#[derive(Debug, Default)]
struct State {
    total: u64,
    count: u64,
    seq: u64,
    pending: HashMap<String, u64>,
}

/// The on-disk thumbnail cache. Cheap to share behind an `Arc`; all methods take `&self`.
pub struct ThumbCache {
    dir: PathBuf,
    cap: u64,
    db: Database,
    state: Mutex<State>,
}

impl std::fmt::Debug for ThumbCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbCache")
            .field("dir", &self.dir)
            .field("cap", &self.cap)
            .finish_non_exhaustive()
    }
}

/// `%LOCALAPPDATA%\Gander\cache`, the spec's cache root.
pub fn default_cache_root() -> io::Result<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("Gander").join("cache"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not set"))
}

impl ThumbCache {
    /// Opens (or creates) the cache under `<root>/thumbs` with a total cap of `cap_bytes`.
    pub fn open(root: &Path, cap_bytes: u64) -> io::Result<Self> {
        Self::open_in(root, "thumbs", cap_bytes)
    }

    /// Opens (or creates) a cache under `<root>/<subdir>` (the preview cache lives in
    /// `previews`, next to `thumbs`), with its own index, LRU and cap.
    pub fn open_in(root: &Path, subdir: &str, cap_bytes: u64) -> io::Result<Self> {
        let dir = root.join(subdir);
        fs::create_dir_all(&dir)?;
        let db = Database::create(dir.join("index.redb")).map_err(db_err)?;
        let mut state = State::default();
        {
            let tx = db.begin_write().map_err(db_err)?;
            {
                let entries = tx.open_table(ENTRIES).map_err(db_err)?;
                let _ = tx.open_table(LRU).map_err(db_err)?;
                for item in entries.iter().map_err(db_err)? {
                    let (_, v) = item.map_err(db_err)?;
                    let (bytes, seq) = v.value();
                    state.total += bytes;
                    state.count += 1;
                    state.seq = state.seq.max(seq);
                }
            }
            tx.commit().map_err(db_err)?;
        }
        Ok(Self {
            dir,
            cap: cap_bytes,
            db,
            state: Mutex::new(state),
        })
    }

    /// Total bytes of cached output.
    pub fn total_bytes(&self) -> u64 {
        self.lock().total
    }

    /// Number of cached entries.
    pub fn len(&self) -> u64 {
        self.lock().count
    }

    /// Whether the cache holds nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The configured cap in bytes.
    pub fn cap_bytes(&self) -> u64 {
        self.cap
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn file_path(&self, key: &str, ext: &str) -> PathBuf {
        let fan = format!("{:02x}", fnv1a64(key.as_bytes()) & 0xff);
        self.dir.join(fan).join(format!("{key}.{ext}"))
    }

    /// Whether `key` is in the index (does not touch recency).
    pub fn contains(&self, key: &ContentKey) -> io::Result<bool> {
        self.contains_named(&key.as_string())
    }

    /// [`Self::contains`] for a caller-built key string (see [`Self::put_named`]).
    pub fn contains_named(&self, key: &str) -> io::Result<bool> {
        let tx = self.db.begin_read().map_err(db_err)?;
        let entries = tx.open_table(ENTRIES).map_err(db_err)?;
        Ok(entries.get(key).map_err(db_err)?.is_some())
    }

    /// Returns the cached bytes and marks the entry most recently used. An index entry whose file
    /// vanished is dropped and reported as a miss.
    pub fn get(&self, key: &ContentKey) -> io::Result<Option<Vec<u8>>> {
        self.get_named(&key.as_string(), key.format.extension())
    }

    /// [`Self::get`] for a caller-built key string. The key must end with `ext` (so eviction can
    /// find the file) and contain only file-name-safe characters; the preview cache builds its
    /// keys that way.
    pub fn get_named(&self, key: &str, ext: &str) -> io::Result<Option<Vec<u8>>> {
        let k = key.to_owned();
        if !self.contains_named(key)? {
            return Ok(None);
        }
        match fs::read(self.file_path(&k, ext)) {
            Ok(bytes) => {
                let flush = {
                    let mut st = self.lock();
                    st.seq += 1;
                    let seq = st.seq;
                    st.pending.insert(k, seq);
                    st.pending.len() >= TOUCH_FLUSH_BATCH
                };
                if flush {
                    self.flush()?;
                }
                Ok(Some(bytes))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.remove_keys(&[k])?;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Opens the cached file for reading (the preview renderer hands this handle to a worker) and
    /// marks the entry most recently used. A vanished file is dropped and reported as a miss.
    pub fn open_named_file(&self, key: &str, ext: &str) -> io::Result<Option<fs::File>> {
        if !self.contains_named(key)? {
            return Ok(None);
        }
        match fs::File::open(self.file_path(key, ext)) {
            Ok(f) => {
                let flush = {
                    let mut st = self.lock();
                    st.seq += 1;
                    let seq = st.seq;
                    st.pending.insert(key.to_owned(), seq);
                    st.pending.len() >= TOUCH_FLUSH_BATCH
                };
                if flush {
                    self.flush()?;
                }
                Ok(Some(f))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.remove_keys(&[key.to_owned()])?;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Stores `bytes` for `key` (replacing any previous entry) and evicts if over the high-water
    /// mark.
    pub fn put(&self, key: &ContentKey, bytes: &[u8]) -> io::Result<()> {
        self.put_named(&key.as_string(), key.format.extension(), bytes)
    }

    /// [`Self::put`] for a caller-built key string (see [`Self::get_named`]).
    pub fn put_named(&self, key: &str, ext: &str, bytes: &[u8]) -> io::Result<()> {
        let k = key.to_owned();
        let path = self.file_path(&k, ext);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
        }
        if let Err(e) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }

        let mut st = self.lock();
        let pending = std::mem::take(&mut st.pending);
        st.seq += 1;
        let seq = st.seq;
        let len = bytes.len() as u64;
        let tx = self.db.begin_write().map_err(db_err)?;
        {
            let mut entries = tx.open_table(ENTRIES).map_err(db_err)?;
            let mut lru = tx.open_table(LRU).map_err(db_err)?;
            apply_touches(&mut entries, &mut lru, &pending)?;
            let old = entries
                .insert(k.as_str(), (len, seq))
                .map_err(db_err)?
                .map(|g| g.value());
            if let Some((old_len, old_seq)) = old {
                lru.remove((old_seq, k.as_str())).map_err(db_err)?;
                st.total -= old_len;
                st.count -= 1;
            }
            lru.insert((seq, k.as_str()), len).map_err(db_err)?;
        }
        tx.commit().map_err(db_err)?;
        st.total += len;
        st.count += 1;
        drop(st);
        self.evict_if_needed()
    }

    /// Writes pending recency updates to the index.
    pub fn flush(&self) -> io::Result<()> {
        let st = self.lock();
        self.flush_locked(st)
    }

    fn flush_locked(&self, mut st: std::sync::MutexGuard<'_, State>) -> io::Result<()> {
        if st.pending.is_empty() {
            return Ok(());
        }
        let pending = std::mem::take(&mut st.pending);
        let tx = self.db.begin_write().map_err(db_err)?;
        {
            let mut entries = tx.open_table(ENTRIES).map_err(db_err)?;
            let mut lru = tx.open_table(LRU).map_err(db_err)?;
            apply_touches(&mut entries, &mut lru, &pending)?;
        }
        tx.commit().map_err(db_err)
    }

    /// Evicts least recently used entries when the total is above 95 % of the cap, down to 80 %.
    /// Returns nothing; [`Self::total_bytes`] reflects the result.
    pub fn evict_if_needed(&self) -> io::Result<()> {
        let mut st = self.lock();
        if st.total.saturating_mul(100) <= self.cap.saturating_mul(95) {
            return Ok(());
        }
        let target = self.cap / 100 * 80;
        let pending = std::mem::take(&mut st.pending);
        let mut removed = Vec::new();
        let tx = self.db.begin_write().map_err(db_err)?;
        {
            let mut entries = tx.open_table(ENTRIES).map_err(db_err)?;
            let mut lru = tx.open_table(LRU).map_err(db_err)?;
            apply_touches(&mut entries, &mut lru, &pending)?;
            while st.total > target {
                let Some((k, v)) = lru.pop_first().map_err(db_err)? else {
                    break;
                };
                let (_, key) = k.value();
                let key = key.to_owned();
                let len = v.value();
                entries.remove(key.as_str()).map_err(db_err)?;
                st.total = st.total.saturating_sub(len);
                st.count = st.count.saturating_sub(1);
                removed.push(key);
            }
        }
        tx.commit().map_err(db_err)?;
        drop(st);
        for key in removed {
            self.remove_files(&key);
        }
        Ok(())
    }

    /// Deletes every entry and file (tray "Purge", share removal).
    pub fn purge(&self) -> io::Result<()> {
        let mut st = self.lock();
        let tx = self.db.begin_write().map_err(db_err)?;
        tx.delete_table(ENTRIES).map_err(db_err)?;
        tx.delete_table(LRU).map_err(db_err)?;
        {
            let _ = tx.open_table(ENTRIES).map_err(db_err)?;
            let _ = tx.open_table(LRU).map_err(db_err)?;
        }
        tx.commit().map_err(db_err)?;
        *st = State {
            seq: st.seq,
            ..State::default()
        };
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(entry.path())?;
            }
        }
        Ok(())
    }

    fn remove_keys(&self, keys: &[String]) -> io::Result<()> {
        let mut st = self.lock();
        let tx = self.db.begin_write().map_err(db_err)?;
        {
            let mut entries = tx.open_table(ENTRIES).map_err(db_err)?;
            let mut lru = tx.open_table(LRU).map_err(db_err)?;
            for k in keys {
                st.pending.remove(k);
                if let Some(g) = entries.remove(k.as_str()).map_err(db_err)? {
                    let (len, seq) = g.value();
                    lru.remove((seq, k.as_str())).map_err(db_err)?;
                    st.total = st.total.saturating_sub(len);
                    st.count = st.count.saturating_sub(1);
                }
            }
        }
        tx.commit().map_err(db_err)?;
        drop(st);
        for k in keys {
            self.remove_files(k);
        }
        Ok(())
    }

    fn remove_files(&self, key: &str) {
        let known = [
            OutputFormat::Webp.extension(),
            OutputFormat::Jpeg.extension(),
            "pdf",
        ];
        for ext in known {
            if key.ends_with(ext) {
                let _ = fs::remove_file(self.file_path(key, ext));
            }
        }
    }
}

impl Drop for ThumbCache {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

fn apply_touches(
    entries: &mut redb::Table<'_, &str, (u64, u64)>,
    lru: &mut redb::Table<'_, (u64, &str), u64>,
    pending: &HashMap<String, u64>,
) -> io::Result<()> {
    for (k, &seq) in pending {
        let current = entries.get(k.as_str()).map_err(db_err)?.map(|g| g.value());
        if let Some((len, old_seq)) = current {
            if old_seq >= seq {
                continue;
            }
            lru.remove((old_seq, k.as_str())).map_err(db_err)?;
            lru.insert((seq, k.as_str()), len).map_err(db_err)?;
            entries.insert(k.as_str(), (len, seq)).map_err(db_err)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gander-cache-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn key(file_id: u128, mtime: i64) -> ContentKey {
        ContentKey {
            identity: FileIdentity {
                volume: 7,
                file_id,
                size: 100,
                mtime,
            },
            renderer_version: 1,
            size_px: 256,
            format: OutputFormat::Webp,
        }
    }

    #[test]
    fn put_get_and_reopen() {
        let root = temp_root("reopen");
        {
            let c = ThumbCache::open(&root, DEFAULT_CAP_BYTES).unwrap();
            c.put(&key(1, 1), b"abc").unwrap();
            assert_eq!(c.get(&key(1, 1)).unwrap().as_deref(), Some(&b"abc"[..]));
            assert_eq!(c.get(&key(1, 2)).unwrap(), None);
        }
        let c = ThumbCache::open(&root, DEFAULT_CAP_BYTES).unwrap();
        assert_eq!(c.total_bytes(), 3);
        assert_eq!(c.len(), 1);
        assert!(c.contains(&key(1, 1)).unwrap());
        c.purge().unwrap();
        assert!(c.is_empty());
        assert_eq!(c.get(&key(1, 1)).unwrap(), None);
        drop(c);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn replacing_an_entry_does_not_double_count() {
        let root = temp_root("replace");
        let c = ThumbCache::open(&root, DEFAULT_CAP_BYTES).unwrap();
        c.put(&key(1, 1), &[0; 10]).unwrap();
        c.put(&key(1, 1), &[0; 4]).unwrap();
        assert_eq!(c.total_bytes(), 4);
        assert_eq!(c.len(), 1);
        drop(c);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cap_evicts_least_recently_used_down_to_80_percent() {
        let root = temp_root("evict");
        // Cap 1000 bytes: high water 950, low water 800.
        let c = ThumbCache::open(&root, 1000).unwrap();
        for id in 0..9u128 {
            c.put(&key(id, 0), &[0u8; 100]).unwrap();
        }
        assert_eq!(c.total_bytes(), 900, "below 95 %, nothing evicted yet");
        // Touch the oldest so it becomes most recent.
        assert!(c.get(&key(0, 0)).unwrap().is_some());
        c.put(&key(9, 0), &[0u8; 100]).unwrap();
        assert_eq!(c.total_bytes(), 800);
        assert_eq!(c.len(), 8);
        assert!(c.contains(&key(0, 0)).unwrap(), "touched entry survives");
        assert!(!c.contains(&key(1, 0)).unwrap(), "oldest untouched evicted");
        assert!(!c.contains(&key(2, 0)).unwrap());
        assert!(c.contains(&key(3, 0)).unwrap());
        assert!(c.contains(&key(9, 0)).unwrap());
        drop(c);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn vanished_file_is_a_miss() {
        let root = temp_root("vanished");
        let c = ThumbCache::open(&root, DEFAULT_CAP_BYTES).unwrap();
        let k = key(5, 5);
        c.put(&k, b"x").unwrap();
        let s = k.as_string();
        fs::remove_file(c.file_path(&s, "webp")).unwrap();
        assert_eq!(c.get(&k).unwrap(), None);
        assert!(c.is_empty());
        drop(c);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn etag_is_stable_and_depends_on_identity() {
        assert_eq!(key(1, 1).etag(), key(1, 1).etag());
        assert_ne!(key(1, 1).etag(), key(1, 2).etag());
        assert!(key(1, 1).etag().starts_with("\"t-"));
    }
}
