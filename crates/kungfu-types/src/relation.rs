use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Imports,
    Exports,
    Contains,
    Implements,
    References,
    RelatedByName,
    RelatedByPath,
    TestFor,
    ConfigFor,
    Calls,
}

impl fmt::Display for RelationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            RelationKind::Imports => "imports",
            RelationKind::Exports => "exports",
            RelationKind::Contains => "contains",
            RelationKind::Implements => "implements",
            RelationKind::References => "references",
            RelationKind::RelatedByName => "related_by_name",
            RelationKind::RelatedByPath => "related_by_path",
            RelationKind::TestFor => "test_for",
            RelationKind::ConfigFor => "config_for",
            RelationKind::Calls => "calls",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub source_id: String,
    pub target_id: String,
    pub kind: RelationKind,
    pub weight: f32,
}

/// Sidecar metadata about call-graph noise filtering, written at relation-build
/// time (`call_graph_meta.json`). Additive shard: older binaries ignore it, and
/// when it is missing, empty-result diagnostics degrade to plain `no_edges`
/// instead of guessing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CallGraphMeta {
    /// Names of callees whose incoming call edges were dropped by the frequency
    /// cutoff (`call_graph.max_caller_files`). Sorted and deduplicated; bounded
    /// by the number of distinct dropped callees, so the shard stays small.
    #[serde(default)]
    pub frequency_dropped_callees: Vec<String>,
}
