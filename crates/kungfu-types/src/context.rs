use serde::{Deserialize, Serialize};

use crate::budget::Budget;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    Lookup,
    Debug,
    Understand,
    Impact,
}

impl std::fmt::Display for Intent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Intent::Lookup => write!(f, "lookup"),
            Intent::Debug => write!(f, "debug"),
            Intent::Understand => write!(f, "understand"),
            Intent::Impact => write!(f, "impact"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPacket {
    pub query: String,
    pub budget: Budget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<Intent>,
    pub items: Vec<ContextItem>,
    /// Files with uncommitted changes (populated by ask_context when git is available)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub changed_files: Vec<String>,
    /// Design rationale and decision context matched to the query
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub rationale: Vec<RationaleItem>,
    /// Timeline events showing how matched code evolved
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub history: Vec<HistoryEvent>,
    /// Verbatim excerpts from docs, comments, or commits as proof
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub evidence: Vec<EvidenceFragment>,
    /// Project memory entries relevant to the query (facts, decisions, warnings)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub project_memory: Vec<ProjectMemoryItem>,
    /// Pairs / clusters of active memory entries that appear to contradict each other
    /// on the same topic. Surface, do not auto-resolve.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub memory_conflicts: Vec<MemoryConflictItem>,
    /// Which retrieval layers produced the candidate pool (retrieval honesty).
    /// `None` for packets built by paths that don't run layered retrieval
    /// (e.g. diff_context).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub retrieval: Option<RetrievalInfo>,
}

/// Retrieval honesty for candidate generation: says whether the vector
/// (embedding) layer contributed, so an agent can tell a keyword-only result
/// from a keyword+vector one. Mirrors the `mode`/`coverage` pattern of
/// `semantic_search`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalInfo {
    /// `"keyword+vector"` when vector candidates entered the pool,
    /// `"keyword_only"` otherwise.
    pub mode: String,
    /// How many candidates the vector layer contributed (added to the pool or
    /// rescored an existing keyword hit). 0 when the layer ran but nothing
    /// cleared the cosine threshold.
    pub vector_candidates: usize,
    /// Present iff the vector layer did not run: why, and the next action to
    /// enable it (e.g. `kungfu embeddings build`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub vector_skipped: Option<String>,
}

/// A surfaced conflict between active memory entries — projected to the packet level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConflictItem {
    /// What groups them: shared tag or symbol.
    pub on: String,
    /// IDs of the entries in the cluster, sorted by updated_at desc.
    pub entry_ids: Vec<String>,
}

/// A project memory entry included in a context packet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMemoryItem {
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub content: String,
    pub pinned: bool,
    pub relevance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextItem {
    #[serde(rename = "type")]
    pub item_type: ContextItemType,
    pub path: String,
    pub name: String,
    pub signature: Option<String>,
    pub why: String,
    pub score: f64,
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextItemType {
    Symbol,
    File,
    Chunk,
    Config,
    Test,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RationaleItem {
    pub source: String,
    pub kind: String,
    pub text: String,
    pub relevance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEvent {
    pub event_type: String,
    pub target: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceFragment {
    pub source: String,
    pub excerpt: String,
}
