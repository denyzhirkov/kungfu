use crate::explore::pick_best_definition;
use crate::KungfuService;
use anyhow::Result;
use kungfu_types::budget::Budget;
use kungfu_types::relation::RelationKind;
use kungfu_types::symbol::Symbol;
use std::collections::HashMap;

/// How many sibling signatures to include before declaring truncation.
const MAX_NEIGHBORS: usize = 30;
/// How many caller entries to list (the full count is always reported).
const MAX_CALLERS: usize = 10;
/// How many attached rationale entries to include.
const MAX_RATIONALE: usize = 5;

impl KungfuService {
    /// Edit-ready context for a symbol: the full verbatim body plus the contracts
    /// around it — sibling signatures, callees, callers, attached rationale.
    ///
    /// Unlike explore_symbol this never truncates the body: the point is that the
    /// agent can construct an exact edit (old_string/new_string) without a
    /// follow-up file read. Cost is declared via line_count instead.
    pub fn edit_context(&self, name: &str, scope: Option<&str>) -> Result<serde_json::Value> {
        let search = self.search();

        let mut candidates = search.find_symbol(name, Budget::Full)?;
        if let Some(prefix) = scope {
            candidates.retain(|c| c.item.path.starts_with(prefix));
        }
        let symbol = match pick_best_definition(&candidates, name) {
            Some(best) => best.item.clone(),
            None => match search.get_symbol(name)? {
                Some(sym) if scope.is_none_or(|p| sym.path.starts_with(p)) => sym,
                _ => {
                    return Ok(serde_json::json!({
                        "error": format!("Symbol '{}' not found{}", name,
                            scope.map(|s| format!(" under '{}'", s)).unwrap_or_default()),
                        "hint": "check the name with find_symbol, or drop the scope filter",
                    }))
                }
            },
        };

        // Full verbatim body — no budget trimming.
        let abs_path = self.project.root.join(&symbol.path);
        let content = std::fs::read_to_string(&abs_path).map_err(|e| {
            anyhow::anyhow!("cannot read {} for symbol '{}': {}", symbol.path, name, e)
        })?;
        let lines: Vec<&str> = content.lines().collect();
        let start = symbol.span.start_line.saturating_sub(1);
        let end = symbol.span.end_line.min(lines.len());
        if start >= end {
            anyhow::bail!(
                "stale span for '{}' in {} (lines {}..{}, file has {}) — reindex and retry",
                name,
                symbol.path,
                symbol.span.start_line,
                symbol.span.end_line,
                lines.len()
            );
        }
        let code = lines[start..end].join("\n");
        let line_count = end - start;

        // Sibling signatures — what else lives in this file (capped, declared).
        let file_symbols = search.get_symbols_for_file(&symbol.path)?;
        let neighbors_total = file_symbols.len().saturating_sub(1);
        let neighbors: Vec<_> = file_symbols
            .iter()
            .filter(|s| s.id != symbol.id)
            .take(MAX_NEIGHBORS)
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "kind": s.kind.to_string(),
                    "line": s.span.start_line,
                    "signature": s.signature,
                })
            })
            .collect();

        // Call graph: what this symbol calls, and who calls it.
        let relations = self.store().relations_arc()?;
        let call_graph_indexed = relations.iter().any(|r| r.kind == RelationKind::Calls);
        let all_symbols = search.get_all_symbols()?;
        let by_id: HashMap<&str, &Symbol> =
            all_symbols.iter().map(|s| (s.id.as_str(), s)).collect();

        let mut callees = Vec::new();
        let mut callers = Vec::new();
        let mut callers_count = 0usize;
        for r in relations.iter() {
            if r.kind != RelationKind::Calls {
                continue;
            }
            if r.source_id == symbol.id {
                if let Some(target) = by_id.get(r.target_id.as_str()) {
                    callees.push(serde_json::json!({
                        "name": target.name,
                        "kind": target.kind.to_string(),
                        "path": target.path,
                        "line": target.span.start_line,
                        "signature": target.signature,
                    }));
                }
            }
            if r.target_id == symbol.id {
                callers_count += 1;
                if callers.len() < MAX_CALLERS {
                    if let Some(source) = by_id.get(r.source_id.as_str()) {
                        callers.push(serde_json::json!({
                            "name": source.name,
                            "path": source.path,
                            "line": source.span.start_line,
                        }));
                    }
                }
            }
        }

        // Attached rationale: comment/doc memories tied to this symbol or its lines.
        let memories = self.store().load_memories().unwrap_or_default();
        let rationale: Vec<_> = memories
            .iter()
            .filter(|m| {
                m.symbol_id.as_deref() == Some(symbol.id.as_str())
                    || (m.path == symbol.path
                        && m.line_range.is_some_and(|(s, e)| {
                            s <= symbol.span.end_line && e >= symbol.span.start_line
                        }))
            })
            .take(MAX_RATIONALE)
            .map(|m| {
                serde_json::json!({
                    "kind": m.kind.to_string(),
                    "text": m.text,
                    "line": m.line_range.map(|(s, _)| s),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "symbol": {
                "name": symbol.name,
                "kind": symbol.kind.to_string(),
                "path": symbol.path,
                "language": symbol.language,
                "exported": symbol.exported,
                "doc_summary": symbol.doc_summary,
            },
            "span": { "start_line": symbol.span.start_line, "end_line": symbol.span.end_line },
            "line_count": line_count,
            "code": code,
            "neighbors": neighbors,
            "neighbors_total": neighbors_total,
            "callees": callees,
            "callers": callers,
            "callers_count": callers_count,
            "call_graph": if call_graph_indexed { "indexed" } else { "not_indexed" },
            "rationale": rationale,
        }))
    }
}
