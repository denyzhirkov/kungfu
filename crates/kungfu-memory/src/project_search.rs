use kungfu_types::memory::{MemoryStatus, ProjectMemoryEntry, ProjectMemoryKind};
use std::collections::HashMap;

/// A conflict between two or more active memory entries that look like they cover the same topic
/// but disagree. Heuristic only — used to surface, not to auto-resolve.
#[derive(Debug, Clone)]
pub struct MemoryConflict {
    /// Common signal that grouped these entries: a shared tag or anchor.
    pub on: String,
    /// Entries in the conflict cluster, sorted by `updated_at` descending.
    pub entries: Vec<ProjectMemoryEntry>,
}

/// Detect potential conflicts between active memory entries.
///
/// Conflict rules:
/// - Group active entries by shared tag (and by shared related_symbol).
/// - A group with ≥2 entries is a conflict candidate.
/// - Drop entries that are linked by `supersedes` (the older one is intentional history).
/// - Require entries to differ in `content` — same content = redundant copy, not conflict.
pub fn detect_conflicts(entries: &[ProjectMemoryEntry]) -> Vec<MemoryConflict> {
    let active: Vec<&ProjectMemoryEntry> = entries
        .iter()
        .filter(|e| e.status == MemoryStatus::Active)
        .collect();

    // Build supersedes chain: any id that is superseded by another active entry.
    let superseded: std::collections::HashSet<String> =
        active.iter().filter_map(|e| e.supersedes.clone()).collect();

    // Index entries by shared signals (tags + related_symbols).
    let mut by_signal: HashMap<String, Vec<&ProjectMemoryEntry>> = HashMap::new();
    for e in &active {
        if superseded.contains(&e.id) {
            continue;
        }
        for tag in &e.tags {
            by_signal.entry(format!("tag:{}", tag)).or_default().push(e);
        }
        for sym in &e.related_symbols {
            by_signal
                .entry(format!("symbol:{}", sym))
                .or_default()
                .push(e);
        }
    }

    let mut out: Vec<MemoryConflict> = Vec::new();
    let mut emitted_clusters: std::collections::HashSet<Vec<String>> =
        std::collections::HashSet::new();

    for (signal, group) in by_signal {
        if group.len() < 2 {
            continue;
        }
        // Drop pairs with identical content (redundant, not conflicting).
        let mut unique: Vec<&ProjectMemoryEntry> = Vec::new();
        for e in group {
            if !unique.iter().any(|u| u.content == e.content) {
                unique.push(e);
            }
        }
        if unique.len() < 2 {
            continue;
        }

        // Dedup cluster across signals: same id-set already reported.
        let mut id_key: Vec<String> = unique.iter().map(|e| e.id.clone()).collect();
        id_key.sort();
        if !emitted_clusters.insert(id_key) {
            continue;
        }

        let mut sorted = unique;
        sorted.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        out.push(MemoryConflict {
            on: signal,
            entries: sorted.into_iter().cloned().collect(),
        });
    }

    // Stable ordering: by signal then by oldest updated_at desc.
    out.sort_by(|a, b| a.on.cmp(&b.on));
    out
}

/// Filters for project memory search.
#[derive(Debug, Default)]
pub struct MemoryFilter {
    pub kind: Option<ProjectMemoryKind>,
    pub tag: Option<String>,
    pub status: Option<MemoryStatus>,
    pub pinned_only: bool,
}

/// Additional ranking context: paths and symbol names from the surrounding
/// code retrieval pipeline. Used to boost memory entries whose `related_files`
/// or `related_symbols` overlap with what the agent is actually looking at.
#[derive(Debug, Default)]
pub struct SearchContext<'a> {
    pub matched_files: &'a [String],
    pub matched_symbols: &'a [String],
}

/// Search project memory with query + filters only.
pub fn search_project_memory(
    query: &str,
    entries: &[ProjectMemoryEntry],
    filter: &MemoryFilter,
) -> Vec<(f64, ProjectMemoryEntry)> {
    search_project_memory_with_context(query, entries, filter, &SearchContext::default())
}

