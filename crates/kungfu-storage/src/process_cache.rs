//! Process-scope cache of parsed index shards.
//!
//! The MCP server rebuilds a fresh `KungfuService` (and thus a fresh `JsonStore`)
//! on every tool call, so the per-instance `RefCell` caches are always cold
//! there — every `callers`/`find_symbol` call used to re-parse multi-megabyte
//! JSON shards from scratch. This static layer survives across store instances.
//!
//! Staleness: every access re-stats the backing file (inode + mtime + len) and
//! reloads only when the stamp changed. Writes in this crate go through
//! `atomic_write` (tmp + rename), so a rewrite always produces a new inode —
//! the O(1 stat) check-on-read is the safety net the project invariant requires
//! ("a missed notify event must not produce stale results").
//!
//! Concurrency: one `RwLock` per shard, at most one snapshot held per shard.
//! Data is handed out as `Arc`, so readers keep a consistent snapshot while a
//! reload swaps the slot. The parse runs under the write lock (single flight):
//! concurrent requests for the same stale shard wait and then hit.

use anyhow::{Context, Result};
use kungfu_types::file::FileEntry;
use kungfu_types::memory::MemoryEntry;
use kungfu_types::relation::Relation;
use kungfu_types::symbol::Symbol;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::SystemTime;
use tracing::debug;

/// A JSON index shard larger than this is corrupt, not big: even a huge repo's
/// symbols shard stays in the low hundreds of MB. Reading past this hangs every
/// tool call (the blow-up symptom), so loads refuse it with a rebuild hint.
const MAX_SHARD_BYTES: u64 = 1024 * 1024 * 1024;

pub(crate) static FILES: Slot<Vec<FileEntry>> = Slot::new("files");
pub(crate) static SYMBOLS: Slot<Vec<Symbol>> = Slot::new("symbols");
pub(crate) static RELATIONS: Slot<Vec<Relation>> = Slot::new("relations");
pub(crate) static MEMORIES: Slot<Vec<MemoryEntry>> = Slot::new("memories");
pub(crate) static FINGERPRINTS: Slot<HashMap<String, String>> = Slot::new("fingerprints");

/// Cheap identity of a store file's on-disk state. `atomic_write` renames a
/// fresh temp file over the target, so any rewrite changes the inode; mtime
/// and length cover in-place edits by external tools.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Stamp {
    mtime: Option<SystemTime>,
    len: u64,
    #[cfg(unix)]
    ino: u64,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(Stamp {
        mtime: meta.modified().ok(),
        len: meta.len(),
        #[cfg(unix)]
        ino: std::os::unix::fs::MetadataExt::ino(&meta),
    })
}

struct Entry<T> {
    path: PathBuf,
    /// `None` = the file did not exist (an empty default is cached for it).
    stamp: Option<Stamp>,
    data: Arc<T>,
}

pub(crate) struct Slot<T> {
    shard: &'static str,
    inner: RwLock<Option<Entry<T>>>,
}

impl<T: Default + Send + Sync> Slot<T> {
    const fn new(shard: &'static str) -> Self {
        Self {
            shard,
            inner: RwLock::new(None),
        }
    }

    /// Drop the snapshot — the next access reloads from disk. Called after a
    /// save so other store instances never see pre-write data via a racy stamp.
    pub(crate) fn clear(&self) {
        *write_lock(&self.inner) = None;
    }

    pub(crate) fn get_or_load(
        &self,
        path: &Path,
        parse: impl FnOnce(&str) -> Result<T>,
    ) -> Result<Arc<T>> {
        let current = stamp(path);
        {
            let guard = read_lock(&self.inner);
            if let Some(e) = guard.as_ref() {
                if e.path == path && e.stamp == current {
                    debug!(shard = self.shard, "process cache hit");
                    return Ok(Arc::clone(&e.data));
                }
            }
        }

        let mut guard = write_lock(&self.inner);
        // Re-check: another thread may have loaded while we waited for the lock.
        let current = stamp(path);
        if let Some(e) = guard.as_ref() {
            if e.path == path && e.stamp == current {
                debug!(shard = self.shard, "process cache hit after reload wait");
                return Ok(Arc::clone(&e.data));
            }
        }

        debug!(shard = self.shard, path = %path.display(), "process cache miss, loading shard");
        let data = match current {
            None => Arc::new(T::default()),
            Some(s) => {
                // Fail loud instead of slurping a corrupt multi-GB shard into
                // memory (the symbol-duplication blow-up hangs every reader on
                // read_to_string). No real index shard approaches this size.
                if s.len > MAX_SHARD_BYTES {
                    anyhow::bail!(
                        "index shard {} is {:.1} GiB — no real index is this large, \
                         it is corrupt (likely duplicate-symbol accumulation). \
                         Rebuild with `kungfu index --full`.",
                        path.display(),
                        s.len as f64 / (1024.0 * 1024.0 * 1024.0)
                    );
                }
                let content = std::fs::read_to_string(path)
                    .with_context(|| format!("reading index shard {}", path.display()))?;
                Arc::new(parse(&content)?)
            }
        };
        // If the file is replaced between the stat above and the read, newer
        // content is cached under the older stamp; the next access sees a
        // mismatch and reloads. The race never serves stale data.
        *guard = Some(Entry {
            path: path.to_path_buf(),
            stamp: current,
            data: Arc::clone(&data),
        });
        Ok(data)
    }
}

fn read_lock<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_lock<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}
