#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use anyhow::{bail, Result};
use kungfu_index::Indexer;
use kungfu_project::Project;
use kungfu_search::{SearchEngine, SearchResult};
use kungfu_storage::JsonStore;
use kungfu_types::budget::Budget;
use kungfu_types::file::FileEntry;
use kungfu_types::symbol::Symbol;
use std::collections::HashMap;
use std::path::Path;
use tracing::info;

mod annotate;
mod ask;
mod debug;
mod edit;
mod embeddings;
mod explore;
mod export;
mod glossary;
mod helpers;
mod history;
mod memory;
mod onboard;
mod review;
mod search_ops;
mod types;
mod verify;

pub use annotate::{AnnotateResult, AnnotationQueue, AnnotationQueueItem};
pub use debug::{DebugTraceResult, TraceFrame};
pub use embeddings::{EmbeddingsBuildResult, EmbeddingsStatus};
pub use export::ExportStats;
pub use glossary::GlossaryEntry;
pub use memory::MemoryDoctor;
pub use search_ops::EmptyCallGraphCause;

pub use ask::StrategyWeights;
pub use types::*;

pub struct KungfuService {
    pub(crate) project: Project,
    pub(crate) store: JsonStore,
}

impl KungfuService {
    pub fn open(start_dir: &Path) -> Result<Self> {
        let project = Project::open(start_dir)?;
        let store = JsonStore::new(&project.index_dir());
        Ok(Self { project, store })
    }

    pub fn config(&self) -> &kungfu_config::KungfuConfig {
        &self.project.config
    }

    pub(crate) fn store(&self) -> &JsonStore {
        &self.store
    }

