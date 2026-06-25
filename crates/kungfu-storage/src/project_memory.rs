//! Manual ("project") memory store: one Markdown file per entry as the durable
//! source of truth, plus a derived, rebuildable `manifest.json` holding metadata
//! and an inverted index for candidate generation.
//!
//! Layout under `.kungfu/`:
//! ```text
//! memory/
//!   mem_0001.md   # `+++` TOML frontmatter (metadata) + markdown body (content)
//!   mem_0002.md
//!   manifest.json # derived: metas + postings (term -> ids), rebuilt on drift
//! ```
//!
//! Invariants:
//! - The `.md` files are the source of truth. `manifest.json` is a cache that can
//!   always be rebuilt by scanning `*.md`; a missing/corrupt/stale manifest never
//!   loses data.
//! - Writes are atomic (temp file + rename) via [`crate::atomic_write`].
//! - Search reads bodies only for candidates, never the whole corpus.

use anyhow::{Context, Result};
use kungfu_types::memory::{MemoryStatus, ProjectMemoryEntry, ProjectMemoryKind};
use serde::{Deserialize, Serialize};
use std::cell::{Ref, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

use crate::atomic_write;

const MANIFEST_VERSION: u32 = 1;
const OPEN_FENCE: &str = "+++";

/// Per-entry metadata — every field of [`ProjectMemoryEntry`] except the body.
/// Used both as the `.md` frontmatter and as a manifest row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMeta {
    pub id: String,
    pub kind: ProjectMemoryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_symbols: Vec<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub status: MemoryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl MemoryMeta {
    fn from_entry(e: &ProjectMemoryEntry) -> Self {
        Self {
            id: e.id.clone(),
            kind: e.kind,
            title: e.title.clone(),
            tags: e.tags.clone(),
            related_files: e.related_files.clone(),
            related_symbols: e.related_symbols.clone(),
            pinned: e.pinned,
            status: e.status,
            supersedes: e.supersedes.clone(),
            created_at: e.created_at.clone(),
            updated_at: e.updated_at.clone(),
        }
    }

    fn into_entry(self, content: String) -> ProjectMemoryEntry {
        ProjectMemoryEntry {
            id: self.id,
            kind: self.kind,
            title: self.title,
            content,
            tags: self.tags,
            related_files: self.related_files,
            related_symbols: self.related_symbols,
            pinned: self.pinned,
            status: self.status,
            supersedes: self.supersedes,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Derived index over all entries. Rebuildable from the `.md` files.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    metas: Vec<MemoryMeta>,
    /// Inverted index for candidate generation: normalized term -> entry ids.
    postings: HashMap<String, Vec<String>>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: MANIFEST_VERSION,
            metas: Vec::new(),
            postings: HashMap::new(),
        }
    }
}

// --- `.md` (de)serialization -------------------------------------------------

/// Render an entry as `+++`-fenced TOML frontmatter followed by the body.
fn render_md(entry: &ProjectMemoryEntry) -> Result<String> {
    let meta = MemoryMeta::from_entry(entry);
    let frontmatter = toml::to_string(&meta).context("serializing memory frontmatter")?;
    let body = entry.content.trim_end_matches('\n');
    Ok(format!(
        "{OPEN_FENCE}\n{frontmatter}{OPEN_FENCE}\n\n{body}\n"
    ))
}

/// Parse a `.md` file back into an entry. The frontmatter is TOML between the
/// first two `+++` fences; everything after is the body.
fn parse_md(text: &str) -> Result<ProjectMemoryEntry> {
    let after_open = text
        .trim_start()
        .strip_prefix(OPEN_FENCE)
        .context("memory file missing opening +++ fence")?
        .strip_prefix('\n')
        .context("malformed opening fence")?;
    // Closing fence is a line that is exactly `+++`.
    let close_at = after_open
        .find(&format!("\n{OPEN_FENCE}"))
        .context("memory file missing closing +++ fence")?;
    let frontmatter = &after_open[..close_at];
    let body = after_open[close_at + 1 + OPEN_FENCE.len()..].trim_start_matches('\n');
    let meta: MemoryMeta = toml::from_str(frontmatter).context("parsing memory frontmatter")?;
    Ok(meta.into_entry(body.trim_end_matches('\n').to_string()))
}

// --- tokenization for the inverted index ------------------------------------

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "are", "was", "not", "but", "you",
    "all", "any", "can", "has", "had", "have", "its", "our", "use", "via",
];

