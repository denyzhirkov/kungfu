#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod config_rank;
mod expand;

pub use config_rank::{
    apply_config_penalty, config_tie_break, query_wants_config, ConfigAct, ConfigActs,
    ConfigRanking, ConfigShare, CONFIG_HEAVY_SHARE, CONFIG_PENALTY,
};
pub use expand::{
    split_identifier, stem_variants, ExpandedQuery, ExpandedToken, STRENGTH_EXACT, STRENGTH_STEM,
    STRENGTH_SYNONYM,
};

use anyhow::Result;
use kungfu_storage::JsonStore;
use kungfu_types::file::FileEntry;
use kungfu_types::relation::RelationKind;
use kungfu_types::symbol::Symbol;
use kungfu_types::Budget;
use std::sync::Arc;

pub struct SearchEngine<'a> {
    store: &'a JsonStore,
}

pub struct SearchResult<T> {
    pub item: T,
    pub score: f64,
}

/// Why a file matched [`SearchEngine::find_files`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileMatch {
    /// The query matched the file path.
    Path,
    /// The query matched a symbol defined in the file (its best-scoring one).
    Symbol(String),
}

impl std::fmt::Display for FileMatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileMatch::Path => write!(f, "path"),
            FileMatch::Symbol(name) => write!(f, "symbol: {name}"),
        }
    }
}

/// A ranked file hit with the reason it matched.
pub struct FileHit {
    pub item: FileEntry,
    pub score: f64,
    pub matched: FileMatch,
}

/// A ranked page plus what the config ranking policy did to it.
pub struct RankedHits<T> {
    pub hits: Vec<T>,
    /// Matches the policy penalised before the top-k trim: symbol ids for a
    /// symbol search, ids of files matched through a config symbol for a file
    /// search.
    pub config_penalized: Vec<String>,
    /// Matches a config-heavy project ranked on equal terms (same ids).
    pub config_lifted: Vec<String>,
}

impl<T> RankedHits<T> {
    /// What the policy did, for [`ConfigRanking::note`].
    pub fn acts(&self) -> ConfigActs {
        ConfigActs {
            penalized: !self.config_penalized.is_empty(),
            lifted: !self.config_lifted.is_empty(),
        }
    }
}

/// Split `(id, act)` pairs into penalised and lifted id lists.
fn split_acts(acted: impl Iterator<Item = (String, ConfigAct)>) -> (Vec<String>, Vec<String>) {
    let mut penalized = Vec::new();
    let mut lifted = Vec::new();
    for (id, act) in acted {
        match act {
            ConfigAct::Penalized => penalized.push(id),
            ConfigAct::Lifted => lifted.push(id),
        }
    }
    (penalized, lifted)
}

/// Score symbols against `query`, keep those under `scope` (a path prefix),
/// apply the config penalty, keep the top `top_k`. Scope filters before the
/// penalty and the cut, so a scoped search keeps its recall and its config
/// note describes the scope; scoring statistics stay project-wide, as in
/// [`rank_files`].
fn rank_symbols(
    symbols: &[Symbol],
    query: &str,
    scope: Option<&str>,
    top_k: usize,
    ranking: ConfigRanking,
) -> RankedHits<SearchResult<Symbol>> {
    let query_lower = query.to_lowercase();

    // Split multi-word queries (and identifier-shaped words) for broader matching
    let words: Vec<&str> = query_lower.split_whitespace().collect();
    let eq = ExpandedQuery::new(&words);

    let scores = score_all_symbols(symbols, &eq);
    let mut results: Vec<SearchResult<Symbol>> = symbols
        .iter()
        .zip(scores)
        .filter_map(|(s, mut score)| {
            // A full-name match always wins, regardless of which path scored it
            // (e.g. query "build_context_packet" takes the multi-token path).
            if s.name.to_lowercase() == query_lower {
                // Exact case bonus: "DataFrame" matches "DataFrame" better than "dataFrame"
                score = if s.name == query { 1.01 } else { 1.0 };
            }
            if score > 0.0 && scope.is_none_or(|p| s.path.starts_with(p)) {
                Some(SearchResult {
                    item: s.clone(),
                    score,
                })
            } else {
                None
            }
        })
        .collect();

    let acted = apply_config_penalty(ranking, &query_lower, &mut results, |r| {
        (&r.item, &mut r.score)
    });
    let (config_penalized, config_lifted) = split_acts(
        acted
            .into_iter()
            .map(|(i, act)| (results[i].item.id.clone(), act)),
    );

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| config_tie_break(&a.item, &b.item))
    });
    results.truncate(top_k);
    RankedHits {
        hits: results,
        config_penalized,
        config_lifted,
    }
}

/// File ranking behind [`SearchEngine::find_files_ranked`]. A file matched
/// through a config symbol carries that symbol's penalty; a path match never
/// does.
fn rank_files(
    files: &[FileEntry],
    symbols: &[Symbol],
    query: &str,
    scope: Option<&str>,
    top_k: usize,
    ranking: ConfigRanking,
) -> RankedHits<FileHit> {
    let query_lower = query.to_lowercase();
    let words: Vec<&str> = query_lower.split_whitespace().collect();
    let eq = ExpandedQuery::new(&words);

    // (symbol, score, what the config policy did to it)
    let mut matched: Vec<(&Symbol, f64, Option<ConfigAct>)> = symbols
        .iter()
        .zip(score_all_symbols(symbols, &eq))
        .filter(|(_, score)| *score > 0.0)
        .map(|(s, score)| (s, score, None))
        .collect();
    let acted = apply_config_penalty(ranking, &query_lower, &mut matched, |m| (m.0, &mut m.1));
    for (i, act) in acted {
        matched[i].2 = Some(act);
    }

    // file_id -> (best symbol score, that symbol, act)
    let mut best_symbol: std::collections::HashMap<&str, (f64, &Symbol, Option<ConfigAct>)> =
        std::collections::HashMap::new();
    for &(sym, sym_score, demoted) in &matched {
        let entry = best_symbol
            .entry(sym.file_id.as_str())
            .or_insert((0.0, sym, demoted));
        if sym_score > entry.0 {
            *entry = (sym_score, sym, demoted);
        }
    }

    let mut file_acts = Vec::new();
    let mut results: Vec<FileHit> = files
        .iter()
        .filter(|f| scope.is_none_or(|s| f.path.starts_with(s)))
        .filter_map(|f| {
            let path_score = score_path_match(&f.path, &query_lower, &eq);
            let (score, matched) = match best_symbol.get(f.id.as_str()) {
                Some(&(s, sym, acted)) if s * 0.8 > path_score => {
                    if let Some(act) = acted {
                        file_acts.push((f.id.clone(), act));
                    }
                    (s * 0.8, FileMatch::Symbol(sym.name.clone()))
                }
                _ => (path_score, FileMatch::Path),
            };
            (score > 0.0).then(|| FileHit {
                item: f.clone(),
                score,
                matched,
            })
        })
        .collect();

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_k);
    let (config_penalized, config_lifted) = split_acts(file_acts.into_iter());
    RankedHits {
        hits: results,
        config_penalized,
        config_lifted,
    }
}

impl<'a> SearchEngine<'a> {
    pub fn new(store: &'a JsonStore) -> Self {
        Self { store }
    }

