use crate::helpers::{is_code_language, is_test_symbol};
use crate::types::{
    AffectedEntry, AffectedResult, CouplingEntry, HotspotEntry, ReviewResult, SmartTestEntry,
    SmartTestResult,
};
use crate::KungfuService;
use anyhow::{bail, Result};
use kungfu_types::relation::RelationKind;
use kungfu_types::symbol::Symbol;
use std::collections::{HashMap, HashSet};

impl KungfuService {
    /// Find largest symbols or files, optionally weighted by git churn.
    pub fn hotspots(&self, top: usize, churn: bool, files_mode: bool) -> Result<Vec<HotspotEntry>> {
        self.ensure_fresh_index()?;

        let churn_counts = if churn && kungfu_git::is_git_repo(&self.project.root) {
            kungfu_git::file_commit_counts(&self.project.root).unwrap_or_default()
        } else {
            HashMap::new()
        };

        let mut entries: Vec<HotspotEntry> = if files_mode {
            let files = self.store().load_files()?;
            files
                .into_iter()
                .map(|f| {
                    let lines = f.size as usize;
                    let file_churn = churn_counts.get(&f.path).copied();
                    let score = if churn {
                        lines as f64 * file_churn.unwrap_or(1) as f64
                    } else {
                        lines as f64
                    };
                    HotspotEntry {
                        name: f.path.rsplit('/').next().unwrap_or(&f.path).to_string(),
                        path: f.path,
                        lines,
                        kind: f.language,
                        churn: file_churn,
                        score,
                    }
                })
                .collect()
        } else {
            let symbols = self.store().load_symbols()?;
            symbols
                .into_iter()
                .map(|s| {
                    let lines = s.span.end_line.saturating_sub(s.span.start_line) + 1;
                    let file_churn = churn_counts.get(&s.path).copied();
                    let score = if churn {
                        lines as f64 * file_churn.unwrap_or(1) as f64
                    } else {
                        lines as f64
                    };
                    HotspotEntry {
                        name: s.name,
                        path: s.path,
                        lines,
                        kind: Some(format!("{:?}", s.kind)),
                        churn: file_churn,
                        score,
                    }
                })
                .collect()
        };

        entries.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        entries.truncate(top);
        Ok(entries)
    }

