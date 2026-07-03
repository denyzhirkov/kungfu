use crate::helpers::is_stop_word;
use crate::KungfuService;
use anyhow::Result;
use kungfu_types::budget::Budget;
use kungfu_types::relation::RelationKind;
use kungfu_types::symbol::Symbol;
use std::collections::{HashMap, HashSet};

impl KungfuService {
    /// Whether the index contains any call relations at all.
    ///
    /// Lets adapters tell "this symbol has 0 callers" apart from "the call graph
    /// was never built for this project" — the latter should steer the agent to
    /// `search_text` rather than reading an empty result as ground truth.
    pub fn has_call_graph(&self) -> Result<bool> {
        let relations = self.store().relations_arc()?;
        Ok(relations.iter().any(|r| r.kind == RelationKind::Calls))
    }

    /// Is the call graph enabled by configuration at all?
    pub fn call_graph_enabled(&self) -> bool {
        self.project.config.call_graph.enabled
    }

    /// Is this name on the ubiquitous-callables stop-list (in any language)?
    /// Such names never get call edges by design — adapters must say so instead
    /// of returning a silent empty result.
    pub fn is_ubiquitous_callee(&self, name: &str) -> bool {
        kungfu_index::is_stoplisted_name(name)
    }

    /// Config-aware description of how call-graph edges are derived and
    /// filtered. Attached as `provenance` to callers/callees/affected responses
    /// so a filtered result never masquerades as an exhaustive one.
    pub fn call_graph_provenance(&self) -> String {
        let cfg = &self.project.config.call_graph;
        if !cfg.enabled {
            return "call graph disabled by config ([call_graph] enabled = false) — no Calls \
                    relations are stored"
                .to_string();
        }
        let mut parts = vec![
            "AST call graph; an edge exists only when the callee name resolves unambiguously — \
             ambiguous names (several same-name definitions) are omitted, not guessed"
                .to_string(),
            "ubiquitous std/utility callee names are filtered per language".to_string(),
        ];
        if cfg.cross_file_only {
            parts.push("cross-file edges only (same-file calls are not stored)".to_string());
        }
        if cfg.max_caller_files > 0 {
            parts.push(format!(
                "callees invoked from more than {} distinct files are dropped as utility-noise",
                cfg.max_caller_files
            ));
        }
        parts.join("; ")
    }

    /// Find all symbols that call the given symbol (callers / "who calls this?").
    pub fn callers(&self, name: &str, budget: Budget) -> Result<Vec<(Symbol, String)>> {
        let budget = self.resolve_budget(budget);
        let relations = self.store().relations_arc()?;
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

        let by_id: HashMap<&str, &Symbol> =
            all_symbols.iter().map(|s| (s.id.as_str(), s)).collect();

        // Find Calls relations where target is our symbol
        let caller_ids = relations
            .iter()
            .filter(|r| r.kind == RelationKind::Calls && target_ids.contains(r.target_id.as_str()))
            .map(|r| r.source_id.as_str());

        let mut results: Vec<(Symbol, String)> = Vec::new();
        let mut seen = HashSet::new();
        for caller_id in caller_ids {
            if !seen.insert(caller_id) {
                continue;
            }
            if let Some(sym) = by_id.get(caller_id) {
                results.push(((*sym).clone(), format!("calls {}", name)));
                if results.len() >= budget.top_k() {
                    break;
                }
            }
        }

        Ok(results)
    }

    /// Find all symbols that the given symbol calls (callees / "what does this call?").
    pub fn callees(&self, name: &str, budget: Budget) -> Result<Vec<(Symbol, String)>> {
        let budget = self.resolve_budget(budget);
        let relations = self.store().relations_arc()?;
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

        let by_id: HashMap<&str, &Symbol> =
            all_symbols.iter().map(|s| (s.id.as_str(), s)).collect();

        // Find Calls relations where source is our symbol
        let callee_ids = relations
            .iter()
            .filter(|r| r.kind == RelationKind::Calls && source_ids.contains(r.source_id.as_str()))
            .map(|r| r.target_id.as_str());

        let mut results: Vec<(Symbol, String)> = Vec::new();
        let mut seen = HashSet::new();
        for callee_id in callee_ids {
            if !seen.insert(callee_id) {
                continue;
            }
            if let Some(sym) = by_id.get(callee_id) {
                results.push(((*sym).clone(), format!("called by {}", name)));
                if results.len() >= budget.top_k() {
                    break;
                }
            }
        }

        Ok(results)
    }

