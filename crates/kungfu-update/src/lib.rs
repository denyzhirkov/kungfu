#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

//! Self-update for the kungfu binary: a version check that is cached for 24h and
//! never runs on a request path, plus an explicit `kungfu update` that swaps the
//! binary in place.
//!
//! Two rules shape everything here:
//! * **A failed check is silence.** No network, a rate-limited GitHub API, a
//!   corporate proxy — none of it may turn into an error, a warning storm, or a
//!   non-zero exit code for an unrelated command.
//! * **Every answer says where it came from.** `UpdateStatus::source` reports
//!   cache / network / never-checked / disabled, so "no update" from a stale
//!   cache is distinguishable from "no update" that was actually verified.

pub mod apply;
pub mod cache;
pub mod github;
pub mod version;

use anyhow::Result;
use cache::{CheckCache, CHECK_TTL_SECS};
use kungfu_config::UpdateConfig;
use std::time::Duration;

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Long enough for a slow link, short enough to sit in a SessionStart hook.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Answer came from a live GitHub query.
    Network,
    /// Answer came from the ≤24h cache.
    Cache,
    /// No check has ever completed on this machine.
    NeverChecked,
    /// Checks are turned off by config or environment.
    Disabled,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Network => "network",
            Source::Cache => "cache",
            Source::NeverChecked => "never_checked",
            Source::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone)]
pub struct UpdateStatus {
    pub current: String,
    pub latest: Option<String>,
    pub update_available: bool,
    pub checked_at: Option<u64>,
    pub source: Source,
}

impl UpdateStatus {
    fn unknown(source: Source) -> Self {
        Self {
            current: CURRENT_VERSION.to_string(),
            latest: None,
            update_available: false,
            checked_at: None,
            source,
        }
    }

    /// One line, safe to print on stderr or paste into an agent packet.
    pub fn summary(&self) -> String {
        match (&self.latest, self.update_available) {
            (Some(latest), true) => format!(
                "kungfu {latest} available (you have {}) — run `kungfu update`",
                self.current
            ),
            (Some(latest), false) => {
                format!("kungfu {} is up to date (latest {latest})", self.current)
            }
            (None, _) => match self.source {
                Source::Disabled => format!("kungfu {} — update check disabled", self.current),
                _ => format!(
                    "kungfu {} — latest release unknown (no successful check yet)",
                    self.current
                ),
            },
        }
    }
}

/// Read-only status: never touches the network, costs one small file read.
/// This is what hot paths (CLI epilogue, `project_status`) are allowed to call.
pub fn status_from_cache(config: &UpdateConfig) -> UpdateStatus {
    let config = config.effective();
    if !config.check {
        return UpdateStatus::unknown(Source::Disabled);
    }
    match cache::read() {
        Some(c) if !c.latest.is_empty() => UpdateStatus {
            current: CURRENT_VERSION.to_string(),
            update_available: version::is_newer(&c.latest, CURRENT_VERSION),
            latest: Some(c.latest),
            checked_at: Some(c.checked_at),
            source: Source::Cache,
        },
        _ => UpdateStatus::unknown(Source::NeverChecked),
    }
}

/// Query GitHub and refresh the cache. Only for explicit commands and the
/// background thread — never call this from a tool handler.
pub fn check_now(repo: &str, timeout: Duration) -> Result<UpdateStatus> {
    let latest = github::latest_version(repo, timeout)?;
    let previous = cache::read().unwrap_or_default();
    let record = CheckCache {
        checked_at: cache::now_secs(),
        latest: latest.clone(),
        notified_at: previous.notified_at,
    };
    if let Err(e) = cache::write(&record) {
        // A read-only HOME must not fail the check itself.
        tracing::debug!("could not persist update check: {e:#}");
    }
    Ok(UpdateStatus {
        current: CURRENT_VERSION.to_string(),
        update_available: version::is_newer(&latest, CURRENT_VERSION),
        latest: Some(latest),
        checked_at: Some(record.checked_at),
        source: Source::Network,
    })
}

