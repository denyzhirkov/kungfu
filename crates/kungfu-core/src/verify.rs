use crate::KungfuService;
use anyhow::{bail, Result};
use kungfu_types::relation::RelationKind;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// How many affected entries to inline before declaring truncation.
const MAX_AFFECTED: usize = 20;
/// How many entries of each path/symbol list to inline before declaring truncation.
const MAX_LISTED: usize = 50;

/// Store `items` under `key`, cut to MAX_LISTED; when cut, `<key>_note` says
/// how much is missing so a partial list is never mistaken for the full one.
fn set_capped<T: Serialize>(out: &mut serde_json::Value, key: &str, items: &[T]) {
    out[key] = serde_json::json!(&items[..items.len().min(MAX_LISTED)]);
    if items.len() > MAX_LISTED {
        out[format!("{key}_note")] = serde_json::json!(format!(
            "showing {MAX_LISTED} of {} — list truncated",
            items.len()
        ));
    }
}

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

        let root = &self.project.root;
        let base = kungfu_git::diff_base(root)?;
        let mut changed_lines = kungfu_git::diff_changed_lines(root, &base)?;
        let deleted_files = kungfu_git::deleted_files(root, &base)?;
        let untracked_files = kungfu_git::untracked_files(root)?;
        // Every tracked file that differs at all — binary and mode-only changes
        // produce no hunks and would otherwise pass for clean.
        let changed_paths = kungfu_git::changed_paths(root, &base)?;
        if changed_lines.is_empty()
            && deleted_files.is_empty()
            && untracked_files.is_empty()
            && changed_paths.is_empty()
        {
            return Ok(serde_json::json!({
                "status": "clean",
                "hint": if base.has_commits {
                    "working tree matches HEAD — nothing to verify"
                } else {
                    "repository has no commits and nothing is staged or tracked — nothing to verify"
                },
            }));
        }

        // A new untracked file is one whole-file change: every symbol in it counts.
        let diffed: HashSet<&str> = changed_lines.iter().map(|(p, _)| p.as_str()).collect();
        let whole_file: Vec<kungfu_git::FileChangedLines> = untracked_files
            .iter()
            .filter(|p| !diffed.contains(p.as_str()))
            .map(|p| (p.clone(), vec![(1, usize::MAX)]))
            .collect();
        changed_lines.extend(whole_file);

        let all_symbols = self.search().get_all_symbols()?;
        let mut symbols_by_path: HashMap<&str, Vec<&kungfu_types::symbol::Symbol>> = HashMap::new();
        for sym in all_symbols.iter() {
            symbols_by_path
                .entry(sym.path.as_str())
                .or_default()
                .push(sym);
        }

        // Symbols whose spans intersect the diff hunks.
        let mut changed_symbols: Vec<&kungfu_types::symbol::Symbol> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for (file_path, ranges) in &changed_lines {
            for sym in symbols_by_path
                .get(file_path.as_str())
                .into_iter()
                .flatten()
            {
                let overlaps = ranges
                    .iter()
                    .any(|&(start, end)| sym.span.start_line <= end && sym.span.end_line >= start);
                if overlaps && seen.insert(sym.id.as_str()) {
                    changed_symbols.push(*sym);
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

        let affected = self.affected_for_lines(depth, &changed_lines)?;
        let tests = self.smart_test_for_lines(&changed_lines)?;

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

        // Deleted, binary and mode-only files carry no hunks, so the name-only
        // list is merged in explicitly.
        let hunked: HashSet<&str> = changed_lines.iter().map(|(p, _)| p.as_str()).collect();
        let mut changed_files: HashSet<&str> = hunked.clone();
        changed_files.extend(deleted_files.iter().map(String::as_str));
        changed_files.extend(changed_paths.iter().map(String::as_str));
        let mut changed_files: Vec<&str> = changed_files.into_iter().collect();
        changed_files.sort_unstable();
        let no_hunk_files: Vec<&str> = changed_paths
            .iter()
            .map(String::as_str)
            .filter(|p| !hunked.contains(p) && !deleted_files.iter().any(|d| d == p))
            .collect();

        let mut out = serde_json::json!({
            "status": "changes",
            "affected": {
                "risk": affected.risk,
                "total": affected_total,
                "entries": affected_entries,
            },
            "test_files": affected.test_files,
            "call_graph": if call_graph_indexed { "indexed" } else { "imports_only" },
        });
        set_capped(&mut out, "changed_files", &changed_files);
        let symbol_names: Vec<String> = changed_symbols
            .iter()
            .map(|s| format!("{}::{}", s.path, s.name))
            .collect();
        set_capped(&mut out, "changed_symbols", &symbol_names);
        set_capped(&mut out, "touched_contracts", &touched_contracts);
        set_capped(&mut out, "suggested_tests", &suggested_tests);
        if !deleted_files.is_empty() {
            set_capped(&mut out, "deleted_files", &deleted_files);
            out["deleted_note"] = serde_json::json!(
                "symbols removed with deleted files are not in the blast radius — run callers/affected on the removed names"
            );
        }
        if !untracked_files.is_empty() {
            set_capped(&mut out, "untracked_files", &untracked_files);
        }
        if !no_hunk_files.is_empty() {
            set_capped(&mut out, "no_hunk_files", &no_hunk_files);
            out["no_hunk_note"] = serde_json::json!(
                "changed without text hunks (binary or mode-only) — not in changed_symbols or the blast radius"
            );
        }
        let mut affected_note = affected.note;
        if affected_total > MAX_AFFECTED {
            crate::review::push_note(
                &mut affected_note,
                &format!(
                    "showing {MAX_AFFECTED} of {affected_total} entries — run affected(<name>) on a changed symbol for its full radius (affected(staged=true) skips untracked files)"
                ),
            );
        }
        if let Some(note) = affected_note {
            out["affected"]["note"] = serde_json::json!(note);
        }
        if !base.has_commits {
            out["base_note"] = serde_json::json!(crate::review::NO_COMMITS_NOTE);
        }
        if !call_graph_indexed {
            out["note"] = serde_json::json!(
                "no call relations indexed — blast radius is a file-level import approximation"
            );
        }
        Ok(out)
    }
}
