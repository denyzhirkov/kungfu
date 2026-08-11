//! The 24h check cache: `~/.cache/kungfu/update-check.json`.
//!
//! Shared across projects (same convention as the embedding model cache) so a
//! machine hits the GitHub API at most once a day regardless of how many
//! repos or MCP servers are running. The hot path only ever *reads* this file.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// How long a recorded check stays authoritative.
pub const CHECK_TTL_SECS: u64 = 24 * 60 * 60;

/// How often the "update available" line may be repeated to the user. Without
/// this a pending update would print on every single CLI invocation.
pub const NOTICE_INTERVAL_SECS: u64 = 6 * 60 * 60;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheckCache {
    /// Unix seconds of the last completed network check.
    #[serde(default)]
    pub checked_at: u64,
    /// Latest release tag seen, normalized (no `v`). Empty when never checked.
    #[serde(default)]
    pub latest: String,
    /// Unix seconds of the last time the notice was shown to a human.
    #[serde(default)]
    pub notified_at: u64,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn cache_path() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".cache/kungfu/update-check.json"),
        None => PathBuf::from(".kungfu/update-check.json"),
    }
}

/// Missing, unreadable or corrupt cache is simply "never checked".
pub fn read() -> Option<CheckCache> {
    let content = std::fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn write(cache: &CheckCache) -> Result<()> {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let body = serde_json::to_string(cache)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

impl CheckCache {
    pub fn is_stale(&self, ttl_secs: u64) -> bool {
        let now = now_secs();
        self.latest.is_empty() || now.saturating_sub(self.checked_at) >= ttl_secs
    }

    pub fn notice_due(&self) -> bool {
        now_secs().saturating_sub(self.notified_at) >= NOTICE_INTERVAL_SECS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cache_is_stale() {
        assert!(CheckCache::default().is_stale(CHECK_TTL_SECS));
    }

    #[test]
    fn fresh_check_is_not_stale() {
        let c = CheckCache {
            checked_at: now_secs(),
            latest: "2.6.2".into(),
            notified_at: 0,
        };
        assert!(!c.is_stale(CHECK_TTL_SECS));
    }

    #[test]
    fn old_check_is_stale() {
        let c = CheckCache {
            checked_at: now_secs().saturating_sub(CHECK_TTL_SECS + 1),
            latest: "2.6.2".into(),
            notified_at: 0,
        };
        assert!(c.is_stale(CHECK_TTL_SECS));
    }

    #[test]
    fn notice_is_due_only_after_the_interval() {
        let mut c = CheckCache {
            checked_at: now_secs(),
            latest: "2.7.0".into(),
            notified_at: now_secs(),
        };
        assert!(!c.notice_due());
        c.notified_at = now_secs().saturating_sub(NOTICE_INTERVAL_SECS + 1);
        assert!(c.notice_due());
    }
}