    /// [`find_symbol_ranked`](Self::find_symbol_ranked) with the config
    /// ranking decided from this query alone.
    pub fn find_symbol(&self, query: &str, budget: Budget) -> Result<Vec<SearchResult<Symbol>>> {
        let ranking = self.config_ranking(query, None)?;
        Ok(self.find_symbol_ranked(query, budget, None, ranking)?.hits)
    }

    /// `scope` (path prefix) filters before the config penalty and the cut.
    pub fn find_symbol_ranked(
        &self,
        query: &str,
        budget: Budget,
        scope: Option<&str>,
        ranking: ConfigRanking,
    ) -> Result<RankedHits<SearchResult<Symbol>>> {
        let symbols = self.store.symbols_arc()?;
        Ok(rank_symbols(
            &symbols,
            query,
            scope,
            budget.top_k(),
            ranking,
        ))
    }

    /// Decide once per request how config symbols rank for `query`: one pass
    /// over the cached file list. `scope` narrows the config-heavy share.
    pub fn config_ranking(&self, query: &str, scope: Option<&str>) -> Result<ConfigRanking> {
        let query_lower = query.to_lowercase();
        let words: Vec<&str> = query_lower.split_whitespace().collect();
        Ok(ConfigRanking::decide(
            &words,
            &self.store.files_arc()?,
            scope,
        ))
    }

    pub fn get_symbol(&self, name: &str) -> Result<Option<Symbol>> {
        let symbols = self.store.symbols_arc()?;
        let name_lower = name.to_lowercase();

        // Try exact match first
        if let Some(sym) = symbols.iter().find(|s| s.name.to_lowercase() == name_lower) {
            return Ok(Some(sym.clone()));
        }

        // Try dotted path match (e.g. "KungfuService.open")
        if let Some(dot_pos) = name.find('.') {
            let method_name = &name[dot_pos + 1..].to_lowercase();
            let parent_name = &name[..dot_pos].to_lowercase();
            if let Some(sym) = symbols.iter().find(|s| {
                s.name.to_lowercase() == *method_name
                    && s.parent_symbol_id
                        .as_ref()
                        .is_some_and(|pid| pid.to_lowercase().contains(parent_name))
            }) {
                return Ok(Some(sym.clone()));
            }
        }

        Ok(None)
    }

    /// [`find_files`](Self::find_files) without the match reasons.
    pub fn search_text(&self, query: &str, budget: Budget) -> Result<Vec<SearchResult<FileEntry>>> {
        Ok(self
            .find_files(query, budget, None)?
            .into_iter()
            .map(|h| SearchResult {
                item: h.item,
                score: h.score,
            })
            .collect())
    }

    /// [`find_files_ranked`](Self::find_files_ranked) with the config ranking
    /// decided from this query and scope.
    pub fn find_files(
        &self,
        query: &str,
        budget: Budget,
        scope: Option<&str>,
    ) -> Result<Vec<FileHit>> {
        let ranking = self.config_ranking(query, scope)?;
        Ok(self.find_files_ranked(query, budget, scope, ranking)?.hits)
    }

    /// Ranked file search: a file scores by the better of its path match and the
    /// best-matching symbol it defines, so a keyword also finds files by contents.
    /// `scope` (path prefix) filters before the top-k trim, so a scoped search keeps
    /// its recall instead of filtering an already-trimmed global page.
    pub fn find_files_ranked(
        &self,
        query: &str,
        budget: Budget,
        scope: Option<&str>,
        ranking: ConfigRanking,
    ) -> Result<RankedHits<FileHit>> {
        let files = self.store.files_arc()?;
        let symbols = self.store.symbols_arc()?;
        Ok(rank_files(
            &files,
            &symbols,
            query,
            scope,
            budget.top_k(),
            ranking,
        ))
    }

    /// Shared snapshot of every indexed file — no per-call clone.
    pub fn get_all_files(&self) -> Result<Arc<Vec<FileEntry>>> {
        self.store.files_arc()
    }

    /// Shared snapshot of every indexed symbol — no per-call clone.
    pub fn get_all_symbols(&self) -> Result<Arc<Vec<Symbol>>> {
        self.store.symbols_arc()
    }

