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
    /// Strategy A2: base score for vector (cosine) matches when an embedding store exists.
    /// Kept lower than direct symbol-name matches so vector results augment rather than displace.
    pub vector_score: f64,
    /// Strategy A2: minimum cosine to accept a vector hit. Below this, the model is essentially
    /// guessing and a string match is more trustworthy.
    pub vector_min_score: f64,
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
            vector_score: 0.55,
            vector_min_score: 0.55,
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
        // store + a real engine are available. Skipped for very short queries (which look like
        // direct symbol lookups, where vectors mostly add noise on top of Strategy A).
        if keywords.len() >= 3 {
            if let Ok(Some(emb_store)) =
                kungfu_embed::EmbeddingStore::load(&self.project.index_dir())
            {
                let engine = kungfu_embed::shared_engine();
                if engine.is_real() {
                    if let Ok(qv) = engine.embed_batch(&[task]) {
                        if let Some(q) = qv.first() {
                            let raw = emb_store.top_k(q, 8);
                            let symbols_by_id: HashMap<&str, &Symbol> =
                                all_symbols.iter().map(|s| (s.id.as_str(), s)).collect();
                            let mut added = 0usize;
                            for (rank, (id, cos)) in raw.into_iter().enumerate() {
                                if added >= 5 {
                                    break;
                                }
                                let cos = cos as f64;
                                if cos < w.vector_min_score {
                                    continue;
                                }
                                if seen_ids.contains(&id) {
                                    continue;
                                }
                                let sym = match symbols_by_id.get(id.as_str()) {
                                    Some(s) => *s,
                                    None => continue,
                                };
                                let rank_decay = 1.0 - (rank as f64 * 0.08).min(0.4);
                                seen_ids.insert(id);
                                scored_symbols.push(ScoredSymbol {
                                    symbol: sym.clone(),
                                    score: w.vector_score * rank_decay,
                                    reason: format!("vector match ({:.2} cosine)", cos),
                                });
                                added += 1;
                            }
                        }
                    }
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

        // File-level fallback: if best symbol score is weak, inject file-level results
        let best_score = scored_symbols
            .iter()
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
