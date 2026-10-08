//! Lazy index freshness check. Runs before every MCP tool call (the service is
//! rebuilt per call), so the fresh path must stay a stat pass over the indexed
//! tree — no hashing, no walk.

use crate::KungfuService;
use anyhow::Result;
use kungfu_config::KungfuConfig;
use kungfu_types::file::{FileEntry, TreeState};
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use tracing::{debug, info};

/// Root manifests/lockfiles: a change can matter even when the file itself is
/// not part of the index.
const ROOT_MARKERS: [&str; 7] = [
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "Cargo.lock",
    "package-lock.json",
    "bun.lock",
];

#[derive(Debug, PartialEq, Eq)]
enum StaleReason {
    NoTreeState,
    Marker(PathBuf),
    Modified(PathBuf),
    Missing(PathBuf),
    NewEntry(PathBuf),
}

impl fmt::Display for StaleReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTreeState => write!(f, "no tree state recorded by the last index run"),
            Self::Marker(p) => write!(f, "root marker {} changed", p.display()),
            Self::Modified(p) => write!(f, "{} modified", p.display()),
            Self::Missing(p) => write!(f, "{} removed", p.display()),
            Self::NewEntry(p) => write!(f, "new entry {}", p.display()),
        }
    }
}

impl KungfuService {
    /// Check if the index is stale and reindex if needed. Returns whether a
    /// reindex ran. Every full/incremental run records its scan start and the
    /// directories it walked (`TreeState`), so a reindex always settles the
    /// check.
    pub fn ensure_fresh_index(&self) -> Result<bool> {
        let fp_path = self.project.index_dir().join("fingerprints.json");
        if !fp_path.exists() {
            info!("no index found, running full index");
            self.index_full()?;
            return Ok(true);
        }

        if !self.index_schema_current() {
            info!("index schema is outdated, running full reindex");
            self.index_full()?;
            return Ok(true);
        }

        let state = self.store.tree_state();
        let reason = match state.as_ref() {
            // Index written before tree states existed: one rescan records it.
            None => Some(StaleReason::NoTreeState),
            Some(state) => {
                let files = self.store.files_arc()?;
                find_staleness(&self.project.root, &self.project.config, &files, state)
            }
        };
        match reason {
            Some(reason) => {
                info!("index is stale ({reason}), running incremental reindex");
                self.index_incremental()?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

/// First reason the index is older than the tree, or `None` when fresh.
///
/// Stale when an indexed file is missing or modified at/after its reference
/// time, or when a directory the last scan walked was modified after its
/// reference and now holds an entry the scanner would pick up but the index
/// does not know. A directory mtime moves on any add/remove/rename; removals
/// already surface as missing files, and ignored or hidden entries
/// (`.DS_Store`, swap files, `target/`) are filtered by the scanner's rules.
fn find_staleness(
    root: &Path,
    config: &KungfuConfig,
    files: &[FileEntry],
    state: &TreeState,
) -> Option<StaleReason> {
    let scanned_at = SystemTime::from(state.scanned_at);
    // A future mtime cannot come from an edit made since the index was written
    // (an edit stamps "now"); counting it would reindex on every call until the
    // clock catches up.
    let now = SystemTime::now();
    let changed_since = |rel: &str, mtime: SystemTime| {
        mtime >= scanned_at
            && mtime <= now
            && state
                .refreshed
                .get(rel)
                .is_none_or(|t| mtime >= SystemTime::from(*t))
    };
    let modified = |rel: &str| {
        std::fs::metadata(root.join(rel))
            .and_then(|m| m.modified())
            .is_ok_and(|t| changed_since(rel, t))
    };

    for marker in ROOT_MARKERS {
        if modified(marker) {
            return Some(StaleReason::Marker(PathBuf::from(marker)));
        }
    }

    let stop = AtomicBool::new(false);
    let file_hit = in_parallel(files, |chunk| {
        for file in chunk {
            if stop.load(Ordering::Relaxed) {
                return None;
            }
            let reason = match std::fs::metadata(root.join(&file.path)) {
                Ok(meta) if meta.modified().is_ok_and(|t| changed_since(&file.path, t)) => {
                    Some(StaleReason::Modified(PathBuf::from(&file.path)))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Some(StaleReason::Missing(PathBuf::from(&file.path)))
                }
                // Unreadable metadata: the indexer would skip it too; flagging
                // it would reindex forever.
                _ => None,
            };
            if reason.is_some() {
                stop.store(true, Ordering::Relaxed);
                return reason;
            }
        }
        None
    });
    if let Some(reason) = file_hit.into_iter().flatten().next() {
        return Some(reason);
    }

    let dirty: Vec<PathBuf> = in_parallel(&state.dirs, |chunk| {
        chunk
            .iter()
            .filter(|d| modified(d))
            .map(|d| root.join(d))
            .collect::<Vec<_>>()
    })
    .into_iter()
    .flatten()
    .collect();
    if dirty.is_empty() {
        return None;
    }

    // A dirty directory stays dirty until the next full scan, so this listing
    // repeats per call — one shallow walk over just those directories.
    let entries = match kungfu_index::scanner::scan_dirs_shallow(root, &dirty, config) {
        Ok(entries) => entries,
        Err(e) => {
            debug!("cannot list changed directories: {e:#}");
            return None;
        }
    };
    let mut known: HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    known.extend(state.dirs.iter().map(String::as_str));
    entries.into_iter().find_map(|abs| {
        let rel = abs.strip_prefix(root).unwrap_or(&abs).to_string_lossy();
        (!known.contains(rel.as_ref())).then(|| StaleReason::NewEntry(PathBuf::from(rel.as_ref())))
    })
}

/// Stat calls dominate the check (~2µs each on APFS) and parallelize well;
/// below this many items a thread spawn costs more than it saves.
const PARALLEL_MIN_ITEMS: usize = 1024;
const MAX_WORKERS: usize = 8;

/// Run `work` over contiguous chunks of `items` on scoped threads, one result
/// per chunk.
fn in_parallel<T: Sync, R: Send>(items: &[T], work: impl Fn(&[T]) -> R + Sync) -> Vec<R> {
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(MAX_WORKERS);
    if workers < 2 || items.len() < PARALLEL_MIN_ITEMS {
        return vec![work(items)];
    }
    let work = &work;
    std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(items.len().div_ceil(workers))
            .map(|chunk| scope.spawn(move || work(chunk)))
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
}
