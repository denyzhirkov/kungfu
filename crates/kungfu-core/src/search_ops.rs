use crate::helpers::is_stop_word;
use crate::KungfuService;
use anyhow::Result;
use kungfu_search::{ConfigActs, ConfigRanking};
use kungfu_types::budget::Budget;
use kungfu_types::file::Language;
use kungfu_types::relation::RelationKind;
use kungfu_types::symbol::Symbol;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Why an empty callers/callees result is empty. Adapters map each cause to a
/// distinct `status` + hint, so a silent `[]` never masquerades as ground truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmptyCallGraphCause {
    /// `[call_graph] enabled = false` — no `Calls` relations are stored at all.
    Disabled,
    /// The name is on the ubiquitous-callables stop-list; edges to it are never stored.
    FilteredUbiquitous,
    /// The index has no `Calls` relations for this project.
    NotIndexed,
    /// The name's incoming edges were built, then dropped by the frequency
    /// cutoff (`call_graph.max_caller_files`) — recorded in `call_graph_meta.json`.
    FrequencyFiltered { max_caller_files: usize },
    /// Several callable definitions share the name — ambiguous callees never
    /// get edges (precision over completeness), so callers is empty by design.
    AmbiguousTarget { definitions: usize },
    /// The graph exists and none of the above applies — genuinely no edges.
    NoEdges,
}

