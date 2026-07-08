use std::collections::HashMap;

pub struct StatusInfo {
    pub project_name: String,
    pub root: String,
    pub indexed_files: usize,
    pub indexed_symbols: usize,
    pub languages: HashMap<String, usize>,
    pub has_git: bool,
}

pub struct RepoOutline {
    pub project_name: String,
    pub total_files: usize,
    pub total_symbols: usize,
    pub languages: HashMap<String, usize>,
    pub top_dirs: Vec<DirEntry>,
    pub entrypoints: Vec<String>,
}

pub struct DirEntry {
    pub path: String,
    pub file_count: usize,
}

pub struct FileOutline {
    pub path: String,
    pub language: Option<String>,
    /// Authored one-liner from the module doc comment — the trusted layer.
    pub purpose: Option<String>,
    /// Heuristic tags from path/import/symbol signals — structural provenance.
    pub tags: Vec<String>,
    pub symbols: Vec<SymbolOutline>,
}

pub struct SymbolOutline {
    pub name: String,
    pub kind: String,
    pub signature: Option<String>,
    pub line: usize,
    pub exported: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OnboardInfo {
    pub project_name: String,
    pub languages: Vec<(String, usize)>,
    pub primary_language: Option<String>,
    pub architecture: String,
    pub top_dirs: Vec<(String, usize)>,
    pub entrypoints: Vec<String>,
    pub key_symbols: Vec<String>,
    pub naming_style: String,
    pub test_pattern: String,
    pub total_files: usize,
    pub total_symbols: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AffectedEntry {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub depth: usize,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AffectedResult {
    pub symbol: String,
    pub entries: Vec<AffectedEntry>,
    pub test_files: Vec<String>,
    pub risk: String,
    /// How the dependency edges were derived and what was filtered out —
    /// a bounded result must never look exhaustive (retrieval honesty).
    pub provenance: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SmartTestEntry {
    pub test_name: String,
    pub test_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SmartTestResult {
    pub changed_symbols: Vec<String>,
    pub tests: Vec<SmartTestEntry>,
    pub total_tests_in_project: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ReviewResult {
    pub changed_files: Vec<String>,
    pub changed_symbols: Vec<String>,
    pub missing_co_changes: Vec<String>,
    pub untested_changes: Vec<String>,
    pub risk: String,
    pub summary: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CouplingEntry {
    pub path: String,
    pub fan_in: usize,
    pub fan_out: usize,
    pub co_change_count: usize,
    pub risk_score: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HotspotEntry {
    pub name: String,
    pub path: String,
    pub lines: usize,
    pub kind: Option<String>,
    pub churn: Option<usize>,
    pub score: f64,
}