    /// Semantic search: expand query with related concepts, then search symbols.
    pub fn semantic_search(&self, query: &str, budget: Budget) -> Result<serde_json::Value> {
        let budget = self.resolve_budget(budget);

        // If a local embedding store exists AND a real engine is available, vector cosine
        // top-K shortcuts the keyword path. The noop engine errors on embed_batch, so
        // builds without `--features semantic` fall through to query expansion below.
        let index_dir = self.project.index_dir();
        if let Ok(Some(store)) = kungfu_embed::EmbeddingStore::load(&index_dir) {
            let engine = kungfu_embed::shared_engine();
            if let Ok(vecs) = engine.embed_batch(&[query]) {
                if let Some(qv) = vecs.first() {
                    // Over-fetch so the test demotion below can't empty the page.
                    let hits = store.top_k(qv, budget.top_k() * 2);
                    if !hits.is_empty() {
                        let all = self.search().get_all_symbols()?;
                        let by_id: std::collections::HashMap<&str, &kungfu_types::symbol::Symbol> =
                            all.iter().map(|s| (s.id.as_str(), s)).collect();
                        // Tests often describe the implementation in their names, so
                        // cosine loves them. Milder demotion than the keyword path —
                        // a strong semantic hit on a test can still surface.
                        let test_ids = crate::helpers::test_symbol_ids(&all);
                        let mut scored: Vec<(&kungfu_types::symbol::Symbol, f64)> = hits
                            .iter()
                            .filter_map(|(id, score)| {
                                by_id.get(id.as_str()).map(|s| {
                                    let mut score = *score as f64;
                                    if test_ids.contains(id) {
                                        score *= 0.7;
                                    }
                                    (*s, score)
                                })
                            })
                            .collect();
                        scored.sort_by(|a, b| {
                            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                        });
                        scored.truncate(budget.top_k());
                        let items: Vec<serde_json::Value> = scored
                            .iter()
                            .map(|(s, score)| {
                                serde_json::json!({
                                    "name": s.name,
                                    "kind": s.kind.to_string(),
                                    "path": s.path,
                                    "line": s.span.start_line,
                                    "score": score,
                                    "match_type": "vector",
                                })
                            })
                            .collect();
                        let embedded = store.len();
                        let total = all.len();
                        let mut out = serde_json::json!({
                            "query": query,
                            "mode": "vector",
                            "coverage": { "embedded": embedded, "indexed_symbols": total },
                            "results": items,
                        });
                        if embedded < total {
                            out["note"] = serde_json::json!(format!(
                                "vectors cover {embedded}/{total} symbols — a sync may be catching up; unembedded symbols are reachable via find_symbol/search_text"
                            ));
                        }
                        return Ok(out);
                    }
                }
            }
        }

        let query_lower = query.to_lowercase();
        let words: Vec<&str> = query_lower.split_whitespace().collect();

        let keywords: Vec<&str> = words.iter().filter(|w| !is_stop_word(w)).copied().collect();

        let expanded = kungfu_search::expand_query(&keywords);
        let new_terms: Vec<&str> = expanded
            .iter()
            .filter(|t| !keywords.contains(&t.as_str()))
            .map(|t| t.as_str())
            .collect();

        let search = self.search();
        let mut results = Vec::new();
        let mut seen = HashSet::new();

        // Lexical name matches pull in test helpers that merely share words with the
        // implementation. Demote them so concept search surfaces real code first.
        let test_ids = crate::helpers::test_symbol_ids(&self.search().get_all_symbols()?);
        const TEST_PENALTY: f64 = 0.5;

        // Search with original keywords
        let keyword_query = keywords.join(" ");
        for r in search.find_symbol(&keyword_query, Budget::Full)? {
            if seen.insert(r.item.id.clone()) {
                let score = if test_ids.contains(&r.item.id) {
                    r.score * TEST_PENALTY
                } else {
                    r.score
                };
                results.push(serde_json::json!({
                    "name": r.item.name,
                    "kind": r.item.kind.to_string(),
                    "path": r.item.path,
                    "line": r.item.span.start_line,
                    "score": score,
                    "match_type": "direct",
                }));
            }
        }

        // Search with expanded terms
        if !new_terms.is_empty() {
            let expanded_query = new_terms.join(" ");
            for r in search.find_symbol(&expanded_query, Budget::Full)? {
                if seen.insert(r.item.id.clone()) && r.score >= 0.5 {
                    let mut score = r.score * 0.6;
                    if test_ids.contains(&r.item.id) {
                        score *= TEST_PENALTY;
                    }
                    results.push(serde_json::json!({
                        "name": r.item.name,
                        "kind": r.item.kind.to_string(),
                        "path": r.item.path,
                        "line": r.item.span.start_line,
                        "score": score,
                        "match_type": "semantic",
                    }));
                }
            }
        }

        // Sort by score and truncate
        results.sort_by(|a, b| {
            b["score"]
                .as_f64()
                .unwrap_or(0.0)
                .partial_cmp(&a["score"].as_f64().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(budget.top_k());

        Ok(serde_json::json!({
            "query": query,
            "mode": "keyword_fallback",
            "hint": "Lexical match + query expansion (no vector embeddings). Run `kungfu embeddings build` for semantic ranking.",
            "keywords": keywords,
            "expanded_terms": new_terms,
            "results": results,
        }))
    }
}