    pub(crate) fn search(&self) -> SearchEngine<'_> {
        SearchEngine::new(&self.store)
    }

    /// Resolve Budget::Auto to a concrete budget based on project size.
    pub fn resolve_budget(&self, budget: Budget) -> Budget {
        if budget != Budget::Auto {
            return budget;
        }
        let file_count = self.store().load_files().map(|f| f.len()).unwrap_or(0);
        budget.resolve(file_count)
    }

    /// Whether the on-disk index was written by the current schema. A mismatch
    /// (or a pre-versioning index) means the persisted semantics changed —
    /// the migration path is a full re-index.
    fn index_schema_current(&self) -> bool {
        self.store.load_schema_version() == Some(kungfu_storage::INDEX_SCHEMA_VERSION)
    }

    /// Check if index is stale and auto-reindex if needed.
    /// Compares fingerprints.json mtime with project files.
    pub fn ensure_fresh_index(&self) -> Result<bool> {
        let fp_path = self.project.index_dir().join("fingerprints.json");
        if !fp_path.exists() {
            // No index at all — full index needed
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

        // Sample a few key project files for staleness check (fast heuristic)
        let root = &self.project.root;
        let markers = [
            "Cargo.toml",
            "package.json",
            "go.mod",
            "pyproject.toml",
            "Cargo.lock",
            "package-lock.json",
            "bun.lock",
        ];
        let mut stale = false;
        for marker in &markers {
            let p = root.join(marker);
            if p.exists() {
                if let Ok(meta) = std::fs::metadata(&p) {
                    if let Ok(mtime) = meta.modified() {
                        if mtime > fp_mtime {
                            stale = true;
                            break;
                        }
                    }
                }
            }
        }

        // Also check src/ directory for any file newer than index
        if !stale {
            let src_dirs = [
                "src", "crates", "packages", "lib", "app", "server", "client",
            ];
            'outer: for dir in &src_dirs {
                let d = root.join(dir);
                if d.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(&d) {
                        for entry in entries.take(20).flatten() {
                            if let Ok(meta) = entry.metadata() {
                                if let Ok(mtime) = meta.modified() {
                                    if mtime > fp_mtime {
                                        stale = true;
                                        break 'outer;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if stale {
            info!("index is stale, running incremental reindex");
            self.index_incremental()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Version of the binary that last wrote the index (`store_meta.json`
    /// stamp), if any. Used by `kungfu doctor` for version-coherence checks.
    pub fn index_store_version(&self) -> Option<String> {
        self.store().load_store_version()
    }

    pub fn status(&self) -> Result<StatusInfo> {
        let store = self.store();
        let files = store.load_files()?;
        let symbols = store.load_symbols()?;

        let mut languages: HashMap<String, usize> = HashMap::new();
        for f in &files {
            if let Some(ref lang) = f.language {
                *languages.entry(lang.clone()).or_default() += 1;
            }
        }

        Ok(StatusInfo {
            project_name: self.project.meta.name.clone(),
            root: self.project.root.to_string_lossy().to_string(),
            indexed_files: files.len(),
            indexed_symbols: symbols.len(),
            languages,
            has_git: kungfu_git::is_git_repo(&self.project.root),
        })
    }

    pub fn index_full(&self) -> Result<kungfu_index::indexer::IndexStats> {
        self.store.invalidate();
        let mut indexer =
            Indexer::new(&self.project.root, self.project.config.clone(), &self.store);
        indexer.index_full()
    }

    pub fn index_incremental(&self) -> Result<kungfu_index::indexer::IndexStats> {
        // An incremental run merges into the existing index; if that index was
        // written under an older schema, merging would mix semantics — rebuild.
        if !self.index_schema_current() {
            info!("index schema is outdated, upgrading via full reindex");
            return self.index_full();
        }
        self.store.invalidate();
        let mut indexer =
            Indexer::new(&self.project.root, self.project.config.clone(), &self.store);
        indexer.index_incremental()
    }

    /// Reindex only the given files. Agent-driven freshness: the editor knows exactly
    /// which files it touched, so it tells us instead of us guessing via mtime scans.
    /// Accepts paths relative to the project root or absolute ones under it; a
    /// path outside the project is an error naming it, and nothing is indexed.
    pub fn index_paths(&self, paths: &[String]) -> Result<kungfu_index::indexer::IndexStats> {
        if paths.is_empty() {
            bail!("no paths given — pass the files you changed, or run a full/incremental index");
        }
        let root = &self.project.root;
        let rels = paths
            .iter()
            .map(|p| {
                project_relative_path(root, Path::new(p)).ok_or_else(|| {
                    anyhow::anyhow!(
                        "{p} is outside the project root {} — pass a path under it (absolute, or relative to the root); nothing was reindexed",
                        root.display()
                    )
                })
            })
            .collect::<Result<Vec<String>>>()?;
        if !self.index_schema_current() {
            info!("index schema is outdated, upgrading via full reindex");
            return self.index_full();
        }
        self.store.invalidate();
        let mut indexer =
            Indexer::new(&self.project.root, self.project.config.clone(), &self.store);
        indexer.index_only(&rels)
    }

    pub fn index_changed(&self) -> Result<kungfu_index::indexer::IndexStats> {
        if !kungfu_git::is_git_repo(&self.project.root) {
            bail!("--changed requires a git repository");
        }
        let changed = kungfu_git::changed_files(&self.project.root)?;
        if changed.is_empty() {
            return Ok(kungfu_index::indexer::IndexStats {
                total_files: 0,
                new_files: 0,
                changed_files: 0,
                removed_files: 0,
                symbols_extracted: 0,
                call_edges_filtered: 0,
            });
        }
        if !self.index_schema_current() {
            info!("index schema is outdated, upgrading via full reindex");
            return self.index_full();
        }
        self.store.invalidate();
        let mut indexer =
            Indexer::new(&self.project.root, self.project.config.clone(), &self.store);
        indexer.index_only(&changed)
    }

    pub fn find_symbol(&self, query: &str, budget: Budget) -> Result<Vec<SearchResult<Symbol>>> {
        let budget = self.resolve_budget(budget);
        self.search().find_symbol(query, budget)
    }

    pub fn get_symbol(&self, name: &str) -> Result<Option<Symbol>> {
        self.search().get_symbol(name)
    }

    pub fn search_text(&self, query: &str, budget: Budget) -> Result<Vec<SearchResult<FileEntry>>> {
        let budget = self.resolve_budget(budget);
        self.search().search_text(query, budget)
    }

    pub fn find_related(
        &self,
        file_path: &str,
        budget: Budget,
    ) -> Result<Vec<SearchResult<FileEntry>>> {
        let budget = self.resolve_budget(budget);
        self.search().find_related(file_path, budget)
    }

    /// Record a tool/command call for persistent usage stats. `raw_baseline` is the on-disk size
    /// of the source files the result referenced (0 when the caller doesn't compute a baseline).
    pub fn track_call(&self, command: &str, bytes: usize, raw_baseline: usize) {
        let mut stats = kungfu_types::stats::UsageStats::load(&self.project.kungfu_dir);
        stats.record(command, bytes as u64, raw_baseline as u64);
        let _ = stats.save(&self.project.kungfu_dir);
    }

    /// Load persistent usage stats.
    pub fn usage_stats(&self) -> Result<kungfu_types::stats::UsageStats> {
        Ok(kungfu_types::stats::UsageStats::load(
            &self.project.kungfu_dir,
        ))
    }
}

/// `path` (absolute, or relative to `root`) as a `/`-separated path relative to
/// `root`, or `None` when it lies outside the project. Symlinked prefixes
/// (macOS `/tmp` → `/private/tmp`) are resolved on both sides; a deleted file
/// resolves through its parent directory.
fn project_relative_path(root: &Path, path: &Path) -> Option<String> {
    use std::path::Component;

    let joined = root.join(path);
    let canonical_root = root.canonicalize().ok();
    let rel = canonical_root
        .as_deref()
        .and_then(|croot| {
            resolve_existing_prefix(&joined)
                .strip_prefix(croot)
                .ok()
                .map(Path::to_path_buf)
        })
        .or_else(|| joined.strip_prefix(root).ok().map(Path::to_path_buf))?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn resolve_existing_prefix(path: &Path) -> std::path::PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => parent
            .canonicalize()
            .map(|p| p.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::project_relative_path;
    use std::path::{Path, PathBuf};

    fn temp_root(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kungfu-core-relpath-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
        dir
    }

    #[test]
    fn relative_and_absolute_paths_inside_the_root() {
        let root = temp_root("inside");
        let abs = root.join("src/a.rs");
        assert_eq!(
            project_relative_path(&root, &abs).as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(
            project_relative_path(&root, Path::new("src/a.rs")).as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(
            project_relative_path(&root, Path::new("./src/a.rs")).as_deref(),
            Some("src/a.rs")
        );
        // Deleted file: resolved through its existing parent.
        assert_eq!(
            project_relative_path(&root, &root.join("src/gone.rs")).as_deref(),
            Some("src/gone.rs")
        );
    }

    #[test]
    fn paths_outside_the_root_are_rejected() {
        let root = temp_root("outside");
        assert_eq!(
            project_relative_path(&root, Path::new("/elsewhere/a.rs")),
            None
        );
        assert_eq!(
            project_relative_path(&root, Path::new("../other/a.rs")),
            None
        );
        assert_eq!(
            project_relative_path(&root, Path::new("src/../../x.rs")),
            None
        );
        assert_eq!(project_relative_path(&root, &root), None);
    }

    #[test]
    fn nonexistent_root_falls_back_to_lexical_match() {
        let root = Path::new("/nonexistent-kungfu-root");
        assert_eq!(
            project_relative_path(root, Path::new("/nonexistent-kungfu-root/src/a.rs")).as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(
            project_relative_path(root, Path::new("/elsewhere/a.rs")),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_root_prefix_is_resolved() {
        let root = temp_root("symlink");
        let link = root.with_extension("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&root, &link).unwrap();
        assert_eq!(
            project_relative_path(&root, &link.join("src/a.rs")).as_deref(),
            Some("src/a.rs")
        );
    }
}