    /// Blast radius analysis: find all transitive callers/dependents of a symbol.
    pub fn affected(&self, name: &str, depth: usize) -> Result<AffectedResult> {
        self.ensure_fresh_index()?;
        let store = self.store();
        let files = store.load_files()?;
        let relations = store.relations_arc()?;
        let all_symbols = self.search().get_all_symbols()?;

        // Find target symbol IDs
        let target_ids: HashSet<String> = all_symbols
            .iter()
            .filter(|s| s.name == name)
            .map(|s| s.id.clone())
            .collect();

        if target_ids.is_empty() {
            bail!("symbol '{}' not found", name);
        }

        // Build unified ID → path map (both file and symbol IDs)
        let mut id_to_path: HashMap<&str, &str> = HashMap::new();
        for f in &files {
            id_to_path.insert(&f.id, &f.path);
        }
        let symbol_map: HashMap<&str, &Symbol> = all_symbols
            .iter()
            .map(|s| {
                id_to_path.insert(&s.id, &s.path);
                (s.id.as_str(), s)
            })
            .collect();

        // Build reverse dependency graph: target_id → source_ids (Calls + Imports)
        // Only include high-confidence relations (weight >= 0.7) to avoid false positives
        let mut reverse_deps: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
        for r in relations.iter() {
            match r.kind {
                RelationKind::Calls if r.weight >= 0.7 => {
                    reverse_deps
                        .entry(r.target_id.as_str())
                        .or_default()
                        .push((r.source_id.as_str(), "calls"));
                }
                RelationKind::Imports => {
                    reverse_deps
                        .entry(r.target_id.as_str())
                        .or_default()
                        .push((r.source_id.as_str(), "imports"));
                }
                _ => {}
            }
        }

        // BFS through reverse dependency graph
        let mut entries: Vec<AffectedEntry> = Vec::new();
        let mut visited: HashSet<String> = target_ids.clone();
        let mut frontier: Vec<(String, usize)> =
            target_ids.iter().map(|id| (id.clone(), 0)).collect();

        // Also collect target file IDs (for file-level relation matching)
        let target_paths: HashSet<&str> = target_ids
            .iter()
            .filter_map(|id| id_to_path.get(id.as_str()).copied())
            .collect();
        // Add file IDs that map to target paths
        for f in &files {
            if target_paths.contains(f.path.as_str()) && !visited.contains(&f.id) {
                frontier.push((f.id.clone(), 0));
                visited.insert(f.id.clone());
            }
        }

        while let Some((current_id, current_depth)) = frontier.pop() {
            if current_depth >= depth {
                continue;
            }
            if let Some(deps) = reverse_deps.get(current_id.as_str()) {
                for &(dep_id, reason_kind) in deps {
                    if visited.insert(dep_id.to_string()) {
                        let (entry_name, entry_path, entry_kind) =
                            if let Some(sym) = symbol_map.get(dep_id) {
                                (sym.name.clone(), sym.path.clone(), sym.kind.to_string())
                            } else if let Some(&path) = id_to_path.get(dep_id) {
                                let fname = path.rsplit('/').next().unwrap_or(path).to_string();
                                (fname, path.to_string(), "file".to_string())
                            } else {
                                continue;
                            };
                        entries.push(AffectedEntry {
                            name: entry_name,
                            path: entry_path,
                            kind: entry_kind,
                            depth: current_depth + 1,
                            reason: format!(
                                "{} {} (depth {})",
                                reason_kind,
                                name,
                                current_depth + 1
                            ),
                        });
                        frontier.push((dep_id.to_string(), current_depth + 1));
                    }
                }
            }
        }

        // Find affected test files via TestFor relations (file-level)
        let affected_paths: HashSet<&str> = entries
            .iter()
            .map(|e| e.path.as_str())
            .chain(target_paths.iter().copied())
            .collect();
        let mut test_files: Vec<String> = Vec::new();
        for r in relations.iter() {
            if r.kind == RelationKind::TestFor {
                let source_path = id_to_path.get(r.source_id.as_str()).copied().unwrap_or("");
                let target_path = id_to_path.get(r.target_id.as_str()).copied().unwrap_or("");
                if !target_path.is_empty()
                    && affected_paths.contains(target_path)
                    && !source_path.is_empty()
                    && !test_files.contains(&source_path.to_string())
                {
                    test_files.push(source_path.to_string());
                }
            }
        }

        // Risk assessment
        let risk = if entries.len() > 20 || test_files.len() > 5 {
            "HIGH".to_string()
        } else if entries.len() > 5 || test_files.len() > 2 {
            "MEDIUM".to_string()
        } else {
            "LOW".to_string()
        };

        entries.sort_by_key(|e| e.depth);

        Ok(AffectedResult {
            symbol: name.to_string(),
            entries,
            test_files,
            risk,
        })
    }

