use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Agent-written annotation for one file, keyed by relative path in the
/// sidecar store (`.kungfu/annotations.json`). Durable knowledge — survives
/// reindexes and is meant to be committed to git alongside project memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAnnotation {
    /// One-line description of what the file is for.
    pub purpose: String,
    /// Project-jargon glossary entries contributed by this file: term → meaning.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub terms: BTreeMap<String, String>,
    /// Content hash (`FileEntry.hash`) at annotation time. A mismatch on read
    /// marks the annotation as stale instead of silently trusting it.
    pub content_hash: String,
    pub annotated_at: String,
}