    /// Find files related to the given file by import relations, directory proximity,
    /// shared symbols, and naming patterns.
    pub fn find_related(
        &self,
        file_path: &str,
        budget: Budget,
    ) -> Result<Vec<SearchResult<FileEntry>>> {
        let files = self.store.files_arc()?;
        let symbols = self.store.symbols_arc()?;
        let relations = self.store.relations_arc()?;
        let top_k = budget.top_k();

        // Normalize: find the file in the index
        let target = files.iter().find(|f| {
            f.path == file_path || f.path.ends_with(file_path) || file_path.ends_with(&f.path)
        });

        let target = match target {
            Some(t) => t.clone(),
            None => return Ok(Vec::new()),
        };

        let target_path = &target.path;

        // Collect import relations for the target file
        let mut relation_scores: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        for rel in relations.iter() {
            if rel.source_id == target.id {
                match rel.kind {
                    RelationKind::Imports => {
                        *relation_scores.entry(rel.target_id.clone()).or_default() += 0.7;
                    }
                    RelationKind::TestFor => {
                        // This test file tests that source file
                        *relation_scores.entry(rel.target_id.clone()).or_default() += 0.8;
                    }
                    RelationKind::ConfigFor => {
                        *relation_scores.entry(rel.target_id.clone()).or_default() += 0.4;
                    }
                    _ => {}
                }
            }
            if rel.target_id == target.id {
                match rel.kind {
                    RelationKind::Imports => {
                        *relation_scores.entry(rel.source_id.clone()).or_default() += 0.6;
                    }
                    RelationKind::TestFor => {
                        // Source file has this test
                        *relation_scores.entry(rel.source_id.clone()).or_default() += 0.8;
                    }
                    RelationKind::ConfigFor => {
                        *relation_scores.entry(rel.source_id.clone()).or_default() += 0.4;
                    }
                    _ => {}
                }
            }
        }

        // Collect symbol names from the target file
        let target_symbols: std::collections::HashSet<String> = symbols
            .iter()
            .filter(|s| s.file_id == target.id)
            .map(|s| s.name.to_lowercase())
            .collect();

        // Path components for proximity scoring
        let target_parts: Vec<&str> = target_path.split('/').collect();
        let target_dir = target_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let target_stem = std::path::Path::new(target_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();

        // Build symbol sets per file for cross-referencing
        let mut file_symbols: std::collections::HashMap<String, std::collections::HashSet<String>> =
            std::collections::HashMap::new();
        for sym in symbols.iter() {
            file_symbols
                .entry(sym.file_id.clone())
                .or_default()
                .insert(sym.name.to_lowercase());
        }

        let mut results: Vec<SearchResult<FileEntry>> = files
            .iter()
            .filter(|f| f.path != target.path)
            .filter_map(|f| {
                let mut score = 0.0_f64;

                // 0. Import/dependency relation (strongest signal)
                if let Some(&rel_score) = relation_scores.get(&f.id) {
                    score += rel_score;
                }

                let f_dir = f.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                let f_stem = std::path::Path::new(&f.path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();

                // 1. Same directory
                if f_dir == target_dir && !target_dir.is_empty() {
                    score += 0.3;
                }

                // 2. Shared parent directory
                let f_parts: Vec<&str> = f.path.split('/').collect();
                let shared_depth = target_parts
                    .iter()
                    .zip(f_parts.iter())
                    .take_while(|(a, b)| a == b)
                    .count();
                if shared_depth > 0 {
                    let depth_score = shared_depth as f64 / target_parts.len().max(1) as f64;
                    score += depth_score * 0.2;
                }

                // 3. Shared symbol names
                if let Some(f_syms) = file_symbols.get(&f.id) {
                    let shared = target_symbols.intersection(f_syms).count();
                    if shared > 0 {
                        let sym_score =
                            (shared as f64 / target_symbols.len().max(1) as f64).min(1.0);
                        score += sym_score * 0.3;
                    }
                }

                // 4. Test file pattern
                if f_stem.contains(&target_stem) || target_stem.contains(&f_stem) {
                    let is_test_pair = kungfu_types::test_paths::is_test_path(&f.path)
                        || kungfu_types::test_paths::is_test_path(target_path);
                    score += if is_test_pair { 0.3 } else { 0.1 };
                }

                // 5. Same language bonus
                if f.language == target.language {
                    score += 0.05;
                }

                if score > 0.05 {
                    Some(SearchResult {
                        item: f.clone(),
                        score,
                    })
                } else {
                    None
                }
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(top_k);
        Ok(results)
    }

    pub fn get_symbols_for_file(&self, file_path: &str) -> Result<Vec<Symbol>> {
        let symbols = self.store.symbols_arc()?;
        let files = self.store.files_arc()?;

        let file_id = files
            .iter()
            .find(|f| f.path == file_path)
            .map(|f| f.id.clone());

        match file_id {
            Some(fid) => Ok(symbols
                .iter()
                .filter(|s| s.file_id == fid)
                .cloned()
                .collect()),
            None => Ok(Vec::new()),
        }
    }
}

fn score_symbol_match(name: &str, query: &str) -> f64 {
    let name_lower = name.to_lowercase();

    // Exact match
    if name_lower == *query {
        return 1.0;
    }
    // Prefix match
    if name_lower.starts_with(query) {
        return 0.9;
    }
    // Substring match — scale by how much of the name the query covers
    if name_lower.contains(query) {
        let coverage = query.len() as f64 / name_lower.len() as f64;
        // "find" in "find_symbol" (0.36) → 0.55; "find" in "find_files" (0.4) → 0.58
        // "symbol" in "find_symbol" (0.54) → 0.67; short queries get lower scores
        return 0.4 + coverage * 0.4;
    }

    // Stem match: "ranking" matches "rank"
    for stem in stem_variants(query) {
        if name_lower.contains(&stem) {
            let coverage = stem.len() as f64 / name_lower.len() as f64;
            return 0.35 + coverage * 0.3;
        }
    }
    // Short root: "authentication" → "auth" matches "useAuthStore"
    if let Some(root) = short_root(query) {
        if name_lower.contains(&root) {
            return 0.4;
        }
    }
    // Reverse: query "rank" matches name "ranking"
    let name_stems = stem_variants(&name_lower);
    if name_stems.iter().any(|s| *s == *query) {
        return 0.55;
    }
    if name_stems
        .iter()
        .any(|s| s.contains(query) || query.contains(s.as_str()))
    {
        return 0.4;
    }

    // Fuzzy: check if all query chars appear in order — only for longer queries
    if query.len() >= 4 {
        let mut query_chars = query.chars().peekable();
        for ch in name_lower.chars() {
            if let Some(&qch) = query_chars.peek() {
                if ch == qch {
                    query_chars.next();
                }
            }
        }
        if query_chars.peek().is_none() {
            let coverage = query.len() as f64 / name_lower.len() as f64;
            return 0.2 + coverage * 0.2;
        }
    }

    0.0
}

/// Score every symbol against the expanded query. Multi-token queries use the
/// two-pass pipeline: analyze all symbols, derive per-token IDF weights, then
/// combine — so discriminative tokens dominate ubiquitous ones. Single-token
/// queries keep the legacy scorer plus the synonym fallback.
fn score_all_symbols(symbols: &[Symbol], eq: &ExpandedQuery) -> Vec<f64> {
    if eq.tokens.len() > 1 {
        let analyses: Vec<Option<SymbolTokenMatch>> = symbols
            .iter()
            .map(|s| analyze_symbol_multi_word(s, eq))
            .collect();
        // Document frequencies come from code symbols whenever any code symbol
        // matches — whatever the ranking decision — so config keys such as
        // `call_graph.*` never reweigh the tokens code is scored by: code scores
        // equal those of a config-free project, also for config queries. Only
        // when no code symbol matches (a Terraform- or Helm-only repo) does
        // config become the corpus. Config symbols are scored with whichever
        // weights apply.
        let n = eq.tokens.len();
        let code = analyses
            .iter()
            .zip(symbols)
            .filter(|(_, s)| !s.kind.is_config())
            .map(|(a, _)| a);
        let idf = token_idf_weights(code, n)
            .or_else(|| token_idf_weights(analyses.iter(), n))
            .unwrap_or_else(|| vec![1.0; n]);
        analyses
            .iter()
            .map(|a| a.as_ref().map_or(0.0, |m| combine_multi_word(m, &idf)))
            .collect()
    } else {
        symbols
            .iter()
            .map(|s| score_symbol_single(&s.name, eq))
            .collect()
    }
}

/// Per-symbol analysis of a multi-token query match, produced in a first pass
/// so token weights (IDF) can be computed before final scores are combined.
struct SymbolTokenMatch {
    /// Early-exit phrase score (0.95/0.98), or 0.0 when no phrase hit.
    phrase: f64,
    /// Per-token match strengths. Positive = matched in the symbol name,
    /// negative = matched only in signature/path (weaker), 0 = no match.
    strengths: Vec<f64>,
    /// Name-coverage + adjacency bonus (applies only to name hits).
    bonus: f64,
}

/// Analyze how a symbol matches an expanded multi-token query.
/// Returns `None` when nothing matches at all.
fn analyze_symbol_multi_word(sym: &Symbol, eq: &ExpandedQuery) -> Option<SymbolTokenMatch> {
    let name_lower = sym.name.to_lowercase();
    let sig_lower = sym
        .signature
        .as_ref()
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let path_lower = sym.path.to_lowercase();

    let words = eq.exact_words();

    // Exact phrase bonus: join words back and check if the phrase appears as-is
    let phrase = words.join(" ");
    let phrase_underscore = words.join("_");
    let phrase_camel_lower = words_to_camel(&words).to_lowercase();
    let phrase_joined = words.concat();

    // Full-name phrase match ("use effect" -> useEffect) beats mere containment
    // ("use effect" -> useInsertionEffect).
    if name_lower == phrase
        || name_lower == phrase_underscore
        || name_lower == phrase_camel_lower
        || name_lower == phrase_joined
    {
        return Some(SymbolTokenMatch {
            phrase: 0.98,
            strengths: vec![STRENGTH_EXACT; eq.tokens.len()],
            bonus: 0.0,
        });
    }

    // A config symbol's signature is `name = value`: its name half is the name
    // itself, and the value (a description, a reference to another block) is
    // not what the symbol is called. So a phrase found only in a config value
    // gets no phrase score (0.95); its tokens still match as signature text
    // below — the signature/path-only tier, at most 0.6.
    let sig_phrase = !sym.kind.is_config()
        && (sig_lower.contains(&phrase) || sig_lower.contains(&phrase_underscore));
    let has_exact_phrase = name_lower.contains(&phrase)
        || name_lower.contains(&phrase_underscore)
        || name_lower.contains(&phrase_camel_lower)
        || sig_phrase;

    if has_exact_phrase {
        return Some(SymbolTokenMatch {
            phrase: 0.95,
            strengths: vec![STRENGTH_EXACT; eq.tokens.len()],
            bonus: 0.0,
        });
    }

    let name_words = split_identifier(&sym.name);
    let name_word_stems: Vec<Vec<String>> = name_words.iter().map(|w| stem_variants(w)).collect();

    let mut strengths = vec![0.0f64; eq.tokens.len()];
    let mut any = false;
    let mut name_hits = 0usize;
    for (i, tok) in eq.tokens.iter().enumerate() {
        let ns = tok.match_in_name(&name_lower, &name_words, &name_word_stems);
        if ns > 0.0 {
            strengths[i] = ns;
            name_hits += 1;
            any = true;
            continue;
        }
        let ts = tok
            .match_in_text(&sig_lower)
            .max(tok.match_in_text(&path_lower));
        if ts > 0.0 {
            strengths[i] = -ts;
            any = true;
        }
    }

    if !any {
        return None;
    }

    let mut bonus = 0.0;
    if name_hits > 0 && !name_words.is_empty() {
        // Name-coverage bonus: reward names fully explained by the query
        // ("useEffect" over "commitHookEffectList" for query "use effect").
        let covered = name_words
            .iter()
            .filter(|w| {
                eq.tokens
                    .iter()
                    .any(|t| t.exact == **w || w.contains(&t.exact))
            })
            .count();
        bonus += 0.04 * (covered as f64 / name_words.len() as f64);

        // Adjacency bonus: consecutive query tokens joined in the name
        // ("auto configuration" inside "AutoConfigurationImportSelector",
        // "ask context" inside "ask_context_with_weights").
        for pair in words.windows(2) {
            if name_lower.contains(&pair.concat()) || name_lower.contains(&pair.join("_")) {
                bonus += 0.03;
                break;
            }
        }
    }

    Some(SymbolTokenMatch {
        phrase: 0.0,
        strengths,
        bonus,
    })
}

/// Per-token IDF weights from document frequencies over the analyzed symbols.
/// Rare (discriminative) tokens weigh more than ubiquitous ones, so
/// "ws middleware" ranks WebSocketMiddleware above the thousands of
/// *ConnectionMiddleware symbols that merely share common vocabulary.
/// `None` when no analysis matched at all.
fn token_idf_weights<'a>(
    analyses: impl Iterator<Item = &'a Option<SymbolTokenMatch>>,
    token_count: usize,
) -> Option<Vec<f64>> {
    let mut df = vec![0usize; token_count];
    let mut n = 0usize;
    for a in analyses.flatten() {
        n += 1;
        for (j, s) in a.strengths.iter().enumerate() {
            if *s != 0.0 {
                df[j] += 1;
            }
        }
    }
    if n == 0 {
        return None;
    }
    Some(
        df.iter()
            .map(|d| (1.0 + n as f64 / (1 + d) as f64).ln())
            .collect(),
    )
}

/// Combine a per-symbol analysis with token IDF weights into a final score.
/// Weighted coverage keeps the historical output range: full name coverage
/// -> ~0.9, signature/path-only coverage -> ~0.6, phrase hits 0.95/0.98.
fn combine_multi_word(m: &SymbolTokenMatch, idf: &[f64]) -> f64 {
    if m.phrase > 0.0 {
        return m.phrase;
    }
    let denom: f64 = idf.iter().sum();
    if denom <= 0.0 {
        return 0.0;
    }
    let mut weighted = 0.0f64;
    let mut name_hit = false;
    for (s, w) in m.strengths.iter().zip(idf) {
        if *s > 0.0 {
            name_hit = true;
        }
        weighted += s.abs() * w;
    }
    let ratio = weighted / denom;
    let score = if name_hit {
        ratio * 0.9 + m.bonus
    } else {
        ratio * 0.6
    };
    score.min(0.94)
}

/// Score a symbol against an expanded multi-token query with uniform token
/// weights. The search paths use the two-pass analyze/IDF/combine pipeline;
/// this direct form pins the scoring contract in unit tests.
#[cfg(test)]
fn score_symbol_multi_word(sym: &Symbol, eq: &ExpandedQuery) -> f64 {
    match analyze_symbol_multi_word(sym, eq) {
        Some(m) => combine_multi_word(&m, &vec![1.0; eq.tokens.len()]),
        None => 0.0,
    }
}

/// Single-token scoring: legacy prefix/substring/stem/fuzzy matching first,
/// then the synonym/abbreviation table as a penalized fallback tier
/// (exact vocabulary always ranks above expanded vocabulary).
fn score_symbol_single(name: &str, eq: &ExpandedQuery) -> f64 {
    let Some(tok) = eq.tokens.first() else {
        return 0.0;
    };
    let base = score_symbol_match(name, &tok.exact);
    if base > 0.0 {
        return base;
    }
    let name_lower = name.to_lowercase();
    let name_words = split_identifier(name);
    tok.synonym_name_score(&name_lower, &name_words)
}

/// Convert words to camelCase for phrase matching (e.g. ["refresh", "token"] → "refreshtoken")
fn words_to_camel(words: &[&str]) -> String {
    let mut result = String::new();
    for (i, word) in words.iter().enumerate() {
        if i == 0 {
            result.push_str(word);
        } else {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                result.extend(first.to_uppercase());
                result.push_str(chars.as_str());
            }
        }
    }
    result
}

fn score_path_match(path: &str, query: &str, eq: &ExpandedQuery) -> f64 {
    let path_lower = path.to_lowercase();

    // Single token: check path containment (exact, stem, or penalized synonym)
    if eq.tokens.len() <= 1 {
        if path_lower.contains(query) {
            let filename = path.rsplit('/').next().unwrap_or(path).to_lowercase();
            if filename.contains(query) {
                return 0.9;
            }
            return 0.6;
        }
        let Some(tok) = eq.tokens.first() else {
            return 0.0;
        };
        // Stem match: "ranking" → "rank", "indexing" → "index"
        match tok.match_in_text(&path_lower) {
            s if s >= STRENGTH_EXACT => return 0.6,
            s if s >= STRENGTH_STEM => return 0.5,
            s if s > 0.0 => return 0.4,
            _ => return 0.0,
        }
    }

    // Multi-token: weighted coverage of the path (exact > stem > synonym)
    let total: f64 = eq
        .tokens
        .iter()
        .map(|tok| tok.match_in_text(&path_lower))
        .sum();
    if total == 0.0 {
        return 0.0;
    }

    total / eq.tokens.len() as f64
}

/// Simple English stemming: strip common suffixes. Returns the primary stem
/// candidate; [`stem_variants`] exposes the full candidate set. Kept as the
/// single stemming entry point for external callers.
pub fn simple_stem(word: &str) -> Option<String> {
    stem_variants(word).into_iter().next()
}

/// Extract a short prefix root for long words (e.g. "authentication" → "auth").
/// Useful when full stem is still too long to match abbreviations in code.
fn short_root(word: &str) -> Option<String> {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() < 8 {
        return None;
    }
    // Skip compound identifiers — they already have meaningful parts
    if word.contains('_') || word.contains('-') {
        return None;
    }
    // Only for ASCII words (avoids UTF-8 boundary issues)
    if !word.is_ascii() {
        return None;
    }
    // Try common roots by cutting at consonant-vowel boundary
    for len in [4, 5, 6] {
        if len < chars.len() && is_vowel(chars[len]) != is_vowel(chars[len - 1]) {
            return Some(chars[..len].iter().collect());
        }
    }
    // Fallback: first 4 chars if word is long enough
    if chars.len() >= 10 {
        Some(chars[..4].iter().collect())
    } else {
        None
    }
}

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

/// Expand query keywords with conceptually related terms (synonym/concept mapping).
/// Returns the original keywords + expansions. Used for semantic-like search.
pub fn expand_query(keywords: &[&str]) -> Vec<String> {
    let mut expanded: Vec<String> = keywords.iter().map(|k| k.to_string()).collect();

    for kw in keywords {
        if let Some(synonyms) = concept_synonyms(kw) {
            for syn in synonyms {
                if !expanded.iter().any(|e| e == syn) {
                    expanded.push(syn.to_string());
                }
            }
        }
    }

    expanded
}

/// Return related terms for common programming concepts.
fn concept_synonyms(word: &str) -> Option<&'static [&'static str]> {
    Some(match word {
        // Authentication & security
        "auth" | "authenticate" | "authentication" => &[
            "login",
            "verify",
            "token",
            "credential",
            "session",
            "password",
            "jwt",
            "oauth",
        ],
        "login" | "signin" => &["auth", "authenticate", "credential", "session"],
        "permission" | "authorize" | "authorization" => {
            &["role", "access", "guard", "policy", "acl"]
        }
        "security" => &["auth", "encrypt", "token", "sanitize", "csrf", "xss"],

        // Database & storage
        "database" | "db" => &[
            "query",
            "connection",
            "pool",
            "migrate",
            "schema",
            "model",
            "repository",
            "dao",
        ],
        "query" => &["select", "filter", "where", "fetch", "find"],
        "migrate" | "migration" => &["schema", "alter", "table", "database"],
        "cache" | "caching" => &["store", "redis", "memcache", "ttl", "invalidate", "expire"],

        // HTTP & networking
        "request" | "req" => &[
            "response",
            "handler",
            "route",
            "middleware",
            "http",
            "endpoint",
        ],
        "response" | "res" => &["request", "handler", "status", "header", "body"],
        "route" | "routing" => &[
            "handler",
            "endpoint",
            "path",
            "middleware",
            "controller",
            "dispatch",
        ],
        "middleware" => &["handler", "filter", "interceptor", "guard", "pipe", "chain"],
        "api" => &["endpoint", "route", "handler", "rest", "controller"],
        "http" => &[
            "request", "response", "handler", "client", "server", "fetch",
        ],
        "websocket" | "ws" => &["socket", "connect", "message", "channel"],

        // Error handling
        "error" | "err" => &[
            "exception",
            "panic",
            "fail",
            "handle",
            "catch",
            "result",
            "unwrap",
        ],
        "exception" => &["error", "throw", "catch", "handle", "try"],
        "panic" => &["error", "crash", "abort", "unwrap", "bail"],
        "retry" => &["backoff", "attempt", "reconnect", "fallback"],

        // Async & concurrency
        "async" | "asynchronous" => &["await", "future", "promise", "spawn", "task", "concurrent"],
        "concurrent" | "concurrency" => &[
            "thread", "mutex", "lock", "sync", "parallel", "atomic", "channel",
        ],
        "thread" => &["spawn", "pool", "mutex", "lock", "concurrent", "parallel"],
        "channel" => &["sender", "receiver", "mpsc", "broadcast", "message"],

        // Serialization & parsing
        "serialize" | "serialization" => &[
            "deserialize",
            "json",
            "serde",
            "encode",
            "marshal",
            "format",
        ],
        "parse" | "parsing" => &["lexer", "tokenize", "ast", "grammar", "syntax", "tree"],
        "json" => &[
            "serialize",
            "deserialize",
            "serde",
            "parse",
            "encode",
            "decode",
        ],
        "validate" | "validation" => &[
            "check",
            "verify",
            "sanitize",
            "constraint",
            "schema",
            "rule",
        ],

        // Logging & observability
        "log" | "logging" => &["trace", "debug", "info", "warn", "logger", "tracing"],
        "metric" | "metrics" => &["counter", "gauge", "histogram", "monitor", "telemetry"],
        "trace" | "tracing" => &["span", "log", "instrument", "observe"],

        // Testing
        "test" | "testing" => &["assert", "mock", "fixture", "spec", "expect", "verify"],
        "mock" | "mocking" => &["stub", "fake", "spy", "test", "fixture"],

        // Config & setup
        "config" | "configuration" => &[
            "settings",
            "options",
            "env",
            "environment",
            "setup",
            "preference",
        ],
        "env" | "environment" => &["config", "variable", "dotenv", "settings"],

        // Data structures
        "list" | "array" => &["vec", "slice", "collection", "iterator", "push", "append"],
        "map" | "hashmap" | "dict" | "dictionary" => {
            &["hash", "lookup", "key", "value", "entry", "table"]
        }
        "queue" => &["enqueue", "dequeue", "fifo", "buffer", "channel"],
        "tree" => &[
            "node", "traverse", "walk", "leaf", "branch", "parent", "child",
        ],

        // File & IO
        "file" => &["read", "write", "path", "open", "stream", "io", "fs"],
        "stream" => &["read", "write", "buffer", "pipe", "io", "reader", "writer"],

        // Common verbs in code
        "create" | "new" => &["init", "build", "construct", "make", "instantiate"],
        "delete" | "remove" => &["drop", "destroy", "clean", "purge", "clear"],
        "update" | "modify" => &["set", "patch", "change", "mutate", "edit"],
        "send" => &["emit", "dispatch", "publish", "notify", "broadcast"],
        "receive" | "recv" => &["listen", "subscribe", "consume", "accept", "handle"],
        "render" => &["display", "draw", "paint", "view", "template", "component"],
        "transform" => &["convert", "map", "adapt", "translate", "process"],

        _ => return None,
    })
}

/// Check if the query suggests interest in test files.
pub fn query_wants_tests(words: &[&str]) -> bool {
    words.iter().any(|w| {
        matches!(
            *w,
            "test" | "tests" | "spec" | "specs" | "testing" | "unittest" | "unit_test"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kungfu_types::symbol::{Span, Symbol, SymbolKind};

    fn make_symbol(name: &str, path: &str, sig: Option<&str>) -> Symbol {
        Symbol {
            id: format!("s:test:1:{}", name),
            file_id: "f:test".to_string(),
            name: name.to_string(),
            kind: SymbolKind::Function,
            language: "rust".to_string(),
            path: path.to_string(),
            signature: sig.map(String::from),
            span: Span {
                start_line: 1,
                end_line: 10,
                start_col: 0,
                end_col: 0,
            },
            parent_symbol_id: None,
            exported: true,
            visibility: None,
            doc_summary: None,
        }
    }

    #[test]
    fn exact_match_scores_highest() {
        assert_eq!(score_symbol_match("Budget", "budget"), 1.0);
    }

    #[test]
    fn prefix_match() {
        assert!(score_symbol_match("BudgetParam", "budget") > 0.8);
    }

    #[test]
    fn contains_match() {
        assert!(score_symbol_match("parse_budget", "budget") > 0.6);
    }

    #[test]
    fn no_match_returns_zero() {
        assert_eq!(score_symbol_match("KungfuService", "budget"), 0.0);
    }

    #[test]
    fn fuzzy_match() {
        // 3-char fuzzy queries are intentionally filtered out (too noisy)
        let score = score_symbol_match("build_context_packet", "bcp");
        assert_eq!(score, 0.0, "3-char fuzzy should not match");
        // 4+ char fuzzy queries should still work
        let score = score_symbol_match("build_context_packet", "bcpk");
        assert!(
            score > 0.2,
            "4-char fuzzy match should score > 0.2, got {}",
            score
        );
    }

    #[test]
    fn stem_match_ranking_to_rank() {
        // "ranking" stems to "rank", and "build_context_packet" doesn't contain "rank"
        // This correctly returns 0 — stem matching applies to path, not symbol name substring
        let score = score_symbol_match("build_context_packet", "ranking");
        assert_eq!(score, 0.0);

        // But a name containing "rank" should match via stem
        let score2 = score_symbol_match("rank_results", "ranking");
        assert!(
            score2 > 0.0,
            "stem should match rank_results, got {}",
            score2
        );
    }

    fn eq(words: &[&str]) -> ExpandedQuery {
        ExpandedQuery::new(words)
    }

    #[test]
    fn exact_phrase_snake_case() {
        let sym = make_symbol("build_context_packet", "src/rank.rs", None);
        let score = score_symbol_multi_word(&sym, &eq(&["context", "packet"]));
        assert_eq!(score, 0.95, "snake_case phrase should score 0.95");
    }

    #[test]
    fn exact_phrase_camel_case() {
        // Full-name phrase match ranks above phrase containment
        let sym = make_symbol("contextPacket", "src/types.rs", None);
        let score = score_symbol_multi_word(&sym, &eq(&["context", "packet"]));
        assert_eq!(score, 0.98, "full-name camelCase phrase should score 0.98");
    }

    #[test]
    fn full_name_match_beats_containment() {
        let exact = make_symbol("useEffect", "src/ReactHooks.js", None);
        let contains = make_symbol("useInsertionEffect", "src/ReactHooks.js", None);
        let q = eq(&["use", "effect"]);
        let s_exact = score_symbol_multi_word(&exact, &q);
        let s_contains = score_symbol_multi_word(&contains, &q);
        assert!(
            s_exact > s_contains,
            "useEffect ({s_exact}) should outrank useInsertionEffect ({s_contains})"
        );
    }

    #[test]
    fn multi_word_partial() {
        let sym = make_symbol("parse_budget", "src/cli.rs", None);
        let score = score_symbol_multi_word(&sym, &eq(&["budget", "validation"]));
        assert!(
            score > 0.0 && score < 0.95,
            "partial multi-word should be between 0 and 0.95"
        );
    }

    #[test]
    fn multi_word_no_match() {
        let sym = make_symbol("KungfuService", "src/core.rs", None);
        let score = score_symbol_multi_word(&sym, &eq(&["database", "migration"]));
        assert_eq!(score, 0.0);
    }

    #[test]
    fn multi_word_coverage_rewarded() {
        // A symbol covering both query tokens beats one covering a single token
        let both = make_symbol("dispatch_request", "src/app.py", None);
        let one = make_symbol("request_finished", "src/signals.py", None);
        let q = eq(&["dispatched", "request"]);
        let s_both = score_symbol_multi_word(&both, &q);
        let s_one = score_symbol_multi_word(&one, &q);
        assert!(
            s_both > s_one,
            "two-token coverage ({s_both}) should beat one-token ({s_one})"
        );
    }

    #[test]
    fn split_name_matches_abbreviated_query() {
        // "ws connection middleware" must surface WebSocketMiddleware via
        // synonym expansion + word-set matching
        let sym = make_symbol(
            "WebSocketMiddleware",
            "Middleware/WebSocketMiddleware.cs",
            None,
        );
        let score = score_symbol_multi_word(&sym, &eq(&["ws", "connection", "middleware"]));
        assert!(
            score > 0.3,
            "abbreviation match should score > 0.3, got {score}"
        );
    }

    #[test]
    fn expanded_match_penalized_vs_exact() {
        // Exact vocabulary ("websocket middleware") must outrank the
        // abbreviation-expanded form ("ws middleware") on the same symbol
        let sym = make_symbol(
            "WebSocketMiddleware",
            "Middleware/WebSocketMiddleware.cs",
            None,
        );
        let s_exact = score_symbol_multi_word(&sym, &eq(&["websocket", "middleware"]));
        let s_expanded = score_symbol_multi_word(&sym, &eq(&["ws", "middleware"]));
        assert!(
            s_exact > s_expanded,
            "exact ({s_exact}) should outrank expanded ({s_expanded})"
        );
        assert!(s_expanded > 0.0, "expanded form should still match");
    }

    #[test]
    fn single_word_synonym_fallback() {
        let q = eq(&["ws"]);
        let score = score_symbol_single("WebSocketHandler", &q);
        assert!(score > 0.0, "ws should reach WebSocketHandler via synonyms");
        // ...but below a direct substring match on the same query
        let direct = score_symbol_single("wsHandler", &q);
        assert!(
            direct > score,
            "direct ({direct}) should beat synonym ({score})"
        );
    }

    #[test]
    fn path_match_filename() {
        let score = score_path_match("src/budget.rs", "budget", &eq(&["budget"]));
        assert_eq!(score, 0.9, "filename match should score 0.9");
    }

    #[test]
    fn path_match_directory() {
        let score = score_path_match("budget/types/mod.rs", "budget", &eq(&["budget"]));
        assert_eq!(score, 0.6, "directory-only match should score 0.6");
    }

    #[test]
    fn path_no_match() {
        let score = score_path_match("src/search.rs", "budget", &eq(&["budget"]));
        assert_eq!(score, 0.0);
    }

    #[test]
    fn path_stem_match() {
        let score = score_path_match("src/ranking.rs", "ranking", &eq(&["ranking"]));
        assert_eq!(score, 0.9, "direct filename match");

        let stem_score = score_path_match("src/rank.rs", "ranking", &eq(&["ranking"]));
        assert!(stem_score > 0.0, "stem match should find rank.rs");
    }

    #[test]
    fn path_multi_word_synonym_penalized() {
        let q = eq(&["expiration", "handling"]);
        let stem_hit = score_path_match("src/expire.c", "expiration handling", &q);
        assert!(stem_hit > 0.0, "expiration should reach expire.c via stem");
        let exact_hit = score_path_match("src/expiration.c", "expiration handling", &q);
        assert!(exact_hit > stem_hit, "exact path hit should score higher");
    }

    #[test]
    fn simple_stem_works() {
        assert_eq!(simple_stem("ranking"), Some("rank".to_string()));
        assert_eq!(simple_stem("indexing"), Some("index".to_string()));
        assert_eq!(simple_stem("configuration"), Some("configur".to_string()));
        assert_eq!(simple_stem("migrations"), Some("migration".to_string()));
        assert_eq!(simple_stem("abc"), None); // too short
    }

    #[test]
    fn words_to_camel_works() {
        assert_eq!(words_to_camel(&["context", "packet"]), "contextPacket");
        assert_eq!(words_to_camel(&["a"]), "a");
        assert_eq!(words_to_camel(&["foo", "bar", "baz"]), "fooBarBaz");
    }

    #[test]
    fn query_wants_tests_detection() {
        assert!(query_wants_tests(&["test", "budget"]));
        assert!(query_wants_tests(&["run", "specs"]));
        assert!(!query_wants_tests(&["budget", "parsing"]));
    }

    #[test]
    fn query_wants_config_detection() {
        assert!(query_wants_config(&["config", "file"]));
        assert!(query_wants_config(&["cargo", "toml"]));
        assert!(!query_wants_config(&["budget", "parsing"]));
    }

    // --- Semantic expansion tests ---

    #[test]
    fn expand_query_auth() {
        let expanded = expand_query(&["auth"]);
        assert!(expanded.contains(&"auth".to_string()));
        assert!(expanded.contains(&"login".to_string()));
        assert!(expanded.contains(&"token".to_string()));
        assert!(expanded.contains(&"session".to_string()));
    }

    #[test]
    fn expand_query_database() {
        let expanded = expand_query(&["database"]);
        assert!(expanded.contains(&"query".to_string()));
        assert!(expanded.contains(&"connection".to_string()));
        assert!(expanded.contains(&"migrate".to_string()));
    }

    #[test]
    fn expand_query_no_expansion_for_unknown() {
        let expanded = expand_query(&["foobar"]);
        assert_eq!(expanded, vec!["foobar"]);
    }

    #[test]
    fn expand_query_multiple_keywords() {
        let expanded = expand_query(&["auth", "error"]);
        assert!(expanded.contains(&"login".to_string()));
        assert!(expanded.contains(&"panic".to_string()));
        // No duplicates
        let unique: std::collections::HashSet<_> = expanded.iter().collect();
        assert_eq!(unique.len(), expanded.len());
    }

    // --- Case-sensitive scoring tests ---

    #[test]
    fn case_exact_match_scores_higher() {
        // Both "DataFrame" and "dataFrame" match "dataframe" with score 1.0
        // But in find_symbol, exact case gets 1.01 bonus
        let score_exact = score_symbol_match("DataFrame", "dataframe");
        let score_camel = score_symbol_match("dataFrame", "dataframe");
        assert_eq!(score_exact, 1.0);
        assert_eq!(score_camel, 1.0);
        // The case bonus happens in find_symbol, not score_symbol_match
    }

    #[test]
    fn class_vs_getter_same_name() {
        // Both match with score 1.0, but class should win via tiebreaker
        let score_class = score_symbol_match("DataFrame", "dataframe");
        let score_getter = score_symbol_match("dataFrame", "dataframe");
        assert_eq!(score_class, score_getter); // same base score
    }

    // --- Path matching tests ---

    #[test]
    fn path_contains_keyword() {
        let score = score_path_match("src/auth/service.ts", "auth", &eq(&["auth"]));
        assert!(score > 0.0, "path containing keyword should match");
    }

    // --- Config penalty in symbol and file ranking ---

    fn config_key(name: &str, path: &str) -> Symbol {
        Symbol {
            kind: SymbolKind::ConfigKey,
            language: "json".to_string(),
            file_id: format!("f:{path}"),
            ..make_symbol(name, path, None)
        }
    }

    fn code_fn(name: &str, path: &str) -> Symbol {
        Symbol {
            file_id: format!("f:{path}"),
            ..make_symbol(name, path, None)
        }
    }

    fn names(hits: &RankedHits<SearchResult<Symbol>>) -> Vec<&str> {
        hits.hits.iter().map(|r| r.item.path.as_str()).collect()
    }

    #[test]
    fn code_outranks_equal_config_match_when_penalized() {
        // Both names are the same prefix match for "timeout".
        let symbols = vec![
            config_key("timeout_ms", "package.json"),
            code_fn("timeout_ms", "src/net.rs"),
        ];
        let ranked = rank_symbols(&symbols, "timeout", None, 10, ConfigRanking::Penalized);
        assert_eq!(names(&ranked), ["src/net.rs", "package.json"]);
        assert_eq!(
            (ranked.config_penalized.len() + ranked.config_lifted.len()),
            1
        );
        assert!(ranked.hits[0].score > ranked.hits[1].score);
    }

    #[test]
    fn config_competes_equally_when_penalty_is_lifted() {
        let symbols = vec![
            config_key("timeout_ms", "package.json"),
            code_fn("timeout_ms", "src/net.rs"),
        ];
        // A share lift is still recorded so the response can declare it.
        for (lifted, acted) in [
            (ConfigRanking::QueryWantsConfig, 0),
            (ConfigRanking::ConfigHeavy, 1),
        ] {
            let ranked = rank_symbols(&symbols, "timeout", None, 10, lifted);
            assert_eq!(
                (ranked.config_penalized.len() + ranked.config_lifted.len()),
                acted,
                "{lifted:?}"
            );
            assert_eq!(ranked.hits[0].score, ranked.hits[1].score, "{lifted:?}");
        }
        // A config key matching better than the code symbol wins outright.
        let symbols = vec![
            code_fn("request_timeout_handler", "src/net.rs"),
            config_key("timeout", "package.json"),
        ];
        let ranked = rank_symbols(&symbols, "timeout", None, 10, ConfigRanking::ConfigHeavy);
        assert_eq!(names(&ranked)[0], "package.json");
    }

    #[test]
    fn config_symbols_leave_code_scores_bit_for_bit_unchanged() {
        let code = vec![
            code_fn("call_graph_provenance", "src/a.rs"),
            code_fn("report_empty_call_graph", "src/b.rs"),
            code_fn("affected_provenance", "src/c.rs"),
            code_fn("build_context_packet", "src/d.rs"),
        ];
        let mut with_config = code.clone();
        with_config.extend([
            config_key("call_graph.enabled", "kungfu.toml"),
            config_key("call_graph.max_caller_files", "kungfu.toml"),
            config_key("call_graph.cross_file_only", "kungfu.toml"),
            config_key("context.packet", "settings.yaml"),
        ]);
        let code_rows = |hits: &RankedHits<SearchResult<Symbol>>| -> Vec<(String, f64)> {
            hits.hits
                .iter()
                .filter(|r| !r.item.kind.is_config())
                .map(|r| (r.item.id.clone(), r.score))
                .collect()
        };
        for q in [
            "callers call graph provenance",
            "build context packet",
            "call graph",
            "call graph config toml",
        ] {
            // Also when config competes on equal terms (config query,
            // config-heavy project): code scores never depend on config.
            for ranking in [
                ConfigRanking::Penalized,
                ConfigRanking::QueryWantsConfig,
                ConfigRanking::ConfigHeavy,
            ] {
                let without = rank_symbols(&code, q, None, 50, ranking);
                let with = rank_symbols(&with_config, q, None, 50, ranking);
                assert!(
                    with.hits.iter().any(|r| r.item.kind.is_config()),
                    "{q}: the query must match config too"
                );
                assert_eq!(code_rows(&without), code_rows(&with), "{q} {ranking:?}");
            }
        }
    }

    #[test]
    fn scope_filters_symbols_before_the_cut() {
        // Six better matches outside the scope would fill a 5-slot page.
        let mut symbols: Vec<Symbol> = (0..6)
            .map(|i| code_fn(&format!("widget_{i}"), &format!("src/w{i}.rs")))
            .collect();
        symbols.push(code_fn("make_widget_thing", "other/misc.rs"));
        symbols.push(config_key("widget_size", "other/app.yaml"));
        let ranked = rank_symbols(
            &symbols,
            "widget",
            Some("other/"),
            5,
            ConfigRanking::Penalized,
        );
        let mut paths: Vec<&str> = ranked.hits.iter().map(|r| r.item.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, ["other/app.yaml", "other/misc.rs"]);
        assert_eq!(
            (ranked.config_penalized.len() + ranked.config_lifted.len()),
            1
        );

        let ranked = rank_symbols(
            &symbols,
            "widget",
            Some("src/"),
            5,
            ConfigRanking::Penalized,
        );
        assert!(
            ranked.acts() == ConfigActs::default(),
            "no config match in scope"
        );
    }

    fn config_sym(
        name: &str,
        kind: SymbolKind,
        path: &str,
        sig: &str,
        line: usize,
        nested: bool,
    ) -> Symbol {
        let mut s = make_symbol(name, path, Some(sig));
        s.id = format!("s:{path}:{line}");
        s.kind = kind;
        s.file_id = format!("f:{path}");
        s.span.start_line = line;
        s.parent_symbol_id = nested.then(|| "parent".to_string());
        s
    }

    #[test]
    fn config_value_text_does_not_score_like_a_name() {
        let q = eq(&["security", "group"]);
        let block = config_sym(
            "aws_security_group.web",
            SymbolKind::ConfigBlock,
            "main.tf",
            "aws_security_group.web = {2 attributes}",
            1,
            false,
        );
        let described = config_sym(
            "output.web_sg_id.description",
            SymbolKind::ConfigKey,
            "outputs.tf",
            "output.web_sg_id.description = \"ID of Security Group web\"",
            2,
            true,
        );
        let referenced = config_sym(
            "output.web_sg_id.value",
            SymbolKind::ConfigKey,
            "outputs.tf",
            "output.web_sg_id.value = aws_security_group.web.id",
            3,
            true,
        );
        assert_eq!(score_symbol_multi_word(&block, &q), 0.95);
        for value_only in [&described, &referenced] {
            let score = score_symbol_multi_word(value_only, &q);
            assert!(score > 0.0 && score <= 0.6, "{}: {score}", value_only.name);
        }
        // Code keeps its signature phrase score.
        let code = make_symbol("check", "src/a.rs", Some("fn check(security_group: &Sg)"));
        assert_eq!(score_symbol_multi_word(&code, &q), 0.95);
    }

    #[test]
    fn equal_config_scores_break_ties_by_level_kind_path_line() {
        let key = |path: &str, line: usize, nested: bool| {
            config_sym("k", SymbolKind::ConfigKey, path, "k = 1", line, nested)
        };
        let block = config_sym("b", SymbolKind::ConfigBlock, "z.tf", "b = {}", 9, false);
        let code = code_fn("c", "src/c.rs");
        let mut v = vec![
            key("a.tf", 5, true),
            key("a.tf", 2, false),
            key("b.tf", 1, false),
            block.clone(),
            code.clone(),
            key("a.tf", 1, false),
        ];
        v.sort_by(config_tie_break);
        let order: Vec<(&str, usize)> = v
            .iter()
            .map(|s| (s.path.as_str(), s.span.start_line))
            .collect();
        assert_eq!(
            order,
            [
                ("src/c.rs", 1),
                ("z.tf", 9),
                ("a.tf", 1),
                ("a.tf", 2),
                ("b.tf", 1),
                ("a.tf", 5)
            ]
        );
        // Code-vs-code: always equal, so a stable sort keeps the existing order.
        assert_eq!(
            config_tie_break(&code, &code_fn("d", "src/d.rs")),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn config_only_repo_weighs_tokens_by_config_frequencies() {
        // HCL-only: `bucket` is everywhere, `policy` rare. Uniform weights
        // would score a bucket-only name like a policy-only one.
        let mut symbols: Vec<Symbol> = (0..6)
            .map(|i| config_key(&format!("aws_s3_bucket.logs_{i}"), "main.tf"))
            .collect();
        // Same word count as the bucket names, so only the token weights differ.
        symbols.push(config_key("aws_s3_policy.logs_p", "iam.tf"));
        let score = |ranked: &RankedHits<SearchResult<Symbol>>, name: &str| {
            ranked
                .hits
                .iter()
                .find(|r| r.item.name == name)
                .map(|r| r.score)
                .unwrap()
        };
        for ranking in [ConfigRanking::ConfigHeavy, ConfigRanking::Penalized] {
            let ranked = rank_symbols(&symbols, "bucket policy", None, 50, ranking);
            assert!(
                score(&ranked, "aws_s3_policy.logs_p") > score(&ranked, "aws_s3_bucket.logs_0"),
                "{ranking:?}: the rare token must weigh more"
            );
        }
    }

    #[test]
    fn file_matched_only_through_config_symbol_is_demoted() {
        let file = |path: &str| FileEntry {
            id: format!("f:{path}"),
            path: path.to_string(),
            extension: None,
            language: None,
            size: 1,
            hash: "h".to_string(),
            indexed_at: Default::default(),
            tags: vec![],
            purpose: None,
            purpose_source: None,
            config_symbols_total: None,
            config_parse_errors: false,
        };
        let files = vec![file("package.json"), file("src/net.rs")];
        let symbols = vec![
            config_key("retries", "package.json"),
            code_fn("retries", "src/net.rs"),
        ];
        let ranked = rank_files(
            &files,
            &symbols,
            "retries",
            None,
            10,
            ConfigRanking::Penalized,
        );
        let paths: Vec<&str> = ranked.hits.iter().map(|h| h.item.path.as_str()).collect();
        assert_eq!(paths, ["src/net.rs", "package.json"]);
        assert_eq!(
            (ranked.config_penalized.len() + ranked.config_lifted.len()),
            1
        );

        // A path match is never penalized.
        let ranked = rank_files(
            &files,
            &symbols,
            "package",
            None,
            10,
            ConfigRanking::Penalized,
        );
        assert_eq!(ranked.hits[0].item.path, "package.json");
        assert_eq!(ranked.hits[0].matched, FileMatch::Path);
        assert_eq!(
            (ranked.config_penalized.len() + ranked.config_lifted.len()),
            0
        );
    }

    #[test]
    fn path_exact_filename() {
        let score = score_path_match("src/router.ts", "router", &eq(&["router"]));
        assert!(
            score >= 0.9,
            "exact filename match should score high, got {}",
            score
        );
    }
}