    /// Reverse of smart_test: given a test symbol name, return the production code it exercises.
    /// Walks Calls-relations from the test symbol, filters out other tests.
    pub fn test_subjects(&self, test_name: &str) -> Result<Vec<(Symbol, String)>> {
        self.ensure_fresh_index()?;
        let store = self.store();
        let relations = store.relations_arc()?;
        let all_symbols = self.search().get_all_symbols()?;

        // Find test symbol candidates by exact name. There may be several if duplicated.
        let test_ids: HashSet<&str> = all_symbols
            .iter()
            .filter(|s| s.name == test_name && is_test_symbol(s))
            .map(|s| s.id.as_str())
            .collect();

        if test_ids.is_empty() {
            bail!("test symbol '{}' not found", test_name);
        }

        // 1-hop callees.
        let mut direct: HashSet<&str> = HashSet::new();
        for r in relations.iter() {
            if r.kind == RelationKind::Calls && test_ids.contains(r.source_id.as_str()) {
                direct.insert(r.target_id.as_str());
            }
        }

        // 2-hop callees via thin test helpers (other test/helper symbols that aren't production).
        // Keep set of "via" edges so we can label why a 2-hop target was included.
        let mut transitive: HashMap<&str, &str> = HashMap::new();
        for r in relations.iter() {
            if r.kind == RelationKind::Calls && direct.contains(r.source_id.as_str()) {
                let target = r.target_id.as_str();
                if !direct.contains(target) && !test_ids.contains(target) {
                    transitive.entry(target).or_insert(r.source_id.as_str());
                }
            }
        }

        let symbol_map: HashMap<&str, &Symbol> =
            all_symbols.iter().map(|s| (s.id.as_str(), s)).collect();

        let mut results: Vec<(Symbol, String)> = Vec::new();
        let mut seen = HashSet::new();

        for id in &direct {
            if let Some(sym) = symbol_map.get(id) {
                if is_test_symbol(sym) {
                    continue;
                }
                if seen.insert(*id) {
                    results.push(((*sym).clone(), format!("called directly by {}", test_name)));
                }
            }
        }

        for (id, via_id) in &transitive {
            if let Some(sym) = symbol_map.get(id) {
                if is_test_symbol(sym) {
                    continue;
                }
                let via_name = symbol_map
                    .get(via_id)
                    .map(|s| s.name.as_str())
                    .unwrap_or("helper");
                if seen.insert(*id) {
                    results.push(((*sym).clone(), format!("via {}", via_name)));
                }
            }
        }

        Ok(results)
    }

    /// Blast radius of all staged changes: collect changed symbols from git diff,
    /// run `affected` on each, merge results.
    pub fn affected_staged(&self, depth: usize) -> Result<AffectedResult> {
        self.ensure_fresh_index()?;
        if !kungfu_git::is_git_repo(&self.project.root) {
            bail!("not a git repository");
        }

        let all_symbols = self.store().load_symbols()?;
        let changed_lines = kungfu_git::diff_changed_lines(&self.project.root)?;

        // Collect unique changed symbol names overlapping diff hunks.
        let mut changed_names: Vec<String> = Vec::new();
        let mut seen_names: HashSet<String> = HashSet::new();
        for (file_path, ranges) in &changed_lines {
            for sym in &all_symbols {
                if sym.path != *file_path {
                    continue;
                }
                let overlaps = ranges
                    .iter()
                    .any(|&(start, end)| sym.span.start_line <= end && sym.span.end_line >= start);
                if overlaps && seen_names.insert(sym.name.clone()) {
                    changed_names.push(sym.name.clone());
                }
            }
        }

        if changed_names.is_empty() {
            return Ok(AffectedResult {
                symbol: "<staged diff>".to_string(),
                entries: Vec::new(),
                test_files: Vec::new(),
                risk: "NONE".to_string(),
            });
        }

        // Run affected per name, merge.
        let mut merged_entries: HashMap<(String, String), AffectedEntry> = HashMap::new();
        let mut merged_tests: HashSet<String> = HashSet::new();

        for name in &changed_names {
            let r = match self.affected(name, depth) {
                Ok(r) => r,
                Err(_) => continue, // symbol may resolve via duplicate name; skip if missing
            };
            for e in r.entries {
                let key = (e.path.clone(), e.name.clone());
                merged_entries
                    .entry(key)
                    .and_modify(|existing| {
                        if e.depth < existing.depth {
                            existing.depth = e.depth;
                            existing.reason = e.reason.clone();
                        }
                    })
                    .or_insert(e);
            }
            for t in r.test_files {
                merged_tests.insert(t);
            }
        }

        let mut entries: Vec<AffectedEntry> = merged_entries.into_values().collect();
        entries.sort_by_key(|e| e.depth);
        let mut test_files: Vec<String> = merged_tests.into_iter().collect();
        test_files.sort();

        let risk = if entries.len() > 20 || test_files.len() > 5 {
            "HIGH"
        } else if entries.len() > 5 || test_files.len() > 2 {
            "MEDIUM"
        } else {
            "LOW"
        };

        Ok(AffectedResult {
            symbol: format!("<staged: {}>", changed_names.join(", ")),
            entries,
            test_files,
            risk: risk.to_string(),
        })
    }

