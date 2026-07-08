use crate::types::{DirEntry, FileOutline, RepoOutline, SymbolOutline};
use crate::KungfuService;
use anyhow::Result;
use kungfu_types::budget::Budget;
use kungfu_types::symbol::Symbol;
use std::collections::HashMap;
use std::path::Path;

impl KungfuService {
    pub fn repo_outline(&self, budget: Budget) -> Result<RepoOutline> {
        let budget = self.resolve_budget(budget);
        let store = self.store();
        let files = store.load_files()?;
        let symbols = store.load_symbols()?;

        let mut languages: HashMap<String, usize> = HashMap::new();
        let mut dirs: HashMap<String, usize> = HashMap::new();

        for f in &files {
            if let Some(ref lang) = f.language {
                *languages.entry(lang.clone()).or_default() += 1;
            }
            if let Some(dir) = Path::new(&f.path).parent() {
                let dir_str = dir.to_string_lossy().to_string();
                if !dir_str.is_empty() {
                    // Get top-level directory
                    let top = dir_str.split('/').next().unwrap_or(&dir_str).to_string();
                    *dirs.entry(top).or_default() += 1;
                }
            }
        }

        let mut top_dirs: Vec<DirEntry> = dirs
            .into_iter()
            .map(|(path, file_count)| DirEntry { path, file_count })
            .collect();
        top_dirs.sort_by(|a, b| b.file_count.cmp(&a.file_count));
        top_dirs.truncate(budget.top_k() * 2);

        // Detect entrypoints
        let entrypoints: Vec<String> = files
            .iter()
            .filter(|f| {
                let p = &f.path;
                p.ends_with("main.rs")
                    || p.ends_with("lib.rs")
                    || p.ends_with("index.ts")
                    || p.ends_with("index.js")
                    || p.ends_with("main.py")
                    || p.ends_with("main.go")
                    || p.ends_with("app.ts")
                    || p.ends_with("app.js")
                    || p == "package.json"
                    || p == "Cargo.toml"
                    || p == "go.mod"
                    || p == "pyproject.toml"
            })
            .map(|f| match f.purpose {
                Some(ref purpose) => format!("{} — {}", f.path, purpose),
                None => f.path.clone(),
            })
            .collect();

        Ok(RepoOutline {
            project_name: self.project.meta.name.clone(),
            total_files: files.len(),
            total_symbols: symbols.len(),
            languages,
            top_dirs,
            entrypoints,
        })
    }

    pub fn file_outline(&self, file_path: &str) -> Result<FileOutline> {
        let search = self.search();
        let files = search.get_all_files()?;

        let file = files
            .iter()
            .find(|f| f.path == file_path || f.path.ends_with(file_path))
            .ok_or_else(|| anyhow::anyhow!("file not found in index: {}", file_path))?;

        let symbols = search.get_symbols_for_file(&file.path)?;

        let outlines = symbols
            .iter()
            .map(|s| SymbolOutline {
                name: s.name.clone(),
                kind: s.kind.to_string(),
                signature: s.signature.clone(),
                line: s.span.start_line,
                exported: s.exported,
            })
            .collect();

        Ok(FileOutline {
            path: file.path.clone(),
            language: file.language.clone(),
            purpose: file.purpose.clone(),
            tags: file.tags.clone(),
            symbols: outlines,
        })
    }

