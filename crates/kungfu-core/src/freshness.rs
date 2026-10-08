//! Lazy index freshness check. Runs before every MCP tool call (the service is
//! rebuilt per call), so the fresh path must stay a stat pass over the indexed
//! tree — no hashing, no walk.

use crate::KungfuService;
use anyhow::Result;
use kungfu_types::file::FileEntry;
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use tracing::info;

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
    Marker(PathBuf),
    Modified(PathBuf),
    Missing(PathBuf),
    NewEntry(PathBuf),
}

impl fmt::Display for StaleReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Marker(p) => write!(f, "root marker {} changed", p.display()),
            Self::Modified(p) => write!(f, "{} modified", p.display()),
            Self::Missing(p) => write!(f, "{} removed", p.display()),
            Self::NewEntry(p) => write!(f, "new entry {}", p.display()),
        }
    }
}

impl KungfuService {
    /// Check if the index is stale and reindex if needed. Returns whether a
    /// reindex ran. Staleness is judged against the fingerprints.json mtime —
    /// every index write rewrites that file, so a reindex always settles the
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

        let fp_mtime = std::fs::metadata(&fp_path)?.modified()?;
        let files = self.store.files_arc()?;
        match find_staleness(
            &self.project.root,
            &files,
            fp_mtime,
            self.project.config.index_hidden,
        ) {
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
/// Stale when an indexed file is missing or newer than the index, or when a
/// directory on the path to an indexed file (root included) is newer and holds
/// an entry the index does not know — a directory mtime moves on any
/// add/remove/rename, but removals already surface as missing files.
fn find_staleness(
    root: &Path,
    files: &[FileEntry],
    index_mtime: SystemTime,
    index_hidden: bool,
) -> Option<StaleReason> {
    // A future mtime cannot come from an edit made since the index was written
    // (an edit stamps "now"); counting it would reindex on every call until the
    // clock catches up.
    let now = SystemTime::now();
    let newer = |path: &Path| {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t > index_mtime && t <= now)
    };

    for marker in ROOT_MARKERS {
        if newer(&root.join(marker)) {
            return Some(StaleReason::Marker(PathBuf::from(marker)));
        }
    }

    let stop = AtomicBool::new(false);
    let file_hit = in_parallel(files, |chunk| {
        for file in chunk {
            if stop.load(Ordering::Relaxed) {
                return None;
            }
            let rel = Path::new(&file.path);
            let reason = match std::fs::metadata(root.join(rel)) {
                Ok(meta) if meta.modified().is_ok_and(|t| t > index_mtime && t <= now) => {
                    Some(StaleReason::Modified(rel.to_path_buf()))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Some(StaleReason::Missing(rel.to_path_buf()))
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

    let mut dirs: HashSet<&Path> = HashSet::new();
    for file in files {
        let mut parent = Path::new(&file.path).parent();
        while let Some(dir) = parent {
            if !dirs.insert(dir) {
                break;
            }
            parent = dir.parent();
        }
    }
    let dir_list: Vec<&Path> = dirs.iter().copied().collect();
    let dirty: Vec<&Path> = in_parallel(&dir_list, |chunk| {
        chunk
            .iter()
            .copied()
            .filter(|d| newer(&root.join(d)))
            .collect::<Vec<_>>()
    })
    .into_iter()
    .flatten()
    .collect();
    if dirty.is_empty() {
        return None;
    }

    // Only an unknown, possibly indexable entry is worth a reindex; hidden
    // entries (.DS_Store, editor swap files) are never indexed unless asked.
    let mut known: HashSet<&Path> = files.iter().map(|f| Path::new(&f.path)).collect();
    known.extend(dirs.iter().copied());
    for dir in dirty {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !index_hidden && name.to_string_lossy().starts_with('.') {
                continue;
            }
            let rel = dir.join(&name);
            if !known.contains(rel.as_path()) {
                return Some(StaleReason::NewEntry(rel));
            }
        }
    }
    None
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
