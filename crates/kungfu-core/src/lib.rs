use anyhow::{bail, Result};
use kungfu_index::Indexer;
use kungfu_project::Project;
use kungfu_rank::{build_context_packet, build_context_packet_full, ScoredSymbol};
use kungfu_search::{SearchEngine, SearchResult};
use kungfu_storage::JsonStore;
use kungfu_types::budget::Budget;
use kungfu_types::context::{ContextPacket, Intent};
use kungfu_types::file::FileEntry;
use kungfu_types::relation::RelationKind;
use kungfu_types::symbol::Symbol;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tracing::info;

/// Tunable weights for ask-context strategy scoring.
/// All values are multipliers or bonuses applied during context selection.
pub struct StrategyWeights {
    /// Strategy B: score multiplier for symbols found via file-text search
    pub file_symbol_score: f64,
    /// Strategy B2: flat score for grep content matches
    pub grep_content_score: f64,
    /// Strategy B3: score multiplier for semantic expansion matches
    pub semantic_score: f64,
    /// Strategy B3: minimum symbol score threshold
    pub semantic_min_score: f64,
    /// Strategy C: sibling score multiplier when keyword-relevant
    pub sibling_relevant_score: f64,
    /// Strategy C: sibling score multiplier when keyword-irrelevant
    pub sibling_irrelevant_score: f64,
    /// Strategy D: score multiplier for related file symbols
    pub related_score: f64,
    /// Bonus: test file proximity
    pub test_bonus: f64,
    /// Bonus: config file proximity
    pub config_bonus: f64,
    /// Bonus: debug-relevant symbol names
    pub debug_bonus: f64,
    /// Bonus: path/directory keyword match
    pub path_match_bonus: f64,
    /// Bonus: recently changed files
    pub changed_file_bonus: f64,
    /// Secondary code language penalty multiplier
    pub secondary_lang_penalty: f64,
}

impl Default for StrategyWeights {
    fn default() -> Self {
        Self {
            file_symbol_score: 0.9,
            grep_content_score: 0.45,
            semantic_score: 0.5,
            semantic_min_score: 0.5,
            sibling_relevant_score: 0.9,
            sibling_irrelevant_score: 0.3,
            related_score: 0.4,
            test_bonus: 0.15,
            config_bonus: 0.15,
            debug_bonus: 0.1,
            path_match_bonus: 0.05,
            changed_file_bonus: 0.3,
            secondary_lang_penalty: 0.85,
        }
    }
}

impl StrategyWeights {
    /// Load weights from environment variables (KUNGFU_W_*), falling back to defaults.
    pub fn from_env() -> Self {
        let mut w = Self::default();
        fn env_f64(key: &str, default: f64) -> f64 {
            std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
        }
        w.file_symbol_score = env_f64("KUNGFU_W_FILE_SYMBOL", w.file_symbol_score);
        w.grep_content_score = env_f64("KUNGFU_W_GREP", w.grep_content_score);
        w.semantic_score = env_f64("KUNGFU_W_SEMANTIC", w.semantic_score);
        w.semantic_min_score = env_f64("KUNGFU_W_SEMANTIC_MIN", w.semantic_min_score);
        w.sibling_relevant_score = env_f64("KUNGFU_W_SIBLING_REL", w.sibling_relevant_score);
        w.sibling_irrelevant_score = env_f64("KUNGFU_W_SIBLING_IRREL", w.sibling_irrelevant_score);
        w.related_score = env_f64("KUNGFU_W_RELATED", w.related_score);
        w.test_bonus = env_f64("KUNGFU_W_TEST", w.test_bonus);
        w.config_bonus = env_f64("KUNGFU_W_CONFIG", w.config_bonus);
        w.debug_bonus = env_f64("KUNGFU_W_DEBUG", w.debug_bonus);
        w.path_match_bonus = env_f64("KUNGFU_W_PATH", w.path_match_bonus);
        w.changed_file_bonus = env_f64("KUNGFU_W_CHANGED", w.changed_file_bonus);
        w.secondary_lang_penalty = env_f64("KUNGFU_W_LANG_PENALTY", w.secondary_lang_penalty);
        w
    }
}

pub struct KungfuService {
    project: Project,
    store: JsonStore,
}

pub struct StatusInfo {
    pub project_name: String,
    pub root: String,
    pub indexed_files: usize,
    pub indexed_symbols: usize,
    pub languages: HashMap<String, usize>,
    pub has_git: bool,
}

pub struct RepoOutline {
    pub project_name: String,
    pub total_files: usize,
    pub total_symbols: usize,
    pub languages: HashMap<String, usize>,
    pub top_dirs: Vec<DirEntry>,
    pub entrypoints: Vec<String>,
}

pub struct DirEntry {
    pub path: String,
    pub file_count: usize,
}

pub struct FileOutline {
    pub path: String,
    pub language: Option<String>,
    pub symbols: Vec<SymbolOutline>,
}

