use crate::KungfuService;
use anyhow::{bail, Result};
use kungfu_types::relation::RelationKind;
use std::collections::HashSet;

/// How many affected entries to inline before declaring truncation.
const MAX_AFFECTED: usize = 20;

impl KungfuService {
    /// One call after editing: what the working-tree diff touched, the blast
    /// radius, the minimal test set, and which public contracts changed.
    /// Composite over affected_staged + smart_test so the agent doesn't have to
    /// remember to run each one.
    pub fn verify_change(&self, depth: usize) -> Result<serde_json::Value> {
        self.ensure_fresh_index()?;
        if !kungfu_git::is_git_repo(&self.project.root) {
            bail!("not a git repository — verify_change needs a diff to analyse");
        }

        let changed_lines = kungfu_git::diff_changed_lines(&self.project.root)?;
        if changed_lines.is_empty() {
            return Ok(serde_json::json!({
                "status": "clean",
                "hint": "working tree matches HEAD — nothing to verify",
            }));
        }

        let all_symbols = self.search().get_all_symbols()?;

        // Symbols whose spans intersect the diff hunks.
        let mut changed_symbols: Vec<&kungfu_types::symbol::Symbol> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for (file_path, ranges) in &changed_lines {
            for sym in all_symbols.iter().filter(|s| s.path == *file_path) {
                let overlaps = ranges
                    .iter()
                    .any(|&(start, end)| sym.span.start_line <= end && sym.span.end_line >= start);
                if overlaps && seen.insert(sym.id.as_str()) {
                    changed_symbols.push(sym);
                }
            }
        }

        // Exported symbols in the diff = changed public contracts. Callers outside
        // this diff may now be broken even if the project still compiles locally.
        let touched_contracts: Vec<_> = changed_symbols
            .iter()
            .filter(|s| s.exported)
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "kind": s.kind.to_string(),
                    "path": s.path,
                    "line": s.span.start_line,
                    "signature": s.signature,
                })
            })
            .collect();

        let affected = self.affected_staged(depth)?;
        let tests = self.smart_test()?;

        let affected_total = affected.entries.len();
        let affected_entries: Vec<_> = affected
            .entries
            .iter()
            .take(MAX_AFFECTED)
            .map(|e| {
                serde_json::json!({
                    "name": e.name,
                    "path": e.path,
                    "depth": e.depth,
                    "reason": e.reason,
                })
            })
            .collect();

        let suggested_tests: Vec<_> = tests
            .tests
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.test_name,
                    "path": t.test_path,
                    "reason": t.reason,
                })
            })
            .collect();

        // Provenance: with no Calls relations the radius rests on imports alone,
        // which over-approximates at file level — say so instead of looking exact.
        let relations = self.store().relations_arc()?;
        let call_graph_indexed = relations.iter().any(|r| r.kind == RelationKind::Calls);

        let mut out = serde_json::json!({
            "status": "changes",
            "changed_files": changed_lines.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
            "changed_symbols": changed_symbols
                .iter()
                .map(|s| format!("{}::{}", s.path, s.name))
                .collect::<Vec<_>>(),
            "touched_contracts": touched_contracts,
            "affected": {
                "risk": affected.risk,
                "total": affected_total,
                "entries": affected_entries,
            },
            "suggested_tests": suggested_tests,
            "test_files": affected.test_files,
            "call_graph": if call_graph_indexed { "indexed" } else { "imports_only" },
        });
        if affected_total > MAX_AFFECTED {
            out["affected"]["note"] = serde_json::json!(format!(
                "showing {MAX_AFFECTED} of {affected_total} — use affected(staged=true) for the full list"
            ));
        }
        if !call_graph_indexed {
            out["note"] = serde_json::json!(
                "no call relations indexed — blast radius is a file-level import approximation"
            );
        }
        Ok(out)
    }
}
