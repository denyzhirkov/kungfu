#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use anyhow::{Context, Result};
use kungfu_types::chunk::Chunk;
use kungfu_types::file::FileEntry;
use kungfu_types::memory::{MemoryEntry, MemoryStatus, ProjectMemoryEntry};
use kungfu_types::relation::Relation;
use kungfu_types::symbol::Symbol;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use tracing::debug;

/// Atomically replace `path` with `contents`: write a sibling temp file, then
/// rename it over the target. Rename is atomic on the same filesystem, so a
/// crash mid-write leaves the old file intact instead of a truncated one — the
/// index/memory store is never observed half-written.
fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("store");
    let tmp = dir.join(format!(".{}.{}.tmp", name, std::process::id()));
    std::fs::write(&tmp, contents)
        .with_context(|| format!("writing temp file {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

pub struct JsonStore {
    base_dir: std::path::PathBuf,
    // In-memory caches: loaded once per JsonStore instance
    files_cache: RefCell<Option<Vec<FileEntry>>>,
    symbols_cache: RefCell<Option<Vec<Symbol>>>,
    relations_cache: RefCell<Option<Vec<Relation>>>,
    fingerprints_cache: RefCell<Option<HashMap<String, String>>>,
    memories_cache: RefCell<Option<Vec<MemoryEntry>>>,
}

impl JsonStore {
    pub fn new(base_dir: &Path) -> Self {
        Self {
            base_dir: base_dir.to_path_buf(),
            files_cache: RefCell::new(None),
            symbols_cache: RefCell::new(None),
            relations_cache: RefCell::new(None),
            fingerprints_cache: RefCell::new(None),
            memories_cache: RefCell::new(None),
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
        *self.files_cache.borrow_mut() = Some(files.to_vec());
        Ok(())
    }

    pub fn load_files(&self) -> Result<Vec<FileEntry>> {
        if let Some(cached) = self.files_cache.borrow().as_ref() {
            return Ok(cached.clone());
        }
        let path = self.base_dir.join("files.json");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&path)?;
        let files: Vec<FileEntry> = serde_json::from_str(&content)?;
        *self.files_cache.borrow_mut() = Some(files.clone());
        Ok(files)
    }

    pub fn save_symbols(&self, symbols: &[Symbol]) -> Result<()> {
        let path = self.base_dir.join("symbols.json");
        let json = serde_json::to_string(symbols)?;
        atomic_write(&path, &json)?;
        debug!("saved {} symbols to index", symbols.len());
        *self.symbols_cache.borrow_mut() = Some(symbols.to_vec());
        Ok(())
    }

    pub fn load_symbols(&self) -> Result<Vec<Symbol>> {
        if let Some(cached) = self.symbols_cache.borrow().as_ref() {
            return Ok(cached.clone());
        }
        let path = self.base_dir.join("symbols.json");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&path)?;
        let symbols: Vec<Symbol> = serde_json::from_str(&content)?;
        *self.symbols_cache.borrow_mut() = Some(symbols.clone());
        Ok(symbols)
    }

    pub fn save_relations(&self, relations: &[Relation]) -> Result<()> {
        let path = self.base_dir.join("relations.json");
        let json = serde_json::to_string(relations)?;
        atomic_write(&path, &json)?;
        *self.relations_cache.borrow_mut() = Some(relations.to_vec());
        Ok(())
    }

    pub fn load_relations(&self) -> Result<Vec<Relation>> {
        if let Some(cached) = self.relations_cache.borrow().as_ref() {
            return Ok(cached.clone());
        }
        let path = self.base_dir.join("relations.json");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&path)?;
        let relations: Vec<Relation> = serde_json::from_str(&content)?;
        *self.relations_cache.borrow_mut() = Some(relations.clone());
        Ok(relations)
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
        *self.fingerprints_cache.borrow_mut() = Some(fingerprints.clone());
        Ok(())
    }

    pub fn load_fingerprints(&self) -> Result<HashMap<String, String>> {
        if let Some(cached) = self.fingerprints_cache.borrow().as_ref() {
            return Ok(cached.clone());
        }
        let path = self.base_dir.join("fingerprints.json");
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let content = std::fs::read_to_string(&path)?;
        let fp: HashMap<String, String> = serde_json::from_str(&content)?;
        *self.fingerprints_cache.borrow_mut() = Some(fp.clone());
        Ok(fp)
    }

    pub fn save_memories(&self, memories: &[MemoryEntry]) -> Result<()> {
        let path = self.base_dir.join("memories.json");
        let json = serde_json::to_string(memories)?;
        atomic_write(&path, &json)?;
        debug!("saved {} memories to index", memories.len());
        *self.memories_cache.borrow_mut() = Some(memories.to_vec());
        Ok(())
    }

    pub fn load_memories(&self) -> Result<Vec<MemoryEntry>> {
        if let Some(cached) = self.memories_cache.borrow().as_ref() {
            return Ok(cached.clone());
        }
        let path = self.base_dir.join("memories.json");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&path)?;
        let memories: Vec<MemoryEntry> = serde_json::from_str(&content)?;
        *self.memories_cache.borrow_mut() = Some(memories.clone());
        Ok(memories)
    }

    // --- Project memory (explicit, user/agent managed) ---

    fn project_memory_path(&self) -> std::path::PathBuf {
        // Store alongside index dir, one level up: .kungfu/project_memory.json
        self.base_dir
            .parent()
            .unwrap_or(&self.base_dir)
            .join("project_memory.json")
    }

    pub fn save_project_memories(&self, entries: &[ProjectMemoryEntry]) -> Result<()> {
        let path = self.project_memory_path();
        let json = serde_json::to_string_pretty(entries)?;
        atomic_write(&path, &json)?;
        debug!("saved {} project memory entries", entries.len());
        Ok(())
    }

    pub fn load_project_memories(&self) -> Result<Vec<ProjectMemoryEntry>> {
        let path = self.project_memory_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&path)?;
        let entries: Vec<ProjectMemoryEntry> = serde_json::from_str(&content)?;
        Ok(entries)
    }

    pub fn next_project_memory_id(&self) -> Result<String> {
        let entries = self.load_project_memories()?;
        let max_num = entries
            .iter()
            .filter_map(|e| {
                e.id.strip_prefix("mem_")
                    .and_then(|n| n.parse::<u32>().ok())
            })
            .max()
            .unwrap_or(0);
        Ok(format!("mem_{:04}", max_num + 1))
    }

    pub fn add_project_memory(&self, entry: ProjectMemoryEntry) -> Result<ProjectMemoryEntry> {
        let mut entries = self.load_project_memories()?;
        entries.push(entry.clone());
        self.save_project_memories(&entries)?;
        Ok(entry)
    }

    pub fn update_project_memory(
        &self,
        id: &str,
        f: impl FnOnce(&mut ProjectMemoryEntry),
    ) -> Result<ProjectMemoryEntry> {
        let mut entries = self.load_project_memories()?;
        let entry = entries
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| anyhow::anyhow!("memory entry not found: {}", id))?;
        f(entry);
        entry.updated_at = chrono::Utc::now().to_rfc3339();
        let result = entry.clone();
        self.save_project_memories(&entries)?;
        Ok(result)
    }

    pub fn remove_project_memory(&self, id: &str) -> Result<()> {
        let mut entries = self.load_project_memories()?;
        let len_before = entries.len();
        entries.retain(|e| e.id != id);
        if entries.len() == len_before {
            anyhow::bail!("memory entry not found: {}", id);
        }
        self.save_project_memories(&entries)?;
        Ok(())
    }

    pub fn archive_project_memory(&self, id: &str) -> Result<ProjectMemoryEntry> {
        self.update_project_memory(id, |e| {
            e.status = MemoryStatus::Archived;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kungfu_types::memory::ProjectMemoryKind;

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