pub struct SymbolOutline {
    pub name: String,
    pub kind: String,
    pub signature: Option<String>,
    pub line: usize,
    pub exported: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OnboardInfo {
    pub project_name: String,
    pub languages: Vec<(String, usize)>,
    pub primary_language: Option<String>,
    pub architecture: String,
    pub top_dirs: Vec<(String, usize)>,
    pub entrypoints: Vec<String>,
    pub key_symbols: Vec<String>,
    pub naming_style: String,
    pub test_pattern: String,
    pub total_files: usize,
    pub total_symbols: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AffectedEntry {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub depth: usize,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AffectedResult {
    pub symbol: String,
    pub entries: Vec<AffectedEntry>,
    pub test_files: Vec<String>,
    pub risk: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SmartTestEntry {
    pub test_name: String,
    pub test_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SmartTestResult {
    pub changed_symbols: Vec<String>,
    pub tests: Vec<SmartTestEntry>,
    pub total_tests_in_project: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ReviewResult {
    pub changed_files: Vec<String>,
    pub changed_symbols: Vec<String>,
    pub missing_co_changes: Vec<String>,
    pub untested_changes: Vec<String>,
    pub risk: String,
    pub summary: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CouplingEntry {
    pub path: String,
    pub fan_in: usize,
    pub fan_out: usize,
    pub co_change_count: usize,
    pub risk_score: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HotspotEntry {
    pub name: String,
    pub path: String,
    pub lines: usize,
    pub kind: Option<String>,
    pub churn: Option<usize>,
    pub score: f64,
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

    fn store(&self) -> &JsonStore {
        &self.store
    }

    fn search(&self) -> SearchEngine<'_> {
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
        let markers = ["Cargo.toml", "package.json", "go.mod", "pyproject.toml", "Cargo.lock", "package-lock.json", "bun.lock"];
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
            let src_dirs = ["src", "crates", "packages", "lib", "app", "server", "client"];
            'outer: for dir in &src_dirs {
                let d = root.join(dir);
                if d.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(&d) {
                        for entry in entries.take(20) {
                            if let Ok(entry) = entry {
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
        let mut indexer = Indexer::new(&self.project.root, self.project.config.clone(), &self.store);
        indexer.index_full()
    }

    pub fn index_incremental(&self) -> Result<kungfu_index::indexer::IndexStats> {
        self.store.invalidate();
        let mut indexer = Indexer::new(&self.project.root, self.project.config.clone(), &self.store);
        indexer.index_incremental()
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
        let mut indexer = Indexer::new(&self.project.root, self.project.config.clone(), &self.store);
        indexer.index_only(&changed)
    }

    pub fn repo_outline(&self, budget: Budget) -> Result<RepoOutline> {
        let budget = self.resolve_budget(budget);
        let store = self.store();
        let files = store.load_files()?;
        let symbols = store.load_symbols()?;

        let mut languages: HashMap<String, usize> = HashMap::new();
        let mut dirs: HashMap<String, usize> = HashMap::new();

        for f in &files {
            if let Some(ref lang) = f.language {
                *languages.entry(lang.clone()).or_default() += 1;
            }
            if let Some(dir) = Path::new(&f.path).parent() {
                let dir_str = dir.to_string_lossy().to_string();
                if !dir_str.is_empty() {
                    // Get top-level directory
                    let top = dir_str.split('/').next().unwrap_or(&dir_str).to_string();
                    *dirs.entry(top).or_default() += 1;
                }
            }
        }

        let mut top_dirs: Vec<DirEntry> = dirs
            .into_iter()
            .map(|(path, file_count)| DirEntry { path, file_count })
            .collect();
        top_dirs.sort_by(|a, b| b.file_count.cmp(&a.file_count));
        top_dirs.truncate(budget.top_k() * 2);

        // Detect entrypoints
        let entrypoints: Vec<String> = files
            .iter()
            .filter(|f| {
                let p = &f.path;
                p.ends_with("main.rs")
                    || p.ends_with("lib.rs")
                    || p.ends_with("index.ts")
                    || p.ends_with("index.js")
                    || p.ends_with("main.py")
                    || p.ends_with("main.go")
                    || p.ends_with("app.ts")
                    || p.ends_with("app.js")
                    || p == "package.json"
                    || p == "Cargo.toml"
                    || p == "go.mod"
                    || p == "pyproject.toml"
            })
            .map(|f| f.path.clone())
            .collect();

        Ok(RepoOutline {
            project_name: self.project.meta.name.clone(),
            total_files: files.len(),
            total_symbols: symbols.len(),
            languages,
            top_dirs,
            entrypoints,
        })
    }

    pub fn file_outline(&self, file_path: &str) -> Result<FileOutline> {
        let search = self.search();
        let files = search.get_all_files()?;

        let file = files
            .iter()
            .find(|f| f.path == file_path || f.path.ends_with(file_path))
            .ok_or_else(|| anyhow::anyhow!("file not found in index: {}", file_path))?;

        let symbols = search.get_symbols_for_file(&file.path)?;

        let outlines = symbols
            .iter()
            .map(|s| SymbolOutline {
                name: s.name.clone(),
                kind: s.kind.to_string(),
                signature: s.signature.clone(),
                line: s.span.start_line,
                exported: s.exported,
            })
            .collect();

        Ok(FileOutline {
            path: file.path.clone(),
            language: file.language.clone(),
            symbols: outlines,
        })
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

    pub fn find_related(&self, file_path: &str, budget: Budget) -> Result<Vec<SearchResult<FileEntry>>> {
        let budget = self.resolve_budget(budget);
        self.search().find_related(file_path, budget)
    }

    pub fn context(&self, query: &str, budget: Budget) -> Result<ContextPacket> {
        let budget = self.resolve_budget(budget);
        let search = self.search();
        let query_lower = query.to_lowercase();
        let words: Vec<&str> = query_lower.split_whitespace().collect();

        // Search symbols
        let symbol_results = search.find_symbol(query, Budget::Full)?;

        let mut scored_symbols: Vec<(Symbol, f64)> = symbol_results
            .into_iter()
            .map(|r| (r.item, r.score))
            .collect();

        // Also search files and pull in their symbols for broader context
        let file_results = search.search_text(query, Budget::Full)?;
        let all_symbols = search.get_all_symbols()?;
        let seen_ids: std::collections::HashSet<String> =
            scored_symbols.iter().map(|(s, _)| s.id.clone()).collect();

        for fr in &file_results {
            let file_syms: Vec<_> = all_symbols
                .iter()
                .filter(|s| s.file_id == fr.item.id && !seen_ids.contains(&s.id))
                .collect();
            for sym in file_syms {
                scored_symbols.push((sym.clone(), fr.score * 0.7));
            }
        }

        // Query-aware bonuses
        let wants_tests = kungfu_search::query_wants_tests(&words);
        let wants_config = kungfu_search::query_wants_config(&words);

        for (sym, score) in &mut scored_symbols {
            // Test proximity bonus
            if wants_tests
                && (sym.path.contains("test")
                    || sym.path.contains("spec")
                    || sym.path.contains("tests/"))
            {
                *score += 0.15;
            }

            // Config proximity bonus
            if wants_config
                && (sym.path.ends_with(".toml")
                    || sym.path.ends_with(".json")
                    || sym.path.ends_with(".yaml")
                    || sym.path.ends_with(".yml")
                    || sym.path.contains("config"))
            {
                *score += 0.15;
            }
        }

        // Changed-file bonus: boost symbols from git-changed files
        if kungfu_git::is_git_repo(&self.project.root) {
            if let Ok(changed) = kungfu_git::changed_files(&self.project.root) {
                if !changed.is_empty() {
                    for (sym, score) in &mut scored_symbols {
                        let is_changed = changed
                            .iter()
                            .any(|c| sym.path.ends_with(c) || c.ends_with(&sym.path));
                        if is_changed {
                            *score += 0.2;
                        }
                    }
                }
            }
        }

        let mut packet = build_context_packet(query, scored_symbols, budget);

        let snippet_lines = budget.max_lines();
        if snippet_lines > 0 {
            self.fill_snippets(&mut packet, snippet_lines, &[]);
        }

        Ok(packet)
    }

    /// High-level context retrieval: parse intent, run multi-strategy search,
    /// rank with contextual signals, return compact packet.
    pub fn ask_context(&self, task: &str, budget: Budget) -> Result<ContextPacket> {
        self.ask_context_with_weights(task, budget, &StrategyWeights::from_env())
    }

    pub fn ask_context_with_weights(&self, task: &str, budget: Budget, w: &StrategyWeights) -> Result<ContextPacket> {
        let budget = self.resolve_budget(budget);
        let query_lower = task.to_lowercase();
        let words: Vec<&str> = query_lower.split_whitespace().collect();

        // 1. Detect intent
        let intent = detect_intent(&words);

        // 2. Extract search terms (filter out stop/intent words)
        let keywords: Vec<&str> = words
            .iter()
            .filter(|w| !is_stop_word(w))
            .copied()
            .collect();
        let keyword_query = keywords.join(" ");

        let search = self.search();
        let store = self.store();

        // 3. Determine primary language for weighting
        let files = store.load_files()?;
        let primary_lang = detect_primary_language(&files);

        // 4. Multi-strategy search
        let mut scored_symbols: Vec<ScoredSymbol> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();

        // Strategy A: symbol search
        let sym_results = search.find_symbol(&keyword_query, Budget::Full)?;
        for r in sym_results {
            seen_ids.insert(r.item.id.clone());
            scored_symbols.push(ScoredSymbol {
                symbol: r.item,
                score: r.score,
                reason: "symbol name match".to_string(),
            });
        }

        // Strategy B: text/file search — only add keyword-relevant symbols
        let file_results = search.search_text(&keyword_query, Budget::Full)?;
        let all_symbols = search.get_all_symbols()?;
        for fr in &file_results {
            let file_syms: Vec<_> = all_symbols
                .iter()
                .filter(|s| s.file_id == fr.item.id && !seen_ids.contains(&s.id))
                .filter(|s| {
                    let name_lower = s.name.to_lowercase();
                    let sig_lower = s.signature.as_deref().unwrap_or("").to_lowercase();
                    keywords
                        .iter()
                        .any(|kw| name_lower.contains(*kw) || sig_lower.contains(*kw))
                })
                .take(3)
                .collect();
            for sym in file_syms {
                seen_ids.insert(sym.id.clone());
                scored_symbols.push(ScoredSymbol {
                    symbol: sym.clone(),
                    score: fr.score * w.file_symbol_score,
                    reason: format!("in matched file {}", fr.item.path),
                });
            }
        }

        // Strategy B2: content grep — search file bodies for keywords
        if scored_symbols.len() < budget.top_k() {
            let content_matches = self.grep_content(&keywords, &seen_ids, budget.top_k());
            for (sym, matched_line) in content_matches {
                seen_ids.insert(sym.id.clone());
                scored_symbols.push(ScoredSymbol {
                    symbol: sym,
                    score: w.grep_content_score,
                    reason: format!("content match: {}", matched_line),
                });
            }
        }

        // Strategy B3: semantic expansion — search with conceptually related terms
        if scored_symbols.len() < budget.top_k() {
            let expanded = kungfu_search::expand_query(&keywords);
            // Only use new terms (not original keywords)
            let new_terms: Vec<&str> = expanded
                .iter()
                .filter(|t| !keywords.contains(&t.as_str()))
                .map(|t| t.as_str())
                .collect();

            if !new_terms.is_empty() {
                let expanded_query = new_terms.join(" ");
                let sem_results = search.find_symbol(&expanded_query, Budget::Full)?;
                for r in sem_results {
                    if seen_ids.contains(&r.item.id) {
                        continue;
                    }
                    // Lower score for semantic matches — they're conceptual, not exact
                    if r.score >= w.semantic_min_score {
                        seen_ids.insert(r.item.id.clone());
                        scored_symbols.push(ScoredSymbol {
                            symbol: r.item,
                            score: r.score * w.semantic_score,
                            reason: "semantic match (related concept)".to_string(),
                        });
                    }
                }
            }
        }

        // Strategy C: sibling symbols from top match's file (important for impact/understand)
        if matches!(intent, Intent::Impact | Intent::Understand) {
            if let Some(top) = scored_symbols
                .iter()
                .filter(|s| s.reason == "symbol name match")
                .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))
            {
                let top_file_id = top.symbol.file_id.clone();
                let top_score = top.score;

                // Add new siblings, scored by keyword relevance
                let mut siblings: Vec<_> = all_symbols
                    .iter()
                    .filter(|s| s.file_id == top_file_id && !seen_ids.contains(&s.id))
                    .map(|s| {
                        let name_lower = s.name.to_lowercase();
                        let sig_lower = s.signature.as_deref().unwrap_or("").to_lowercase();
                        let relevance: usize = keywords
                            .iter()
                            .filter(|kw| name_lower.contains(*kw) || sig_lower.contains(*kw))
                            .count();
                        (s, relevance)
                    })
                    .collect();
                siblings.sort_by(|a, b| {
                    b.1.cmp(&a.1)
                        .then_with(|| b.0.exported.cmp(&a.0.exported))
                });
                // Impact/understand: allow more siblings since we want the full picture
                let max_siblings = if intent == Intent::Impact { 5 } else { 3 };
                for (sym, relevance) in siblings.iter().take(max_siblings) {
                    // Skip keyword-irrelevant siblings for non-Impact intents
                    if *relevance == 0 && intent != Intent::Impact {
                        continue;
                    }
                    seen_ids.insert(sym.id.clone());
                    let score = if *relevance > 0 {
                        top_score * w.sibling_relevant_score
                    } else {
                        top_score * w.sibling_irrelevant_score
                    };
                    scored_symbols.push(ScoredSymbol {
                        symbol: (*sym).clone(),
                        score,
                        reason: "same file as matched symbol".to_string(),
                    });
                }

                // Also boost existing symbols from that file if keyword-relevant
                for s in &mut scored_symbols {
                    if s.symbol.file_id == top_file_id
                        && s.reason != "symbol name match"
                        && s.reason != "same file as matched symbol"
                    {
                        let name_lower = s.symbol.name.to_lowercase();
                        let sig_lower =
                            s.symbol.signature.as_deref().unwrap_or("").to_lowercase();
                        let is_relevant = keywords
                            .iter()
                            .any(|kw| name_lower.contains(*kw) || sig_lower.contains(*kw));
                        if is_relevant && s.score < top_score * w.sibling_relevant_score {
                            s.score = top_score * w.sibling_relevant_score;
                            s.reason = "same file as matched symbol".to_string();
                        }
                    }
                }
            }
        }

        // Strategy D: related files (for impact/debug intents)
        if matches!(intent, Intent::Impact | Intent::Debug) && !file_results.is_empty() {
            let top_file = &file_results[0].item;
            if let Ok(related) = search.find_related(&top_file.path, Budget::Small) {
                for r in related {
                    let rel_syms: Vec<_> = all_symbols
                        .iter()
                        .filter(|s| s.file_id == r.item.id && !seen_ids.contains(&s.id))
                        .take(3)
                        .collect();
                    for sym in rel_syms {
                        seen_ids.insert(sym.id.clone());
                        scored_symbols.push(ScoredSymbol {
                            symbol: sym.clone(),
                            score: r.score * w.related_score,
                            reason: format!("related to {}", top_file.path),
                        });
                    }
                }
            }
        }

        // Strategy D: import chain (for impact intent)
        if intent == Intent::Impact {
            let relations = store.load_relations()?;
            let file_ids: HashSet<String> = file_results.iter().map(|r| r.item.id.clone()).collect();

            for rel in &relations {
                if rel.kind == RelationKind::Imports && file_ids.contains(&rel.target_id) {
                    let importer_syms: Vec<_> = all_symbols
                        .iter()
                        .filter(|s| s.file_id == rel.source_id && !seen_ids.contains(&s.id))
                        .take(1)
                        .collect();
                    for sym in importer_syms {
                        seen_ids.insert(sym.id.clone());
                        scored_symbols.push(ScoredSymbol {
                            symbol: sym.clone(),
                            score: 0.35,
                            reason: "imports affected file".to_string(),
                        });
                    }
                }
            }
        }

        // 4. Apply intent-specific bonuses
        let wants_tests = kungfu_search::query_wants_tests(&words);
        let wants_config = kungfu_search::query_wants_config(&words);

        for s in &mut scored_symbols {
            if wants_tests
                && (s.symbol.path.contains("test")
                    || s.symbol.path.contains("spec")
                    || s.symbol.path.contains("tests/"))
            {
                s.score += w.test_bonus;
            }
            if wants_config
                && (s.symbol.path.ends_with(".toml")
                    || s.symbol.path.ends_with(".json")
                    || s.symbol.path.ends_with(".yaml")
                    || s.symbol.path.contains("config"))
            {
                s.score += w.config_bonus;
            }
            if intent == Intent::Debug {
                let name_lower = s.symbol.name.to_lowercase();
                if name_lower.contains("error")
                    || name_lower.contains("err")
                    || name_lower.contains("handle")
                    || name_lower.contains("validate")
                {
                    s.score += w.debug_bonus;
                }
            }
        }

        // Path/directory boost: if keyword matches a directory or filename, boost those symbols
        for s in &mut scored_symbols {
            let path_lower = s.symbol.path.to_lowercase();
            let path_match = keywords.iter().any(|kw| {
                kw.len() >= 3 && path_lower.split('/').any(|seg| {
                    seg.contains(kw) || seg.trim_end_matches(".ts").trim_end_matches(".js")
                        .trim_end_matches(".rs").trim_end_matches(".py").trim_end_matches(".go")
                        .contains(kw)
                })
            });
            if path_match {
                s.score += w.path_match_bonus;
                if !s.reason.contains("path match") {
                    s.reason = format!("{}, path match", s.reason);
                }
            }
        }

        // File-level fallback: if best symbol score is weak, inject file-level results
        let best_score = scored_symbols.iter().map(|s| s.score).fold(0.0f64, f64::max);
        if best_score < 0.6 {
            for fr in &file_results {
                let path_lower = fr.item.path.to_lowercase();
                let path_match = keywords.iter().any(|kw| kw.len() >= 3 && path_lower.contains(kw));
                if path_match && !seen_ids.contains(&fr.item.id) {
                    // Pick the top exported symbol from this file as representative
                    if let Some(rep) = all_symbols
                        .iter()
                        .filter(|s| s.file_id == fr.item.id && !seen_ids.contains(&s.id))
                        .max_by_key(|s| (s.exported as u8, s.span.end_line - s.span.start_line))
                    {
                        seen_ids.insert(rep.id.clone());
                        scored_symbols.push(ScoredSymbol {
                            symbol: rep.clone(),
                            score: 0.55,
                            reason: format!("file path match: {}", fr.item.path),
                        });
                    }
                }
            }
        }

        // Language importance weighting
        if let Some(ref primary) = primary_lang {
            for s in &mut scored_symbols {
                let sym_lang = &s.symbol.language;
                if sym_lang == primary {
                    // Primary language: no change (×1.0)
                } else if is_code_language(sym_lang) {
                    // Secondary code language: slight penalty
                    s.score *= w.secondary_lang_penalty;
                }
            }
        }

        // Changed-file bonus
        let changed = if kungfu_git::is_git_repo(&self.project.root) {
            kungfu_git::changed_files(&self.project.root).unwrap_or_default()
        } else {
            Vec::new()
        };

        if !changed.is_empty() {
            for s in &mut scored_symbols {
                if changed.iter().any(|c| {
                    s.symbol.path.ends_with(c) || c.ends_with(&s.symbol.path)
                }) {
                    s.score += w.changed_file_bonus;
                    s.reason = format!("{}, recently changed", s.reason);
                }
            }
        }

        // 5. Build packet
        let mut packet = build_context_packet_full(
            task,
            scored_symbols,
            budget,
            Some(intent),
        );

        // 6. Attach changed files list
        packet.changed_files = changed;

        // 7. Extract snippets based on budget
        let snippet_lines = budget.max_lines();
        if snippet_lines > 0 {
            self.fill_snippets(&mut packet, snippet_lines, &keywords);
        }

        // 8. Collect rationale from memory layer
        let memories = store.load_memories().unwrap_or_default();
        if !memories.is_empty() {
            let rationale = kungfu_memory::matcher::match_memories(task, &memories, budget);
            // Build evidence fragments from matched rationale
            let evidence: Vec<kungfu_types::context::EvidenceFragment> = rationale
                .iter()
                .filter(|r| !r.text.is_empty())
                .map(|r| kungfu_types::context::EvidenceFragment {
                    source: r.source.clone(),
                    excerpt: truncate_text(&r.text, 200),
                })
                .collect();
            packet.rationale = rationale;
            packet.evidence = evidence;
        }

        Ok(packet)
    }

    /// Grep file contents for keywords, return matching symbols with the matched line.
    fn grep_content(
        &self,
        keywords: &[&str],
        seen_ids: &HashSet<String>,
        limit: usize,
    ) -> Vec<(Symbol, String)> {
        if keywords.is_empty() {
            return Vec::new();
        }

        let store = self.store();
        let files = store.load_files().unwrap_or_default();
        let symbols = store.load_symbols().unwrap_or_default();

        // Build file_id → symbols map
        let mut file_symbols: HashMap<&str, Vec<&Symbol>> = HashMap::new();
        for sym in &symbols {
            if !seen_ids.contains(&sym.id) {
                file_symbols.entry(sym.file_id.as_str()).or_default().push(sym);
            }
        }

        let mut results: Vec<(Symbol, String)> = Vec::new();

        // Only scan code files
        for f in &files {
            if results.len() >= limit {
                break;
            }

            let lang = f.language.as_deref().unwrap_or("");
            if !matches!(lang, "rust" | "typescript" | "javascript" | "python" | "go") {
                continue;
            }

            let abs_path = self.project.root.join(&f.path);
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Precompute stems for keywords
            let keyword_stems: Vec<Option<String>> = keywords
                .iter()
                .map(|kw| kungfu_search::simple_stem(kw))
                .collect();

            // Check if any keyword appears in file content
            let content_lower = content.to_lowercase();
            let kw_idx = keywords.iter().enumerate().position(|(i, kw)| {
                content_lower.contains(*kw)
                    || keyword_stems[i].as_ref().map_or(false, |s| content_lower.contains(s.as_str()))
            });

            let kw_idx = match kw_idx {
                Some(idx) => idx,
                None => continue,
            };
            let keyword = keywords[kw_idx];
            let stem = keyword_stems[kw_idx].as_deref();

            // Single pass: find matched line and its number
            let mut matched_line = "";
            let mut match_line_num = 0usize;
            for (i, line) in content.lines().enumerate() {
                let ll = line.to_lowercase();
                if ll.contains(keyword) || stem.map_or(false, |s| ll.contains(s)) {
                    matched_line = line.trim();
                    match_line_num = i + 1;
                    break;
                }
            }

            if matched_line.is_empty() {
                continue;
            }

            let snippet = if matched_line.len() > 100 {
                let truncated: String = matched_line.chars().take(100).collect();
                format!("{}...", truncated)
            } else {
                matched_line.to_string()
            };

            // Find the best symbol in this file to attach the match to
            if let Some(file_syms) = file_symbols.get(f.id.as_str()) {

                let best = file_syms
                    .iter()
                    .filter(|s| s.span.start_line <= match_line_num && s.span.end_line >= match_line_num)
                    .min_by_key(|s| s.span.end_line - s.span.start_line) // smallest containing symbol
                    .or_else(|| file_syms.first()); // fallback: first symbol in file

                if let Some(sym) = best {
                    if !seen_ids.contains(&sym.id) {
                        results.push(((*sym).clone(), snippet));
                    }
                }
            }
        }

        results
    }

    /// Fill snippet fields in context packet items by reading source files.
    /// If keywords are provided, extract lines containing those keywords with context.
    /// Falls back to first N lines of symbol if no keyword matches found.
    fn fill_snippets(&self, packet: &mut ContextPacket, max_lines: usize, keywords: &[&str]) {
        let mut file_cache: HashMap<String, Vec<String>> = HashMap::new();
        let all_symbols = self.search().get_all_symbols().unwrap_or_default();

        // Build lookup map for O(1) span resolution
        let span_map: HashMap<(&str, &str), (usize, usize)> = all_symbols
            .iter()
            .map(|s| ((s.path.as_str(), s.name.as_str()), (s.span.start_line, s.span.end_line)))
            .collect();

        for item in &mut packet.items {
            let (start, end) = match span_map.get(&(item.path.as_str(), item.name.as_str())) {
                Some(&s) => s,
                None => continue,
            };

            let lines = file_cache
                .entry(item.path.clone())
                .or_insert_with(|| {
                    let abs_path = self.project.root.join(&item.path);
                    std::fs::read_to_string(&abs_path)
                        .map(|c| c.lines().map(String::from).collect())
                        .unwrap_or_default()
                });

            if lines.is_empty() {
                continue;
            }

            let start_idx = start.saturating_sub(1);
            let end_idx = end.min(lines.len());

            // Try keyword-relevant extraction first
            if !keywords.is_empty() && end_idx > start_idx {
                let relevant = extract_keyword_lines(lines, start_idx, end_idx, keywords, max_lines);
                if !relevant.is_empty() {
                    item.snippet = Some(relevant);
                    continue;
                }
            }

            // Fallback: first max_lines of symbol
            let symbol_len = end_idx - start_idx;
            let take = symbol_len.min(max_lines);
            let snippet: Vec<&str> = lines[start_idx..start_idx + take]
                .iter()
                .map(|s| s.as_str())
                .collect();
            if !snippet.is_empty() {
                item.snippet = Some(snippet.join("\n"));
            }
        }
    }

    /// Composite: explore a symbol — find + detail + related symbols + snippets in one call.
    pub fn explore_symbol(&self, name: &str, budget: Budget) -> Result<serde_json::Value> {
        let budget = self.resolve_budget(budget);
        let search = self.search();

        // 1. Find symbol candidates
        let candidates = search.find_symbol(name, budget)?;

        // 2. Pick best candidate — on tie prefer: definitions > variables, src > test, exported
        let (symbol, score) = if let Some(best) = candidates
            .iter()
            .max_by(|a, b| {
                a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        // Strongest signal: prefer source over test/example paths
                        fn is_non_src(path: &str) -> bool {
                            path.contains("test") || path.contains("example")
                                || path.contains("spec/") || path.contains("fixture")
                                || path.contains("evals/") || path.contains("/bench/")
                                || path.contains("__tests__") || path.contains("__mocks__")
                        }
                        let a_test = is_non_src(&a.item.path);
                        let b_test = is_non_src(&b.item.path);
                        b_test.cmp(&a_test)
                    })
                    .then_with(|| {
                        // Prefer exact case match
                        let a_exact = a.item.name == name;
                        let b_exact = b.item.name == name;
                        a_exact.cmp(&b_exact)
                    })
                    .then_with(|| {
                        // Prefer definition kinds over variables/modules
                        fn kind_rank(sym: &Symbol) -> u8 {
                            match sym.kind {
                                kungfu_types::symbol::SymbolKind::Class => 5,
                                kungfu_types::symbol::SymbolKind::Struct => 5,
                                kungfu_types::symbol::SymbolKind::Trait => 5,
                                kungfu_types::symbol::SymbolKind::Interface => 5,
                                kungfu_types::symbol::SymbolKind::Enum => 4,
                                kungfu_types::symbol::SymbolKind::Function => 3,
                                kungfu_types::symbol::SymbolKind::Method => 3,
                                kungfu_types::symbol::SymbolKind::Impl => 2,
                                kungfu_types::symbol::SymbolKind::Module => 1,
                                _ => 0,
                            }
                        }
                        kind_rank(&a.item).cmp(&kind_rank(&b.item))
                    })
                    .then_with(|| {
                        // Prefer larger symbols (class definition > getter/field)
                        let a_size = a.item.span.end_line.saturating_sub(a.item.span.start_line);
                        let b_size = b.item.span.end_line.saturating_sub(b.item.span.start_line);
                        a_size.cmp(&b_size)
                    })
                    .then_with(|| a.item.exported.cmp(&b.item.exported))
            })
        {
            (best.item.clone(), best.score)
        } else if let Some(sym) = search.get_symbol(name)? {
            (sym, 1.0)
        } else {
            return Ok(serde_json::json!({ "error": format!("Symbol '{}' not found", name) }));
        };

        // 3. File outline for related symbols
        let file_outline = self.file_outline(&symbol.path)?;
        let siblings: Vec<_> = file_outline
            .symbols
            .iter()
            .filter(|s| s.name != symbol.name)
            .take(budget.top_k())
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "kind": s.kind,
                    "line": s.line,
                    "exported": s.exported,
                })
            })
            .collect();

        // 4. Snippet for the primary symbol
        let snippet = self.symbol_snippet(&symbol, 30);

        // 5. Other candidates (if fuzzy matched)
        let other_matches: Vec<_> = candidates
            .iter()
            .filter(|c| c.item.id != symbol.id)
            .take(5)
            .map(|c| {
                serde_json::json!({
                    "name": c.item.name,
                    "kind": c.item.kind.to_string(),
                    "path": c.item.path,
                    "line": c.item.span.start_line,
                    "score": c.score,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "symbol": {
                "name": symbol.name,
                "kind": symbol.kind.to_string(),
                "path": symbol.path,
                "line": symbol.span.start_line,
                "end_line": symbol.span.end_line,
                "signature": symbol.signature,
                "exported": symbol.exported,
                "language": symbol.language,
                "score": score,
            },
            "snippet": snippet,
            "siblings_in_file": siblings,
            "other_matches": other_matches,
        }))
    }

    /// Composite: explore a file — outline + related files + key symbols in one call.
    pub fn explore_file(&self, path: &str, budget: Budget) -> Result<serde_json::Value> {
        let budget = self.resolve_budget(budget);
        // 1. File outline
        let outline = self.file_outline(path)?;

        // 2. Related files
        let related = self.find_related(path, budget).unwrap_or_default();
        let related_files: Vec<_> = related
            .iter()
            .map(|r| {
                serde_json::json!({
                    "path": r.item.path,
                    "language": r.item.language,
                    "score": r.score,
                })
            })
            .collect();

        // 3. Key symbols (exported first, then by line order)
        let mut key_symbols: Vec<_> = outline.symbols.iter().collect();
        key_symbols.sort_by(|a, b| b.exported.cmp(&a.exported).then(a.line.cmp(&b.line)));
        let key_symbols: Vec<_> = key_symbols
            .iter()
            .take(budget.top_k() * 2)
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "kind": s.kind,
                    "signature": s.signature,
                    "line": s.line,
                    "exported": s.exported,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "path": outline.path,
            "language": outline.language,
            "total_symbols": outline.symbols.len(),
            "key_symbols": key_symbols,
            "related_files": related_files,
        }))
    }

    /// Composite: investigate a query — ask_context + diff boost in one call.
    pub fn investigate(&self, query: &str, budget: Budget) -> Result<serde_json::Value> {
        let budget = self.resolve_budget(budget);
        // 1. Main context via ask_context
        let packet = self.ask_context(query, budget)?;

        // 2. Gather diff info if available
        let diff_info = if kungfu_git::is_git_repo(&self.project.root) {
            let changed = kungfu_git::changed_files(&self.project.root).unwrap_or_default();
            if changed.is_empty() {
                None
            } else {
                // Find which changed files overlap with context results
                let relevant_changes: Vec<_> = changed
                    .iter()
                    .filter(|c| {
                        packet.items.iter().any(|item| {
                            item.path.ends_with(c.as_str()) || c.ends_with(&item.path)
                        })
                    })
                    .cloned()
                    .collect();
                Some(serde_json::json!({
                    "total_changed_files": changed.len(),
                    "relevant_changed_files": relevant_changes,
                }))
            }
        } else {
            None
        };

        // 3. Build combined result
        let items: Vec<_> = packet
            .items
            .iter()
            .map(|item| {
                serde_json::json!({
                    "name": item.name,
                    "type": item.item_type,
                    "path": item.path,
                    "score": item.score,
                    "why": item.why,
                    "signature": item.signature,
                    "snippet": item.snippet,
                })
            })
            .collect();

        let mut result = serde_json::json!({
            "query": packet.query,
            "intent": packet.intent.map(|i| format!("{:?}", i)),
            "budget": format!("{:?}", packet.budget),
            "items": items,
        });

        if let Some(diff) = diff_info {
            result.as_object_mut().unwrap().insert("diff".to_string(), diff);
        }

        Ok(result)
    }

    /// Extract a snippet for a single symbol (helper for composite tools).
    fn symbol_snippet(&self, symbol: &Symbol, max_lines: usize) -> Option<String> {
        let abs_path = self.project.root.join(&symbol.path);
        let content = std::fs::read_to_string(&abs_path).ok()?;
        let lines: Vec<&str> = content.lines().collect();

        let start = symbol.span.start_line.saturating_sub(1);
        let end = symbol.span.end_line.min(lines.len());
        if start >= end {
            return None;
        }

        let take = (end - start).min(max_lines);
        let snippet: Vec<&str> = lines[start..start + take].to_vec();
        if snippet.is_empty() {
            None
        } else {
            Some(snippet.join("\n"))
        }
    }

    /// Find all symbols that call the given symbol (callers / "who calls this?").
    pub fn callers(&self, name: &str, budget: Budget) -> Result<Vec<(Symbol, String)>> {
        let budget = self.resolve_budget(budget);
        let store = self.store();
        let relations = store.load_relations()?;
        let all_symbols = self.search().get_all_symbols()?;

        // Find target symbol IDs matching name
        let target_ids: HashSet<&str> = all_symbols
            .iter()
            .filter(|s| s.name == name)
            .map(|s| s.id.as_str())
            .collect();

        if target_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Find Calls relations where target is our symbol
        let caller_ids: Vec<&str> = relations
            .iter()
            .filter(|r| r.kind == RelationKind::Calls && target_ids.contains(r.target_id.as_str()))
            .map(|r| r.source_id.as_str())
            .collect();

        let mut results: Vec<(Symbol, String)> = Vec::new();
        let mut seen = HashSet::new();
        for caller_id in &caller_ids {
            if seen.contains(caller_id) {
                continue;
            }
            if let Some(sym) = all_symbols.iter().find(|s| s.id == *caller_id) {
                seen.insert(*caller_id);
                results.push((sym.clone(), format!("calls {}", name)));
            }
        }

        results.truncate(budget.top_k());
        Ok(results)
    }

    /// Find all symbols that the given symbol calls (callees / "what does this call?").
    pub fn callees(&self, name: &str, budget: Budget) -> Result<Vec<(Symbol, String)>> {
        let budget = self.resolve_budget(budget);
        let store = self.store();
        let relations = store.load_relations()?;
        let all_symbols = self.search().get_all_symbols()?;

        // Find source symbol IDs matching name
        let source_ids: HashSet<&str> = all_symbols
            .iter()
            .filter(|s| s.name == name)
            .map(|s| s.id.as_str())
            .collect();

        if source_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Find Calls relations where source is our symbol
        let callee_ids: Vec<&str> = relations
            .iter()
            .filter(|r| r.kind == RelationKind::Calls && source_ids.contains(r.source_id.as_str()))
            .map(|r| r.target_id.as_str())
            .collect();

        let mut results: Vec<(Symbol, String)> = Vec::new();
        let mut seen = HashSet::new();
        for callee_id in &callee_ids {
            if seen.contains(callee_id) {
                continue;
            }
            if let Some(sym) = all_symbols.iter().find(|s| s.id == *callee_id) {
                seen.insert(*callee_id);
                results.push((sym.clone(), format!("called by {}", name)));
            }
        }

        results.truncate(budget.top_k());
        Ok(results)
    }

    /// Semantic search: expand query with related concepts, then search symbols.
    pub fn semantic_search(&self, query: &str, budget: Budget) -> Result<serde_json::Value> {
        let budget = self.resolve_budget(budget);
        let query_lower = query.to_lowercase();
        let words: Vec<&str> = query_lower.split_whitespace().collect();

        let keywords: Vec<&str> = words
            .iter()
            .filter(|w| !is_stop_word(w))
            .copied()
            .collect();

        let expanded = kungfu_search::expand_query(&keywords);
        let new_terms: Vec<&str> = expanded
            .iter()
            .filter(|t| !keywords.contains(&t.as_str()))
            .map(|t| t.as_str())
            .collect();

        let search = self.search();
        let mut results = Vec::new();
        let mut seen = HashSet::new();

        // Search with original keywords
        let keyword_query = keywords.join(" ");
        for r in search.find_symbol(&keyword_query, Budget::Full)? {
            if seen.insert(r.item.id.clone()) {
                results.push(serde_json::json!({
                    "name": r.item.name,
                    "kind": r.item.kind.to_string(),
                    "path": r.item.path,
                    "line": r.item.span.start_line,
                    "score": r.score,
                    "match_type": "direct",
                }));
            }
        }

        // Search with expanded terms
        if !new_terms.is_empty() {
            let expanded_query = new_terms.join(" ");
            for r in search.find_symbol(&expanded_query, Budget::Full)? {
                if seen.insert(r.item.id.clone()) && r.score >= 0.5 {
                    results.push(serde_json::json!({
                        "name": r.item.name,
                        "kind": r.item.kind.to_string(),
                        "path": r.item.path,
                        "line": r.item.span.start_line,
                        "score": r.score * 0.6,
                        "match_type": "semantic",
                    }));
                }
            }
        }

        // Sort by score and truncate
        results.sort_by(|a, b| {
            b["score"].as_f64().unwrap_or(0.0)
                .partial_cmp(&a["score"].as_f64().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(budget.top_k());

        Ok(serde_json::json!({
            "query": query,
            "keywords": keywords,
            "expanded_terms": new_terms,
            "results": results,
        }))
    }

    /// Get git history for a file: recent commits.
    pub fn file_history(&self, path: &str, max_entries: usize) -> Result<serde_json::Value> {
        if !kungfu_git::is_git_repo(&self.project.root) {
            bail!("not a git repository");
        }
        let entries = kungfu_git::file_log(&self.project.root, path, max_entries)?;
        let items: Vec<_> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "hash": e.hash,
                    "date": e.date,
                    "author": e.author,
                    "message": e.message,
                })
            })
            .collect();
        Ok(serde_json::json!({ "path": path, "commits": items }))
    }

