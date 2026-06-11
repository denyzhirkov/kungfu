use crate::helpers::is_stop_word;
use crate::KungfuService;
use anyhow::Result;
use kungfu_types::budget::Budget;
use kungfu_types::relation::RelationKind;
use kungfu_types::symbol::Symbol;
use std::collections::HashSet;

impl KungfuService {
    /// Whether the index contains any call relations at all.
    ///
    /// Lets adapters tell "this symbol has 0 callers" apart from "the call graph
    /// was never built for this project" — the latter should steer the agent to
    /// `search_text` rather than reading an empty result as ground truth.
    pub fn has_call_graph(&self) -> Result<bool> {
        let relations = self.store().load_relations()?;
        Ok(relations.iter().any(|r| r.kind == RelationKind::Calls))
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

        // If a local embedding store exists AND a real engine is available, vector cosine
        // top-K shortcuts the keyword path. The noop engine errors on embed_batch, so
        // builds without `--features semantic` fall through to query expansion below.
        let index_dir = self.project.index_dir();
        if let Ok(Some(store)) = kungfu_embed::EmbeddingStore::load(&index_dir) {
            let engine = kungfu_embed::shared_engine();
            if let Ok(vecs) = engine.embed_batch(&[query]) {
                if let Some(qv) = vecs.first() {
                    let hits = store.top_k(qv, budget.top_k());
                    if !hits.is_empty() {
                        let all = self.search().get_all_symbols()?;
                        let by_id: std::collections::HashMap<&str, &kungfu_types::symbol::Symbol> =
                            all.iter().map(|s| (s.id.as_str(), s)).collect();
                        let items: Vec<serde_json::Value> = hits
                            .iter()
                            .filter_map(|(id, score)| by_id.get(id.as_str()).map(|s| (s, score)))
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
                        return Ok(serde_json::json!({
                            "query": query,
                            "mode": "vector",
                            "results": items,
                        }));
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