impl KungfuService {
    /// Whether the index contains any call relations at all.
    ///
    /// Lets adapters tell "this symbol has 0 callers" apart from "the call graph
    /// was never built for this project" — the latter should steer the agent to
    /// a textual search rather than reading an empty result as ground truth.
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
        parts.push(
            "edges from files not re-parsed since the last full index may be missing — \
             `kungfu index --full` rebuilds them"
                .to_string(),
        );
        parts.join("; ")
    }

    /// Diagnose why `callers(name)` came back empty, from data already in the
    /// process cache (symbols + relations) plus the small `call_graph_meta.json`
    /// sidecar. Order matters: exact recorded evidence (the frequency cutoff
    /// fired for this name) beats graph-level checks, and the ambiguity
    /// heuristic (many same-name definitions) beats a plain `no_edges`. A
    /// missing sidecar (index written by an older binary) simply never yields
    /// `FrequencyFiltered`.
    ///
    /// A name that is not in the index at all falls through to `NoEdges` —
    /// unchanged pre-existing behavior for not-found symbols.
    pub fn diagnose_empty_callers(&self, name: &str) -> Result<EmptyCallGraphCause> {
        if !self.call_graph_enabled() {
            return Ok(EmptyCallGraphCause::Disabled);
        }
        if self.is_ubiquitous_callee(name) {
            return Ok(EmptyCallGraphCause::FilteredUbiquitous);
        }
        // Exact recorded evidence outranks the graph-emptiness check: the
        // cutoff can drop every edge of a small project, and "the cutoff fired
        // for this name" is still the honest answer there.
        if self
            .store()
            .load_call_graph_meta()
            .frequency_dropped_callees
            .iter()
            .any(|n| n == name)
        {
            return Ok(EmptyCallGraphCause::FrequencyFiltered {
                max_caller_files: self.project.config.call_graph.max_caller_files,
            });
        }
        if !self.has_call_graph()? {
            return Ok(EmptyCallGraphCause::NotIndexed);
        }
        let definitions = self.callable_definition_count(name)?;
        if definitions > 1 {
            return Ok(EmptyCallGraphCause::AmbiguousTarget { definitions });
        }
        Ok(EmptyCallGraphCause::NoEdges)
    }

    /// Diagnose why `callees(name)` came back empty. The callers-side causes
    /// (ambiguity, frequency cutoff) describe edges INTO a name; they cannot
    /// empty an outgoing-edge query — `callees` unions the outgoing edges of
    /// every same-name source symbol — so they are deliberately not reported
    /// here: only causes that can actually produce the emptiness are named.
    pub fn diagnose_empty_callees(&self, name: &str) -> Result<EmptyCallGraphCause> {
        if !self.call_graph_enabled() {
            return Ok(EmptyCallGraphCause::Disabled);
        }
        if self.is_ubiquitous_callee(name) {
            return Ok(EmptyCallGraphCause::FilteredUbiquitous);
        }
        if !self.has_call_graph()? {
            return Ok(EmptyCallGraphCause::NotIndexed);
        }
        Ok(EmptyCallGraphCause::NoEdges)
    }

    /// How many callable (function/method) definitions share this name —
    /// the same population the call-graph builder resolves callees against.
    fn callable_definition_count(&self, name: &str) -> Result<usize> {
        use kungfu_types::symbol::SymbolKind;
        Ok(self
            .search()
            .get_all_symbols()?
            .iter()
            .filter(|s| {
                s.name == name && matches!(s.kind, SymbolKind::Function | SymbolKind::Method)
            })
            .count())
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
        let ranking = self.search().config_ranking(query, None)?;

        // If a local embedding store exists AND a real engine is available, vector cosine
        // top-K shortcuts the keyword path. The noop engine errors on embed_batch, so
        // builds without `--features semantic` fall through to query expansion below.
        let index_dir = self.project.index_dir();
        if let Ok(Some(store)) = kungfu_embed::EmbeddingStore::load(&index_dir) {
            let engine = kungfu_embed::shared_engine();
            if let Ok(vecs) = engine.embed_batch(&[query]) {
                if let Some(qv) = vecs.first() {
                    let all = self.search().get_all_symbols()?;
                    let by_id: std::collections::HashMap<&str, &kungfu_types::symbol::Symbol> =
                        all.iter().map(|s| (s.id.as_str(), s)).collect();
                    let files = self.search().get_all_files()?;
                    let file_by_id: std::collections::HashMap<
                        &str,
                        &kungfu_types::file::FileEntry,
                    > = files.iter().map(|f| (f.id.as_str(), f)).collect();
                    let hits = weighted_vector_hits(
                        &store,
                        qv,
                        budget.top_k(),
                        ranking,
                        &by_id,
                        &file_by_id,
                    );
                    if !hits.is_empty() {
                        let test_ids = crate::helpers::test_symbol_ids(&all);
                        let (mut scored, config_acts) =
                            score_vector_hits(&hits, &by_id, &file_by_id, &test_ids, ranking);
                        scored.sort_by(|a, b| {
                            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                        });
                        scored.truncate(budget.top_k());
                        let items: Vec<serde_json::Value> = scored
                            .into_iter()
                            .map(|(mut item, score)| {
                                item["score"] = serde_json::json!(score);
                                item
                            })
                            .collect();
                        let embedded = store
                            .manifest()
                            .offsets
                            .keys()
                            .filter(|id| id.starts_with("s:"))
                            .count();
                        let total = all.len();
                        let mut out = serde_json::json!({
                            "query": query,
                            "mode": "vector",
                            "coverage": { "embedded": embedded, "indexed_symbols": total },
                            "results": items,
                        });
                        if embedded < total {
                            out["note"] = serde_json::json!(format!(
                                "vectors cover {embedded}/{total} symbols — a sync may be catching up; unembedded symbols are reachable via find_symbol/find_files"
                            ));
                        }
                        if let Some(note) = ranking.note(budget, config_acts) {
                            out["config_ranking"] = serde_json::json!(note);
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

        // Search with original keywords. The config ranking stays the one decided
        // from the user's query: expanded terms ("settings", "env") must not lift it.
        let keyword_query = keywords.join(" ");
        let direct = search.find_symbol_ranked(&keyword_query, Budget::Full, None, ranking)?;
        let mut config_acts = direct.acts();
        for r in direct.hits {
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
            let expanded =
                search.find_symbol_ranked(&expanded_query, Budget::Full, None, ranking)?;
            let acts = expanded.acts();
            config_acts.penalized |= acts.penalized;
            config_acts.lifted |= acts.lifted;
            for r in expanded.hits {
                // The quality gate reads the score after the config penalty (same
                // order as ask_context B3): a demoted config hit needs a stronger
                // match to enter at all.
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

        let mut out = serde_json::json!({
            "query": query,
            "mode": "keyword_fallback",
            "hint": "Lexical match + query expansion (no vector embeddings). Run `kungfu embeddings build` for semantic ranking.",
            "keywords": keywords,
            "expanded_terms": new_terms,
            "results": results,
        });
        if let Some(note) = ranking.note(budget, config_acts) {
            out["config_ranking"] = serde_json::json!(note);
        }
        Ok(out)
    }
}

/// The vector fetch for semantic_search: over-fetched so the test demotion
/// cannot empty the page, with config vectors weighted before the store's cut
/// so they cannot take slots from code.
fn weighted_vector_hits(
    store: &kungfu_embed::EmbeddingStore,
    query: &[f32],
    top_k: usize,
    ranking: ConfigRanking,
    by_id: &HashMap<&str, &Symbol>,
    file_by_id: &HashMap<&str, &kungfu_types::file::FileEntry>,
) -> Vec<(String, f32)> {
    store.top_k_weighted(query, top_k * 2, |id| {
        crate::helpers::config_vector_weight(
            ranking,
            by_id.get(id).copied(),
            file_by_id.get(id).map(|f| f.path.as_str()),
        )
    })
}

/// Score raw vector top-K hits for semantic_search. The store holds symbol
/// vectors (s:) and file vectors (f:, path + purpose + tags) side by side;
/// both rank in one list. Returns the scored items and what the config
/// policy did to them.
fn score_vector_hits(
    hits: &[(String, f32)],
    by_id: &HashMap<&str, &Symbol>,
    file_by_id: &HashMap<&str, &kungfu_types::file::FileEntry>,
    test_ids: &HashSet<String>,
    ranking: ConfigRanking,
) -> (Vec<(serde_json::Value, f64)>, ConfigActs) {
    let mut acts = ConfigActs::default();
    let scored = hits
        .iter()
        .filter_map(|(id, cos)| {
            let mut score = *cos as f64;
            if let Some(s) = by_id.get(id.as_str()) {
                // Tests often describe the implementation in their names, so
                // cosine loves them. Milder demotion than the keyword path — a
                // strong semantic hit on a test can still surface.
                if test_ids.contains(id) {
                    score *= 0.7;
                }
                // Cosine is not a name match, so no exact-name exemption.
                let is_config = s.kind.is_config();
                acts.record(ranking.act(is_config, &s.path));
                score *= ranking.factor(is_config, &s.path);
                Some((
                    serde_json::json!({
                        "name": s.name,
                        "kind": s.kind.to_string(),
                        "path": s.path,
                        "line": s.span.start_line,
                        "match_type": "vector",
                    }),
                    score,
                ))
            } else {
                file_by_id.get(id.as_str()).map(|f| {
                    let is_config = Language::from_path(Path::new(&f.path)).is_config();
                    acts.record(ranking.act(is_config, &f.path));
                    score *= ranking.factor(is_config, &f.path);
                    (
                        serde_json::json!({
                            "kind": "file",
                            "path": f.path,
                            "purpose": f.purpose,
                            "tags": f.tags,
                            "match_type": "vector-file",
                        }),
                        score,
                    )
                })
            }
        })
        .collect();
    (scored, acts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kungfu_types::file::FileEntry;
    use kungfu_types::symbol::{Span, SymbolKind};

    fn sym(id: &str, kind: SymbolKind, path: &str) -> Symbol {
        Symbol {
            id: id.to_string(),
            file_id: format!("f:{path}"),
            name: id.to_string(),
            kind,
            language: "x".to_string(),
            path: path.to_string(),
            signature: None,
            span: Span {
                start_line: 1,
                end_line: 1,
                start_col: 0,
                end_col: 0,
            },
            parent_symbol_id: None,
            exported: true,
            visibility: None,
            doc_summary: None,
        }
    }

    fn file(id: &str, path: &str) -> FileEntry {
        FileEntry {
            id: id.to_string(),
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
        }
    }

    fn score(ranking: ConfigRanking) -> (HashMap<String, f64>, ConfigActs) {
        let symbols = [
            sym("s:code", SymbolKind::Function, "src/net.rs"),
            sym("s:key", SymbolKind::ConfigKey, "package.json"),
        ];
        let files = [file("f:pkg", "package.json"), file("f:src", "src/net.rs")];
        let by_id: HashMap<&str, &Symbol> = symbols.iter().map(|s| (s.id.as_str(), s)).collect();
        let file_by_id: HashMap<&str, &FileEntry> =
            files.iter().map(|f| (f.id.as_str(), f)).collect();
        let hits: Vec<(String, f32)> = ["s:code", "s:key", "f:pkg", "f:src"]
            .iter()
            .map(|id| (id.to_string(), 0.8f32))
            .collect();
        let (scored, acted) =
            score_vector_hits(&hits, &by_id, &file_by_id, &HashSet::new(), ranking);
        let by_path_kind = scored
            .into_iter()
            .map(|(v, s)| {
                let key = format!(
                    "{}|{}",
                    v["kind"].as_str().unwrap(),
                    v["path"].as_str().unwrap()
                );
                (key, s)
            })
            .collect();
        (by_path_kind, acted)
    }

    #[test]
    fn vector_hits_penalise_config_symbols_and_config_files() {
        let (s, acts) = score(ConfigRanking::Penalized);
        assert!(acts.penalized);
        let cos = 0.8f32 as f64;
        assert!((s["function|src/net.rs"] - cos).abs() < 1e-9);
        assert!((s["file|src/net.rs"] - cos).abs() < 1e-9);
        assert!((s["config_key|package.json"] - cos * kungfu_search::CONFIG_PENALTY).abs() < 1e-9);
        assert!((s["file|package.json"] - cos * kungfu_search::CONFIG_PENALTY).abs() < 1e-9);
    }

    /// Round 3 scenario: at small budget (5 → 10 fetched) ten locale keys at
    /// cosine 0.85 would fill the window; weighted before the cut (0.51) they
    /// cannot keep `process_user_login` (0.80) out. The full engine cannot run
    /// in tests (no model), so this drives the store fetch and the scoring that
    /// semantic_search composes, on a real on-disk store.
    #[test]
    fn locale_vectors_cannot_push_code_out_of_the_semantic_window() {
        let tmp =
            std::env::temp_dir().join(format!("kungfu_core_sem_window_{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        std::fs::create_dir_all(&tmp).unwrap();
        let mut symbols: Vec<Symbol> = (0..10)
            .map(|i| {
                sym(
                    &format!("s:l{i:02}"),
                    SymbolKind::ConfigKey,
                    &format!("locales/l{i:02}.json"),
                )
            })
            .collect();
        symbols.push(sym("s:login", SymbolKind::Function, "src/auth.rs"));
        let mut m = kungfu_embed::EmbeddingManifest::new("test-model", 2);
        for s in &symbols {
            let cos: f32 = if s.kind.is_config() { 0.85 } else { 0.80 };
            let v = [cos, (1.0 - cos * cos).sqrt()];
            kungfu_embed::append_vector(&tmp, &mut m, &s.id, &s.id, &v).unwrap();
        }
        m.save(&tmp).unwrap();
        let store = kungfu_embed::EmbeddingStore::load(&tmp).unwrap().unwrap();
        let by_id: HashMap<&str, &Symbol> = symbols.iter().map(|s| (s.id.as_str(), s)).collect();
        let no_files = HashMap::new();
        let top_k = Budget::Small.top_k();

        // Unweighted the code hit misses the 10-slot window entirely.
        assert!(!store
            .top_k(&[1.0, 0.0], top_k * 2)
            .iter()
            .any(|h| h.0 == "s:login"));

        let hits = weighted_vector_hits(
            &store,
            &[1.0, 0.0],
            top_k,
            ConfigRanking::Penalized,
            &by_id,
            &no_files,
        );
        assert_eq!(hits[0].0, "s:login");
        let (mut scored, acted) = score_vector_hits(
            &hits,
            &by_id,
            &no_files,
            &HashSet::new(),
            ConfigRanking::Penalized,
        );
        assert!(acted.penalized);
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        assert_eq!(scored[0].0["path"], "src/auth.rs");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn vector_hits_keep_equal_terms_when_lifted() {
        let (s, acts) = score(ConfigRanking::ConfigHeavy);
        assert!(acts.lifted, "a share lift is declared");
        assert_eq!(s["config_key|package.json"], s["function|src/net.rs"]);

        let (s, acts) = score(ConfigRanking::QueryWantsConfig);
        assert_eq!(acts, ConfigActs::default());
        assert_eq!(s["file|package.json"], s["file|src/net.rs"]);
    }
}
