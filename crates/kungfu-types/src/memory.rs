use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Decision,
    Todo,
    Fixme,
    Note,
    DocSection,
    Rationale,
}

impl std::fmt::Display for MemoryKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            MemoryKind::Decision => "decision",
            MemoryKind::Todo => "todo",
            MemoryKind::Fixme => "fixme",
            MemoryKind::Note => "note",
            MemoryKind::DocSection => "doc_section",
            MemoryKind::Rationale => "rationale",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub path: String,
    pub kind: MemoryKind,
    pub text: String,
    pub anchors: Vec<String>,
    pub weight: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_range: Option<(usize, usize)>,
}