/// Cached answer when it is fresh, a live query when it is not. Disabled checks
/// short-circuit before any network access.
pub fn ensure_checked(config: &UpdateConfig, repo: &str) -> Result<UpdateStatus> {
    let config = config.effective();
    if !config.check {
        return Ok(UpdateStatus::unknown(Source::Disabled));
    }
    let cached = cache::read().unwrap_or_default();
    if !cached.is_stale(CHECK_TTL_SECS) {
        return Ok(UpdateStatus {
            current: CURRENT_VERSION.to_string(),
            update_available: version::is_newer(&cached.latest, CURRENT_VERSION),
            latest: Some(cached.latest),
            checked_at: Some(cached.checked_at),
            source: Source::Cache,
        });
    }
    check_now(repo, CHECK_TIMEOUT)
}

/// The user-facing "new version" line, rate-limited to once every
/// [`cache::NOTICE_INTERVAL_SECS`]. Returns `None` when there is nothing to say —
/// calling it marks the notice as shown, so call it only when you will print it.
pub fn take_notice(config: &UpdateConfig) -> Option<String> {
    let status = status_from_cache(config);
    if !status.update_available {
        return None;
    }
    let mut record = cache::read()?;
    if !record.notice_due() {
        return None;
    }
    record.notified_at = cache::now_secs();
    let _ = cache::write(&record);
    Some(status.summary())
}

/// One background update job per process, mirroring the embeddings-sync guard.
static UPDATE_JOB_RUNNING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Refresh the check (and, when `auto` is on, install the update) off the
/// request path. No-ops when checks are disabled, when the cache is still fresh,
/// or when a job is already running. Errors are logged at debug and dropped.
pub fn spawn_background_check(config: UpdateConfig) {
    use std::sync::atomic::Ordering;
    let config = config.effective();
    if !config.check {
        return;
    }
    if cache::read().is_some_and(|c| !c.is_stale(CHECK_TTL_SECS)) && !config.auto {
        return;
    }
    if UPDATE_JOB_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    std::thread::spawn(move || {
        match ensure_checked(&config, github::REPO) {
            Ok(status) if status.update_available => {
                let latest = status.latest.unwrap_or_default();
                if config.auto {
                    match apply::apply(github::REPO, &latest, CURRENT_VERSION) {
                        Ok(applied) => tracing::info!(
                            "kungfu auto-updated {} -> {} ({}); restart the session to load it",
                            applied.from,
                            applied.to,
                            applied.checksum.as_str()
                        ),
                        Err(e) => tracing::warn!("auto-update to {latest} failed: {e:#}"),
                    }
                } else {
                    tracing::info!("kungfu {latest} available (running {CURRENT_VERSION})");
                }
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("update check failed: {e:#}"),
        }
        UPDATE_JOB_RUNNING.store(false, Ordering::SeqCst);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_config_reports_disabled_without_touching_the_cache() {
        let cfg = UpdateConfig {
            check: false,
            auto: false,
        };
        let status = status_from_cache(&cfg);
        assert_eq!(status.source, Source::Disabled);
        assert!(!status.update_available);
        assert!(status.latest.is_none());
    }

    #[test]
    fn summary_distinguishes_unknown_from_up_to_date() {
        let unknown = UpdateStatus::unknown(Source::NeverChecked);
        assert!(unknown.summary().contains("unknown"));

        let current = UpdateStatus {
            current: "2.7.0".into(),
            latest: Some("2.7.0".into()),
            update_available: false,
            checked_at: Some(0),
            source: Source::Network,
        };
        assert!(current.summary().contains("up to date"));

        let stale = UpdateStatus {
            current: "2.6.2".into(),
            latest: Some("2.7.0".into()),
            update_available: true,
            checked_at: Some(0),
            source: Source::Cache,
        };
        assert!(stale.summary().contains("kungfu update"));
    }
}
