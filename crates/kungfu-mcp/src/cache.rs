use crate::scope::apply_scope;
use crate::KungfuMcp;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::SystemTime;
use tracing::info;

pub(crate) const CACHE_CAPACITY: usize = 64;

pub(crate) struct CacheState {
    pub entries: HashMap<u64, String>,
    pub order: Vec<u64>,
    pub index_mtime: Option<SystemTime>,
    pub hits: u64,
    pub misses: u64,
    /// Total bytes returned to agent via kungfu
    pub bytes_served: u64,
    /// Total MCP tool calls served
    pub calls_served: u64,
    /// Sum of on-disk sizes of the distinct source files referenced by served results — i.e. the
    /// bytes an agent would have read by opening those files directly. This is the real baseline
    /// the served bytes are compared against, not a per-call constant.
    pub raw_bytes_baseline: u64,
    /// Cached path → file size map (from the index), reloaded after a reindex clears the cache.
    pub file_sizes: Option<HashMap<String, u64>>,
}

impl CacheState {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: Vec::new(),
            index_mtime: None,
            hits: 0,
            misses: 0,
            bytes_served: 0,
            calls_served: 0,
            raw_bytes_baseline: 0,
            file_sizes: None,
        }
    }

    pub fn get(&mut self, key: u64) -> Option<&String> {
        if let Some(val) = self.entries.get(&key) {
            self.hits += 1;
            // Move to end (most recent)
            self.order.retain(|k| *k != key);
            self.order.push(key);
            Some(val)
        } else {
            self.misses += 1;
            None
        }
    }

    pub fn put(&mut self, key: u64, value: String) {
        if self.entries.len() >= CACHE_CAPACITY && !self.entries.contains_key(&key) {
            // Evict oldest
            if let Some(oldest) = self.order.first().copied() {
                self.order.remove(0);
                self.entries.remove(&oldest);
            }
        }
        self.order.retain(|k| *k != key);
        self.order.push(key);
        self.entries.insert(key, value);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        // File sizes may have changed with the reindex that triggered this clear.
        self.file_sizes = None;
    }
}

/// Load path → file size from the index, for computing the raw-read baseline.
fn load_file_sizes(project_root: &std::path::Path) -> HashMap<String, u64> {
    let path = project_root
        .join(".kungfu")
        .join("index")
        .join("files.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| serde_json::from_str::<Vec<kungfu_types::file::FileEntry>>(&c).ok())
        .map(|files| files.into_iter().map(|f| (f.path, f.size)).collect())
        .unwrap_or_default()
}

/// Distinct values of every `"path"` string field in a result JSON — the source files an agent
/// would otherwise open. Non-JSON or path-less results yield an empty set (baseline 0).
fn referenced_paths(result: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(result) {
        collect_paths(&value, &mut out);
    }
    out
}

fn collect_paths(value: &serde_json::Value, out: &mut std::collections::HashSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if k == "path" {
                    if let Some(p) = v.as_str() {
                        out.insert(p.to_string());
                    }
                } else {
                    collect_paths(v, out);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                collect_paths(v, out);
            }
        }
        _ => {}
    }
}

fn cache_key(tool: &str, query: &str, budget: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tool.hash(&mut hasher);
    query.hash(&mut hasher);
    budget.hash(&mut hasher);
    hasher.finish()
}

impl KungfuMcp {
    pub(crate) fn service(&self) -> std::result::Result<kungfu_core::KungfuService, String> {
        let svc =
            kungfu_core::KungfuService::open(&self.project_root).map_err(|e| e.to_string())?;
        // Auto-reindex if stale (best-effort, don't fail on reindex errors)
        let reindexed = svc.ensure_fresh_index().unwrap_or(false);
        if reindexed {
            if let Ok(mut cache) = self.cache.lock() {
                cache.clear();
                info!("cache cleared after reindex");
            }
        }
        Ok(svc)
    }

    /// Open the service WITHOUT the freshness check. Used by side paths (usage-stats
    /// tracking) that must not pay for a fingerprint scan or trigger a reindex — the
    /// freshness guarantee is already provided by the `service()` call in the compute path.
    pub(crate) fn service_untracked(
        &self,
    ) -> std::result::Result<kungfu_core::KungfuService, String> {
        kungfu_core::KungfuService::open(&self.project_root).map_err(|e| e.to_string())
    }

