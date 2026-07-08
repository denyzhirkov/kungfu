use crate::KungfuService;
use anyhow::Result;
use kungfu_types::annotation::FileAnnotation;
use std::collections::BTreeMap;

/// Outcome of `annotate_file`, explicit about what the annotation changed:
/// the authored module doc keeps precedence, so an annotation on a documented
/// file is stored (for glossary terms and future use) but does not become the
/// served purpose.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AnnotateResult {
    pub path: String,
    /// "applied" — purpose now served from this annotation;
    /// "stored_doc_wins" — saved, but the authored module doc stays the purpose.
    pub status: String,
    pub purpose: String,
    pub terms_recorded: usize,
    pub hint: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AnnotationQueueItem {
    pub path: String,
    pub tags: Vec<String>,
    /// Why this file ranks here (import degree, entrypoint).
    pub why: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AnnotationQueue {
    pub total_unannotated: usize,
    pub items: Vec<AnnotationQueueItem>,
    /// What the agent is expected to do with the items.
    pub instruction: String,
}

impl KungfuService {
    /// Record an agent-written one-line purpose (and optional glossary terms)
    /// for a file. Stored durably in `.kungfu/annotations.json` and merged
    /// into the index immediately; the file's vector text picks it up on the
    /// next `embeddings_build`/sync.
    pub fn annotate_file(
        &self,
        path: &str,
        purpose: &str,
        terms: BTreeMap<String, String>,
    ) -> Result<AnnotateResult> {
        let purpose = purpose.trim();
        anyhow::ensure!(!purpose.is_empty(), "purpose must not be empty");
        anyhow::ensure!(
            purpose.len() <= 300,
            "purpose is a one-liner (max 300 chars) — put longer rationale into memory_add"
        );

        self.ensure_fresh_index()?;
        let mut files = self.store.load_files()?;
        let entry = files
            .iter_mut()
            .find(|f| f.path == path || f.path.ends_with(path))
            .ok_or_else(|| {
                anyhow::anyhow!("file not found in index: {path} — check the path or reindex")
            })?;
        let canonical_path = entry.path.clone();

        let terms_recorded = terms.len();
        let annotation = FileAnnotation {
            purpose: purpose.to_string(),
            terms,
            content_hash: entry.hash.clone(),
            annotated_at: chrono::Utc::now().to_rfc3339(),
        };
        self.store
            .annotations()
            .upsert(&canonical_path, annotation)?;

        // Merge into the live index right away — same precedence as the
        // index-time merge: the authored module doc wins.
        let doc_wins = entry.purpose_source.as_deref() == Some("doc");
        if !doc_wins {
            entry.purpose = Some(purpose.to_string());
            entry.purpose_source = Some("agent".to_string());
        }
        self.store.save_files(&files)?;

        let (status, hint) = if doc_wins {
            (
                "stored_doc_wins",
                "this file has an authored module doc; it stays the served purpose — \
                 the annotation is kept for glossary terms"
                    .to_string(),
            )
        } else {
            (
                "applied",
                "run embeddings_build (or wait for the next sync) to refresh the file's vector"
                    .to_string(),
            )
        };
        Ok(AnnotateResult {
            path: canonical_path,
            status: status.to_string(),
            purpose: purpose.to_string(),
            terms_recorded,
            hint,
        })
    }

    /// The files most worth annotating: no purpose from any source, code, not
    /// tests — ranked by how much of the project imports them (a file many
    /// modules depend on is the one an agent most needs described).
    pub fn annotation_queue(&self, limit: usize) -> Result<AnnotationQueue> {
        self.ensure_fresh_index()?;
        let files = self.store.load_files()?;
        let relations = self.store.relations_arc()?;

        // Import degree per file id (import relations are file-level).
        let mut degree: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for r in relations.iter() {
            *degree.entry(r.target_id.as_str()).or_default() += 1;
        }

        let mut candidates: Vec<(&kungfu_types::file::FileEntry, usize)> = files
            .iter()
            .filter(|f| {
                f.purpose.is_none()
                    && f.language
                        .as_deref()
                        .map(|l| l != "unknown" && l != "json" && l != "yaml" && l != "toml")
                        .unwrap_or(false)
                    && !f.tags.iter().any(|t| t == "tests")
            })
            .map(|f| {
                let mut score = degree.get(f.id.as_str()).copied().unwrap_or(0);
                if f.tags.iter().any(|t| t == "entrypoint") {
                    score += 5;
                }
                (f, score)
            })
            .collect();
        let total_unannotated = candidates.len();
        candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.path.cmp(&b.0.path)));
        candidates.truncate(limit.clamp(1, 50));

        let items = candidates
            .into_iter()
            .map(|(f, score)| AnnotationQueueItem {
                path: f.path.clone(),
                tags: f.tags.clone(),
                why: if f.tags.iter().any(|t| t == "entrypoint") {
                    format!(
                        "entrypoint, imported by {score_rest} files",
                        score_rest = score.saturating_sub(5)
                    )
                } else {
                    format!("imported by {score} files")
                },
            })
            .collect();

        Ok(AnnotationQueue {
            total_unannotated,
            items,
            instruction: "For each file: understand it (file_outline / explore_file), then call \
                          annotate_file with a one-line purpose in the project's language; add \
                          `terms` for project jargon the file defines. Skip files you are not \
                          confident about — a wrong purpose is worse than none."
                .to_string(),
        })
    }
}
