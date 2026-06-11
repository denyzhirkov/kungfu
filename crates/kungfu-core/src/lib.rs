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

mod ask;
mod debug;
mod edit;
mod embeddings;
mod explore;
mod export;
mod helpers;
mod history;
mod memory;
mod onboard;
mod review;
mod search_ops;
mod types;

pub use debug::{DebugTraceResult, TraceFrame};
pub use embeddings::{EmbeddingsBuildResult, EmbeddingsStatus};
pub use export::ExportStats;

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
        self.store.invalidate();
        let mut indexer =
            Indexer::new(&self.project.root, self.project.config.clone(), &self.store);
        indexer.index_incremental()
    }

    /// Reindex only the given files. Agent-driven freshness: the editor knows exactly
    /// which files it touched, so it tells us instead of us guessing via mtime scans.
    /// Accepts paths relative to the project root or absolute ones under it.
    pub fn index_paths(&self, paths: &[String]) -> Result<kungfu_index::indexer::IndexStats> {
        if paths.is_empty() {
            bail!("no paths given — pass the files you changed, or run a full/incremental index");
        }
        let root = &self.project.root;
        let rels: Vec<String> = paths
            .iter()
            .map(|p| {
                let path = std::path::Path::new(p);
                let rel = path.strip_prefix(root).unwrap_or(path);
                rel.to_string_lossy().trim_start_matches("./").to_string()
            })
            .collect();
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
            });
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

    /// Record a tool/command call for persistent usage stats.
    pub fn track_call(&self, command: &str, bytes: usize) {
        let mut stats = kungfu_types::stats::UsageStats::load(&self.project.kungfu_dir);
        stats.record(command, bytes as u64);
        let _ = stats.save(&self.project.kungfu_dir);
    }

    /// Load persistent usage stats.
    pub fn usage_stats(&self) -> Result<kungfu_types::stats::UsageStats> {
        Ok(kungfu_types::stats::UsageStats::load(
            &self.project.kungfu_dir,
        ))
    }
}