/// Search project memory with query + filters + cross-reference context.
/// Memory entries whose `related_files` or `related_symbols` overlap with
/// `ctx.matched_files` / `ctx.matched_symbols` receive a significant boost
/// even if the query text doesn't match directly.
pub fn search_project_memory_with_context(
    query: &str,
    entries: &[ProjectMemoryEntry],
    filter: &MemoryFilter,
    ctx: &SearchContext<'_>,
) -> Vec<(f64, ProjectMemoryEntry)> {
    let query_lower = query.to_lowercase();
    let query_words: Vec<&str> = query_lower.split_whitespace().collect();

    // Empty query with no filters: caller should use list_all instead
    // But if any filter is set, allow empty query to return filtered results

    let mut results: Vec<(f64, &ProjectMemoryEntry)> = entries
        .iter()
        .filter(|e| {
            // Status filter (default: only active)
            let status = filter.status.unwrap_or(MemoryStatus::Active);
            if e.status != status {
                return false;
            }
            // Kind filter
            if let Some(kind) = filter.kind {
                if e.kind != kind {
                    return false;
                }
            }
            // Tag filter
            if let Some(ref tag) = filter.tag {
                if !e.tags.iter().any(|t| t == tag) {
                    return false;
                }
            }
            // Pinned filter
            if filter.pinned_only && !e.pinned {
                return false;
            }
            true
        })
        .filter_map(|e| {
            // Cross-reference overlap (computed regardless of query)
            let file_overlap = e
                .related_files
                .iter()
                .any(|rf| ctx.matched_files.iter().any(|mf| paths_overlap(rf, mf)));
            let symbol_overlap = e
                .related_symbols
                .iter()
                .any(|rs| ctx.matched_symbols.iter().any(|ms| rs == ms));

            // If no query words, return all filtered entries with base score
            if query_words.is_empty() {
                let mut score = if e.pinned { 1.0 } else { 0.5 };
                if file_overlap {
                    score += 0.3;
                }
                if symbol_overlap {
                    score += 0.3;
                }
                return Some((score, e));
            }

            let content_lower = e.content.to_lowercase();
            let title_lower = e
                .title
                .as_ref()
                .map(|t| t.to_lowercase())
                .unwrap_or_default();

            // Word overlap scoring
            let mut hits = 0usize;
            for word in &query_words {
                if content_lower.contains(word) || title_lower.contains(word) {
                    hits += 1;
                }
                // Tag match bonus
                if e.tags.iter().any(|t| t.to_lowercase().contains(word)) {
                    hits += 1;
                }
            }

            // Allow entries to survive without text hits if cross-ref overlaps
            if hits == 0 && !file_overlap && !symbol_overlap {
                return None;
            }

            let mut score = hits as f64 / query_words.len().max(1) as f64;

            // Kind bonus
            match e.kind {
                ProjectMemoryKind::Warning => score += 0.15,
                ProjectMemoryKind::Decision => score += 0.1,
                _ => {}
            }

            // Pinned bonus
            if e.pinned {
                score += 0.2;
            }

            // Cross-reference boosts (based on surrounding code retrieval)
            if file_overlap {
                score += 0.3;
            }
            if symbol_overlap {
                score += 0.3;
            }

            // Query-word matching against related_files/symbols (weaker signal)
            for word in &query_words {
                if e.related_files
                    .iter()
                    .any(|f| f.to_lowercase().contains(word))
                {
                    score += 0.1;
                    break;
                }
            }
            for word in &query_words {
                if e.related_symbols
                    .iter()
                    .any(|s| s.to_lowercase().contains(word))
                {
                    score += 0.1;
                    break;
                }
            }

            Some((score, e))
        })
        .collect();

    results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    results.into_iter().map(|(s, e)| (s, e.clone())).collect()
}