/// Normalize text into index terms: lowercase, split on non-alphanumeric, keep
/// tokens of length >= 3 that aren't stopwords. Deduplicated.
fn tokenize(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .map(|t| t.to_lowercase())
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .collect()
}

/// All index terms for an entry: title + tags + related symbols + content.
fn entry_terms(meta: &MemoryMeta, content: &str) -> HashSet<String> {
    let mut terms = tokenize(content);
    if let Some(t) = &meta.title {
        terms.extend(tokenize(t));
    }
    for tag in &meta.tags {
        terms.extend(tokenize(tag));
    }
    for sym in &meta.related_symbols {
        terms.extend(tokenize(sym));
    }
    terms
}

/// The manual-memory store. Owns its directory and an in-memory manifest cache.
pub struct ProjectMemoryStore {
    dir: PathBuf,
    legacy_json: PathBuf,
    manifest: RefCell<Option<Manifest>>,
}

impl ProjectMemoryStore {
    /// `kungfu_dir` is the `.kungfu` directory (the parent of the index dir).
    pub fn new(kungfu_dir: &Path) -> Self {
        Self {
            dir: kungfu_dir.join("memory"),
            legacy_json: kungfu_dir.join("project_memory.json"),
            manifest: RefCell::new(None),
        }
    }

    fn entry_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.md"))
    }

    fn manifest_path(&self) -> PathBuf {
        self.dir.join("manifest.json")
    }

    /// Ensure the store dir exists, migrating the legacy single-file store on
    /// first use, and load (or rebuild) the manifest into the cache.
    fn ensure_ready(&self) -> Result<()> {
        if self.manifest.borrow().is_some() {
            return Ok(());
        }
        if !self.dir.exists() {
            std::fs::create_dir_all(&self.dir)
                .with_context(|| format!("creating {}", self.dir.display()))?;
            if self.legacy_json.exists() {
                self.migrate_legacy()?;
            }
        }
        let manifest = self.load_or_rebuild_manifest()?;
        *self.manifest.borrow_mut() = Some(manifest);
        Ok(())
    }

    /// Count `.md` files in the store dir (cheap staleness signal).
    fn count_md_files(&self) -> Result<usize> {
        let mut n = 0;
        for entry in std::fs::read_dir(&self.dir)
            .with_context(|| format!("reading {}", self.dir.display()))?
        {
            let entry = entry?;
            if entry.path().extension().and_then(|e| e.to_str()) == Some("md") {
                n += 1;
            }
        }
        Ok(n)
    }

    fn load_or_rebuild_manifest(&self) -> Result<Manifest> {
        let path = self.manifest_path();
        if path.exists() {
            match std::fs::read_to_string(&path)
                .ok()
                .and_then(|c| serde_json::from_str::<Manifest>(&c).ok())
            {
                Some(m)
                    if m.version == MANIFEST_VERSION
                        && self
                            .count_md_files()
                            .map(|n| n == m.metas.len())
                            .unwrap_or(false) =>
                {
                    return Ok(m)
                }
                _ => warn!("project memory manifest missing/stale/corrupt — rebuilding"),
            }
        }
        self.rebuild_manifest()
    }

    /// Rebuild the manifest by scanning every `.md` file. Source of truth wins.
    fn rebuild_manifest(&self) -> Result<Manifest> {
        let mut metas = Vec::new();
        let mut postings: HashMap<String, Vec<String>> = HashMap::new();
        for dirent in std::fs::read_dir(&self.dir)
            .with_context(|| format!("reading {}", self.dir.display()))?
        {
            let path = dirent?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    warn!("skipping unreadable memory file {}: {}", path.display(), e);
                    continue;
                }
            };
            let entry = match parse_md(&text) {
                Ok(e) => e,
                Err(e) => {
                    warn!("skipping malformed memory file {}: {}", path.display(), e);
                    continue;
                }
            };
            let meta = MemoryMeta::from_entry(&entry);
            for term in entry_terms(&meta, &entry.content) {
                postings.entry(term).or_default().push(meta.id.clone());
            }
            metas.push(meta);
        }
        metas.sort_by(|a, b| a.id.cmp(&b.id));
        let manifest = Manifest {
            version: MANIFEST_VERSION,
            metas,
            postings,
        };
        self.persist_manifest(&manifest)?;
        Ok(manifest)
    }

    fn persist_manifest(&self, manifest: &Manifest) -> Result<()> {
        let json = serde_json::to_string(manifest)?;
        atomic_write(&self.manifest_path(), &json)
    }

    /// One-time migration: legacy `project_memory.json` array -> per-entry `.md`
    /// + manifest. Non-destructive: the old file is renamed to `.bak`.
    fn migrate_legacy(&self) -> Result<()> {
        let content = std::fs::read_to_string(&self.legacy_json)
            .with_context(|| format!("reading {}", self.legacy_json.display()))?;
        let entries: Vec<ProjectMemoryEntry> =
            serde_json::from_str(&content).context("parsing legacy project_memory.json")?;
        debug!(
            "migrating {} legacy project memory entries to .md",
            entries.len()
        );
        for entry in &entries {
            let md = render_md(entry)?;
            atomic_write(&self.entry_path(&entry.id), &md)?;
        }
        let backup = self.legacy_json.with_extension("json.bak");
        std::fs::rename(&self.legacy_json, &backup).with_context(|| {
            format!(
                "backing up {} -> {}",
                self.legacy_json.display(),
                backup.display()
            )
        })?;
        Ok(())
    }

    fn with_manifest<T>(&self, f: impl FnOnce(&Manifest) -> T) -> Result<T> {
        self.ensure_ready()?;
        let borrow: Ref<Option<Manifest>> = self.manifest.borrow();
        let manifest = borrow
            .as_ref()
            .context("manifest not loaded after ensure_ready")?;
        Ok(f(manifest))
    }

    /// Rebuild postings + metas from current cache metas is not enough (need
    /// bodies for terms); on add/update we recompute the changed entry's terms
    /// only, so we thread the entry's content through.
    fn upsert_into_manifest(&self, entry: &ProjectMemoryEntry) {
        let mut borrow = self.manifest.borrow_mut();
        let manifest = borrow.get_or_insert_with(Manifest::default);
        // Drop any existing postings/meta for this id.
        remove_id_from_postings(&mut manifest.postings, &entry.id);
        manifest.metas.retain(|m| m.id != entry.id);
        let meta = MemoryMeta::from_entry(entry);
        for term in entry_terms(&meta, &entry.content) {
            manifest
                .postings
                .entry(term)
                .or_default()
                .push(entry.id.clone());
        }
        manifest.metas.push(meta);
        manifest.metas.sort_by(|a, b| a.id.cmp(&b.id));
    }

    fn drop_from_manifest(&self, id: &str) {
        let mut borrow = self.manifest.borrow_mut();
        if let Some(manifest) = borrow.as_mut() {
            remove_id_from_postings(&mut manifest.postings, id);
            manifest.metas.retain(|m| m.id != id);
        }
    }

    fn save_cached_manifest(&self) -> Result<()> {
        let borrow = self.manifest.borrow();
        if let Some(manifest) = borrow.as_ref() {
            self.persist_manifest(manifest)?;
        }
        Ok(())
    }

    // --- public API ----------------------------------------------------------

    pub fn next_id(&self) -> Result<String> {
        self.ensure_ready()?;
        let max = self.with_manifest(|m| {
            m.metas
                .iter()
                .filter_map(|e| {
                    e.id.strip_prefix("mem_")
                        .and_then(|n| n.parse::<u32>().ok())
                })
                .max()
                .unwrap_or(0)
        })?;
        Ok(format!("mem_{:04}", max + 1))
    }

    pub fn add(&self, entry: ProjectMemoryEntry) -> Result<ProjectMemoryEntry> {
        self.ensure_ready()?;
        let md = render_md(&entry)?;
        atomic_write(&self.entry_path(&entry.id), &md)?;
        self.upsert_into_manifest(&entry);
        self.save_cached_manifest()?;
        debug!("added project memory {}", entry.id);
        Ok(entry)
    }

    pub fn get(&self, id: &str) -> Result<ProjectMemoryEntry> {
        self.ensure_ready()?;
        let path = self.entry_path(id);
        if !path.exists() {
            anyhow::bail!("memory entry not found: {}", id);
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        parse_md(&text)
    }

    pub fn update(
        &self,
        id: &str,
        f: impl FnOnce(&mut ProjectMemoryEntry),
    ) -> Result<ProjectMemoryEntry> {
        let mut entry = self.get(id)?;
        f(&mut entry);
        entry.updated_at = chrono::Utc::now().to_rfc3339();
        let md = render_md(&entry)?;
        atomic_write(&self.entry_path(id), &md)?;
        self.upsert_into_manifest(&entry);
        self.save_cached_manifest()?;
        Ok(entry)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        self.ensure_ready()?;
        let path = self.entry_path(id);
        if !path.exists() {
            anyhow::bail!("memory entry not found: {}", id);
        }
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        self.drop_from_manifest(id);
        self.save_cached_manifest()?;
        Ok(())
    }

    pub fn archive(&self, id: &str) -> Result<ProjectMemoryEntry> {
        self.update(id, |e| e.status = MemoryStatus::Archived)
    }

    /// All entry metadata (no bodies) — cheap, for listing/filtering.
    pub fn list_meta(&self) -> Result<Vec<MemoryMeta>> {
        self.with_manifest(|m| m.metas.clone())
    }

    /// Candidate entry ids for a query: union of postings for each query token,
    /// including prefix matches (so "auth" reaches "authentication"). This is a
    /// recall filter; precise scoring happens on the loaded bodies.
    pub fn candidate_ids(&self, query: &str) -> Result<Vec<String>> {
        let q_terms = tokenize(query);
        self.with_manifest(|m| {
            let mut ids: HashSet<String> = HashSet::new();
            for qt in &q_terms {
                for (term, posting) in &m.postings {
                    if term == qt || term.starts_with(qt.as_str()) {
                        ids.extend(posting.iter().cloned());
                    }
                }
            }
            ids.into_iter().collect()
        })
    }

    /// Load full entries (bodies) for the given ids, skipping any that vanished.
    pub fn load_bodies(&self, ids: &[String]) -> Result<Vec<ProjectMemoryEntry>> {
        self.ensure_ready()?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            match self.get(id) {
                Ok(e) => out.push(e),
                Err(e) => warn!("candidate {} could not be loaded: {}", id, e),
            }
        }
        Ok(out)
    }

    /// Load every entry with its body. O(N) reads — for export/migration and
    /// maintenance ops (conflict detection), not for hot retrieval paths.
    pub fn load_all(&self) -> Result<Vec<ProjectMemoryEntry>> {
        self.ensure_ready()?;
        let ids =
            self.with_manifest(|m| m.metas.iter().map(|e| e.id.clone()).collect::<Vec<_>>())?;
        self.load_bodies(&ids)
    }
}

