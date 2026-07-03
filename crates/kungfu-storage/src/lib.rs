#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use anyhow::{Context, Result};
use kungfu_types::chunk::Chunk;
use kungfu_types::file::FileEntry;
use kungfu_types::memory::{MemoryEntry, ProjectMemoryEntry};
use kungfu_types::relation::Relation;
use kungfu_types::symbol::Symbol;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tracing::debug;

mod process_cache;
mod project_memory;
pub use project_memory::{AbsorbReport, MemoryMeta, ProjectMemoryStore};

/// Atomically replace `path` with `contents`: write a sibling temp file, then
/// rename it over the target. Rename is atomic on the same filesystem, so a
/// crash mid-write leaves the old file intact instead of a truncated one — the
/// index/memory store is never observed half-written.
pub(crate) fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("store");
    let tmp = dir.join(format!(".{}.{}.tmp", name, std::process::id()));
    std::fs::write(&tmp, contents)
        .with_context(|| format!("writing temp file {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Sidecar stamp naming the binary version that last wrote the index.
/// Additive: older binaries ignore it; its absence means the index was
/// written by a version that predates the stamp.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoreMeta {
    written_by: String,
}

const STORE_META_FILE: &str = "store_meta.json";

pub struct JsonStore {
    base_dir: std::path::PathBuf,
    // Per-instance snapshots: pinned on first load so one service call sees a
    // consistent view. The process-scope cache behind them (`process_cache`)
    // survives across instances and stamp-checks the files on each access.
    files_cache: RefCell<Option<Arc<Vec<FileEntry>>>>,
    symbols_cache: RefCell<Option<Arc<Vec<Symbol>>>>,
    relations_cache: RefCell<Option<Arc<Vec<Relation>>>>,
    fingerprints_cache: RefCell<Option<Arc<HashMap<String, String>>>>,
    memories_cache: RefCell<Option<Arc<Vec<MemoryEntry>>>>,
    pmem: ProjectMemoryStore,
}

impl JsonStore {
    pub fn new(base_dir: &Path) -> Self {
        // Manual memory lives under `.kungfu/`, one level up from the index dir.
        let kungfu_dir = base_dir.parent().unwrap_or(base_dir);
        Self {
            base_dir: base_dir.to_path_buf(),
            files_cache: RefCell::new(None),
            symbols_cache: RefCell::new(None),
            relations_cache: RefCell::new(None),
            fingerprints_cache: RefCell::new(None),
            memories_cache: RefCell::new(None),
            pmem: ProjectMemoryStore::new(kungfu_dir),
        }
    }

    /// Invalidate all caches (call after save operations that modify the index).
    pub fn invalidate(&self) {
        *self.files_cache.borrow_mut() = None;
        *self.symbols_cache.borrow_mut() = None;
        *self.relations_cache.borrow_mut() = None;
        *self.fingerprints_cache.borrow_mut() = None;
        *self.memories_cache.borrow_mut() = None;
    }

    pub fn save_files(&self, files: &[FileEntry]) -> Result<()> {
        let path = self.base_dir.join("files.json");
        let json = serde_json::to_string_pretty(files)?;
        atomic_write(&path, &json)?;
        debug!("saved {} files to index", files.len());
        process_cache::FILES.clear();
        *self.files_cache.borrow_mut() = Some(Arc::new(files.to_vec()));
        self.stamp_store_version()?;
        Ok(())
    }

    /// Record which binary version wrote the index (every index run saves
    /// `files.json`, so stamping here cannot be forgotten). Workspace crates
    /// share one version, so `CARGO_PKG_VERSION` equals the binary version.
    fn stamp_store_version(&self) -> Result<()> {
        let meta = StoreMeta {
            written_by: env!("CARGO_PKG_VERSION").to_string(),
        };
        let path = self.base_dir.join(STORE_META_FILE);
        atomic_write(&path, &serde_json::to_string(&meta)?)
    }

    /// Version of the binary that last wrote the index, if stamped.
    /// `None` means no index yet, a pre-stamp binary wrote it, or the stamp is
    /// unreadable (logged at debug) — callers treat all three as "unknown".
    pub fn load_store_version(&self) -> Option<String> {
        let path = self.base_dir.join(STORE_META_FILE);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                debug!("failed to read {}: {}", path.display(), e);
                return None;
            }
        };
        match serde_json::from_str::<StoreMeta>(&content) {
            Ok(meta) => Some(meta.written_by),
            Err(e) => {
                debug!("failed to parse {}: {}", path.display(), e);
                None
            }
        }
    }

    /// Shared snapshot of the files shard — no per-call clone.
    pub fn files_arc(&self) -> Result<Arc<Vec<FileEntry>>> {
        if let Some(cached) = self.files_cache.borrow().as_ref() {
            return Ok(Arc::clone(cached));
        }
        let data = process_cache::FILES.get_or_load(&self.base_dir.join("files.json"), |s| {
            Ok(serde_json::from_str(s)?)
        })?;
        *self.files_cache.borrow_mut() = Some(Arc::clone(&data));
        Ok(data)
    }

    pub fn load_files(&self) -> Result<Vec<FileEntry>> {
        Ok(self.files_arc()?.as_ref().clone())
    }

    pub fn save_symbols(&self, symbols: &[Symbol]) -> Result<()> {
        let path = self.base_dir.join("symbols.json");
        let json = serde_json::to_string(symbols)?;
        atomic_write(&path, &json)?;
        debug!("saved {} symbols to index", symbols.len());
        process_cache::SYMBOLS.clear();
        *self.symbols_cache.borrow_mut() = Some(Arc::new(symbols.to_vec()));
        Ok(())
    }

    /// Shared snapshot of the symbols shard — no per-call clone.
    pub fn symbols_arc(&self) -> Result<Arc<Vec<Symbol>>> {
        if let Some(cached) = self.symbols_cache.borrow().as_ref() {
            return Ok(Arc::clone(cached));
        }
        let data = process_cache::SYMBOLS
            .get_or_load(&self.base_dir.join("symbols.json"), |s| {
                Ok(serde_json::from_str(s)?)
            })?;
        *self.symbols_cache.borrow_mut() = Some(Arc::clone(&data));
        Ok(data)
    }

    pub fn load_symbols(&self) -> Result<Vec<Symbol>> {
        Ok(self.symbols_arc()?.as_ref().clone())
    }

    pub fn save_relations(&self, relations: &[Relation]) -> Result<()> {
        let path = self.base_dir.join("relations.json");
        let json = serde_json::to_string(relations)?;
        atomic_write(&path, &json)?;
        process_cache::RELATIONS.clear();
        *self.relations_cache.borrow_mut() = Some(Arc::new(relations.to_vec()));
        Ok(())
    }

    /// Shared snapshot of the relations shard — no per-call clone. On large
    /// projects this is the biggest shard (call graph), so hot paths should
    /// prefer this over `load_relations`.
    pub fn relations_arc(&self) -> Result<Arc<Vec<Relation>>> {
        if let Some(cached) = self.relations_cache.borrow().as_ref() {
            return Ok(Arc::clone(cached));
        }
        let data = process_cache::RELATIONS
            .get_or_load(&self.base_dir.join("relations.json"), |s| {
                Ok(serde_json::from_str(s)?)
            })?;
        *self.relations_cache.borrow_mut() = Some(Arc::clone(&data));
        Ok(data)
    }

    pub fn load_relations(&self) -> Result<Vec<Relation>> {
        Ok(self.relations_arc()?.as_ref().clone())
    }

    pub fn save_chunks(&self, chunks: &[Chunk]) -> Result<()> {
        let path = self.base_dir.join("chunks.json");
        let json = serde_json::to_string(chunks)?;
        atomic_write(&path, &json)?;
        Ok(())
    }

    pub fn load_chunks(&self) -> Result<Vec<Chunk>> {
        let path = self.base_dir.join("chunks.json");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn save_fingerprints(&self, fingerprints: &HashMap<String, String>) -> Result<()> {
        let path = self.base_dir.join("fingerprints.json");
        let json = serde_json::to_string(fingerprints)?;
        atomic_write(&path, &json)?;
        process_cache::FINGERPRINTS.clear();
        *self.fingerprints_cache.borrow_mut() = Some(Arc::new(fingerprints.clone()));
        Ok(())
    }

    pub fn load_fingerprints(&self) -> Result<HashMap<String, String>> {
        if let Some(cached) = self.fingerprints_cache.borrow().as_ref() {
            return Ok(cached.as_ref().clone());
        }
        let data = process_cache::FINGERPRINTS
            .get_or_load(&self.base_dir.join("fingerprints.json"), |s| {
                Ok(serde_json::from_str(s)?)
            })?;
        *self.fingerprints_cache.borrow_mut() = Some(Arc::clone(&data));
        Ok(data.as_ref().clone())
    }

    pub fn save_memories(&self, memories: &[MemoryEntry]) -> Result<()> {
        let path = self.base_dir.join("memories.json");
        let json = serde_json::to_string(memories)?;
        atomic_write(&path, &json)?;
        debug!("saved {} memories to index", memories.len());
        process_cache::MEMORIES.clear();
        *self.memories_cache.borrow_mut() = Some(Arc::new(memories.to_vec()));
        Ok(())
    }

    /// Shared snapshot of the code-memories shard — no per-call clone.
    pub fn memories_arc(&self) -> Result<Arc<Vec<MemoryEntry>>> {
        if let Some(cached) = self.memories_cache.borrow().as_ref() {
            return Ok(Arc::clone(cached));
        }
        let data = process_cache::MEMORIES
            .get_or_load(&self.base_dir.join("memories.json"), |s| {
                Ok(serde_json::from_str(s)?)
            })?;
        *self.memories_cache.borrow_mut() = Some(Arc::clone(&data));
        Ok(data)
    }

    pub fn load_memories(&self) -> Result<Vec<MemoryEntry>> {
        Ok(self.memories_arc()?.as_ref().clone())
    }

    // --- Project memory (explicit, user/agent managed) ---
    //
    // Backed by `.kungfu/memory/` (one `.md` per entry + derived manifest). The
    // single-file `project_memory.json` is migrated on first use. These methods
    // delegate to `ProjectMemoryStore`; CLI/MCP adapters are unaffected.

    pub fn next_project_memory_id(&self) -> Result<String> {
        self.pmem.next_id()
    }

    pub fn add_project_memory(&self, entry: ProjectMemoryEntry) -> Result<ProjectMemoryEntry> {
        self.pmem.add(entry)
    }

    pub fn get_project_memory(&self, id: &str) -> Result<ProjectMemoryEntry> {
        self.pmem.get(id)
    }

    pub fn update_project_memory(
        &self,
        id: &str,
        f: impl FnOnce(&mut ProjectMemoryEntry),
    ) -> Result<ProjectMemoryEntry> {
        self.pmem.update(id, f)
    }

    pub fn remove_project_memory(&self, id: &str) -> Result<()> {
        self.pmem.remove(id)
    }

    pub fn archive_project_memory(&self, id: &str) -> Result<ProjectMemoryEntry> {
        self.pmem.archive(id)
    }

    /// All entry metadata (no bodies) — cheap listing/filtering.
    pub fn list_project_memory_meta(&self) -> Result<Vec<MemoryMeta>> {
        self.pmem.list_meta()
    }

    /// Candidate ids for a query via the inverted index (recall filter).
    pub fn project_memory_candidates(&self, query: &str) -> Result<Vec<String>> {
        self.pmem.candidate_ids(query)
    }

    /// Load full entries for specific ids (bodies read only for these).
    pub fn load_project_memory_bodies(&self, ids: &[String]) -> Result<Vec<ProjectMemoryEntry>> {
        self.pmem.load_bodies(ids)
    }

    /// Every entry with its body. O(N) reads — for export/maintenance, not hot paths.
    pub fn load_project_memories(&self) -> Result<Vec<ProjectMemoryEntry>> {
        self.pmem.load_all()
    }

    /// True if a stray legacy `project_memory.json` sits next to the `.md` store.
    pub fn project_memory_legacy_present(&self) -> bool {
        self.pmem.legacy_present()
    }

    /// Absorb a stray legacy memory file into the `.md` store (doctor --fix).
    pub fn absorb_project_memory_legacy(&self) -> Result<Option<AbsorbReport>> {
        self.pmem.absorb_legacy()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kungfu_types::memory::{MemoryStatus, ProjectMemoryKind};

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        let unique = format!(
            "kungfu-store-test-{}-{}-{:p}",
            tag,
            std::process::id(),
            &dir as *const _
        );
        dir.push(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(id: &str, content: &str) -> ProjectMemoryEntry {
        ProjectMemoryEntry {
            id: id.to_string(),
            kind: ProjectMemoryKind::Fact,
            title: None,
            content: content.to_string(),
            tags: vec![],
            related_files: vec![],
            related_symbols: vec![],
            pinned: false,
            status: MemoryStatus::Active,
            supersedes: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn atomic_write_round_trips_and_leaves_no_tmp() {
        let dir = temp_dir("atomic");
        let path = dir.join("data.json");
        atomic_write(&path, "{\"k\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"k\":1}");
        // Overwrite, then confirm no stray .tmp siblings remain in the dir.
        atomic_write(&path, "{\"k\":2}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"k\":2}");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "tmp file left behind: {:?}",
            leftovers
        );
    }

    // The process cache holds one snapshot per shard; tests touching the same
    // shard from different dirs would thrash each other under the parallel test
    // runner, so they serialize on this lock.
    static PROCESS_CACHE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn relation(source: &str, target: &str) -> Relation {
        Relation {
            source_id: source.to_string(),
            target_id: target.to_string(),
            kind: kungfu_types::relation::RelationKind::Calls,
            weight: 1.0,
        }
    }

    #[test]
    fn process_cache_shares_one_parse_across_store_instances() {
        let _guard = PROCESS_CACHE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("proc-cache-hit");
        JsonStore::new(&dir)
            .save_relations(&[relation("a", "b")])
            .unwrap();

        // Two fresh instances (the MCP per-call pattern): the second must get
        // the exact snapshot the first parse produced, not a re-parse.
        let first = JsonStore::new(&dir).relations_arc().unwrap();
        let second = JsonStore::new(&dir).relations_arc().unwrap();
        assert_eq!(first.len(), 1);
        assert!(
            Arc::ptr_eq(&first, &second),
            "unchanged store file must be served from the process cache"
        );
    }

    #[test]
    fn process_cache_detects_rewrite_behind_it() {
        let _guard = PROCESS_CACHE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("proc-cache-stale");
        JsonStore::new(&dir)
            .save_relations(&[relation("a", "b")])
            .unwrap();
        let before = JsonStore::new(&dir).relations_arc().unwrap();
        assert_eq!(before.len(), 1);

        // Rewrite the file directly (simulates another process / external tool
        // touching the index — no save_* invalidation involved).
        let rewritten = serde_json::to_string(&[relation("a", "b"), relation("c", "d")]).unwrap();
        std::fs::write(dir.join("relations.json"), rewritten).unwrap();

        let after = JsonStore::new(&dir).relations_arc().unwrap();
        assert_eq!(
            after.len(),
            2,
            "stamp check on read must pick up the rewrite"
        );
        // The pre-rewrite snapshot stays intact for readers already holding it.
        assert_eq!(before.len(), 1);
    }

    #[test]
    fn process_cache_refreshes_after_save_from_another_instance() {
        let _guard = PROCESS_CACHE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("proc-cache-save");
        JsonStore::new(&dir)
            .save_relations(&[relation("a", "b")])
            .unwrap();
        assert_eq!(JsonStore::new(&dir).relations_arc().unwrap().len(), 1);

        JsonStore::new(&dir)
            .save_relations(&[relation("a", "b"), relation("b", "c"), relation("c", "d")])
            .unwrap();
        assert_eq!(
            JsonStore::new(&dir).relations_arc().unwrap().len(),
            3,
            "a save must invalidate the process cache for later readers"
        );
    }

    #[test]
    fn process_cache_handles_missing_and_appearing_file() {
        let _guard = PROCESS_CACHE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("proc-cache-missing");
        assert!(JsonStore::new(&dir).relations_arc().unwrap().is_empty());

        JsonStore::new(&dir)
            .save_relations(&[relation("x", "y")])
            .unwrap();
        assert_eq!(
            JsonStore::new(&dir).relations_arc().unwrap().len(),
            1,
            "a file appearing after a cached 'missing' must be picked up"
        );
    }

    #[test]
    fn save_files_stamps_store_version() {
        let dir = temp_dir("stamp");
        let store = JsonStore::new(&dir);
        assert_eq!(store.load_store_version(), None);

        store.save_files(&[]).unwrap();
        assert_eq!(
            store.load_store_version().as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn corrupt_store_stamp_reads_as_unknown() {
        let dir = temp_dir("stamp-corrupt");
        let store = JsonStore::new(&dir);
        std::fs::write(dir.join("store_meta.json"), "{ broken").unwrap();
        assert_eq!(store.load_store_version(), None);
    }

    #[test]
    fn project_memory_add_and_remove_persist() {
        let index_dir = temp_dir("projmem").join("index");
        std::fs::create_dir_all(&index_dir).unwrap();
        let store = JsonStore::new(&index_dir);

        store
            .add_project_memory(entry("mem_0001", "first"))
            .unwrap();
        store
            .add_project_memory(entry("mem_0002", "second"))
            .unwrap();
        assert_eq!(store.load_project_memories().unwrap().len(), 2);

        store.remove_project_memory("mem_0001").unwrap();
        let left = store.load_project_memories().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "mem_0002");
    }
}