/// Loose path overlap: either path ends with the other, so that
/// "crates/kungfu-core/src/lib.rs" matches "kungfu-core/src/lib.rs"
/// and vice versa.
fn paths_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let a_norm = a.trim_start_matches("./");
    let b_norm = b.trim_start_matches("./");
    a_norm.ends_with(b_norm) || b_norm.ends_with(a_norm)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(
        id: &str,
        kind: ProjectMemoryKind,
        content: &str,
        tags: &[&str],
        pinned: bool,
    ) -> ProjectMemoryEntry {
        ProjectMemoryEntry {
            id: id.to_string(),
            kind,
            title: Some(
                content
                    .split_whitespace()
                    .take(5)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            content: content.to_string(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            related_files: Vec::new(),
            related_symbols: Vec::new(),
            pinned,
            status: MemoryStatus::Active,
            supersedes: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn search_by_content() {
        let entries = vec![
            make_entry(
                "mem_0001",
                ProjectMemoryKind::Fact,
                "backend uses sqlite",
                &[],
                false,
            ),
            make_entry(
                "mem_0002",
                ProjectMemoryKind::Fact,
                "frontend uses react",
                &[],
                false,
            ),
        ];
        let results = search_project_memory("sqlite", &entries, &MemoryFilter::default());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.id, "mem_0001");
    }

    #[test]
    fn filter_by_kind() {
        let entries = vec![
            make_entry(
                "mem_0001",
                ProjectMemoryKind::Fact,
                "uses sqlite",
                &[],
                false,
            ),
            make_entry(
                "mem_0002",
                ProjectMemoryKind::Warning,
                "legacy auth",
                &[],
                false,
            ),
        ];
        let filter = MemoryFilter {
            kind: Some(ProjectMemoryKind::Warning),
            ..Default::default()
        };
        let results = search_project_memory("", &entries, &filter);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.id, "mem_0002");
    }

    #[test]
    fn pinned_gets_bonus() {
        let entries = vec![
            make_entry(
                "mem_0001",
                ProjectMemoryKind::Fact,
                "uses sqlite database",
                &[],
                false,
            ),
            make_entry(
                "mem_0002",
                ProjectMemoryKind::Fact,
                "sqlite is the main store",
                &[],
                true,
            ),
        ];
        let results = search_project_memory("sqlite", &entries, &MemoryFilter::default());
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].1.id, "mem_0002"); // pinned first
    }

    #[test]
    fn archived_excluded_by_default() {
        let mut e = make_entry("mem_0001", ProjectMemoryKind::Fact, "old info", &[], false);
        e.status = MemoryStatus::Archived;
        let results = search_project_memory("old", &[e], &MemoryFilter::default());
        assert!(results.is_empty());
    }

    #[test]
    fn cross_ref_file_overlap_surfaces_entry_without_text_match() {
        let mut e = make_entry(
            "mem_0001",
            ProjectMemoryKind::Warning,
            "core crate is too large — split before extending",
            &[],
            false,
        );
        e.related_files = vec!["crates/kungfu-core/src/lib.rs".to_string()];
        let entries = vec![e];

        let matched = vec!["kungfu-core/src/lib.rs".to_string()];
        let ctx = SearchContext {
            matched_files: &matched,
            matched_symbols: &[],
        };

        // Query has no word overlap with the warning content
        let results = search_project_memory_with_context(
            "add method to KungfuService",
            &entries,
            &MemoryFilter::default(),
            &ctx,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.id, "mem_0001");
    }

    #[test]
    fn cross_ref_symbol_overlap_boosts_score() {
        let mut e = make_entry(
            "mem_0001",
            ProjectMemoryKind::Decision,
            "never inline this helper",
            &[],
            false,
        );
        e.related_symbols = vec!["KungfuService".to_string()];
        let entries = vec![e];

        let matched_symbols = vec!["KungfuService".to_string()];
        let ctx = SearchContext {
            matched_files: &[],
            matched_symbols: &matched_symbols,
        };

        let results = search_project_memory_with_context(
            "something unrelated",
            &entries,
            &MemoryFilter::default(),
            &ctx,
        );
        assert_eq!(results.len(), 1);
        assert!(results[0].0 > 0.3); // cross-ref boost
    }

    fn entry(
        id: &str,
        content: &str,
        tags: &[&str],
        supersedes: Option<&str>,
    ) -> ProjectMemoryEntry {
        ProjectMemoryEntry {
            id: id.into(),
            kind: ProjectMemoryKind::Decision,
            title: None,
            content: content.into(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            related_files: vec![],
            related_symbols: vec![],
            pinned: false,
            status: MemoryStatus::Active,
            supersedes: supersedes.map(|s| s.to_string()),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn conflict_detected_on_shared_tag_differing_content() {
        let entries = vec![
            entry("a", "use sqlite", &["storage"], None),
            entry("b", "use json", &["storage"], None),
        ];
        let conflicts = detect_conflicts(&entries);
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].on.contains("storage"));
        assert_eq!(conflicts[0].entries.len(), 2);
    }

    #[test]
    fn supersedes_pair_is_not_conflict() {
        let entries = vec![
            entry("old", "use json", &["storage"], None),
            entry("new", "use sqlite", &["storage"], Some("old")),
        ];
        let conflicts = detect_conflicts(&entries);
        assert!(conflicts.is_empty(), "supersedes pair surfaced as conflict");
    }

    #[test]
    fn identical_content_is_not_conflict() {
        let entries = vec![
            entry("a", "same text", &["topic"], None),
            entry("b", "same text", &["topic"], None),
        ];
        let conflicts = detect_conflicts(&entries);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn cluster_dedup_across_multiple_signals() {
        let mut a = entry("a", "use sqlite", &["storage", "perf"], None);
        let mut b = entry("b", "use json", &["storage", "perf"], None);
        a.related_symbols.push("Store".into());
        b.related_symbols.push("Store".into());
        let entries = vec![a, b];
        let conflicts = detect_conflicts(&entries);
        // Same pair appears under tag:storage, tag:perf, symbol:Store — but dedup → 1.
        assert_eq!(conflicts.len(), 1);
    }

    #[test]
    fn paths_overlap_loose_matching() {
        assert!(paths_overlap(
            "crates/kungfu-core/src/lib.rs",
            "kungfu-core/src/lib.rs"
        ));
        assert!(paths_overlap(
            "kungfu-core/src/lib.rs",
            "crates/kungfu-core/src/lib.rs"
        ));
        assert!(paths_overlap("/abs/foo.rs", "abs/foo.rs"));
        assert!(!paths_overlap("src/foo.rs", "src/bar.rs"));
    }
}