    /// Find minimal set of tests to run based on git diff.
    pub fn smart_test(&self) -> Result<SmartTestResult> {
        self.ensure_fresh_index()?;
        if !kungfu_git::is_git_repo(&self.project.root) {
            bail!("not a git repository");
        }

        let store = self.store();
        let all_symbols = store.load_symbols()?;
        let relations = store.relations_arc()?;

        // Get changed line ranges
        let changed_lines = kungfu_git::diff_changed_lines(&self.project.root)?;
        if changed_lines.is_empty() {
            return Ok(SmartTestResult {
                changed_symbols: vec![],
                tests: vec![],
                total_tests_in_project: count_test_symbols(&all_symbols),
            });
        }

        // Find symbols that overlap with changed lines
        let mut changed_symbols: Vec<String> = Vec::new();
        let mut changed_symbol_ids: HashSet<String> = HashSet::new();
        let mut changed_file_paths: HashSet<String> = HashSet::new();

        for (file_path, ranges) in &changed_lines {
            changed_file_paths.insert(file_path.clone());
            for sym in &all_symbols {
                if sym.path == *file_path {
                    for &(start, end) in ranges {
                        if sym.span.start_line <= end
                            && sym.span.end_line >= start
                            && changed_symbol_ids.insert(sym.id.clone())
                        {
                            changed_symbols.push(format!("{}::{}", sym.path, sym.name));
                        }
                    }
                }
            }
        }

        // Find test symbols via call graph and test_for relations
        let mut test_entries: Vec<SmartTestEntry> = Vec::new();
        let mut seen_tests: HashSet<String> = HashSet::new();

        // Build reverse call graph
        let mut reverse_calls: HashMap<&str, Vec<&str>> = HashMap::new();
        for r in relations.iter() {
            if r.kind == RelationKind::Calls {
                reverse_calls
                    .entry(r.target_id.as_str())
                    .or_default()
                    .push(r.source_id.as_str());
            }
        }

        let symbol_map: HashMap<&str, &Symbol> =
            all_symbols.iter().map(|s| (s.id.as_str(), s)).collect();

        // Direct: test_for relations pointing to changed files
        for r in relations.iter() {
            if r.kind == RelationKind::TestFor {
                if let Some(target_sym) = symbol_map.get(r.target_id.as_str()) {
                    if changed_file_paths.contains(&target_sym.path) {
                        if let Some(source_sym) = symbol_map.get(r.source_id.as_str()) {
                            let key = format!("{}::{}", source_sym.path, source_sym.name);
                            if seen_tests.insert(key) {
                                test_entries.push(SmartTestEntry {
                                    test_name: source_sym.name.clone(),
                                    test_path: source_sym.path.clone(),
                                    reason: format!("test_for {}", target_sym.path),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Call graph: find test functions that call changed symbols (1 hop)
        for changed_id in &changed_symbol_ids {
            if let Some(callers) = reverse_calls.get(changed_id.as_str()) {
                for &caller_id in callers {
                    if let Some(caller) = symbol_map.get(caller_id) {
                        if is_test_symbol(caller) {
                            let key = format!("{}::{}", caller.path, caller.name);
                            if seen_tests.insert(key) {
                                let changed_name = symbol_map
                                    .get(changed_id.as_str())
                                    .map(|s| s.name.as_str())
                                    .unwrap_or("?");
                                test_entries.push(SmartTestEntry {
                                    test_name: caller.name.clone(),
                                    test_path: caller.path.clone(),
                                    reason: format!("calls changed symbol {}", changed_name),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Also include test files in the same directory as changed files
        let changed_dirs: HashSet<&str> = changed_file_paths
            .iter()
            .filter_map(|p| p.rsplit_once('/').map(|(dir, _)| dir))
            .collect();

        for sym in &all_symbols {
            if is_test_symbol(sym) {
                if let Some((dir, _)) = sym.path.rsplit_once('/') {
                    if changed_dirs.contains(dir) {
                        let key = format!("{}::{}", sym.path, sym.name);
                        if seen_tests.insert(key) {
                            test_entries.push(SmartTestEntry {
                                test_name: sym.name.clone(),
                                test_path: sym.path.clone(),
                                reason: "co-located test".to_string(),
                            });
                        }
                    }
                }
            }
        }

        Ok(SmartTestResult {
            changed_symbols,
            tests: test_entries,
            total_tests_in_project: count_test_symbols(&all_symbols),
        })
    }

    /// Code review context: analyze diff for risks, missing co-changes, untested code.
    pub fn review(&self) -> Result<ReviewResult> {
        self.ensure_fresh_index()?;
        if !kungfu_git::is_git_repo(&self.project.root) {
            bail!("not a git repository");
        }

        let store = self.store();
        let all_symbols = store.load_symbols()?;
        let relations = store.relations_arc()?;

        // Changed files
        let changed_files = kungfu_git::diff_files(&self.project.root)?;
        if changed_files.is_empty() {
            return Ok(ReviewResult {
                changed_files: vec![],
                changed_symbols: vec![],
                missing_co_changes: vec![],
                untested_changes: vec![],
                risk: "NONE".to_string(),
                summary: "No changes detected".to_string(),
            });
        }

        // Changed symbols
        let changed_lines = kungfu_git::diff_changed_lines(&self.project.root)?;
        let mut changed_symbols: Vec<String> = Vec::new();
        let changed_file_set: HashSet<&str> = changed_files.iter().map(|s| s.as_str()).collect();

        for (file_path, ranges) in &changed_lines {
            for sym in &all_symbols {
                if sym.path == *file_path {
                    for &(start, end) in ranges {
                        if sym.span.start_line <= end && sym.span.end_line >= start {
                            changed_symbols.push(format!("{}::{}", sym.path, sym.name));
                            break;
                        }
                    }
                }
            }
        }

        // Co-change analysis: find files that usually change together
        let co_changes = kungfu_git::co_change_pairs(&self.project.root, 3).unwrap_or_default();
        let mut missing_co_changes: Vec<String> = Vec::new();
        for file in &changed_files {
            if let Some(partners) = co_changes.get(file) {
                for (partner, count) in partners.iter().take(5) {
                    if !changed_file_set.contains(partner.as_str()) {
                        missing_co_changes
                            .push(format!("{} (co-changed {}x with {})", partner, count, file));
                    }
                }
            }
        }
        missing_co_changes.sort();
        missing_co_changes.dedup();

        // Find untested changes
        let symbol_map: HashMap<&str, &Symbol> =
            all_symbols.iter().map(|s| (s.id.as_str(), s)).collect();
        let mut tested_paths: HashSet<&str> = HashSet::new();
        for r in relations.iter() {
            if r.kind == RelationKind::TestFor {
                if let Some(target) = symbol_map.get(r.target_id.as_str()) {
                    tested_paths.insert(&target.path);
                }
            }
        }

        let untested_changes: Vec<String> = changed_files
            .iter()
            .filter(|f| {
                let is_code = f.ends_with(".rs")
                    || f.ends_with(".ts")
                    || f.ends_with(".js")
                    || f.ends_with(".py")
                    || f.ends_with(".go");
                let is_test = f.contains("test") || f.contains("spec");
                is_code && !is_test && !tested_paths.contains(f.as_str())
            })
            .cloned()
            .collect();

        // Risk assessment
        let risk = if changed_files.len() > 10
            || !missing_co_changes.is_empty() && untested_changes.len() > 3
        {
            "HIGH"
        } else if changed_files.len() > 5
            || !missing_co_changes.is_empty()
            || !untested_changes.is_empty()
        {
            "MEDIUM"
        } else {
            "LOW"
        };

        let summary = format!(
            "{} files changed, {} symbols modified, {} missing co-changes, {} untested",
            changed_files.len(),
            changed_symbols.len(),
            missing_co_changes.len(),
            untested_changes.len()
        );

        Ok(ReviewResult {
            changed_files,
            changed_symbols,
            missing_co_changes,
            untested_changes,
            risk: risk.to_string(),
            summary,
        })
    }

    /// Analyze module coupling: fan-in, fan-out, co-change frequency.
    pub fn coupling(&self, top: usize) -> Result<Vec<CouplingEntry>> {
        self.ensure_fresh_index()?;
        let store = self.store();
        let files = store.load_files()?;
        let relations = store.relations_arc()?;

        // Build ID → path maps for both files and symbols
        let all_symbols = store.load_symbols()?;
        let mut id_to_path: HashMap<&str, &str> = HashMap::new();
        for f in &files {
            id_to_path.insert(&f.id, &f.path);
        }
        for s in &all_symbols {
            id_to_path.insert(&s.id, &s.path);
        }

        let mut fan_in: HashMap<String, usize> = HashMap::new();
        let mut fan_out: HashMap<String, usize> = HashMap::new();

        for r in relations.iter() {
            // Only count high-confidence structural relations
            let dominated = matches!(
                r.kind,
                RelationKind::Imports | RelationKind::ConfigFor | RelationKind::TestFor
            ) || (r.kind == RelationKind::Calls && r.weight >= 0.7);
            if !dominated {
                continue;
            }
            let source_file = id_to_path.get(r.source_id.as_str()).copied().unwrap_or("");
            let target_file = id_to_path.get(r.target_id.as_str()).copied().unwrap_or("");
            if !source_file.is_empty() && !target_file.is_empty() && source_file != target_file {
                *fan_out.entry(source_file.to_string()).or_default() += 1;
                *fan_in.entry(target_file.to_string()).or_default() += 1;
            }
        }

        // Co-change counts
        let co_changes = if kungfu_git::is_git_repo(&self.project.root) {
            kungfu_git::co_change_pairs(&self.project.root, 2).unwrap_or_default()
        } else {
            HashMap::new()
        };

        let mut entries: Vec<CouplingEntry> = files
            .iter()
            .filter(|f| f.language.as_deref().map(is_code_language).unwrap_or(false))
            .map(|f| {
                let fi = *fan_in.get(&f.path).unwrap_or(&0);
                let fo = *fan_out.get(&f.path).unwrap_or(&0);
                let co = co_changes.get(&f.path).map(|v| v.len()).unwrap_or(0);
                let risk_score = (fi as f64 * 0.4) + (fo as f64 * 0.3) + (co as f64 * 0.3);
                CouplingEntry {
                    path: f.path.clone(),
                    fan_in: fi,
                    fan_out: fo,
                    co_change_count: co,
                    risk_score,
                }
            })
            .filter(|e| e.fan_in > 0 || e.fan_out > 0)
            .collect();

        entries.sort_by(|a, b| {
            b.risk_score
                .partial_cmp(&a.risk_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        entries.truncate(top);
        Ok(entries)
    }
}

pub(crate) fn count_test_symbols(symbols: &[Symbol]) -> usize {
    symbols.iter().filter(|s| is_test_symbol(s)).count()
}
