use serde::{Deserialize, Serialize};

// --- Code memory (auto-extracted from source comments and docs) ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeMemoryKind {
    Decision,
    Todo,
    Fixme,
    Note,
    DocSection,
    Rationale,
}

impl std::fmt::Display for CodeMemoryKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            CodeMemoryKind::Decision => "decision",
            CodeMemoryKind::Todo => "todo",
            CodeMemoryKind::Fixme => "fixme",
            CodeMemoryKind::Note => "note",
            CodeMemoryKind::DocSection => "doc_section",
            CodeMemoryKind::Rationale => "rationale",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeMemoryEntry {
    pub id: String,
    pub path: String,
    pub kind: CodeMemoryKind,
    pub text: String,
    pub anchors: Vec<String>,
    pub weight: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_range: Option<(usize, usize)>,
}

// Backward-compatible aliases
pub type MemoryEntry = CodeMemoryEntry;
pub type MemoryKind = CodeMemoryKind;

// --- Project memory (explicit, user/agent managed) ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectMemoryKind {
    Fact,
    Decision,
    Warning,
    SessionSummary,
}

impl std::fmt::Display for ProjectMemoryKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ProjectMemoryKind::Fact => "fact",
            ProjectMemoryKind::Decision => "decision",
            ProjectMemoryKind::Warning => "warning",
            ProjectMemoryKind::SessionSummary => "session_summary",
        };
        write!(f, "{}", s)
    }
}

impl std::str::FromStr for ProjectMemoryKind {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "fact" => Ok(ProjectMemoryKind::Fact),
            "decision" => Ok(ProjectMemoryKind::Decision),
            "warning" => Ok(ProjectMemoryKind::Warning),
            "session_summary" | "session" => Ok(ProjectMemoryKind::SessionSummary),
            _ => Err(format!("unknown memory kind: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    #[default]
    Active,
    Archived,
    Superseded,
}

impl std::fmt::Display for MemoryStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            MemoryStatus::Active => "active",
            MemoryStatus::Archived => "archived",
            MemoryStatus::Superseded => "superseded",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMemoryEntry {
    pub id: String,
    pub kind: ProjectMemoryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_symbols: Vec<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub status: MemoryStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
