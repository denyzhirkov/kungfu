#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use kungfu_types::budget::Budget;
use kungfu_types::context::{ContextItem, ContextItemType, ContextPacket};
use kungfu_types::symbol::Symbol;
use std::collections::HashMap;

// --- Diversity (marginal-relevance) tuning ---
//
// Packet slots are selected greedily: each round, every remaining candidate's
// effective score is its base score decayed by what the packet already holds
// from the same file and the same directory. Multiplicative decay (score × f^n)
// was chosen over a subtractive penalty because candidate scores come from
// heterogeneous strategies with different scales (symbol-name match ~1.0,
// grep content ~0.45, vector ~0.55): a fixed subtraction would zero out the
// weak-scored strategies while barely denting strong ones, whereas a
// multiplier is scale-free. It also preserves legitimately concentrated
// answers naturally: when one file's fragments outscore every competitor by a
// wide margin, repeated decay still leaves them on top.

/// Score multiplier applied once per already-selected fragment from the same
/// file. Strong: a second fragment from a file the packet already covers must
/// beat a fresh file scoring half as much.
pub const SAME_FILE_DECAY: f64 = 0.5;

/// Score multiplier applied once per already-selected fragment from the same
/// directory but a *different* file. Weaker: adjacent files are related, not
/// redundant. Same-file picks pay only the file decay, never both.
pub const SAME_DIR_DECAY: f64 = 0.85;

/// Hard cap on the candidate pool scanned by the greedy diversity pass.
/// Keeps assembly O(top_k × cap) regardless of how many candidates the
/// strategies produced.
const MAX_DIVERSITY_CANDIDATES: usize = 200;

pub fn build_context_packet(
    query: &str,
    symbols: Vec<(Symbol, f64)>,
    budget: Budget,
) -> ContextPacket {
    build_context_packet_with_intent(query, symbols, budget, None)
}

/// Scored symbol with a reason string for context packet.
pub struct ScoredSymbol {
    pub symbol: Symbol,
    pub score: f64,
    pub reason: String,
}

pub fn build_context_packet_with_intent(
    query: &str,
    symbols: Vec<(Symbol, f64)>,
    budget: Budget,
    intent: Option<kungfu_types::context::Intent>,
) -> ContextPacket {
    let scored: Vec<ScoredSymbol> = symbols
        .into_iter()
        .map(|(sym, score)| ScoredSymbol {
            symbol: sym,
            score,
            reason: String::new(),
        })
        .collect();
    build_context_packet_full(query, scored, budget, intent)
}

pub fn build_context_packet_full(
    query: &str,
    symbols: Vec<ScoredSymbol>,
    budget: Budget,
    intent: Option<kungfu_types::context::Intent>,
) -> ContextPacket {
    let selected = select_diverse(symbols, budget.top_k(), SAME_FILE_DECAY, SAME_DIR_DECAY);

    let items: Vec<ContextItem> = selected
        .into_iter()
        .map(|s| {
            let why = if s.reason.is_empty() {
                format!("score {:.2}", s.score)
            } else {
                s.reason
            };
            ContextItem {
                item_type: ContextItemType::Symbol,
                path: s.symbol.path,
                name: s.symbol.name,
                signature: s.symbol.signature,
                why,
                score: s.score,
                snippet: None,
            }
        })
        .collect();

    ContextPacket {
        query: query.to_string(),
        budget,
        intent,
        items,
        changed_files: Vec::new(),
        rationale: Vec::new(),
        history: Vec::new(),
        evidence: Vec::new(),
        project_memory: Vec::new(),
        memory_conflicts: Vec::new(),
        retrieval: None,
    }
}

