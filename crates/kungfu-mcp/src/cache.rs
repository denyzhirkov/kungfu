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
        if let Ok(mut cache) = self.cache.lock() {
            if let Some(val) = cache.get(key) {
                let val = val.clone();
                cache.bytes_served += val.len() as u64;
                cache.calls_served += 1;
                // Persistent stats for cache hits too
                drop(cache);
                if let Ok(svc) = self.service() {
                    svc.track_call(tool, val.len());
                }
                return Ok(val);
            }
        }

        // Compute
        let result = compute()?;

        // Apply scope filter
        let result = apply_scope(&result, scope);

        // Store + track
        if let Ok(mut cache) = self.cache.lock() {
            cache.bytes_served += result.len() as u64;
            cache.calls_served += 1;
            cache.put(key, result.clone());
        }

        // Persistent stats
        if let Ok(svc) = self.service() {
            svc.track_call(tool, result.len());
        }

        Ok(result)
    }
}
