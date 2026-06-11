use kungfu_types::Budget;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BudgetParam {
    /// Budget level: "tiny", "small", "medium", "full", or "auto". Default: "auto"
    pub budget: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FilePathParam {
    /// Path to the file (relative to project root)
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EditContextParam {
    /// Symbol name to get edit-ready context for
    pub name: String,
    /// Disambiguate same-name symbols: only consider files under this path prefix (e.g. "crates/kungfu-core")
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReindexParam {
    /// Files to reindex (relative to project root, or absolute under it).
    /// Pass exactly the files you just created/edited/deleted.
    pub paths: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct QueryParam {
    /// Search query or symbol name
    pub query: String,
    /// Budget level: "tiny", "small", "medium", "full", or "auto". Default: "auto" (adapts to project size)
    pub budget: Option<String>,
    /// Limit results to files under this directory path prefix (e.g. "src/", "crates/kungfu-core")
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SymbolNameParam {
    /// Exact symbol name
    pub name: String,
    /// Budget level: "tiny", "small", "medium", "full", or "auto". Default: "auto"
    pub budget: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FilePathBudgetParam {
    /// Path to the file (relative to project root)
    pub path: String,
    /// Budget level: "tiny", "small", "medium", "full", or "auto". Default: "auto"
    pub budget: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SymbolBudgetParam {
    /// Symbol name to explore
    pub name: String,
    /// Budget level: "tiny", "small", "medium", "full", or "auto". Default: "auto"
    pub budget: Option<String>,
    /// Limit results to files under this directory path prefix (e.g. "src/", "crates/kungfu-core")
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HotspotsParam {
    /// Number of results to return. Default: 20
    pub top: Option<usize>,
    /// Weight by git change frequency (LOC × commits). Default: false
    pub churn: Option<bool>,
    /// Show file-level hotspots instead of symbol-level. Default: false
    pub files: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AffectedParam {
    /// Symbol name to analyze blast radius for. Omit when `staged=true`.
    #[serde(default)]
    pub name: String,
    /// Max depth of transitive analysis. Default: 3
    pub depth: Option<usize>,
    /// Analyze blast radius of all currently staged-diff changes instead of a single symbol.
    pub staged: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CouplingParam {
    /// Number of results to return. Default: 20
    pub top: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskContextParam {
    /// Search query or task description
    pub query: String,
    /// Budget level: "tiny", "small", "medium", "full", or "auto". Default: "auto"
    pub budget: Option<String>,
    /// Limit results to files under this directory path prefix (e.g. "src/", "crates/kungfu-core")
    pub scope: Option<String>,
    /// Context layers to include: "code" (default), "rationale", "history". Example: ["code", "rationale"]
    pub include: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CommitContextParam {
    /// Commit hash (full or short).
    pub hash: String,
    /// Budget level: "tiny", "small", "medium", "full", or "auto". Default: "auto"
    pub budget: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PrContextParam {
    /// PR number.
    pub num: u32,
    /// Budget level: "tiny", "small", "medium", "full", or "auto". Default: "auto"
    pub budget: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DebugTraceParam {
    /// Stack trace, panic, or traceback text. Multi-line OK.
    pub trace: String,
    /// Budget level: "tiny", "small", "medium", "full", or "auto". Default: "auto"
    pub budget: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryAddParam {
    /// Content of the memory entry
    pub content: String,
    /// Kind: "fact", "decision", "warning", "session_summary"
    pub kind: String,
    /// Short title (optional)
    pub title: Option<String>,
    /// Tags for categorization
    pub tags: Option<Vec<String>>,
    /// Related file paths
    pub files: Option<Vec<String>>,
    /// Related symbol names
    pub symbols: Option<Vec<String>>,
    /// Pin for higher priority in context assembly
    pub pin: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemorySearchParam {
    /// Search query
    pub query: String,
    /// Filter by kind: "fact", "decision", "warning", "session_summary"
    pub kind: Option<String>,
    /// Filter by tag
    pub tag: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryListParam {
    /// Filter by kind: "fact", "decision", "warning", "session_summary"
    pub kind: Option<String>,
    /// Filter by tag
    pub tag: Option<String>,
    /// Show only pinned entries
    pub pinned: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryIdParam {
    /// Memory entry ID (e.g. "mem_0001")
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryUpdateParam {
    /// Memory entry ID
    pub id: String,
    /// New content (optional)
    pub content: Option<String>,
    /// New title (optional)
    pub title: Option<String>,
    /// Replace tags (optional)
    pub tags: Option<Vec<String>>,
    /// Set pinned state (optional)
    pub pin: Option<bool>,
}

pub(crate) fn parse_budget(s: Option<&str>) -> Budget {
    s.and_then(|s| s.parse().ok()).unwrap_or(Budget::Auto)
}