/// Greedy marginal-relevance selection: sort by score, dedupe by (path, name),
/// then fill `top_k` slots picking the candidate with the highest *effective*
/// score — base score decayed by how much the packet already contains from the
/// same file (`file_decay^n`) and the same directory via other files
/// (`dir_decay^m`). Items keep their base score; only selection order and
/// membership are affected.
fn select_diverse(
    mut symbols: Vec<ScoredSymbol>,
    top_k: usize,
    file_decay: f64,
    dir_decay: f64,
) -> Vec<ScoredSymbol> {
    symbols.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Deduplicate by (path, name) — keep highest-scored entry
    let mut seen = std::collections::HashSet::new();
    let mut candidates: Vec<ScoredSymbol> = symbols
        .into_iter()
        .filter(|s| seen.insert((s.symbol.path.clone(), s.symbol.name.clone())))
        .take(MAX_DIVERSITY_CANDIDATES)
        .collect();

    let mut selected: Vec<ScoredSymbol> = Vec::with_capacity(top_k.min(candidates.len()));
    let mut per_file: HashMap<String, usize> = HashMap::new();
    let mut per_dir: HashMap<String, usize> = HashMap::new();

    while selected.len() < top_k && !candidates.is_empty() {
        let mut best_idx = 0usize;
        let mut best_eff = f64::NEG_INFINITY;
        for (i, c) in candidates.iter().enumerate() {
            let n_file = per_file.get(&c.symbol.path).copied().unwrap_or(0);
            let n_dir_total = per_dir.get(dir_of(&c.symbol.path)).copied().unwrap_or(0);
            // Same-file picks already pay the (stronger) file decay; only
            // fragments from *other* files in the directory add the dir decay.
            let n_dir_other = n_dir_total.saturating_sub(n_file);
            let eff = c.score * file_decay.powi(n_file as i32) * dir_decay.powi(n_dir_other as i32);
            // Strict `>` keeps the earliest (highest base score) on ties.
            if eff > best_eff {
                best_eff = eff;
                best_idx = i;
            }
        }
        let chosen = candidates.remove(best_idx);
        *per_file.entry(chosen.symbol.path.clone()).or_insert(0) += 1;
        *per_dir
            .entry(dir_of(&chosen.symbol.path).to_string())
            .or_insert(0) += 1;
        selected.push(chosen);
    }

    selected
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kungfu_types::symbol::{Span, SymbolKind};

    fn sym(path: &str, name: &str) -> Symbol {
        Symbol {
            id: format!("{path}::{name}"),
            file_id: path.to_string(),
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

    fn scored(path: &str, name: &str, score: f64) -> ScoredSymbol {
        ScoredSymbol {
            symbol: sym(path, name),
            score,
            reason: String::new(),
        }
    }

    fn paths(packet: &ContextPacket) -> Vec<&str> {
        packet.items.iter().map(|i| i.path.as_str()).collect()
    }

    #[test]
    fn same_file_decay_promotes_second_file() {
        // Three fragments of a.rs crowd out b.rs under pure top-k (Tiny = 3 slots).
        let candidates = vec![
            scored("src/a.rs", "one", 1.0),
            scored("src/a.rs", "two", 0.95),
            scored("src/a.rs", "three", 0.9),
            scored("other/b.rs", "answer", 0.7),
        ];
        let packet = build_context_packet_full("q", candidates, Budget::Tiny, None);
        assert_eq!(packet.items.len(), 3);
        // After a.rs::one (1.0), a.rs::two decays to 0.475 < b.rs 0.7.
        assert_eq!(paths(&packet), vec!["src/a.rs", "other/b.rs", "src/a.rs"]);
        // Items keep their base score — the decay affects selection, not honesty.
        assert!((packet.items[1].score - 0.7).abs() < 1e-9);
    }

    #[test]
    fn directory_decay_promotes_other_directory() {
        // a/x.rs picked first; a/y.rs (0.9 × 0.85 = 0.765) loses to b/z.rs (0.8).
        let candidates = vec![
            scored("a/x.rs", "x", 1.0),
            scored("a/y.rs", "y", 0.9),
            scored("b/z.rs", "z", 0.8),
        ];
        let packet = build_context_packet_full("q", candidates, Budget::Tiny, None);
        assert_eq!(paths(&packet), vec!["a/x.rs", "b/z.rs", "a/y.rs"]);
    }

    #[test]
    fn same_file_and_dir_decay_do_not_stack_on_one_pick() {
        // Second fragment of a/x.rs pays only the file decay (0.9 × 0.5 = 0.45),
        // not file × dir (0.9 × 0.5 × 0.85 = 0.3825): it must beat a 0.4 rival.
        let candidates = vec![
            scored("a/x.rs", "x1", 1.0),
            scored("a/x.rs", "x2", 0.9),
            scored("b/y.rs", "y", 0.4),
        ];
        let packet = build_context_packet_full("q", candidates, Budget::Tiny, None);
        assert_eq!(paths(&packet), vec!["a/x.rs", "a/x.rs", "b/y.rs"]);
    }

    #[test]
    fn concentrated_answer_still_dominates() {
        // Single-file lookup: the right file's fragments outscore noise by a wide
        // margin, so decay must not starve the packet of them.
        let candidates = vec![
            scored("src/answer.rs", "main_fn", 1.0),
            scored("src/answer.rs", "helper", 0.98),
            scored("src/answer.rs", "detail", 0.96),
            scored("noise/other.rs", "unrelated", 0.2),
            scored("noise/more.rs", "unrelated2", 0.15),
        ];
        let packet = build_context_packet_full("q", candidates, Budget::Tiny, None);
        // 0.98 × 0.5 = 0.49 and 0.96 × 0.25 = 0.24 both beat the 0.2 noise.
        assert_eq!(
            paths(&packet),
            vec!["src/answer.rs", "src/answer.rs", "src/answer.rs"]
        );
    }

    #[test]
    fn budget_top_k_still_trims() {
        let candidates: Vec<ScoredSymbol> = (0..20)
            .map(|i| scored(&format!("src/f{i}.rs"), "f", 1.0 - i as f64 * 0.01))
            .collect();
        let packet = build_context_packet_full("q", candidates, Budget::Small, None);
        assert_eq!(packet.items.len(), Budget::Small.top_k());
    }

    #[test]
    fn dedupe_keeps_highest_scored_entry() {
        let candidates = vec![
            scored("src/a.rs", "same", 0.4),
            scored("src/a.rs", "same", 0.9),
            scored("src/b.rs", "other", 0.5),
        ];
        let packet = build_context_packet_full("q", candidates, Budget::Tiny, None);
        assert_eq!(packet.items.len(), 2);
        let a = packet
            .items
            .iter()
            .find(|i| i.name == "same")
            .map(|i| i.score);
        assert_eq!(a, Some(0.9));
    }

    #[test]
    fn empty_candidates_yield_empty_packet() {
        let packet = build_context_packet_full("q", Vec::new(), Budget::Small, None);
        assert!(packet.items.is_empty());
    }

    #[test]
    fn root_level_files_share_the_root_directory() {
        // Files without a slash live in the "" dir — they decay each other at
        // dir level but must not panic or collide with real dirs.
        let candidates = vec![
            scored("README.md", "a", 1.0),
            scored("BACKLOG.md", "b", 0.9),
            scored("src/x.rs", "x", 0.88),
        ];
        let packet = build_context_packet_full("q", candidates, Budget::Tiny, None);
        // BACKLOG.md decays to 0.765 < src/x.rs 0.88.
        assert_eq!(paths(&packet), vec!["README.md", "src/x.rs", "BACKLOG.md"]);
    }
}
