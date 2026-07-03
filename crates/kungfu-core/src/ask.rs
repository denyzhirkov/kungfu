use crate::helpers::{
    detect_intent, detect_primary_language, is_code_language, is_stop_word, truncate_text,
};
use crate::KungfuService;
use anyhow::Result;
use kungfu_rank::{build_context_packet_full, ScoredSymbol};
use kungfu_types::budget::Budget;
use kungfu_types::context::{ContextPacket, Intent};
use kungfu_types::relation::RelationKind;
use kungfu_types::symbol::Symbol;
use std::collections::{HashMap, HashSet};

/// Strategy A2 caps: how many top-cosine rows to fetch from the vector store and
/// how many may enter the candidate pool. Bounded so packet assembly cost stays
/// flat no matter how large the embedding store is.
const VECTOR_FETCH_K: usize = 20;
const VECTOR_ACCEPT_MAX: usize = 10;
/// At most this many *new* vector candidates per file. Vector recall exists to
/// widen file/concept coverage; same-file siblings all embed similarly and would
/// otherwise crowd out the keyword hit for the file's main symbol (sibling
/// expansion is Strategy C's job, keyed to the query's keywords).
const VECTOR_PER_FILE_MAX: usize = 1;
/// Cosine at which a vector hit is trusted enough to outrank name-search hits.
/// Below this (the 0.65–0.75 mid-band), a hit only backfills: its score is
/// capped just under the best name-match score, so it never displaces a weak
/// but exact keyword answer. Measured on the bench: mid-band displacement is
/// where the vector layer loses points; ≥0.75 hits are consistently right.
const VECTOR_TRUSTED_COS: f64 = 0.75;
/// How far below the best name-match score a capped mid-band hit sits.
const VECTOR_CAP_MARGIN: f64 = 0.02;

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
    /// Strategy A2: score for a vector (cosine) hit at the minimum accepted cosine.
    /// Kept lower than direct symbol-name matches so vector results augment rather than displace.
    pub vector_score: f64,
    /// Strategy A2: minimum cosine to accept a vector hit. Below this, the model is essentially
    /// guessing and a string match is more trustworthy. For bge-small, cosines under ~0.65
    /// are an empirical noise band (measured on the 50-case bench): unrelated symbols cluster
    /// at 0.60–0.68, genuine concept hits at 0.69+.
    pub vector_min_score: f64,
    /// Strategy A2: score for a vector hit at cosine 1.0. Hits ramp linearly from
    /// `vector_score` (at `vector_min_score`) up to this — a high-confidence semantic hit
    /// must be able to outrank weak keyword noise, while still losing to an exact
    /// phrase/name match (0.95+).
    pub vector_strong_score: f64,
    /// File-level fallback: score for a symbol injected purely because its file path matched a
    /// keyword. A filename match is a weaker signal than a symbol-name match, so this sits below
    /// a typical name hit — it should backfill, not outrank genuine matches.
    pub path_fallback_score: f64,
    /// Multiplier applied to test symbols for Understand/Lookup intents, where the user wants the
    /// implementation, not its tests. <1.0 demotes; tests still appear, just below real code.
    pub test_penalty: f64,
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
            vector_score: 0.6,
            vector_min_score: 0.65,
            vector_strong_score: 0.9,
            path_fallback_score: 0.3,
            test_penalty: 0.5,
        }
    }
}