fn remove_id_from_postings(postings: &mut HashMap<String, Vec<String>>, id: &str) {
    postings.retain(|_, ids| {
        ids.retain(|i| i != id);
        !ids.is_empty()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "kungfu-pmem-test-{}-{}-{:p}",
            tag,
            std::process::id(),
            &tag as *const _
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(id: &str, title: &str, content: &str, tags: &[&str]) -> ProjectMemoryEntry {
        ProjectMemoryEntry {
            id: id.to_string(),
            kind: ProjectMemoryKind::Decision,
            title: Some(title.to_string()),
            content: content.to_string(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            related_files: vec!["crates/foo/src/lib.rs".to_string()],
            related_symbols: vec!["do_thing".to_string()],
            pinned: false,
            status: MemoryStatus::Active,
            supersedes: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn md_round_trip_preserves_all_fields() {
        let e = entry(
            "mem_0001",
            "Title: with colon",
            "Line one\nLine two\n",
            &["a", "b"],
        );
        let md = render_md(&e).unwrap();
        assert!(md.starts_with("+++\n"));
        let back = parse_md(&md).unwrap();
        assert_eq!(back.id, e.id);
        assert_eq!(back.title, e.title);
        assert_eq!(back.content, "Line one\nLine two");
        assert_eq!(back.tags, e.tags);
        assert_eq!(back.related_symbols, e.related_symbols);
        assert_eq!(back.kind, e.kind);
    }

    #[test]
    fn add_get_update_remove_round_trip() {
        let store = ProjectMemoryStore::new(&temp_dir("crud"));
        store
            .add(entry("mem_0001", "First", "alpha beta", &["x"]))
            .unwrap();
        store
            .add(entry("mem_0002", "Second", "gamma delta", &["y"]))
            .unwrap();
        assert_eq!(store.next_id().unwrap(), "mem_0003");

        let got = store.get("mem_0001").unwrap();
        assert_eq!(got.content, "alpha beta");

        store
            .update("mem_0001", |e| e.content = "alpha rewritten".to_string())
            .unwrap();
        assert_eq!(store.get("mem_0001").unwrap().content, "alpha rewritten");

        store.remove("mem_0002").unwrap();
        assert!(store.get("mem_0002").is_err());
        assert_eq!(store.list_meta().unwrap().len(), 1);
    }

    #[test]
    fn candidate_ids_recall_and_prefix() {
        let store = ProjectMemoryStore::new(&temp_dir("cand"));
        store
            .add(entry(
                "mem_0001",
                "Auth",
                "authentication and rate limiting",
                &["security"],
            ))
            .unwrap();
        store
            .add(entry("mem_0002", "Cache", "the caching layer", &["perf"]))
            .unwrap();

        // Prefix: "auth" should reach "authentication".
        let c = store.candidate_ids("auth").unwrap();
        assert!(c.contains(&"mem_0001".to_string()));
        assert!(!c.contains(&"mem_0002".to_string()));

        // Tag term indexed.
        let c2 = store.candidate_ids("perf").unwrap();
        assert!(c2.contains(&"mem_0002".to_string()));
    }

    #[test]
    fn manifest_rebuilds_from_md_when_deleted() {
        let dir = temp_dir("rebuild");
        let store = ProjectMemoryStore::new(&dir);
        store
            .add(entry("mem_0001", "One", "content one", &["t"]))
            .unwrap();
        store
            .add(entry("mem_0002", "Two", "content two", &["t"]))
            .unwrap();

        // Wipe the manifest + drop the cache; the store must rebuild from .md.
        std::fs::remove_file(dir.join("memory").join("manifest.json")).unwrap();
        let store2 = ProjectMemoryStore::new(&dir);
        assert_eq!(store2.list_meta().unwrap().len(), 2);
        assert!(store2.candidate_ids("content").unwrap().len() == 2);
    }

    #[test]
    fn migrates_legacy_json_non_destructively() {
        let dir = temp_dir("migrate");
        let legacy = dir.join("project_memory.json");
        let entries = vec![
            entry("mem_0001", "Legacy one", "legacy body one", &["old"]),
            entry("mem_0002", "Legacy two", "legacy body two", &["old"]),
        ];
        std::fs::write(&legacy, serde_json::to_string(&entries).unwrap()).unwrap();

        let store = ProjectMemoryStore::new(&dir);
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 2);
        assert!(dir.join("memory").join("mem_0001.md").exists());
        // Old file preserved as backup, not deleted.
        assert!(!legacy.exists());
        assert!(dir.join("project_memory.json.bak").exists());
    }
}