    /// Check if index has changed since last cache validation, clear cache if so.
    fn validate_cache(&self) {
        let fp_path = self
            .project_root
            .join(".kungfu")
            .join("index")
            .join("fingerprints.json");
        let current_mtime = std::fs::metadata(&fp_path).and_then(|m| m.modified()).ok();

        if let Ok(mut cache) = self.cache.lock() {
            if cache.index_mtime != current_mtime {
                cache.clear();
                cache.index_mtime = current_mtime;
            }
        }
    }

    /// Bytes an agent would have read to reproduce this result by opening the files it references.
    fn raw_baseline_bytes(&self, result: &str) -> u64 {
        let paths = referenced_paths(result);
        if paths.is_empty() {
            return 0;
        }
        let mut cache = match self.cache.lock() {
            Ok(c) => c,
            Err(_) => return 0,
        };
        if cache.file_sizes.is_none() {
            cache.file_sizes = Some(load_file_sizes(&self.project_root));
        }
        let sizes = cache.file_sizes.as_ref().expect("just populated");
        paths
            .iter()
            .filter_map(|p| sizes.get(p.as_str()).copied())
            .sum()
    }

    /// Single accounting point for every served tool result — session counters (calls, bytes,
    /// raw baseline) and the persistent per-command stats. Cached and uncached tools both route
    /// through here so usage stats cover the full surface, not just the cached subset.
    pub(crate) fn record_served(&self, tool: &str, result: &str) {
        let baseline = self.raw_baseline_bytes(result);
        if let Ok(mut cache) = self.cache.lock() {
            cache.bytes_served += result.len() as u64;
            cache.calls_served += 1;
            cache.raw_bytes_baseline += baseline;
        }
        if let Ok(svc) = self.service_untracked() {
            svc.track_call(tool, result.len());
        }
    }

    /// Try to get a cached result, or compute and cache it.
    /// If scope is provided, it's included in the cache key and applied as a post-filter.
    pub(crate) fn cached(
        &self,
        tool: &str,
        query: &str,
        budget: &str,
        compute: impl FnOnce() -> std::result::Result<String, String>,
    ) -> std::result::Result<String, String> {
        self.cached_scoped(tool, query, budget, None, compute)
    }

    pub(crate) fn cached_scoped(
        &self,
        tool: &str,
        query: &str,
        budget: &str,
        scope: Option<&str>,
        compute: impl FnOnce() -> std::result::Result<String, String>,
    ) -> std::result::Result<String, String> {
        self.validate_cache();
        // Include scope in cache key
        let full_key = match scope {
            Some(s) if !s.is_empty() => format!("{}:{}:{}:{}", tool, query, budget, s),
            _ => format!("{}:{}:{}", tool, query, budget),
        };
        let key = cache_key(&full_key, "", "");

        // Check cache
        let cached_val = match self.cache.lock() {
            Ok(mut cache) => cache.get(key).cloned(),
            Err(_) => None,
        };
        if let Some(val) = cached_val {
            self.record_served(tool, &val);
            return Ok(val);
        }

        // Compute
        let result = compute()?;

        // Apply scope filter
        let result = apply_scope(&result, scope);

        if let Ok(mut cache) = self.cache.lock() {
            cache.put(key, result.clone());
        }
        self.record_served(tool, &result);

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn referenced_paths_collects_distinct_path_fields() {
        let json = r#"{
            "query": "x",
            "items": [
                {"name": "a", "path": "src/foo.rs"},
                {"name": "b", "path": "src/foo.rs"},
                {"name": "c", "path": "src/bar.rs"}
            ],
            "nested": {"path": "src/baz.rs"}
        }"#;
        let paths = referenced_paths(json);
        assert_eq!(paths.len(), 3, "dedups repeated paths: {paths:?}");
        assert!(paths.contains("src/foo.rs"));
        assert!(paths.contains("src/bar.rs"));
        assert!(paths.contains("src/baz.rs"));
    }

    #[test]
    fn referenced_paths_empty_for_pathless_or_invalid() {
        assert!(referenced_paths(r#"{"project_name":"k","files":3}"#).is_empty());
        assert!(referenced_paths("not json").is_empty());
    }
}