    /// Get git blame for a symbol: who changed its code and why.
    pub fn symbol_history(&self, name: &str) -> Result<serde_json::Value> {
        if !kungfu_git::is_git_repo(&self.project.root) {
            bail!("not a git repository");
        }
        let sym = self.search().get_symbol(name)?;
        let symbol = match sym {
            Some(s) => s,
            None => return Ok(serde_json::json!({ "error": format!("Symbol '{}' not found", name) })),
        };

        let blame = kungfu_git::blame_lines(
            &self.project.root,
            &symbol.path,
            symbol.span.start_line,
            symbol.span.end_line,
        )
        .unwrap_or_default();

        let blame_items: Vec<_> = blame
            .iter()
            .map(|b| {
                serde_json::json!({
                    "hash": b.hash,
                    "author": b.author,
                    "date": b.date,
                    "summary": b.summary,
                })
            })
            .collect();

        let log = kungfu_git::file_log(&self.project.root, &symbol.path, 5).unwrap_or_default();
        let log_items: Vec<_> = log
            .iter()
            .map(|e| {
                serde_json::json!({
                    "hash": e.hash,
                    "date": e.date,
                    "author": e.author,
                    "message": e.message,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "symbol": name,
            "path": symbol.path,
            "lines": format!("{}-{}", symbol.span.start_line, symbol.span.end_line),
            "blame": blame_items,
            "recent_commits": log_items,
        }))
    }

    pub fn search_rationale(&self, query: &str, budget: Budget) -> Result<Vec<kungfu_types::context::RationaleItem>> {
        let budget = self.resolve_budget(budget);
        let memories = self.store().load_memories()?;
        Ok(kungfu_memory::matcher::match_memories(query, &memories, budget))
    }

    pub fn change_timeline(&self, target: &str, budget: Budget) -> Result<Vec<kungfu_types::context::HistoryEvent>> {
        let budget = self.resolve_budget(budget);
        let mut events = Vec::new();

        // Find the file for this target (symbol name or file path)
        let search = self.search();
        let file_path = if let Some(sym) = search.get_symbol(target)? {
            sym.path.clone()
        } else {
            // Try as a file path
            let files = self.store().load_files()?;
            match files.iter().find(|f| f.path.contains(target)) {
                Some(f) => f.path.clone(),
                None => return Ok(events),
            }
        };

        if !kungfu_git::is_git_repo(&self.project.root) {
            return Ok(events);
        }

        // Git log
        let max_entries = match budget {
            Budget::Tiny => 3,
            Budget::Small => 5,
            Budget::Medium => 10,
            _ => 20,
        };
        let log = kungfu_git::file_log(&self.project.root, &file_path, max_entries)
            .unwrap_or_default();

        if let Some(first) = log.last() {
            events.push(kungfu_types::context::HistoryEvent {
                event_type: "introduced".to_string(),
                target: target.to_string(),
                detail: format!("First appeared: {} by {}", first.message, first.author),
                date: Some(first.date.clone()),
            });
        }

        // Churn analysis
        let churn = kungfu_git::file_commit_counts(&self.project.root)
            .unwrap_or_default();
        if let Some(count) = churn.iter().find(|(p, _)| p.contains(&file_path)).map(|(_, c)| *c) {
            let avg = if churn.is_empty() { 1 } else {
                churn.iter().map(|(_, c)| *c).sum::<usize>() / churn.len()
            };
            if count > avg * 2 {
                events.push(kungfu_types::context::HistoryEvent {
                    event_type: "high_churn".to_string(),
                    target: file_path.clone(),
                    detail: format!("{} commits (project avg: {})", count, avg),
                    date: None,
                });
            }
        }

        // Recent changes
        for entry in log.iter().take(3) {
            events.push(kungfu_types::context::HistoryEvent {
                event_type: "recent_change".to_string(),
                target: target.to_string(),
                detail: format!("{}: {}", entry.author, entry.message),
                date: Some(entry.date.clone()),
            });
        }

        // Decision references from memory
        let memories = self.store().load_memories().unwrap_or_default();
        for mem in &memories {
            if mem.kind == kungfu_types::memory::MemoryKind::Decision {
                let related = mem.path == file_path
                    || mem.anchors.iter().any(|a| target.to_lowercase().contains(a));
                if related {
                    events.push(kungfu_types::context::HistoryEvent {
                        event_type: "decision_ref".to_string(),
                        target: mem.path.clone(),
                        detail: mem.text.chars().take(200).collect(),
                        date: None,
                    });
                }
            }
        }

        Ok(events)
    }

    pub fn diff_context(&self, budget: Budget) -> Result<ContextPacket> {
        let budget = self.resolve_budget(budget);
        if !kungfu_git::is_git_repo(&self.project.root) {
            bail!("not a git repository");
        }

        let changed = kungfu_git::changed_files(&self.project.root)?;
        if changed.is_empty() {
            return Ok(ContextPacket {
                query: "diff context".to_string(),
                budget,
                intent: None,
                items: Vec::new(),
                changed_files: Vec::new(),
                rationale: Vec::new(),
                history: Vec::new(),
                evidence: Vec::new(),
            });
        }

        info!("building context for {} changed files", changed.len());

        let search = self.search();
        let all_symbols = search.get_all_symbols()?;

        let scored: Vec<(Symbol, f64)> = all_symbols
            .into_iter()
            .filter_map(|s| {
                let is_changed = changed.iter().any(|c| s.path.ends_with(c) || c.ends_with(&s.path));
                if is_changed {
                    Some((s, 0.9))
                } else {
                    None
                }
            })
            .collect();

        Ok(build_context_packet("diff context", scored, budget))
    }

    /// Record a tool/command call for persistent usage stats.
    pub fn track_call(&self, command: &str, bytes: usize) {
        let mut stats = kungfu_types::stats::UsageStats::load(&self.project.kungfu_dir);
        stats.record(command, bytes as u64);
        let _ = stats.save(&self.project.kungfu_dir);
    }

    /// Load persistent usage stats.
    pub fn usage_stats(&self) -> Result<kungfu_types::stats::UsageStats> {
        Ok(kungfu_types::stats::UsageStats::load(&self.project.kungfu_dir))
    }

    /// Find largest symbols or files, optionally weighted by git churn.
    pub fn hotspots(&self, top: usize, churn: bool, files_mode: bool) -> Result<Vec<HotspotEntry>> {
        self.ensure_fresh_index()?;

        let churn_counts = if churn && kungfu_git::is_git_repo(&self.project.root) {
            kungfu_git::file_commit_counts(&self.project.root).unwrap_or_default()
        } else {
            HashMap::new()
        };

        let mut entries: Vec<HotspotEntry> = if files_mode {
            let files = self.store().load_files()?;
            files
                .into_iter()
                .map(|f| {
                    let lines = f.size as usize;
                    let file_churn = churn_counts.get(&f.path).copied();
                    let score = if churn {
                        lines as f64 * file_churn.unwrap_or(1) as f64
                    } else {
                        lines as f64
                    };
                    HotspotEntry {
                        name: f.path.rsplit('/').next().unwrap_or(&f.path).to_string(),
                        path: f.path,
                        lines,
                        kind: f.language,
                        churn: file_churn,
                        score,
                    }
                })
                .collect()
        } else {
            let symbols = self.store().load_symbols()?;
            symbols
                .into_iter()
                .map(|s| {
                    let lines = s.span.end_line.saturating_sub(s.span.start_line) + 1;
                    let file_churn = churn_counts.get(&s.path).copied();
                    let score = if churn {
                        lines as f64 * file_churn.unwrap_or(1) as f64
                    } else {
                        lines as f64
                    };
                    HotspotEntry {
                        name: s.name,
                        path: s.path,
                        lines,
                        kind: Some(format!("{:?}", s.kind)),
                        churn: file_churn,
                        score,
                    }
                })
                .collect()
        };

        entries.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        entries.truncate(top);
        Ok(entries)
    }

    /// Generate a project onboarding summary: architecture, patterns, entrypoints, naming.
    pub fn onboard(&self) -> Result<OnboardInfo> {
        self.ensure_fresh_index()?;
        let store = self.store();
        let files = store.load_files()?;
        let symbols = store.load_symbols()?;
        let relations = store.load_relations()?;

        // Languages
        let mut lang_counts: HashMap<String, usize> = HashMap::new();
        for f in &files {
            if let Some(ref lang) = f.language {
                *lang_counts.entry(lang.clone()).or_default() += 1;
            }
        }
        let mut languages: Vec<(String, usize)> = lang_counts.into_iter().collect();
        languages.sort_by(|a, b| b.1.cmp(&a.1));
        let primary_language = detect_primary_language(&files);

        // Top directories
        let mut dir_counts: HashMap<String, usize> = HashMap::new();
        for f in &files {
            if let Some(dir) = Path::new(&f.path).parent() {
                let dir_str = dir.to_string_lossy().to_string();
                if !dir_str.is_empty() {
                    let top = dir_str.split('/').next().unwrap_or(&dir_str).to_string();
                    *dir_counts.entry(top).or_default() += 1;
                }
            }
        }
        let mut top_dirs: Vec<(String, usize)> = dir_counts.into_iter().collect();
        top_dirs.sort_by(|a, b| b.1.cmp(&a.1));
        top_dirs.truncate(15);

        // Entrypoints
        let entrypoints: Vec<String> = files
            .iter()
            .filter(|f| {
                let p = &f.path;
                p.ends_with("main.rs") || p.ends_with("lib.rs")
                    || p.ends_with("index.ts") || p.ends_with("index.js")
                    || p.ends_with("main.py") || p.ends_with("main.go")
                    || p.ends_with("app.ts") || p.ends_with("app.js")
                    || p == "package.json" || p == "Cargo.toml"
                    || p == "go.mod" || p == "pyproject.toml"
            })
            .map(|f| f.path.clone())
            .collect();

        // Architecture detection
        let architecture = detect_architecture(&files, &symbols);

        // Key symbols (most connected)
        let mut symbol_connections: HashMap<String, usize> = HashMap::new();
        for r in &relations {
            *symbol_connections.entry(r.source_id.clone()).or_default() += 1;
            *symbol_connections.entry(r.target_id.clone()).or_default() += 1;
        }
        let symbol_map: HashMap<&str, &Symbol> = symbols.iter().map(|s| (s.id.as_str(), s)).collect();
        let mut connected: Vec<(&str, usize)> = symbol_connections.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        connected.sort_by(|a, b| b.1.cmp(&a.1));
        let key_symbols: Vec<String> = connected
            .iter()
            .take(10)
            .filter_map(|(id, _)| symbol_map.get(id).map(|s| {
                if let Some(ref sig) = s.signature {
                    format!("[{}] {}", s.path, sig)
                } else {
                    format!("[{}] {}", s.path, s.name)
                }
            }))
            .collect();

        // Naming style detection
        let naming_style = detect_naming_style(&symbols);

        // Test pattern detection
        let test_pattern = detect_test_pattern(&files);

        Ok(OnboardInfo {
            project_name: self.project.meta.name.clone(),
            languages,
            primary_language,
            architecture,
            top_dirs,
            entrypoints,
            key_symbols,
            naming_style,
            test_pattern,
            total_files: files.len(),
            total_symbols: symbols.len(),
        })
    }

    /// Blast radius analysis: find all transitive callers/dependents of a symbol.
    pub fn affected(&self, name: &str, depth: usize) -> Result<AffectedResult> {
        self.ensure_fresh_index()?;
        let store = self.store();
        let files = store.load_files()?;
        let relations = store.load_relations()?;
        let all_symbols = self.search().get_all_symbols()?;

        // Find target symbol IDs
        let target_ids: HashSet<String> = all_symbols
            .iter()
            .filter(|s| s.name == name)
            .map(|s| s.id.clone())
            .collect();

        if target_ids.is_empty() {
            bail!("symbol '{}' not found", name);
        }

        // Build unified ID → path map (both file and symbol IDs)
        let mut id_to_path: HashMap<&str, &str> = HashMap::new();
        for f in &files {
            id_to_path.insert(&f.id, &f.path);
        }
        let symbol_map: HashMap<&str, &Symbol> = all_symbols.iter().map(|s| {
            id_to_path.insert(&s.id, &s.path);
            (s.id.as_str(), s)
        }).collect();

        // Build reverse dependency graph: target_id → source_ids (Calls + Imports)
        // Only include high-confidence relations (weight >= 0.7) to avoid false positives
        let mut reverse_deps: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
        for r in &relations {
            match r.kind {
                RelationKind::Calls if r.weight >= 0.7 => {
                    reverse_deps.entry(r.target_id.as_str()).or_default().push((r.source_id.as_str(), "calls"));
                }
                RelationKind::Imports => {
                    reverse_deps.entry(r.target_id.as_str()).or_default().push((r.source_id.as_str(), "imports"));
                }
                _ => {}
            }
        }

        // BFS through reverse dependency graph
        let mut entries: Vec<AffectedEntry> = Vec::new();
        let mut visited: HashSet<String> = target_ids.clone();
        let mut frontier: Vec<(String, usize)> = target_ids.iter().map(|id| (id.clone(), 0)).collect();

        // Also collect target file IDs (for file-level relation matching)
        let target_paths: HashSet<&str> = target_ids.iter()
            .filter_map(|id| id_to_path.get(id.as_str()).copied())
            .collect();
        // Add file IDs that map to target paths
        for f in &files {
            if target_paths.contains(f.path.as_str()) && !visited.contains(&f.id) {
                frontier.push((f.id.clone(), 0));
                visited.insert(f.id.clone());
            }
        }

        while let Some((current_id, current_depth)) = frontier.pop() {
            if current_depth >= depth {
                continue;
            }
            if let Some(deps) = reverse_deps.get(current_id.as_str()) {
                for &(dep_id, reason_kind) in deps {
                    if visited.insert(dep_id.to_string()) {
                        let (entry_name, entry_path, entry_kind) = if let Some(sym) = symbol_map.get(dep_id) {
                            (sym.name.clone(), sym.path.clone(), sym.kind.to_string())
                        } else if let Some(&path) = id_to_path.get(dep_id) {
                            let fname = path.rsplit('/').next().unwrap_or(path).to_string();
                            (fname, path.to_string(), "file".to_string())
                        } else {
                            continue;
                        };
                        entries.push(AffectedEntry {
                            name: entry_name,
                            path: entry_path,
                            kind: entry_kind,
                            depth: current_depth + 1,
                            reason: format!("{} {} (depth {})", reason_kind, name, current_depth + 1),
                        });
                        frontier.push((dep_id.to_string(), current_depth + 1));
                    }
                }
            }
        }

        // Find affected test files via TestFor relations (file-level)
        let affected_paths: HashSet<&str> = entries.iter().map(|e| e.path.as_str())
            .chain(target_paths.iter().copied())
            .collect();
        let mut test_files: Vec<String> = Vec::new();
        for r in &relations {
            if r.kind == RelationKind::TestFor {
                let source_path = id_to_path.get(r.source_id.as_str()).copied().unwrap_or("");
                let target_path = id_to_path.get(r.target_id.as_str()).copied().unwrap_or("");
                if !target_path.is_empty() && affected_paths.contains(target_path) {
                    if !source_path.is_empty() && !test_files.contains(&source_path.to_string()) {
                        test_files.push(source_path.to_string());
                    }
                }
            }
        }

        // Risk assessment
        let risk = if entries.len() > 20 || test_files.len() > 5 {
            "HIGH".to_string()
        } else if entries.len() > 5 || test_files.len() > 2 {
            "MEDIUM".to_string()
        } else {
            "LOW".to_string()
        };

        entries.sort_by_key(|e| e.depth);

        Ok(AffectedResult {
            symbol: name.to_string(),
            entries,
            test_files,
            risk,
        })
    }

    /// Find minimal set of tests to run based on git diff.
    pub fn smart_test(&self) -> Result<SmartTestResult> {
        self.ensure_fresh_index()?;
        if !kungfu_git::is_git_repo(&self.project.root) {
            bail!("not a git repository");
        }

        let store = self.store();
        let all_symbols = store.load_symbols()?;
        let relations = store.load_relations()?;

        // Get changed line ranges
        let changed_lines = kungfu_git::diff_changed_lines(&self.project.root)?;
        if changed_lines.is_empty() {
            return Ok(SmartTestResult {
                changed_symbols: vec![],
                tests: vec![],
                total_tests_in_project: count_test_symbols(&all_symbols),
            });
        }

        // Find symbols that overlap with changed lines
        let mut changed_symbols: Vec<String> = Vec::new();
        let mut changed_symbol_ids: HashSet<String> = HashSet::new();
        let mut changed_file_paths: HashSet<String> = HashSet::new();

        for (file_path, ranges) in &changed_lines {
            changed_file_paths.insert(file_path.clone());
            for sym in &all_symbols {
                if sym.path == *file_path {
                    for &(start, end) in ranges {
                        if sym.span.start_line <= end && sym.span.end_line >= start {
                            if changed_symbol_ids.insert(sym.id.clone()) {
                                changed_symbols.push(format!("{}::{}", sym.path, sym.name));
                            }
                        }
                    }
                }
            }
        }

        // Find test symbols via call graph and test_for relations
        let mut test_entries: Vec<SmartTestEntry> = Vec::new();
        let mut seen_tests: HashSet<String> = HashSet::new();

        // Build reverse call graph
        let mut reverse_calls: HashMap<&str, Vec<&str>> = HashMap::new();
        for r in &relations {
            if r.kind == RelationKind::Calls {
                reverse_calls.entry(r.target_id.as_str()).or_default().push(r.source_id.as_str());
            }
        }

        let symbol_map: HashMap<&str, &Symbol> = all_symbols.iter().map(|s| (s.id.as_str(), s)).collect();

        // Direct: test_for relations pointing to changed files
        for r in &relations {
            if r.kind == RelationKind::TestFor {
                if let Some(target_sym) = symbol_map.get(r.target_id.as_str()) {
                    if changed_file_paths.contains(&target_sym.path) {
                        if let Some(source_sym) = symbol_map.get(r.source_id.as_str()) {
                            let key = format!("{}::{}", source_sym.path, source_sym.name);
                            if seen_tests.insert(key) {
                                test_entries.push(SmartTestEntry {
                                    test_name: source_sym.name.clone(),
                                    test_path: source_sym.path.clone(),
                                    reason: format!("test_for {}", target_sym.path),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Call graph: find test functions that call changed symbols (1 hop)
        for changed_id in &changed_symbol_ids {
            if let Some(callers) = reverse_calls.get(changed_id.as_str()) {
                for &caller_id in callers {
                    if let Some(caller) = symbol_map.get(caller_id) {
                        if is_test_symbol(caller) {
                            let key = format!("{}::{}", caller.path, caller.name);
                            if seen_tests.insert(key) {
                                let changed_name = symbol_map.get(changed_id.as_str())
                                    .map(|s| s.name.as_str())
                                    .unwrap_or("?");
                                test_entries.push(SmartTestEntry {
                                    test_name: caller.name.clone(),
                                    test_path: caller.path.clone(),
                                    reason: format!("calls changed symbol {}", changed_name),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Also include test files in the same directory as changed files
        let changed_dirs: HashSet<&str> = changed_file_paths.iter()
            .filter_map(|p| p.rsplit_once('/').map(|(dir, _)| dir))
            .collect();

        for sym in &all_symbols {
            if is_test_symbol(sym) {
                if let Some((dir, _)) = sym.path.rsplit_once('/') {
                    if changed_dirs.contains(dir) {
                        let key = format!("{}::{}", sym.path, sym.name);
                        if seen_tests.insert(key) {
                            test_entries.push(SmartTestEntry {
                                test_name: sym.name.clone(),
                                test_path: sym.path.clone(),
                                reason: "co-located test".to_string(),
                            });
                        }
                    }
                }
            }
        }

        Ok(SmartTestResult {
            changed_symbols,
            tests: test_entries,
            total_tests_in_project: count_test_symbols(&all_symbols),
        })
    }

    /// Code review context: analyze diff for risks, missing co-changes, untested code.
    pub fn review(&self) -> Result<ReviewResult> {
        self.ensure_fresh_index()?;
        if !kungfu_git::is_git_repo(&self.project.root) {
            bail!("not a git repository");
        }

        let store = self.store();
        let all_symbols = store.load_symbols()?;
        let relations = store.load_relations()?;

        // Changed files
        let changed_files = kungfu_git::diff_files(&self.project.root)?;
        if changed_files.is_empty() {
            return Ok(ReviewResult {
                changed_files: vec![],
                changed_symbols: vec![],
                missing_co_changes: vec![],
                untested_changes: vec![],
                risk: "NONE".to_string(),
                summary: "No changes detected".to_string(),
            });
        }

        // Changed symbols
        let changed_lines = kungfu_git::diff_changed_lines(&self.project.root)?;
        let mut changed_symbols: Vec<String> = Vec::new();
        let changed_file_set: HashSet<&str> = changed_files.iter().map(|s| s.as_str()).collect();

        for (file_path, ranges) in &changed_lines {
            for sym in &all_symbols {
                if sym.path == *file_path {
                    for &(start, end) in ranges {
                        if sym.span.start_line <= end && sym.span.end_line >= start {
                            changed_symbols.push(format!("{}::{}", sym.path, sym.name));
                            break;
                        }
                    }
                }
            }
        }

        // Co-change analysis: find files that usually change together
        let co_changes = kungfu_git::co_change_pairs(&self.project.root, 3).unwrap_or_default();
        let mut missing_co_changes: Vec<String> = Vec::new();
        for file in &changed_files {
            if let Some(partners) = co_changes.get(file) {
                for (partner, count) in partners.iter().take(5) {
                    if !changed_file_set.contains(partner.as_str()) {
                        missing_co_changes.push(format!("{} (co-changed {}x with {})", partner, count, file));
                    }
                }
            }
        }
        missing_co_changes.sort();
        missing_co_changes.dedup();

        // Find untested changes
        let symbol_map: HashMap<&str, &Symbol> = all_symbols.iter().map(|s| (s.id.as_str(), s)).collect();
        let mut tested_paths: HashSet<&str> = HashSet::new();
        for r in &relations {
            if r.kind == RelationKind::TestFor {
                if let Some(target) = symbol_map.get(r.target_id.as_str()) {
                    tested_paths.insert(&target.path);
                }
            }
        }

        let untested_changes: Vec<String> = changed_files
            .iter()
            .filter(|f| {
                let is_code = f.ends_with(".rs") || f.ends_with(".ts") || f.ends_with(".js")
                    || f.ends_with(".py") || f.ends_with(".go");
                let is_test = f.contains("test") || f.contains("spec");
                is_code && !is_test && !tested_paths.contains(f.as_str())
            })
            .cloned()
            .collect();

        // Risk assessment
        let risk = if changed_files.len() > 10 || !missing_co_changes.is_empty() && untested_changes.len() > 3 {
            "HIGH"
        } else if changed_files.len() > 5 || !missing_co_changes.is_empty() || !untested_changes.is_empty() {
            "MEDIUM"
        } else {
            "LOW"
        };

        let summary = format!(
            "{} files changed, {} symbols modified, {} missing co-changes, {} untested",
            changed_files.len(), changed_symbols.len(),
            missing_co_changes.len(), untested_changes.len()
        );

        Ok(ReviewResult {
            changed_files,
            changed_symbols,
            missing_co_changes,
            untested_changes,
            risk: risk.to_string(),
            summary,
        })
    }

    /// Analyze module coupling: fan-in, fan-out, co-change frequency.
    pub fn coupling(&self, top: usize) -> Result<Vec<CouplingEntry>> {
        self.ensure_fresh_index()?;
        let store = self.store();
        let files = store.load_files()?;
        let relations = store.load_relations()?;

        // Build ID → path maps for both files and symbols
        let all_symbols = store.load_symbols()?;
        let mut id_to_path: HashMap<&str, &str> = HashMap::new();
        for f in &files {
            id_to_path.insert(&f.id, &f.path);
        }
        for s in &all_symbols {
            id_to_path.insert(&s.id, &s.path);
        }

        let mut fan_in: HashMap<String, usize> = HashMap::new();
        let mut fan_out: HashMap<String, usize> = HashMap::new();

        for r in &relations {
            // Only count high-confidence structural relations
            let dominated = matches!(r.kind,
                RelationKind::Imports | RelationKind::ConfigFor | RelationKind::TestFor
            ) || (r.kind == RelationKind::Calls && r.weight >= 0.7);
            if !dominated {
                continue;
            }
            let source_file = id_to_path.get(r.source_id.as_str()).copied().unwrap_or("");
            let target_file = id_to_path.get(r.target_id.as_str()).copied().unwrap_or("");
            if !source_file.is_empty() && !target_file.is_empty() && source_file != target_file {
                *fan_out.entry(source_file.to_string()).or_default() += 1;
                *fan_in.entry(target_file.to_string()).or_default() += 1;
            }
        }

        // Co-change counts
        let co_changes = if kungfu_git::is_git_repo(&self.project.root) {
            kungfu_git::co_change_pairs(&self.project.root, 2).unwrap_or_default()
        } else {
            HashMap::new()
        };

        let mut entries: Vec<CouplingEntry> = files
            .iter()
            .filter(|f| f.language.as_deref().map(is_code_language).unwrap_or(false))
            .map(|f| {
                let fi = *fan_in.get(&f.path).unwrap_or(&0);
                let fo = *fan_out.get(&f.path).unwrap_or(&0);
                let co = co_changes.get(&f.path).map(|v| v.len()).unwrap_or(0);
                let risk_score = (fi as f64 * 0.4) + (fo as f64 * 0.3) + (co as f64 * 0.3);
                CouplingEntry {
                    path: f.path.clone(),
                    fan_in: fi,
                    fan_out: fo,
                    co_change_count: co,
                    risk_score,
                }
            })
            .filter(|e| e.fan_in > 0 || e.fan_out > 0)
            .collect();

        entries.sort_by(|a, b| b.risk_score.partial_cmp(&a.risk_score).unwrap_or(std::cmp::Ordering::Equal));
        entries.truncate(top);
        Ok(entries)
    }
}

/// Extract lines from a symbol body that contain query keywords, with 1 line of context.
fn extract_keyword_lines(
    lines: &[String],
    start_idx: usize,
    end_idx: usize,
    keywords: &[&str],
    max_lines: usize,
) -> String {
    use kungfu_search::simple_stem;

    // Find line indices within symbol that contain any keyword (or stem)
    let mut hit_indices: Vec<usize> = Vec::new();
    for i in start_idx..end_idx {
        let line_lower = lines[i].to_lowercase();
        let matches = keywords.iter().any(|kw| {
            line_lower.contains(kw)
                || simple_stem(kw).map_or(false, |s| line_lower.contains(&s))
        });
        if matches {
            hit_indices.push(i);
        }
    }

    if hit_indices.is_empty() {
        return String::new();
    }

    // Always include first line (signature) + keyword-matched lines with 1 line context
    let mut include: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    include.insert(start_idx); // signature line

    for &idx in &hit_indices {
        let ctx_start = idx.saturating_sub(1).max(start_idx);
        let ctx_end = (idx + 2).min(end_idx);
        for i in ctx_start..ctx_end {
            include.insert(i);
        }
    }

    // Build snippet, inserting "..." for gaps
    let indices: Vec<usize> = include.into_iter().collect();
    let mut result = Vec::new();
    let mut prev: Option<usize> = None;

    for &i in &indices {
        if result.len() >= max_lines {
            break;
        }
        if let Some(p) = prev {
            if i > p + 1 {
                result.push("    ...".to_string());
            }
        }
        // Highlight keyword lines with >>> marker
        if hit_indices.contains(&i) {
            result.push(format!(">>> {}", highlight_keywords(&lines[i], keywords)));
        } else {
            result.push(lines[i].clone());
        }
        prev = Some(i);
    }

    result.join("\n")
}

/// Highlight keyword occurrences in a line by wrapping them with «» markers.
fn highlight_keywords(line: &str, keywords: &[&str]) -> String {
    use kungfu_search::simple_stem;

    let line_lower = line.to_lowercase();
    // Collect all match positions (start, end) in the original line
    let mut matches: Vec<(usize, usize)> = Vec::new();

    for kw in keywords {
        // Find all occurrences of keyword (case-insensitive)
        let mut pos = 0;
        while let Some(idx) = line_lower[pos..].find(kw) {
            let start = pos + idx;
            let end = start + kw.len();
            matches.push((start, end));
            pos = end;
        }
        // Also try stem
        if let Some(stem) = simple_stem(kw) {
            pos = 0;
            while let Some(idx) = line_lower[pos..].find(&stem) {
                let start = pos + idx;
                let end = start + stem.len();
                matches.push((start, end));
                pos = end;
            }
        }
    }

    if matches.is_empty() {
        return line.to_string();
    }

    // Sort by start position and merge overlapping ranges
    matches.sort_by_key(|&(s, _)| s);
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in matches {
        if let Some(last) = merged.last_mut() {
            if s <= last.1 {
                last.1 = last.1.max(e);
                continue;
            }
        }
        merged.push((s, e));
    }

    // Build highlighted string
    let mut result = String::with_capacity(line.len() + merged.len() * 4);
    let mut cursor = 0;
    for (s, e) in &merged {
        result.push_str(&line[cursor..*s]);
        result.push('«');
        result.push_str(&line[*s..*e]);
        result.push('»');
        cursor = *e;
    }
    result.push_str(&line[cursor..]);
    result
}

fn detect_primary_language(files: &[FileEntry]) -> Option<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for f in files {
        if let Some(ref lang) = f.language {
            if is_code_language(lang) {
                *counts.entry(lang.clone()).or_default() += 1;
            }
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(lang, _)| lang)
}

fn truncate_text(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_string()
    } else {
        format!("{}...", &text[..max_len])
    }
}

fn is_code_language(lang: &str) -> bool {
    matches!(
        lang,
        "rust" | "typescript" | "javascript" | "python" | "go" | "java" | "csharp" | "kotlin" | "c" | "cpp"
    )
}

fn detect_intent(words: &[&str]) -> Intent {
    for w in words {
        match *w {
            "find" | "where" | "locate" | "show" | "get" | "lookup" | "search" => {
                return Intent::Lookup
            }
            "bug" | "fix" | "error" | "crash" | "broken" | "fail" | "debug" | "wrong"
            | "issue" | "panic" => return Intent::Debug,
            "how" | "explain" | "understand" | "what" | "why" | "does" | "works" | "overview" => {
                return Intent::Understand
            }
            "impact" | "affects" | "uses" | "calls" | "callers" | "consumers" | "depends"
            | "dependents" | "change" | "refactor" | "rename" | "remove" | "delete" => {
                return Intent::Impact
            }
            _ => {}
        }
    }
    Intent::Lookup
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_lookup() {
        assert_eq!(detect_intent(&["find", "budget"]), Intent::Lookup);
        assert_eq!(detect_intent(&["where", "is", "config"]), Intent::Lookup);
        assert_eq!(detect_intent(&["show", "symbols"]), Intent::Lookup);
    }

    #[test]
    fn intent_debug() {
        assert_eq!(detect_intent(&["fix", "crash"]), Intent::Debug);
        assert_eq!(detect_intent(&["error", "parsing"]), Intent::Debug);
        assert_eq!(detect_intent(&["bug", "in", "indexer"]), Intent::Debug);
    }

    #[test]
    fn intent_understand() {
        assert_eq!(detect_intent(&["how", "does", "ranking"]), Intent::Understand);
        assert_eq!(detect_intent(&["explain", "budget"]), Intent::Understand);
        assert_eq!(detect_intent(&["what", "is", "context"]), Intent::Understand);
    }

    #[test]
    fn intent_impact() {
        assert_eq!(detect_intent(&["impact", "of", "change"]), Intent::Impact);
        assert_eq!(detect_intent(&["refactor", "budget"]), Intent::Impact);
        assert_eq!(detect_intent(&["rename", "symbol"]), Intent::Impact);
    }

    #[test]
    fn intent_default_is_lookup() {
        assert_eq!(detect_intent(&["foobar", "baz"]), Intent::Lookup);
    }

    #[test]
    fn stop_words_filtered() {
        assert!(is_stop_word("the"));
        assert!(is_stop_word("find"));
        assert!(is_stop_word("add"));
        assert!(!is_stop_word("budget"));
        assert!(!is_stop_word("parser"));
        assert!(!is_stop_word("language"));
    }

    #[test]
    fn architecture_detection_workspace() {
        let files = vec![
            FileEntry {
                id: "1".into(), path: "crates/core/src/lib.rs".into(), extension: Some("rs".into()),
                language: Some("rust".into()), size: 100, hash: "h1".into(),
                indexed_at: Default::default(), tags: vec![],
            },
        ];
        let result = detect_architecture(&files, &[]);
        assert!(result.contains("Workspace"), "got: {}", result);
    }

    #[test]
    fn architecture_detection_mvc() {
        let files = vec![
            FileEntry {
                id: "1".into(), path: "src/controller/auth.ts".into(), extension: Some("ts".into()),
                language: Some("typescript".into()), size: 100, hash: "h1".into(),
                indexed_at: Default::default(), tags: vec![],
            },
            FileEntry {
                id: "2".into(), path: "src/service/auth.ts".into(), extension: Some("ts".into()),
                language: Some("typescript".into()), size: 100, hash: "h2".into(),
                indexed_at: Default::default(), tags: vec![],
            },
            FileEntry {
                id: "3".into(), path: "src/model/user.ts".into(), extension: Some("ts".into()),
                language: Some("typescript".into()), size: 100, hash: "h3".into(),
                indexed_at: Default::default(), tags: vec![],
            },
        ];
        let result = detect_architecture(&files, &[]);
        assert!(result.contains("MVC") || result.contains("Layered"), "got: {}", result);
    }

    #[test]
    fn naming_style_snake() {
        use kungfu_types::symbol::{SymbolKind, Span};
        let symbols = vec![
            Symbol {
                id: "1".into(), file_id: "f1".into(), name: "my_function".into(),
                kind: SymbolKind::Function, language: "rust".into(), path: "a.rs".into(),
                signature: None, span: Span { start_line: 1, end_line: 5, start_col: 0, end_col: 0 },
                parent_symbol_id: None, exported: true, visibility: None, doc_summary: None,
            },
            Symbol {
                id: "2".into(), file_id: "f1".into(), name: "another_func".into(),
                kind: SymbolKind::Function, language: "rust".into(), path: "a.rs".into(),
                signature: None, span: Span { start_line: 6, end_line: 10, start_col: 0, end_col: 0 },
                parent_symbol_id: None, exported: true, visibility: None, doc_summary: None,
            },
        ];
        let result = detect_naming_style(&symbols);
        assert!(result.contains("snake_case"), "got: {}", result);
    }

    #[test]
    fn naming_style_camel() {
        use kungfu_types::symbol::{SymbolKind, Span};
        let symbols = vec![
            Symbol {
                id: "1".into(), file_id: "f1".into(), name: "myFunction".into(),
                kind: SymbolKind::Function, language: "ts".into(), path: "a.ts".into(),
                signature: None, span: Span { start_line: 1, end_line: 5, start_col: 0, end_col: 0 },
                parent_symbol_id: None, exported: true, visibility: None, doc_summary: None,
            },
            Symbol {
                id: "2".into(), file_id: "f1".into(), name: "anotherFunc".into(),
                kind: SymbolKind::Function, language: "ts".into(), path: "a.ts".into(),
                signature: None, span: Span { start_line: 6, end_line: 10, start_col: 0, end_col: 0 },
                parent_symbol_id: None, exported: true, visibility: None, doc_summary: None,
            },
        ];
        let result = detect_naming_style(&symbols);
        assert!(result.contains("camelCase"), "got: {}", result);
    }

    #[test]
    fn test_pattern_detection() {
        let files = vec![
            FileEntry {
                id: "1".into(), path: "src/auth.ts".into(), extension: Some("ts".into()),
                language: Some("typescript".into()), size: 100, hash: "h1".into(),
                indexed_at: Default::default(), tags: vec![],
            },
            FileEntry {
                id: "2".into(), path: "tests/auth.test.ts".into(), extension: Some("ts".into()),
                language: Some("typescript".into()), size: 100, hash: "h2".into(),
                indexed_at: Default::default(), tags: vec![],
            },
        ];
        let result = detect_test_pattern(&files);
        assert!(result.contains("test"), "got: {}", result);
    }

    #[test]
    fn test_is_test_symbol() {
        use kungfu_types::symbol::{SymbolKind, Span};
        let sym = Symbol {
            id: "1".into(), file_id: "f1".into(), name: "test_something".into(),
            kind: SymbolKind::Function, language: "rust".into(), path: "src/lib.rs".into(),
            signature: None, span: Span { start_line: 1, end_line: 5, start_col: 0, end_col: 0 },
            parent_symbol_id: None, exported: false, visibility: None, doc_summary: None,
        };
        assert!(is_test_symbol(&sym));

        let sym2 = Symbol {
            id: "2".into(), file_id: "f1".into(), name: "do_work".into(),
            kind: SymbolKind::Function, language: "rust".into(), path: "src/lib.rs".into(),
            signature: None, span: Span { start_line: 6, end_line: 10, start_col: 0, end_col: 0 },
            parent_symbol_id: None, exported: true, visibility: None, doc_summary: None,
        };
        assert!(!is_test_symbol(&sym2));
    }

    #[test]
    fn primary_language_detection() {
        let files = vec![
            FileEntry {
                id: "1".into(), path: "a.rs".into(), extension: Some("rs".into()),
                language: Some("rust".into()), size: 100, hash: "h1".into(),
                indexed_at: Default::default(), tags: vec![],
            },
            FileEntry {
                id: "2".into(), path: "b.rs".into(), extension: Some("rs".into()),
                language: Some("rust".into()), size: 100, hash: "h2".into(),
                indexed_at: Default::default(), tags: vec![],
            },
            FileEntry {
                id: "3".into(), path: "c.py".into(), extension: Some("py".into()),
                language: Some("python".into()), size: 100, hash: "h3".into(),
                indexed_at: Default::default(), tags: vec![],
            },
        ];
        assert_eq!(detect_primary_language(&files), Some("rust".to_string()));
    }
}

fn detect_architecture(files: &[FileEntry], _symbols: &[Symbol]) -> String {
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();

    // Check for common patterns
    let has_src = paths.iter().any(|p| p.starts_with("src/"));
    let has_crates = paths.iter().any(|p| p.starts_with("crates/"));
    let has_packages = paths.iter().any(|p| p.starts_with("packages/"));
    let has_cmd = paths.iter().any(|p| p.starts_with("cmd/"));
    let has_internal = paths.iter().any(|p| p.starts_with("internal/"));
    let has_controllers = paths.iter().any(|p| p.contains("controller") || p.contains("handler"));
    let has_services = paths.iter().any(|p| p.contains("service"));
    let has_models = paths.iter().any(|p| p.contains("model") || p.contains("entity"));
    let has_routes = paths.iter().any(|p| p.contains("route") || p.contains("router"));
    let has_components = paths.iter().any(|p| p.contains("component"));

    if has_crates || has_packages {
        "Workspace / Monorepo — multiple crates/packages".to_string()
    } else if has_cmd && has_internal {
        "Go-style — cmd/ for binaries, internal/ for packages".to_string()
    } else if has_controllers && has_services && has_models {
        "MVC / Layered — controllers, services, models".to_string()
    } else if has_routes && has_services {
        "Service-oriented — routes + services".to_string()
    } else if has_components {
        "Component-based — UI components".to_string()
    } else if has_src {
        "Standard — src/ based".to_string()
    } else {
        "Flat / Custom".to_string()
    }
}

fn detect_naming_style(symbols: &[Symbol]) -> String {
    let mut snake = 0;
    let mut camel = 0;
    let mut pascal = 0;

    for s in symbols {
        let name = &s.name;
        if name.contains('_') && name == &name.to_lowercase() {
            snake += 1;
        } else if name.chars().next().map(|c| c.is_lowercase()).unwrap_or(false) && name.contains(char::is_uppercase) {
            camel += 1;
        } else if name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            pascal += 1;
        }
    }

    let total = snake + camel + pascal;
    if total == 0 {
        return "unknown".to_string();
    }

    let mut styles = vec![];
    if snake > total / 4 { styles.push(format!("snake_case ({}%)", snake * 100 / total)); }
    if camel > total / 4 { styles.push(format!("camelCase ({}%)", camel * 100 / total)); }
    if pascal > total / 4 { styles.push(format!("PascalCase ({}%)", pascal * 100 / total)); }

    if styles.is_empty() {
        "mixed".to_string()
    } else {
        styles.join(", ")
    }
}

fn detect_test_pattern(files: &[FileEntry]) -> String {
    let test_files: Vec<&str> = files.iter()
        .map(|f| f.path.as_str())
        .filter(|p| p.contains("test") || p.contains("spec"))
        .collect();

    if test_files.is_empty() {
        return "no tests detected".to_string();
    }

    let co_located = test_files.iter().any(|p| !p.starts_with("test") && !p.starts_with("tests") && !p.starts_with("__tests__"));
    let separate_dir = test_files.iter().any(|p| p.starts_with("test") || p.starts_with("tests") || p.starts_with("__tests__"));
    let spec_style = test_files.iter().any(|p| p.contains(".spec."));
    let test_suffix = test_files.iter().any(|p| p.contains(".test.") || p.contains("_test."));

    let mut patterns = vec![];
    if co_located { patterns.push("co-located"); }
    if separate_dir { patterns.push("tests/ directory"); }
    if spec_style { patterns.push("*.spec.* naming"); }
    if test_suffix { patterns.push("*.test.* / *_test.* naming"); }

    format!("{} test files — {}", test_files.len(), patterns.join(", "))
}

fn is_test_symbol(sym: &Symbol) -> bool {
    sym.name.starts_with("test_")
        || sym.name.starts_with("Test")
        || sym.name.contains("_test")
        || sym.path.contains("test")
        || sym.path.contains("spec")
}

fn count_test_symbols(symbols: &[Symbol]) -> usize {
    symbols.iter().filter(|s| is_test_symbol(s)).count()
}

fn is_stop_word(word: &str) -> bool {
    matches!(
        word,
        // English stop words
        "the" | "a" | "an" | "is" | "are" | "was" | "were" | "in" | "on" | "at" | "to"
            | "for" | "of" | "with" | "by" | "from" | "and" | "or" | "not" | "it" | "this"
            | "that" | "be" | "has" | "have" | "do" | "does" | "did" | "will" | "would"
            | "could" | "should" | "can" | "may" | "i" | "me" | "my" | "we"
            // Intent trigger words (already captured by detect_intent, noise in search)
            | "find" | "where" | "locate" | "show" | "get" | "lookup" | "search"
            | "bug" | "fix" | "crash" | "broken" | "debug" | "wrong" | "issue"
            | "how" | "explain" | "understand" | "what" | "why" | "works" | "overview"
            | "impact" | "affects" | "uses" | "calls" | "callers" | "consumers"
            | "depends" | "dependents" | "change" | "refactor" | "rename"
            | "remove" | "delete" | "implemented" | "work" | "system" | "break"
            | "new" | "add" | "create" | "make" | "build" | "implement" | "support" | "need"
            | "want" | "like" | "also" | "just" | "all" | "every" | "each" | "some" | "any"
    )
}
