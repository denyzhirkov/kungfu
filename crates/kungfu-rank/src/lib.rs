#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use kungfu_types::budget::Budget;
use kungfu_types::context::{ContextItem, ContextItemType, ContextPacket};
use kungfu_types::symbol::Symbol;

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
    mut symbols: Vec<ScoredSymbol>,
    budget: Budget,
    intent: Option<kungfu_types::context::Intent>,
) -> ContextPacket {
    let top_k = budget.top_k();

    // Sort by score descending
    symbols.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Deduplicate by (path, name) — keep highest-scored entry
    let mut seen = std::collections::HashSet::new();
    let items: Vec<ContextItem> = symbols
        .into_iter()
        .filter(|s| seen.insert((s.symbol.path.clone(), s.symbol.name.clone())))
        .take(top_k)
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
    }
}