    /// Composite: explore a symbol — find + detail + related symbols + snippets in one call.
    pub fn explore_symbol(&self, name: &str, budget: Budget) -> Result<serde_json::Value> {
        let budget = self.resolve_budget(budget);
        let search = self.search();

        // 1. Find symbol candidates
        let candidates = search.find_symbol(name, budget)?;

        // 2. Pick best candidate — on tie prefer: definitions > variables, src > test, exported
        let (symbol, score) = if let Some(best) = pick_best_definition(&candidates, name) {
            (best.item.clone(), best.score)
        } else if let Some(sym) = search.get_symbol(name)? {
            (sym, 1.0)
        } else {
            return Ok(serde_json::json!({ "error": format!("Symbol '{}' not found", name) }));
        };

        // 3. File outline for related symbols
        let file_outline = self.file_outline(&symbol.path)?;
        let siblings: Vec<_> = file_outline
            .symbols
            .iter()
            .filter(|s| s.name != symbol.name)
            .take(budget.top_k())
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "kind": s.kind,
                    "line": s.line,
                    "exported": s.exported,
                })
            })
            .collect();

        // 4. Snippet for the primary symbol
        let snippet = self.symbol_snippet(&symbol, 30);

        // 5. Other candidates (if fuzzy matched)
        let other_matches: Vec<_> = candidates
            .iter()
            .filter(|c| c.item.id != symbol.id)
            .take(5)
            .map(|c| {
                serde_json::json!({
                    "name": c.item.name,
                    "kind": c.item.kind.to_string(),
                    "path": c.item.path,
                    "line": c.item.span.start_line,
                    "score": c.score,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "symbol": {
                "name": symbol.name,
                "kind": symbol.kind.to_string(),
                "path": symbol.path,
                "line": symbol.span.start_line,
                "end_line": symbol.span.end_line,
                "signature": symbol.signature,
                "exported": symbol.exported,
                "language": symbol.language,
                "score": score,
            },
            "snippet": snippet,
            "siblings_in_file": siblings,
            "other_matches": other_matches,
        }))
    }

    /// Composite: explore a file — outline + related files + key symbols in one call.
    pub fn explore_file(&self, path: &str, budget: Budget) -> Result<serde_json::Value> {
        let budget = self.resolve_budget(budget);
        // 1. File outline
        let outline = self.file_outline(path)?;

        // 2. Related files
        let related = self.find_related(path, budget).unwrap_or_default();
        let related_files: Vec<_> = related
            .iter()
            .map(|r| {
                serde_json::json!({
                    "path": r.item.path,
                    "language": r.item.language,
                    "score": r.score,
                })
            })
            .collect();

        // 3. Key symbols (exported first, then by line order)
        let mut key_symbols: Vec<_> = outline.symbols.iter().collect();
        key_symbols.sort_by(|a, b| b.exported.cmp(&a.exported).then(a.line.cmp(&b.line)));
        let key_symbols: Vec<_> = key_symbols
            .iter()
            .take(budget.top_k() * 2)
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "kind": s.kind,
                    "signature": s.signature,
                    "line": s.line,
                    "exported": s.exported,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "path": outline.path,
            "language": outline.language,
            "purpose": outline.purpose,
            "tags": outline.tags,
            "total_symbols": outline.symbols.len(),
            "key_symbols": key_symbols,
            "related_files": related_files,
        }))
    }

    /// Composite: investigate a query — ask_context + diff boost in one call.
    pub fn investigate(&self, query: &str, budget: Budget) -> Result<serde_json::Value> {
        let budget = self.resolve_budget(budget);
        // 1. Main context via ask_context
        let packet = self.ask_context(query, budget)?;

        // 2. Gather diff info if available
        let diff_info = if kungfu_git::is_git_repo(&self.project.root) {
            let changed = kungfu_git::changed_files(&self.project.root).unwrap_or_default();
            if changed.is_empty() {
                None
            } else {
                // Find which changed files overlap with context results
                let relevant_changes: Vec<_> = changed
                    .iter()
                    .filter(|c| {
                        packet
                            .items
                            .iter()
                            .any(|item| item.path.ends_with(c.as_str()) || c.ends_with(&item.path))
                    })
                    .cloned()
                    .collect();
                Some(serde_json::json!({
                    "total_changed_files": changed.len(),
                    "relevant_changed_files": relevant_changes,
                }))
            }
        } else {
            None
        };

        // 3. Build combined result
        let items: Vec<_> = packet
            .items
            .iter()
            .map(|item| {
                serde_json::json!({
                    "name": item.name,
                    "type": item.item_type,
                    "path": item.path,
                    "score": item.score,
                    "why": item.why,
                    "signature": item.signature,
                    "snippet": item.snippet,
                })
            })
            .collect();

        let mut result = serde_json::json!({
            "query": packet.query,
            "intent": packet.intent.map(|i| format!("{:?}", i)),
            "budget": format!("{:?}", packet.budget),
            "retrieval": packet.retrieval,
            "items": items,
        });

        if let (Some(diff), Some(obj)) = (diff_info, result.as_object_mut()) {
            obj.insert("diff".to_string(), diff);
        }

        Ok(result)
    }

    /// Extract a snippet for a single symbol (helper for composite tools).
    pub(crate) fn symbol_snippet(&self, symbol: &Symbol, max_lines: usize) -> Option<String> {
        let abs_path = self.project.root.join(&symbol.path);
        let content = std::fs::read_to_string(&abs_path).ok()?;
        let lines: Vec<&str> = content.lines().collect();

        let start = symbol.span.start_line.saturating_sub(1);
        let end = symbol.span.end_line.min(lines.len());
        if start >= end {
            return None;
        }

        let symbol_len = end - start;
        let take = symbol_len.min(max_lines);
        let mut snippet: Vec<String> = lines[start..start + take]
            .iter()
            .map(|s| s.to_string())
            .collect();
        if snippet.is_empty() {
            return None;
        }
        if take < symbol_len {
            snippet.push(format!(
                "    … truncated, showing {} of {} lines",
                take, symbol_len
            ));
        }
        Some(snippet.join("\n"))
    }
}

