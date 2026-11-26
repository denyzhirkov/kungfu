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
    /// Present iff `changed_files` is capped, withheld from ranking, or could not be computed.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub changed_files_note: Option<String>,
    /// Full change-set size when `changed_files` was capped or withheld from ranking.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub changed_files_total: Option<usize>,
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
    /// Present iff `memory_conflicts` was truncated or clipped: what was cut and the next action.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub memory_conflicts_note: Option<String>,
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
    /// Present iff config-file symbols matched and the config ranking policy
    /// acted on them (penalised below code, or put on equal terms by a
    /// config-heavy project).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub config_ranking: Option<ConfigRankingNote>,
}

/// How config-file symbols (`config_key` / `config_block`) were ranked
/// against code in one response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigRankingNote {
    pub status: ConfigRankingStatus,
    /// Multiplier applied to config-symbol scores; 1.0 when not penalised.
    pub factor: f64,
    /// One sentence: what happened and how to change it. Shorter at `tiny`.
    pub hint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigRankingStatus {
    /// Config symbols scored below code by `factor`.
    Penalized,
    /// Config-heavy project: config symbols compete with code on equal terms.
    ConfigHeavy,
}

impl ConfigRankingNote {
    pub fn penalized(factor: f64, budget: Budget) -> Self {
        let hint = if budget == Budget::Tiny {
            format!("config ranked x{factor} below code; name the format or file to lift")
        } else {
            format!(
                "config symbols ranked x{factor} below code; name the format or file \
                 (yaml, terraform, package.json) to rank them equally"
            )
        };
        Self {
            status: ConfigRankingStatus::Penalized,
            factor,
            hint,
        }
    }

    /// `fixtures_penalized`: config under test paths was still ranked below code.
    pub fn config_heavy(budget: Budget, fixtures_penalized: bool) -> Self {
        let hint = match (budget == Budget::Tiny, fixtures_penalized) {
            (true, false) => "config-heavy project: config ranks equal to code",
            (true, true) => "config-heavy project: config ranks equal to code, test fixtures below",
            (false, false) => "config-heavy project — config symbols rank on equal terms with code",
            (false, true) => {
                "config-heavy project — config symbols rank on equal terms with code; \
                 config under test paths still ranks below it"
            }
        };
        Self {
            status: ConfigRankingStatus::ConfigHeavy,
            factor: 1.0,
            hint: hint.to_string(),
        }
    }

    /// The ranking was lifted (config query or config-heavy project) and only
    /// config under test paths was penalised: fixtures are not the project's
    /// configuration.
    pub fn test_config_penalized(factor: f64, budget: Budget) -> Self {
        let hint = if budget == Budget::Tiny {
            format!("test-fixture config ranked x{factor} below code")
        } else {
            format!(
                "config under test paths ranked x{factor} below code (fixtures, not project \
                 config); other config ranks equally"
            )
        };
        Self {
            status: ConfigRankingStatus::Penalized,
            factor,
            hint,
        }
    }
}

/// A surfaced conflict between active memory entries — projected to the packet level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConflictItem {
    /// What groups them: `symbol:<name>` (a shared related_symbol).
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