impl StrategyWeights {
    /// Load weights from environment variables (KUNGFU_W_*), falling back to defaults.
    pub fn from_env() -> Self {
        let mut w = Self::default();
        fn env_f64(key: &str, default: f64) -> f64 {
            std::env::var(key)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
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
        w.vector_score = env_f64("KUNGFU_W_VECTOR", w.vector_score);
        w.vector_min_score = env_f64("KUNGFU_W_VECTOR_MIN", w.vector_min_score);
        w.vector_strong_score = env_f64("KUNGFU_W_VECTOR_STRONG", w.vector_strong_score);
        w.path_fallback_score = env_f64("KUNGFU_W_PATH_FALLBACK", w.path_fallback_score);
        w.test_penalty = env_f64("KUNGFU_W_TEST_PENALTY", w.test_penalty);
        w
    }
}

impl KungfuService {
    /// High-level context retrieval: parse intent, run multi-strategy search,
    /// rank with contextual signals, return compact packet.
    pub fn ask_context(&self, task: &str, budget: Budget) -> Result<ContextPacket> {
        self.ask_context_with_weights(task, budget, &StrategyWeights::from_env())
    }

    pub fn ask_context_with_weights(
        &self,
        task: &str,
        budget: Budget,
        w: &StrategyWeights,
    ) -> Result<ContextPacket> {
        let budget = self.resolve_budget(budget);
        let query_lower = task.to_lowercase();
        let words: Vec<&str> = query_lower.split_whitespace().collect();

        // 1. Detect intent
        let intent = detect_intent(&words);

        // 2. Extract search terms (filter out stop/intent words)
        let keywords: Vec<&str> = words.iter().filter(|w| !is_stop_word(w)).copied().collect();
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

        // Load all symbols up front — needed by Strategy A2 (vector) below as well as
        // later strategies (B sibling expansion, D related-file walk, …).
        let all_symbols = search.get_all_symbols()?;

        // Strategy A2: vector cosine match — augment with concept-level hits when an embedding
        // store + a real engine are available. Runs for every intent; only single-keyword
        // queries skip it (those are direct symbol lookups, where vectors mostly add noise
        // on top of Strategy A). Whatever happens here is declared in `packet.retrieval`.
        let mut retrieval = kungfu_types::context::RetrievalInfo {
            mode: "keyword_only".to_string(),
            vector_candidates: 0,
            vector_skipped: None,
        };
        let mut vector_added = 0usize;
        if keywords.len() < 2 {
            retrieval.vector_skipped = Some(
                "single-keyword query — name search is authoritative, vector recall skipped"
                    .to_string(),
            );
        } else {
            match kungfu_embed::EmbeddingStore::load(&self.project.index_dir()) {
                Ok(Some(emb_store)) => {
                    let engine = kungfu_embed::shared_engine();
                    if !engine.is_real() {
                        retrieval.vector_skipped = Some(
                            "embedding engine unavailable (weights or `semantic` feature \
                             missing) — see `kungfu embeddings status`"
                                .to_string(),
                        );
                    } else {
                        match engine.embed_batch(&[task]) {
                            Ok(qv) if !qv.is_empty() => {
                                let raw = emb_store.top_k(&qv[0], VECTOR_FETCH_K);
                                let symbols_by_id: HashMap<&str, &Symbol> =
                                    all_symbols.iter().map(|s| (s.id.as_str(), s)).collect();
                                // Best Strategy A (name search) score — the mid-band
                                // trust cap in blend_vector_hits is relative to it.
                                let best_name_score = scored_symbols
                                    .iter()
                                    .map(|s| s.score)
                                    .fold(0.0f64, f64::max);
                                retrieval.mode = "keyword+vector".to_string();
                                let (contributed, added) = blend_vector_hits(
                                    &raw,
                                    &symbols_by_id,
                                    best_name_score,
                                    &mut seen_ids,
                                    &mut scored_symbols,
                                    w,
                                );
                                retrieval.vector_candidates = contributed;
                                vector_added = added;
                            }
                            _ => {
                                retrieval.vector_skipped = Some(
                                    "query embedding failed — keyword strategies only".to_string(),
                                );
                            }
                        }
                    }
                }
                Ok(None) => {
                    retrieval.vector_skipped = Some(
                        "no embedding store — run `kungfu embeddings build` to enable \
                         vector recall"
                            .to_string(),
                    );
                }
                Err(e) => {
                    retrieval.vector_skipped = Some(format!(
                        "embedding store unreadable ({e}) — rebuild with `kungfu embeddings build`"
                    ));
                }
            }
        }

        // Strategy B: text/file search — only add keyword-relevant symbols
        let file_results = search.search_text(&keyword_query, Budget::Full)?;
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

        // Strategy B2: content grep — search file bodies for keywords.
        // Sparse-pool gate counts only keyword-sourced candidates: vector hits
        // augment the pool but must not suppress keyword backfill.
        if scored_symbols.len() - vector_added < budget.top_k() {
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
        // (same keyword-only sparse-pool gate as B2)
        if scored_symbols.len() - vector_added < budget.top_k() {
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
                .max_by(|a, b| {
                    a.score
                        .partial_cmp(&b.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
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
                siblings
                    .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.exported.cmp(&a.0.exported)));
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
                        let sig_lower = s.symbol.signature.as_deref().unwrap_or("").to_lowercase();
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
            let relations = store.relations_arc()?;
            let file_ids: HashSet<String> =
                file_results.iter().map(|r| r.item.id.clone()).collect();

            for rel in relations.iter() {
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
                kw.len() >= 3
                    && path_lower.split('/').any(|seg| {
                        seg.contains(kw)
                            || seg
                                .trim_end_matches(".ts")
                                .trim_end_matches(".js")
                                .trim_end_matches(".rs")
                                .trim_end_matches(".py")
                                .trim_end_matches(".go")
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

        // File-level fallback: if the best *keyword* symbol score is weak, inject
        // file-level results. Vector hits are excluded from the gate: a cosine hit
        // is a hypothesis, not a confirmation, so it must not switch off the
        // path-match safety net the keyword pipeline relied on.
        let best_score = scored_symbols
            .iter()
            .filter(|s| !s.reason.starts_with("vector match"))
            .map(|s| s.score)
            .fold(0.0f64, f64::max);
        if best_score < 0.6 {
            for fr in &file_results {
                let path_lower = fr.item.path.to_lowercase();
                let path_match = keywords
                    .iter()
                    .any(|kw| kw.len() >= 3 && path_lower.contains(kw));
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
                            score: w.path_fallback_score,
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
                if changed
                    .iter()
                    .any(|c| s.symbol.path.ends_with(c) || c.ends_with(&s.symbol.path))
                {
                    s.score += w.changed_file_bonus;
                    s.reason = format!("{}, recently changed", s.reason);
                }
            }
        }

        // Demote test symbols when the user wants the implementation, not its tests.
        // Uses the symbol-table walk so inline `#[cfg(test)] mod tests` members are caught,
        // not just `test_`-prefixed names.
        if matches!(intent, Intent::Understand | Intent::Lookup) {
            let test_ids = crate::helpers::test_symbol_ids(&all_symbols);
            for s in &mut scored_symbols {
                if test_ids.contains(&s.symbol.id) {
                    s.score *= w.test_penalty;
                }
            }
        }

        // 5. Build packet
        let mut packet = build_context_packet_full(task, scored_symbols, budget, Some(intent));
        packet.retrieval = Some(retrieval);

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

        // 9. Inject project memory (facts, decisions, warnings).
        // Selective: pull only candidate bodies (query terms ∪ pinned ∪ cross-ref
        // overlap) and conflict-cluster bodies, instead of loading every entry.
        let metas = store.list_project_memory_meta().unwrap_or_default();
        if !metas.is_empty() {
            let max_memory = match budget {
                Budget::Tiny => 1,
                Budget::Small => 3,
                Budget::Medium => 5,
                _ => 8,
            };
            // Collect matched files and symbols from the code packet for cross-ref scoring
            let matched_files: Vec<String> = packet
                .items
                .iter()
                .map(|it| it.path.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            let matched_symbols: Vec<String> = packet
                .items
                .iter()
                .map(|it| it.name.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();

            // Recall set, all derived from metadata (no bodies read yet):
            //   query-term candidates ∪ active-pinned ∪ cross-ref overlap.
            let mut want: HashSet<String> = store
                .project_memory_candidates(task)
                .unwrap_or_default()
                .into_iter()
                .collect();
            for m in &metas {
                if m.status != kungfu_types::memory::MemoryStatus::Active {
                    continue;
                }
                let xref = m
                    .related_files
                    .iter()
                    .any(|rf| matched_files.iter().any(|mf| meta_path_overlap(rf, mf)))
                    || m.related_symbols
                        .iter()
                        .any(|rs| matched_symbols.iter().any(|ms| rs == ms));
                if m.pinned || xref {
                    want.insert(m.id.clone());
                }
            }

            let pool = store
                .load_project_memory_bodies(&want.into_iter().collect::<Vec<_>>())
                .unwrap_or_default();

            let ctx = kungfu_memory::project_search::SearchContext {
                matched_files: &matched_files,
                matched_symbols: &matched_symbols,
            };
            let filter = kungfu_memory::project_search::MemoryFilter::default();
            let matched = kungfu_memory::project_search::search_project_memory_with_context(
                task, &pool, &filter, &ctx,
            );

            // Also include pinned entries not already matched
            let matched_ids: HashSet<String> = matched.iter().map(|(_, e)| e.id.clone()).collect();
            let pinned: Vec<_> = pool
                .iter()
                .filter(|e| {
                    e.pinned
                        && e.status == kungfu_types::memory::MemoryStatus::Active
                        && !matched_ids.contains(&e.id)
                })
                .map(|e| (0.5, e.clone()))
                .collect();

            let mut all: Vec<_> = matched.into_iter().chain(pinned).collect();
            all.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            all.truncate(max_memory);

            packet.project_memory = all
                .into_iter()
                .map(|(score, e)| kungfu_types::context::ProjectMemoryItem {
                    id: e.id,
                    kind: e.kind.to_string(),
                    title: e.title,
                    content: e.content,
                    pinned: e.pinned,
                    relevance: score,
                })
                .collect();

            // Surface contradictions across active memory. Conflicts can only form
            // between entries sharing a tag/symbol, so cluster on metadata first and
            // load bodies only for entries in a cluster of ≥2.
            let cluster_ids = conflict_cluster_ids(&metas);
            if !cluster_ids.is_empty() {
                let cluster_pool = store
                    .load_project_memory_bodies(&cluster_ids)
                    .unwrap_or_default();
                let conflicts = kungfu_memory::project_search::detect_conflicts(&cluster_pool);
                packet.memory_conflicts = conflicts
                    .into_iter()
                    .map(|c| kungfu_types::context::MemoryConflictItem {
                        on: c.on,
                        entry_ids: c.entries.iter().map(|e| e.id.clone()).collect(),
                    })
                    .collect();
            }
        }

        Ok(packet)
    }

    /// Grep file contents for keywords, return matching symbols with the matched line.
    pub(crate) fn grep_content(
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
                file_symbols
                    .entry(sym.file_id.as_str())
                    .or_default()
                    .push(sym);
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
                    || keyword_stems[i]
                        .as_ref()
                        .is_some_and(|s| content_lower.contains(s.as_str()))
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
                if ll.contains(keyword) || stem.is_some_and(|s| ll.contains(s)) {
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
                    .filter(|s| {
                        s.span.start_line <= match_line_num && s.span.end_line >= match_line_num
                    })
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
    pub(crate) fn fill_snippets(
        &self,
        packet: &mut ContextPacket,
        max_lines: usize,
        keywords: &[&str],
    ) {
        let mut file_cache: HashMap<String, Vec<String>> = HashMap::new();
        let all_symbols = self.search().get_all_symbols().unwrap_or_default();

        // Build lookup map for O(1) span resolution
        let span_map: HashMap<(&str, &str), (usize, usize)> = all_symbols
            .iter()
            .map(|s| {
                (
                    (s.path.as_str(), s.name.as_str()),
                    (s.span.start_line, s.span.end_line),
                )
            })
            .collect();

        for item in &mut packet.items {
            let (start, end) = match span_map.get(&(item.path.as_str(), item.name.as_str())) {
                Some(&s) => s,
                None => continue,
            };

            let lines = file_cache.entry(item.path.clone()).or_insert_with(|| {
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

            // Stale span: the indexed symbol points past the current file
            // (file shrank since indexing). Skip rather than slice out of range.
            if start_idx >= lines.len() || end_idx <= start_idx {
                continue;
            }

            // Try keyword-relevant extraction first
            if !keywords.is_empty() && end_idx > start_idx {
                let relevant =
                    extract_keyword_lines(lines, start_idx, end_idx, keywords, max_lines);
                if !relevant.is_empty() {
                    item.snippet = Some(relevant);
                    continue;
                }
            }

            // Fallback: first max_lines of symbol
            let symbol_len = end_idx - start_idx;
            let take = symbol_len.min(max_lines);
            let mut snippet: Vec<String> = lines[start_idx..start_idx + take].to_vec();
            if take < symbol_len {
                snippet.push(format!(
                    "    … truncated, showing {} of {} lines",
                    take, symbol_len
                ));
            }
            if !snippet.is_empty() {
                item.snippet = Some(snippet.join("\n"));
            }
        }
    }
}

/// Blend raw vector top-K hits into the candidate pool.
///
/// - Hits below `vector_min_score` cosine are dropped (the model is guessing).
/// - Accepted hits score on a linear ramp from `vector_score` (at the minimum
///   cosine) to `vector_strong_score` (at cosine 1.0), so a high-confidence
///   semantic hit can outrank weak keyword noise but never an exact name match.
/// - Mid-band hits (below `VECTOR_TRUSTED_COS`) are additionally capped just
///   under `best_name_score`: they widen coverage but must not displace the
///   name search's best answer, however weak it scored.
/// - A hit already found by a keyword strategy keeps the stronger of the two
///   scores (a symbol must not rank lower because two strategies agree on it).
/// - At most `VECTOR_ACCEPT_MAX` new symbols enter the pool.
///
/// Returns `(contributed, added_new)`: how many candidates the vector layer
/// contributed in total (added or rescored) and how many were brand-new pool
/// entries. The latter lets callers keep sparse-pool gates (Strategies B2/B3)
/// keyed to the *keyword* pool size, so vector hits augment the pool without
/// suppressing keyword backfill.
fn blend_vector_hits(
    raw: &[(String, f32)],
    symbols_by_id: &HashMap<&str, &Symbol>,
    best_name_score: f64,
    seen_ids: &mut HashSet<String>,
    scored_symbols: &mut Vec<ScoredSymbol>,
    w: &StrategyWeights,
) -> (usize, usize) {
    let mut contributed = 0usize;
    let mut added = 0usize;
    let mut per_file: HashMap<&str, usize> = HashMap::new();
    for (id, cos) in raw {
        if added >= VECTOR_ACCEPT_MAX {
            break;
        }
        let cos = *cos as f64;
        if cos < w.vector_min_score {
            continue;
        }
        let span = (1.0 - w.vector_min_score).max(f64::EPSILON);
        let t = ((cos - w.vector_min_score) / span).clamp(0.0, 1.0);
        let mut vector_score = w.vector_score + t * (w.vector_strong_score - w.vector_score);
        if cos < VECTOR_TRUSTED_COS && best_name_score > 0.0 {
            vector_score = vector_score.min(best_name_score - VECTOR_CAP_MARGIN);
        }
        if vector_score <= 0.0 {
            continue;
        }
        let reason = format!("vector match ({:.2} cosine)", cos);
        if seen_ids.contains(id) {
            if let Some(existing) = scored_symbols
                .iter_mut()
                .find(|s| s.symbol.id == *id && s.score < vector_score)
            {
                existing.score = vector_score;
                existing.reason = reason;
                contributed += 1;
            }
            continue;
        }
        let Some(sym) = symbols_by_id.get(id.as_str()) else {
            continue;
        };
        let file_count = per_file.entry(sym.path.as_str()).or_insert(0);
        if *file_count >= VECTOR_PER_FILE_MAX {
            continue;
        }
        *file_count += 1;
        seen_ids.insert(id.clone());
        scored_symbols.push(ScoredSymbol {
            symbol: (*sym).clone(),
            score: vector_score,
            reason,
        });
        added += 1;
        contributed += 1;
    }
    (contributed, added)
}

/// Permissive overlap between a memory's `related_*` path and a packet path:
/// equal, equal basename, or one being a suffix of the other. Over-inclusive on
/// purpose — the memory scorer makes the final call; this only widens recall.
fn meta_path_overlap(a: &str, b: &str) -> bool {
    if a == b || a.ends_with(b) || b.ends_with(a) {
        return true;
    }
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    base(a) == base(b)
}

/// Ids of active entries that share a tag or related-symbol with at least one
/// other active entry — the only entries that can take part in a conflict.
/// Computed from metadata so bodies are read only for genuine clusters.
fn conflict_cluster_ids(metas: &[kungfu_storage::MemoryMeta]) -> Vec<String> {
    use kungfu_types::memory::MemoryStatus;
    let mut by_signal: HashMap<String, Vec<String>> = HashMap::new();
    for m in metas {
        if m.status != MemoryStatus::Active {
            continue;
        }
        for tag in &m.tags {
            by_signal
                .entry(format!("tag:{tag}"))
                .or_default()
                .push(m.id.clone());
        }
        for sym in &m.related_symbols {
            by_signal
                .entry(format!("sym:{sym}"))
                .or_default()
                .push(m.id.clone());
        }
    }
    let mut ids: HashSet<String> = HashSet::new();
    for group in by_signal.values() {
        if group.len() >= 2 {
            ids.extend(group.iter().cloned());
        }
    }
    ids.into_iter().collect()
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
    #[allow(clippy::needless_range_loop)]
    for i in start_idx..end_idx {
        let line_lower = lines[i].to_lowercase();
        let matches = keywords.iter().any(|kw| {
            line_lower.contains(kw) || simple_stem(kw).is_some_and(|s| line_lower.contains(&s))
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
            result.push("    … (truncated)".to_string());
            break;
        }
        if let Some(p) = prev {
            if i > p + 1 {
                result.push("    ...".to_string());
            }
        }
        // Mark keyword lines with a >>> prefix; the line itself stays verbatim
        // so snippets remain valid, greppable, copy-pasteable code.
        if hit_indices.contains(&i) {
            result.push(format!(">>> {}", lines[i]));
        } else {
            result.push(lines[i].clone());
        }
        prev = Some(i);
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kungfu_types::symbol::{Span, SymbolKind};

    fn sym(id: &str, name: &str) -> Symbol {
        sym_at(id, name, "src/test.rs")
    }

    fn sym_at(id: &str, name: &str, path: &str) -> Symbol {
        Symbol {
            id: id.to_string(),
            file_id: format!("f:{path}"),
            name: name.to_string(),
            kind: SymbolKind::Function,
            language: "rust".to_string(),
            path: path.to_string(),
            signature: None,
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

    fn blend(
        raw: &[(String, f32)],
        symbols: &[Symbol],
        scored: &mut Vec<ScoredSymbol>,
    ) -> (usize, usize) {
        let by_id: HashMap<&str, &Symbol> = symbols.iter().map(|s| (s.id.as_str(), s)).collect();
        let mut seen: HashSet<String> = scored.iter().map(|s| s.symbol.id.clone()).collect();
        // Mirror the call site: the trust cap is relative to the best score
        // already in the pool (Strategy A name matches).
        let best = scored.iter().map(|s| s.score).fold(0.0f64, f64::max);
        blend_vector_hits(
            raw,
            &by_id,
            best,
            &mut seen,
            scored,
            &StrategyWeights::default(),
        )
    }

    #[test]
    fn vector_hits_score_on_cosine_ramp_below_exact_name_matches() {
        let symbols = vec![
            sym_at("s:1", "handle_events", "src/events.rs"),
            sym_at("s:2", "poll_loop", "src/poll.rs"),
        ];
        let raw = vec![("s:1".to_string(), 1.0f32), ("s:2".to_string(), 0.66f32)];
        let mut scored = Vec::new();
        let (contributed, added) = blend(&raw, &symbols, &mut scored);
        assert_eq!((contributed, added), (2, 2));
        // cosine 1.0 -> vector_strong_score, still below an exact name match (1.0)
        // and below a phrase hit (0.95).
        assert!((scored[0].score - 0.9).abs() < 1e-9);
        assert!(scored[0].score < 0.95);
        // cosine just above the minimum -> near the vector_score floor.
        assert!(
            scored[1].score > 0.6 && scored[1].score < 0.62,
            "expected ~0.61 near the floor, got {}",
            scored[1].score
        );
    }

    #[test]
    fn vector_hits_below_min_cosine_are_dropped() {
        // 0.6 sits in the empirical bge noise band, below the 0.65 default minimum.
        let symbols = vec![sym("s:1", "handle_events")];
        let raw = vec![("s:1".to_string(), 0.6f32)];
        let mut scored = Vec::new();
        let (contributed, added) = blend(&raw, &symbols, &mut scored);
        assert_eq!((contributed, added), (0, 0));
        assert!(scored.is_empty());
    }

    #[test]
    fn vector_dedup_keeps_stronger_score_either_way() {
        let symbols = vec![sym("s:strong", "exact_hit"), sym("s:weak", "grep_hit")];
        // s:strong already in the pool from a strong name match; s:weak from grep.
        let mut scored = vec![
            ScoredSymbol {
                symbol: sym("s:strong", "exact_hit"),
                score: 0.95,
                reason: "symbol name match".to_string(),
            },
            ScoredSymbol {
                symbol: sym("s:weak", "grep_hit"),
                score: 0.45,
                reason: "content match: x".to_string(),
            },
        ];
        let raw = vec![
            ("s:strong".to_string(), 0.7f32), // ramp score ~0.67 < 0.95 -> keep name score
            ("s:weak".to_string(), 0.9f32),   // ramp score ~0.82 > 0.45 -> upgrade
        ];
        let (contributed, added) = blend(&raw, &symbols, &mut scored);
        assert_eq!(added, 0, "dedup must not add duplicate pool entries");
        assert_eq!(
            contributed, 1,
            "only the upgraded hit counts as contributed"
        );
        assert!((scored[0].score - 0.95).abs() < 1e-9);
        assert_eq!(scored[0].reason, "symbol name match");
        assert!(scored[1].score > 0.8, "weak grep hit must be upgraded");
        assert!(scored[1].reason.starts_with("vector match"));
    }

    #[test]
    fn vector_accept_is_bounded() {
        let symbols: Vec<Symbol> = (0..VECTOR_FETCH_K)
            .map(|i| {
                sym_at(
                    &format!("s:{i}"),
                    &format!("fn_{i}"),
                    &format!("src/f{i}.rs"),
                )
            })
            .collect();
        let raw: Vec<(String, f32)> = (0..VECTOR_FETCH_K)
            .map(|i| (format!("s:{i}"), 0.9f32))
            .collect();
        let mut scored = Vec::new();
        let (_, added) = blend(&raw, &symbols, &mut scored);
        assert_eq!(added, VECTOR_ACCEPT_MAX);
        assert_eq!(scored.len(), VECTOR_ACCEPT_MAX);
    }

    #[test]
    fn mid_band_hits_backfill_below_best_name_match() {
        // The name search found a weak but exact answer (0.42). A mid-band
        // vector hit (cosine < 0.75) must slot in *below* it; a trusted hit
        // (>= 0.75) keeps its full ramped score and may outrank it.
        let symbols = vec![
            sym_at("s:mid", "plausible_guess", "src/guess.rs"),
            sym_at("s:hot", "true_concept", "src/concept.rs"),
        ];
        let mut scored = vec![ScoredSymbol {
            symbol: sym_at("s:kw", "exact_but_weak", "src/answer.rs"),
            score: 0.42,
            reason: "symbol name match".to_string(),
        }];
        let raw = vec![
            ("s:mid".to_string(), 0.70f32),
            ("s:hot".to_string(), 0.80f32),
        ];
        blend(&raw, &symbols, &mut scored);
        let get = |name: &str| {
            scored
                .iter()
                .find(|s| s.symbol.name == name)
                .map(|s| s.score)
                .unwrap_or_default()
        };
        assert!(
            get("plausible_guess") < 0.42,
            "mid-band hit must not displace the best name match, got {}",
            get("plausible_guess")
        );
        assert!(
            get("true_concept") > 0.7,
            "trusted hit keeps its ramped score, got {}",
            get("true_concept")
        );
    }

    #[test]
    fn vector_admits_only_best_hit_per_file() {
        // Same-file siblings embed similarly; only the best-cosine one may enter
        // as a new candidate — the rest would crowd out keyword hits for the
        // file's main symbol.
        let symbols = vec![
            sym_at("s:1", "SearchConfig", "src/config.rs"),
            sym_at("s:2", "LanguagesConfig", "src/config.rs"),
            sym_at("s:3", "load_index", "src/store.rs"),
        ];
        let raw = vec![
            ("s:1".to_string(), 0.80f32),
            ("s:2".to_string(), 0.79f32),
            ("s:3".to_string(), 0.70f32),
        ];
        let mut scored = Vec::new();
        let (contributed, added) = blend(&raw, &symbols, &mut scored);
        assert_eq!((contributed, added), (2, 2));
        let names: Vec<&str> = scored.iter().map(|s| s.symbol.name.as_str()).collect();
        assert_eq!(names, vec!["SearchConfig", "load_index"]);
    }

    #[test]
    fn vector_ids_missing_from_symbol_table_are_skipped() {
        let symbols = vec![sym("s:1", "real")];
        let raw = vec![
            ("s:gone".to_string(), 0.99f32), // stale embedding row
            ("s:1".to_string(), 0.8f32),
        ];
        let mut scored = Vec::new();
        let (contributed, added) = blend(&raw, &symbols, &mut scored);
        assert_eq!((contributed, added), (1, 1));
        assert_eq!(scored[0].symbol.id, "s:1");
    }

    #[test]
    fn keyword_lines_mark_matched_lines_and_keep_code_verbatim() {
        let lines: Vec<String> = vec![
            "fn handle(budget: Budget) {".into(),
            "    let limit = budget.max_lines();".into(),
            "    do_work(limit);".into(),
        ];
        let out = extract_keyword_lines(&lines, 0, lines.len(), &["budget"], 40);
        // Matched lines get the >>> prefix; identifiers stay intact (no «» wrapping).
        assert!(out.contains(">>> fn handle(budget: Budget) {"));
        assert!(!out.contains('\u{ab}') && !out.contains('\u{bb}'));
    }

    #[test]
    fn keyword_lines_emit_truncation_marker_when_capped() {
        let mut lines: Vec<String> = vec!["fn big(needle: u8) {".into()];
        for i in 0..30 {
            lines.push(format!("    needle_{i}();"));
        }
        lines.push("}".into());
        let out = extract_keyword_lines(&lines, 0, lines.len(), &["needle"], 5);
        assert!(
            out.contains("(truncated)"),
            "expected truncation marker, got:\n{out}"
        );
    }
}
