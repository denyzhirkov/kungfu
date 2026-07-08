//! Sidecar store for agent-written file annotations: `.kungfu/annotations.json`,
//! keyed by relative file path. Durable knowledge, not a derived index shard —
//! it survives reindexes and is meant to be committed to git. The index merges
//! it into `FileEntry.purpose` at build time with `purpose_source: agent`.

use anyhow::{Context, Result};
use kungfu_types::annotation::FileAnnotation;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const ANNOTATIONS_FILE: &str = "annotations.json";

pub struct AnnotationStore {
    path: PathBuf,
}

impl AnnotationStore {
    /// `kungfu_dir` is the `.kungfu` directory (the parent of the index dir).
    pub fn new(kungfu_dir: &Path) -> Self {
        Self {
            path: kungfu_dir.join(ANNOTATIONS_FILE),
        }
    }

    pub fn load(&self) -> Result<BTreeMap<String, FileAnnotation>> {
        if !self.path.exists() {
            return Ok(BTreeMap::new());
        }
        let raw = std::fs::read_to_string(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("parsing {} — fix or delete it", self.path.display()))
    }

    pub fn save(&self, annotations: &BTreeMap<String, FileAnnotation>) -> Result<()> {
        let json = serde_json::to_string_pretty(annotations)?;
        crate::atomic_write(&self.path, &json)
    }

    /// Insert or replace the annotation for `path`. Returns the full map.
    pub fn upsert(
        &self,
        path: &str,
        annotation: FileAnnotation,
    ) -> Result<BTreeMap<String, FileAnnotation>> {
        let mut all = self.load()?;
        all.insert(path.to_string(), annotation);
        self.save(&all)?;
        Ok(all)
    }

    pub fn remove(&self, path: &str) -> Result<Option<FileAnnotation>> {
        let mut all = self.load()?;
        let removed = all.remove(path);
        if removed.is_some() {
            self.save(&all)?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "kungfu-ann-test-{}-{}-{:p}",
            tag,
            std::process::id(),
            &tag as *const _
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trip_and_upsert() {
        let dir = temp_dir("crud");
        let store = AnnotationStore::new(&dir);
        assert!(store.load().unwrap().is_empty());

        let ann = FileAnnotation {
            purpose: "Routing setup".into(),
            terms: BTreeMap::from([("MMR".into(), "marginal relevance".into())]),
            content_hash: "abc".into(),
            annotated_at: "2026-07-08T00:00:00Z".into(),
        };
        store.upsert("src/router.ts", ann).unwrap();

        let all = store.load().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all["src/router.ts"].purpose, "Routing setup");

        assert!(store.remove("src/router.ts").unwrap().is_some());
        assert!(store.load().unwrap().is_empty());
        assert!(store.remove("missing").unwrap().is_none());
    }
}
