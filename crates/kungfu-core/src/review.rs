use crate::helpers::{is_code_language, is_test_symbol};
use crate::types::{
    AffectedEntry, AffectedResult, CouplingEntry, HotspotEntry, ReviewResult, SmartTestEntry,
    SmartTestKind, SmartTestResult,
};
use crate::KungfuService;
use anyhow::{bail, Result};
use kungfu_types::file::FileEntry;
use kungfu_types::relation::{Relation, RelationKind};
use kungfu_types::symbol::{Symbol, SymbolKind};
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
        let files = store.files_arc()?;
        let relations = store.relations_arc()?;
        let all_symbols = store.symbols_arc()?;
        let graph = DepGraph::build(&files, &all_symbols, &relations);

        let Some(mut walk) = graph.walk(name, depth) else {
            bail!("symbol '{}' not found", name);
        };
        walk.test_files.sort();
        Ok(AffectedResult {
            symbol: name.to_string(),
            risk: risk_level(walk.entries.len(), walk.test_files.len()).to_string(),
            entries: walk.entries,
            test_files: walk.test_files,
            provenance: self.affected_provenance(),
            note: None,
        })
    }

    fn affected_provenance(&self) -> String {
        format!(
            "reverse deps over imports + call edges; call edges: {}",
            self.call_graph_provenance()
        )
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
        let base = kungfu_git::diff_base(&self.project.root)?;
        let changed_lines = kungfu_git::diff_changed_lines(&self.project.root, &base)?;
        let mut result = self.affected_for_lines(depth, &changed_lines)?;
        if !base.has_commits {
            push_note(&mut result.note, NO_COMMITS_NOTE);
        }
        Ok(result)
    }

    /// `affected_staged` over an explicit change set (verify_change adds
    /// untracked files, which `git diff` cannot see). The graph is built once
    /// and walked per changed symbol; the walk is capped at
    /// `MAX_WALKED_SYMBOLS` names, declared in `note` when it bites.
    pub(crate) fn affected_for_lines(
        &self,
        depth: usize,
        changed_lines: &[kungfu_git::FileChangedLines],
    ) -> Result<AffectedResult> {
        let store = self.store();
        let files = store.files_arc()?;
        let relations = store.relations_arc()?;
        let all_symbols = store.symbols_arc()?;
        let mut symbols_by_path: HashMap<&str, Vec<&Symbol>> = HashMap::new();
        for sym in all_symbols.iter() {
            symbols_by_path
                .entry(sym.path.as_str())
                .or_default()
                .push(sym);
        }

        // Collect unique changed symbol names overlapping diff hunks.
        let mut changed_names: Vec<&str> = Vec::new();
        let mut seen_names: HashSet<&str> = HashSet::new();
        for (file_path, ranges) in changed_lines {
            for sym in symbols_by_path
                .get(file_path.as_str())
                .into_iter()
                .flatten()
            {
                let overlaps = ranges
                    .iter()
                    .any(|&(start, end)| sym.span.start_line <= end && sym.span.end_line >= start);
                if overlaps && seen_names.insert(sym.name.as_str()) {
                    changed_names.push(sym.name.as_str());
                }
            }
        }

        if changed_names.is_empty() {
            return Ok(AffectedResult {
                symbol: "<staged diff>".to_string(),
                entries: Vec::new(),
                test_files: Vec::new(),
                risk: "NONE".to_string(),
                provenance: self.affected_provenance(),
                note: None,
            });
        }

        // One graph, one walk per name (bounded), merge.
        let graph = DepGraph::build(&files, &all_symbols, &relations);
        let walked = changed_names.len().min(MAX_WALKED_SYMBOLS);
        let mut merged_entries: HashMap<(String, String), AffectedEntry> = HashMap::new();
        let mut merged_tests: HashSet<String> = HashSet::new();

        for name in &changed_names[..walked] {
            let Some(walk) = graph.walk(name, depth) else {
                continue;
            };
            for e in walk.entries {
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
            merged_tests.extend(walk.test_files);
        }

        let mut entries: Vec<AffectedEntry> = merged_entries.into_values().collect();
        entries.sort_by_key(|e| e.depth);
        let mut test_files: Vec<String> = merged_tests.into_iter().collect();
        test_files.sort();

        let mut note = None;
        if walked < changed_names.len() {
            push_note(
                &mut note,
                &format!(
                    "blast radius computed for {walked} of {} changed symbols — run affected(<name>) for the rest",
                    changed_names.len()
                ),
            );
        }

        let shown = changed_names.len().min(MAX_NAMES_IN_LABEL);
        let mut label = changed_names[..shown].join(", ");
        if shown < changed_names.len() {
            label.push_str(&format!(", … +{} more", changed_names.len() - shown));
        }

        Ok(AffectedResult {
            symbol: format!("<staged: {label}>"),
            risk: risk_level(entries.len(), test_files.len()).to_string(),
            entries,
            test_files,
            provenance: self.affected_provenance(),
            note,
        })
    }

    /// Find minimal set of tests to run based on git diff.
    pub fn smart_test(&self) -> Result<SmartTestResult> {
        self.ensure_fresh_index()?;
        if !kungfu_git::is_git_repo(&self.project.root) {
            bail!("not a git repository");
        }
        let base = kungfu_git::diff_base(&self.project.root)?;
        let changed_lines = kungfu_git::diff_changed_lines(&self.project.root, &base)?;
        self.smart_test_for_lines(&changed_lines)
    }

    /// `smart_test` over an explicit change set (see `affected_for_lines`).
    pub(crate) fn smart_test_for_lines(
        &self,
        changed_lines: &[kungfu_git::FileChangedLines],
    ) -> Result<SmartTestResult> {
        let store = self.store();
        let all_symbols = store.symbols_arc()?;
        let relations = store.relations_arc()?;
        let files = store.files_arc()?;
        let mut symbols_by_path: HashMap<&str, Vec<&Symbol>> = HashMap::new();
        for sym in all_symbols.iter() {
            symbols_by_path
                .entry(sym.path.as_str())
                .or_default()
                .push(sym);
        }

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

        for (file_path, ranges) in changed_lines {
            changed_file_paths.insert(file_path.clone());
            for sym in symbols_by_path
                .get(file_path.as_str())
                .into_iter()
                .flatten()
            {
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

        // Direct: test files paired with a changed file. The pairing is
        // file-level, so the entry is the whole test file with no test name.
        for (test_path, source_path) in test_for_pairs(&files, &relations) {
            if changed_file_paths.contains(source_path)
                && seen_tests.insert(format!("{test_path}::"))
            {
                test_entries.push(SmartTestEntry {
                    test_name: String::new(),
                    test_path: test_path.to_string(),
                    reason: format!("test_for {source_path}"),
                    kind: SmartTestKind::File,
                });
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
                                    kind: SmartTestKind::Test,
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

        for sym in all_symbols.iter() {
            if is_test_symbol(sym) {
                if let Some((dir, _)) = sym.path.rsplit_once('/') {
                    if changed_dirs.contains(dir) {
                        let key = format!("{}::{}", sym.path, sym.name);
                        if seen_tests.insert(key) {
                            test_entries.push(SmartTestEntry {
                                test_name: sym.name.clone(),
                                test_path: sym.path.clone(),
                                reason: "co-located test".to_string(),
                                kind: SmartTestKind::Test,
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
        let files = store.files_arc()?;

        // Changed files (content, mode, binary, deletion — anything tracked that differs)
        let base = kungfu_git::diff_base(&self.project.root)?;
        let changed_files = kungfu_git::changed_paths(&self.project.root, &base)?;
        if changed_files.is_empty() {
            return Ok(ReviewResult {
                changed_files: vec![],
                changed_symbols: vec![],
                missing_co_changes: vec![],
                untested_changes: vec![],
                risk: "NONE".to_string(),
                summary: if base.has_commits {
                    "No changes detected".to_string()
                } else {
                    format!("No changes detected; {NO_COMMITS_NOTE}")
                },
            });
        }

        // Changed symbols
        let changed_lines = kungfu_git::diff_changed_lines(&self.project.root, &base)?;
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
        let mut missing_co_changes: Vec<String> =
            missing_co_changes(&self.project.root, &changed_file_set)
                .unwrap_or_default()
                .into_iter()
                .map(|m| {
                    format!(
                        "{} (co-changed {}x with {})",
                        m.file, m.count, m.co_changed_with
                    )
                })
                .collect();
        missing_co_changes.sort();
        missing_co_changes.dedup();

        // Find untested changes
        let tested_paths = tested_paths(&files, &relations);
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

        let mut summary = format!(
            "{} files changed, {} symbols modified, {} missing co-changes, {} untested",
            changed_files.len(),
            changed_symbols.len(),
            missing_co_changes.len(),
            untested_changes.len()
        );
        if !base.has_commits {
            summary.push_str("; ");
            summary.push_str(NO_COMMITS_NOTE);
        }

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

/// How many changed symbol names a staged blast radius walks before it stops
/// and says so. 3k untracked files (12k symbols) walked in full took 90s;
/// the cap keeps the composite tools interactive.
const MAX_WALKED_SYMBOLS: usize = 200;
/// How many changed names the `<staged: …>` label spells out.
const MAX_NAMES_IN_LABEL: usize = 20;

/// Honesty note shared by every diff-based tool on a repository without commits.
pub(crate) const NO_COMMITS_NOTE: &str =
    "repository has no commits — changes are measured against the empty tree, so every staged file counts as added";

pub(crate) fn push_note(note: &mut Option<String>, text: &str) {
    match note {
        Some(existing) => {
            existing.push_str("; ");
            existing.push_str(text);
        }
        None => *note = Some(text.to_string()),
    }
}

fn risk_level(entries: usize, test_files: usize) -> &'static str {
    if entries > 20 || test_files > 5 {
        "HIGH"
    } else if entries > 5 || test_files > 2 {
        "MEDIUM"
    } else {
        "LOW"
    }
}

/// Co-change pairs seen fewer times than this are noise.
const CO_CHANGE_MIN_COUNT: usize = 3;
/// Share of the changed file's commits that also touched the partner.
const CO_CHANGE_MIN_CONFIDENCE: f64 = 0.5;
/// A partner touched by more than this share of all commits is a hub
/// (registration list, manifest) that co-changes with everything.
const CO_CHANGE_UBIQUITOUS_SHARE: f64 = 0.25;
/// Below this many commits every active file looks ubiquitous, so the hub
/// filter is off.
const CO_CHANGE_UBIQUITY_MIN_COMMITS: usize = 20;
/// A changed file with fewer commits than this has no meaningful partners:
/// three shared commits out of three read as confidence 1.0.
const CO_CHANGE_MIN_FILE_COMMITS: usize = 5;
/// Strongest partners reported per changed file.
const CO_CHANGE_PARTNERS: usize = 5;
/// Regenerated with their manifest, never edited by hand.
const LOCKFILES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "poetry.lock",
    "Pipfile.lock",
    "uv.lock",
    "go.sum",
    "Gemfile.lock",
    "composer.lock",
    "mix.lock",
    "pubspec.lock",
    "Podfile.lock",
    "flake.lock",
    "packages.lock.json",
];

/// What `missing_co_changes` reads and keeps, for the response to declare.
pub(crate) fn co_change_window() -> String {
    format!(
        "last {} commits (commits touching >{} files skipped); only changed files with ≥{} commits in the window are checked; kept: co-changed ≥{}x and in ≥{:.0}% of the changed file's commits, top {} per changed file; dropped: lockfiles, and files in >{:.0}% of all commits once history has ≥{} commits",
        kungfu_git::CO_CHANGE_WINDOW,
        kungfu_git::CO_CHANGE_MAX_FILES,
        CO_CHANGE_MIN_FILE_COMMITS,
        CO_CHANGE_MIN_COUNT,
        CO_CHANGE_MIN_CONFIDENCE * 100.0,
        CO_CHANGE_PARTNERS,
        CO_CHANGE_UBIQUITOUS_SHARE * 100.0,
        CO_CHANGE_UBIQUITY_MIN_COMMITS,
    )
}

/// A file that historically changes together with a changed file but is
/// absent from the change set.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct MissingCoChange {
    pub file: String,
    pub co_changed_with: String,
    pub count: usize,
    /// count / commits touching `co_changed_with`, two decimals
    pub confidence: f64,
}

/// Files that usually change with `changed` but are not in it, filtered by
/// the rules `co_change_window` states. One git log for the whole set;
/// partners no longer on disk are dropped (renamed or deleted in history).
/// Strongest first.
pub(crate) fn missing_co_changes(
    root: &std::path::Path,
    changed: &HashSet<&str>,
) -> Result<Vec<MissingCoChange>> {
    let history = kungfu_git::co_change_history(root, CO_CHANGE_MIN_COUNT)?;
    let ubiquitous = |path: &str| {
        history.commits >= CO_CHANGE_UBIQUITY_MIN_COMMITS
            && history.file_commits.get(path).copied().unwrap_or(0) as f64
                > history.commits as f64 * CO_CHANGE_UBIQUITOUS_SHARE
    };
    let is_lockfile = |path: &str| LOCKFILES.contains(&path.rsplit('/').next().unwrap_or(path));

    let mut out = Vec::new();
    for file in changed {
        let Some(partners) = history.partners.get(*file) else {
            continue;
        };
        let own_commits = history.file_commits.get(*file).copied().unwrap_or(0);
        if own_commits < CO_CHANGE_MIN_FILE_COMMITS {
            continue;
        }
        let kept = partners
            .iter()
            .map(|(partner, count)| (partner, *count, *count as f64 / own_commits as f64))
            .filter(|(partner, _, confidence)| {
                *confidence >= CO_CHANGE_MIN_CONFIDENCE
                    && !changed.contains(partner.as_str())
                    && !is_lockfile(partner)
                    && !ubiquitous(partner)
                    && root.join(partner).exists()
            })
            .take(CO_CHANGE_PARTNERS);
        for (partner, count, confidence) in kept {
            out.push(MissingCoChange {
                file: partner.clone(),
                co_changed_with: (*file).to_string(),
                count,
                confidence: (confidence * 100.0).round() / 100.0,
            });
        }
    }
    out.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then_with(|| b.count.cmp(&a.count))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.co_changed_with.cmp(&b.co_changed_with))
    });
    Ok(out)
}

/// (test file, source file) path pairs. TestFor links file ids, not symbols.
fn test_for_pairs<'a>(files: &'a [FileEntry], relations: &[Relation]) -> Vec<(&'a str, &'a str)> {
    let path_by_id: HashMap<&str, &str> = files
        .iter()
        .map(|f| (f.id.as_str(), f.path.as_str()))
        .collect();
    relations
        .iter()
        .filter(|r| r.kind == RelationKind::TestFor)
        .filter_map(|r| {
            Some((
                *path_by_id.get(r.source_id.as_str())?,
                *path_by_id.get(r.target_id.as_str())?,
            ))
        })
        .collect()
}

/// Source files paired with a test file.
fn tested_paths<'a>(files: &'a [FileEntry], relations: &[Relation]) -> HashSet<&'a str> {
    test_for_pairs(files, relations)
        .into_iter()
        .map(|(_, source)| source)
        .collect()
}

/// Changed functions and methods with no test evidence: no test file paired
/// with their file (TestFor), no test calling them directly, no test in the
/// same file (inline test module). Test code itself is never listed.
pub(crate) fn untested_symbols<'s>(
    changed: &[&'s Symbol],
    all_symbols: &[Symbol],
    files: &[FileEntry],
    relations: &[Relation],
) -> Vec<&'s Symbol> {
    let test_ids = crate::helpers::test_symbol_ids(all_symbols);
    let tested_files = tested_paths(files, relations);
    let files_with_tests: HashSet<&str> = all_symbols
        .iter()
        .filter(|s| test_ids.contains(&s.id))
        .map(|s| s.path.as_str())
        .collect();
    let test_called: HashSet<&str> = relations
        .iter()
        .filter(|r| r.kind == RelationKind::Calls && test_ids.contains(&r.source_id))
        .map(|r| r.target_id.as_str())
        .collect();
    changed
        .iter()
        .copied()
        .filter(|s| {
            matches!(s.kind, SymbolKind::Function | SymbolKind::Method)
                && !test_ids.contains(&s.id)
                && !tested_files.contains(s.path.as_str())
                && !files_with_tests.contains(s.path.as_str())
                && !test_called.contains(s.id.as_str())
        })
        .collect()
}

/// Result of one reverse-dependency walk from a symbol name.
struct Walk {
    entries: Vec<AffectedEntry>,
    test_files: Vec<String>,
}

/// Reverse-dependency graph over the index, built once per request so a
/// walk per changed symbol costs a BFS, not a reload of files + relations.
struct DepGraph<'a> {
    /// file id or symbol id → path
    id_to_path: HashMap<&'a str, &'a str>,
    symbol_map: HashMap<&'a str, &'a Symbol>,
    ids_by_name: HashMap<&'a str, Vec<&'a str>>,
    file_ids_by_path: HashMap<&'a str, &'a str>,
    /// target id → (source id, "calls" | "imports"); Calls kept at weight >= 0.7
    /// to avoid false positives.
    reverse_deps: HashMap<&'a str, Vec<(&'a str, &'static str)>>,
    /// TestFor: tested path → test file paths
    tests_for_path: HashMap<&'a str, Vec<&'a str>>,
}

impl<'a> DepGraph<'a> {
    fn build(files: &'a [FileEntry], symbols: &'a [Symbol], relations: &'a [Relation]) -> Self {
        let mut id_to_path: HashMap<&str, &str> =
            HashMap::with_capacity(files.len() + symbols.len());
        let mut file_ids_by_path: HashMap<&str, &str> = HashMap::with_capacity(files.len());
        for f in files {
            id_to_path.insert(&f.id, &f.path);
            file_ids_by_path.insert(&f.path, &f.id);
        }
        let mut symbol_map: HashMap<&str, &Symbol> = HashMap::with_capacity(symbols.len());
        let mut ids_by_name: HashMap<&str, Vec<&str>> = HashMap::new();
        for s in symbols {
            id_to_path.insert(&s.id, &s.path);
            symbol_map.insert(&s.id, s);
            ids_by_name.entry(s.name.as_str()).or_default().push(&s.id);
        }

        let mut reverse_deps: HashMap<&str, Vec<(&str, &'static str)>> = HashMap::new();
        let mut tests_for_path: HashMap<&str, Vec<&str>> = HashMap::new();
        for r in relations {
            match r.kind {
                RelationKind::Calls if r.weight >= 0.7 => reverse_deps
                    .entry(r.target_id.as_str())
                    .or_default()
                    .push((r.source_id.as_str(), "calls")),
                RelationKind::Imports => reverse_deps
                    .entry(r.target_id.as_str())
                    .or_default()
                    .push((r.source_id.as_str(), "imports")),
                RelationKind::TestFor => {
                    if let (Some(&test_path), Some(&tested_path)) = (
                        id_to_path.get(r.source_id.as_str()),
                        id_to_path.get(r.target_id.as_str()),
                    ) {
                        tests_for_path
                            .entry(tested_path)
                            .or_default()
                            .push(test_path);
                    }
                }
                _ => {}
            }
        }

        Self {
            id_to_path,
            symbol_map,
            ids_by_name,
            file_ids_by_path,
            reverse_deps,
            tests_for_path,
        }
    }

    /// BFS over reverse deps from every symbol called `name`, up to `depth`
    /// hops. `None` when no symbol has that name.
    fn walk(&self, name: &str, depth: usize) -> Option<Walk> {
        let target_ids = self.ids_by_name.get(name)?;

        let mut visited: HashSet<&str> = target_ids.iter().copied().collect();
        let mut frontier: Vec<(&str, usize)> = target_ids.iter().map(|&id| (id, 0)).collect();

        // File-level relations attach to the file id, so seed the files too.
        let target_paths: HashSet<&str> = target_ids
            .iter()
            .filter_map(|id| self.id_to_path.get(id).copied())
            .collect();
        for &path in &target_paths {
            if let Some(&file_id) = self.file_ids_by_path.get(path) {
                if visited.insert(file_id) {
                    frontier.push((file_id, 0));
                }
            }
        }

        let mut entries: Vec<AffectedEntry> = Vec::new();
        while let Some((current_id, current_depth)) = frontier.pop() {
            if current_depth >= depth {
                continue;
            }
            let Some(deps) = self.reverse_deps.get(current_id) else {
                continue;
            };
            for &(dep_id, reason_kind) in deps {
                if !visited.insert(dep_id) {
                    continue;
                }
                let (entry_name, entry_path, entry_kind) =
                    if let Some(sym) = self.symbol_map.get(dep_id) {
                        (sym.name.clone(), sym.path.clone(), sym.kind.to_string())
                    } else if let Some(&path) = self.id_to_path.get(dep_id) {
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
                    reason: format!("{} {} (depth {})", reason_kind, name, current_depth + 1),
                });
                frontier.push((dep_id, current_depth + 1));
            }
        }

        // Test files covering the targets or anything in the radius (file-level).
        let mut test_files: Vec<String> = Vec::new();
        let mut seen_tests: HashSet<&str> = HashSet::new();
        let affected_paths = entries
            .iter()
            .map(|e| e.path.as_str())
            .chain(target_paths.iter().copied());
        for path in affected_paths {
            for &test in self.tests_for_path.get(path).into_iter().flatten() {
                if seen_tests.insert(test) {
                    test_files.push(test.to_string());
                }
            }
        }

        entries.sort_by_key(|e| e.depth);
        Some(Walk {
            entries,
            test_files,
        })
    }
}