/// Pick the best definition among same-name candidates — on tie prefer:
/// source over test/example paths, exact case, definition kinds over
/// variables/modules, larger bodies, exported. Shared by explore_symbol
/// and edit_context so both resolve a name to the same symbol.
pub(crate) fn pick_best_definition<'a>(
    candidates: &'a [kungfu_search::SearchResult<Symbol>],
    name: &str,
) -> Option<&'a kungfu_search::SearchResult<Symbol>> {
    candidates.iter().max_by(|a, b| {
        a.score
            .partial_cmp(&b.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                // Strongest signal: prefer source over test/example paths
                fn is_non_src(path: &str) -> bool {
                    path.contains("test")
                        || path.contains("example")
                        || path.contains("spec/")
                        || path.contains("fixture")
                        || path.contains("evals/")
                        || path.contains("/bench/")
                        || path.contains("__tests__")
                        || path.contains("__mocks__")
                }
                let a_test = is_non_src(&a.item.path);
                let b_test = is_non_src(&b.item.path);
                b_test.cmp(&a_test)
            })
            .then_with(|| {
                // Prefer exact case match
                let a_exact = a.item.name == name;
                let b_exact = b.item.name == name;
                a_exact.cmp(&b_exact)
            })
            .then_with(|| {
                // Prefer definition kinds over variables/modules
                fn kind_rank(sym: &Symbol) -> u8 {
                    match sym.kind {
                        kungfu_types::symbol::SymbolKind::Class => 5,
                        kungfu_types::symbol::SymbolKind::Struct => 5,
                        kungfu_types::symbol::SymbolKind::Trait => 5,
                        kungfu_types::symbol::SymbolKind::Interface => 5,
                        kungfu_types::symbol::SymbolKind::Enum => 4,
                        kungfu_types::symbol::SymbolKind::Function => 3,
                        kungfu_types::symbol::SymbolKind::Method => 3,
                        kungfu_types::symbol::SymbolKind::Impl => 2,
                        kungfu_types::symbol::SymbolKind::Module => 1,
                        _ => 0,
                    }
                }
                kind_rank(&a.item).cmp(&kind_rank(&b.item))
            })
            .then_with(|| {
                // Prefer larger symbols (class definition > getter/field)
                let a_size = a.item.span.end_line.saturating_sub(a.item.span.start_line);
                let b_size = b.item.span.end_line.saturating_sub(b.item.span.start_line);
                a_size.cmp(&b_size)
            })
            .then_with(|| a.item.exported.cmp(&b.item.exported))
    })
}
