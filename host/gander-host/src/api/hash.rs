// SPDX-License-Identifier: Apache-2.0
//! The `/hash` cache: one digest per (file ETag, algorithm), computed from the resolved handle.
//!
//! Small files are hashed inside the request. Larger ones are hashed in the background (at most
//! `hash_concurrency` at once) and the request answers `503 generating` with a retry hint until
//! the digest is ready. A digest is stored only if the file's identity (size, last write, id) is
//! the same after reading as before, so a file written during hashing never gets a digest for
//! the wrong content; the next request sees the new ETag and starts over.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io;
use std::os::windows::fs::FileExt as _;
use std::sync::{Arc, Mutex};

use gander_core::protocol::host::HashAlgo;
use gander_win::thumbs::cache::FileIdentity;
use gander_win::thumbs::identity::file_identity;
use tokio::sync::Semaphore;

/// Digests remembered (oldest dropped first).
const MAX_ENTRIES: usize = 4096;
/// Read size while hashing.
const CHUNK: usize = 1024 * 1024;

/// What a `/hash` request gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HashOutcome {
    /// Lower-case hex digest.
    Done(String),
    /// Being computed; answer 503 `generating`.
    Computing,
    /// The file changed while it was being read inline (412 `file_changed`).
    Changed,
    /// Reading failed.
    Failed,
}

#[derive(Debug, Clone)]
enum Slot {
    Computing,
    Done(String),
}

type Key = (String, HashAlgo);

#[derive(Debug, Default)]
struct Inner {
    map: HashMap<Key, Slot>,
    order: VecDeque<Key>,
}

impl Inner {
    fn insert(&mut self, key: Key, slot: Slot) {
        if self.map.insert(key.clone(), slot).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > MAX_ENTRIES {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }

    fn remove(&mut self, key: &Key) {
        if self.map.remove(key).is_some() {
            self.order.retain(|k| k != key);
        }
    }
}

/// Digest cache plus the background job limit.
#[derive(Debug)]
pub(super) struct HashCache {
    inner: Arc<Mutex<Inner>>,
    jobs: Arc<Semaphore>,
}

/// Client retry hint for a background hash of `size` bytes (about 400 MB/s).
pub(super) fn retry_estimate_ms(size: u64) -> u64 {
    (size / 400_000).clamp(250, 10_000)
}

fn digest(file: &File, algo: HashAlgo, expected: u64) -> io::Result<String> {
    let mut b3 = blake3::Hasher::new();
    let mut sha = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; CHUNK];
    let mut offset = 0u64;
    loop {
        let n = file.seek_read(&mut buf, offset)?;
        if n == 0 {
            break;
        }
        match algo {
            HashAlgo::Blake3 => {
                b3.update(&buf[..n]);
            }
            HashAlgo::Sha256 => sha.update(&buf[..n]),
        }
        offset += n as u64;
    }
    if offset != expected {
        return Err(io::Error::other("file size changed while hashing"));
    }
    Ok(match algo {
        HashAlgo::Blake3 => b3.finalize().to_hex().to_string(),
        HashAlgo::Sha256 => sha
            .finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    })
}

/// Hashes and confirms the file did not change meanwhile.
fn hash_checked(file: &File, id: &FileIdentity, algo: HashAlgo) -> HashOutcome {
    match digest(file, algo, id.size) {
        Ok(hex) => match file_identity(file) {
            Ok(after) if after == *id => HashOutcome::Done(hex),
            Ok(_) => HashOutcome::Changed,
            Err(_) => HashOutcome::Failed,
        },
        Err(_) => match file_identity(file) {
            Ok(after) if after != *id => HashOutcome::Changed,
            _ => HashOutcome::Failed,
        },
    }
}

impl HashCache {
    pub(super) fn new(concurrency: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            jobs: Arc::new(Semaphore::new(concurrency.max(1))),
        }
    }

    /// The cached digest, or computes it (inline up to `inline_max` bytes, else in the
    /// background). Blocking.
    pub(super) fn get_or_start(
        &self,
        runtime: &tokio::runtime::Handle,
        file: File,
        id: FileIdentity,
        etag: String,
        algo: HashAlgo,
        inline_max: u64,
    ) -> HashOutcome {
        let key = (etag, algo);
        {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            match inner.map.get(&key) {
                Some(Slot::Done(hex)) => return HashOutcome::Done(hex.clone()),
                Some(Slot::Computing) => return HashOutcome::Computing,
                None => {}
            }
        }
        if id.size <= inline_max {
            let outcome = hash_checked(&file, &id, algo);
            if let HashOutcome::Done(hex) = &outcome {
                self.inner
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(key, Slot::Done(hex.clone()));
            }
            return outcome;
        }
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone(), Slot::Computing);
        let inner = Arc::clone(&self.inner);
        let jobs = Arc::clone(&self.jobs);
        runtime.spawn(async move {
            let Ok(_permit) = jobs.acquire_owned().await else {
                return;
            };
            let done = tokio::task::spawn_blocking(move || hash_checked(&file, &id, algo)).await;
            let mut inner = inner.lock().unwrap_or_else(|e| e.into_inner());
            match done {
                Ok(HashOutcome::Done(hex)) => inner.insert(key, Slot::Done(hex)),
                // Changed or failed: forget the slot; the next request starts over (with the
                // new ETag if the file changed).
                _ => inner.remove(&key),
            }
        });
        HashOutcome::Computing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_digests() {
        let dir = std::env::temp_dir().join(format!("gander-hash-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("abc.txt");
        std::fs::write(&p, b"abc").unwrap();
        let f = File::open(&p).unwrap();
        assert_eq!(
            digest(&f, HashAlgo::Sha256, 3).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            digest(&f, HashAlgo::Blake3, 3).unwrap(),
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
        assert!(digest(&f, HashAlgo::Blake3, 4).is_err());
        drop(f);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn eviction_keeps_the_bound() {
        let mut inner = Inner::default();
        for i in 0..(MAX_ENTRIES + 10) {
            inner.insert((i.to_string(), HashAlgo::Blake3), Slot::Computing);
        }
        assert_eq!(inner.map.len(), MAX_ENTRIES);
        assert!(!inner.map.contains_key(&("0".to_owned(), HashAlgo::Blake3)));
    }
}
