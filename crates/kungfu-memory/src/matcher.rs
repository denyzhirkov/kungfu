use kungfu_types::context::RationaleItem;
use kungfu_types::memory::{MemoryEntry, MemoryKind};
use kungfu_types::Budget;

use crate::anchor::extract_anchors;

/// Match memories against a query, returning ranked RationaleItems.
pub fn match_memories(query: &str, memories: &[MemoryEntry], budget: Budget) -> Vec<RationaleItem> {
    let max_results = match budget {
        Budget::Tiny => 0,
        Budget::Small => 2,
        Budget::Medium => 4,
        Budget::Full => 8,
        Budget::Auto => 3,
    };

    if max_results == 0 || memories.is_empty() {
        return Vec::new();
    }

    let query_anchors = extract_anchors(query);
    if query_anchors.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(f64, &MemoryEntry)> = memories
        .iter()
        .filter_map(|m| {
            let overlap = query_anchors
                .iter()
                .filter(|qa| {
                    m.anchors
                        .iter()
                        .any(|a| a == *qa || a.contains(qa.as_str()) || qa.contains(a.as_str()))
                })
                .count();

            if overlap == 0 {
                return None;
            }

            let base_score = overlap as f64 / query_anchors.len().max(1) as f64;

            // Kind bonus
            let kind_bonus = match m.kind {
                MemoryKind::Decision => 0.2,
                MemoryKind::Fixme => 0.15,
                MemoryKind::Todo => 0.1,
                MemoryKind::Note => 0.05,
                _ => 0.0,
            };

            let score = (base_score + kind_bonus) * m.weight as f64;
            Some((score, m))
        })
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    scored
        .into_iter()
        .take(max_results)
        .map(|(score, m)| RationaleItem {
            source: m.path.clone(),
            kind: m.kind.to_string(),
            text: m.text.clone(),
            relevance: score,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_memory(kind: MemoryKind, text: &str, anchors: &[&str], weight: f32) -> MemoryEntry {
        MemoryEntry {
            id: "test".to_string(),
            path: "test.rs".to_string(),
            kind,
            text: text.to_string(),
            anchors: anchors.iter().map(|s| s.to_string()).collect(),
            weight,
            symbol_id: None,
            line_range: None,
        }
    }

    #[test]
    fn matches_by_anchor_overlap() {
        let memories = vec![
            make_memory(
                MemoryKind::Note,
                "auth uses JWT tokens",
                &["auth", "jwt", "tokens"],
                0.7,
            ),
            make_memory(
                MemoryKind::Note,
                "parser handles tree-sitter",
                &["parser", "tree", "sitter"],
                0.7,
            ),
        ];
        let results = match_memories("how does auth work", &memories, Budget::Small);
        assert_eq!(results.len(), 1);
        assert!(results[0].text.contains("JWT"));
    }

    #[test]
    fn decision_gets_bonus() {
        let memories = vec![
            make_memory(
                MemoryKind::Decision,
                "chose JWT for auth",
                &["auth", "jwt"],
                0.9,
            ),
            make_memory(
                MemoryKind::DocSection,
                "auth module overview",
                &["auth", "module"],
                0.5,
            ),
        ];
        let results = match_memories("auth", &memories, Budget::Full);
        assert!(!results.is_empty());
        assert_eq!(results[0].kind, "decision");
    }

    #[test]
    fn tiny_budget_returns_empty() {
        let memories = vec![make_memory(
            MemoryKind::Note,
            "important note",
            &["important", "note"],
            0.7,
        )];
        let results = match_memories("important", &memories, Budget::Tiny);
        assert!(results.is_empty());
    }

    #[test]
    fn no_overlap_returns_empty() {
        let memories = vec![make_memory(
            MemoryKind::Note,
            "handles parsing",
            &["parsing", "handles"],
            0.7,
        )];
        let results = match_memories("authentication", &memories, Budget::Full);
        assert!(results.is_empty());
    }

    #[test]
    fn respects_max_results() {
        let memories: Vec<_> = (0..10)
            .map(|i| make_memory(MemoryKind::Note, &format!("note {}", i), &["auth"], 0.7))
            .collect();
        let results = match_memories("auth", &memories, Budget::Small);
        assert_eq!(results.len(), 2);
    }
}
