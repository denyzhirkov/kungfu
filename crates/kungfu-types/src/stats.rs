use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Per-tool counters. Legacy `stats.json` stored a bare call count per tool; it loads as
/// `calls` with unknown (0) bytes.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct CommandStats {
    pub calls: u64,
    /// Bytes of results whose baseline was measured (MCP) — keeps raw/served undiluted.
    pub bytes_served: u64,
    pub raw_bytes_baseline: u64,
    /// Bytes of results served without a baseline measurement (CLI).
    pub unmeasured_bytes_served: u64,
}

impl<'de> Deserialize<'de> for CommandStats {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Legacy(u64),
            Full {
                #[serde(default)]
                calls: u64,
                #[serde(default)]
                bytes_served: u64,
                #[serde(default)]
                raw_bytes_baseline: u64,
                #[serde(default)]
                unmeasured_bytes_served: u64,
            },
        }
        Ok(match Repr::deserialize(d)? {
            Repr::Legacy(calls) => CommandStats {
                calls,
                ..Default::default()
            },
            Repr::Full {
                calls,
                bytes_served,
                raw_bytes_baseline,
                unmeasured_bytes_served,
            } => CommandStats {
                calls,
                bytes_served,
                raw_bytes_baseline,
                unmeasured_bytes_served,
            },
        })
    }
}

/// Compression ratio `raw / served`, or `None` when there is no file-backed data (no honest number exists).
pub fn compression_ratio(raw: u64, served: u64) -> Option<f64> {
    (served > 0 && raw > 0).then(|| raw as f64 / served as f64)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageStats {
    pub total_calls: u64,
    /// All bytes served, file-backed and path-less together.
    pub total_bytes_served: u64,
    /// Cumulative on-disk size of the source files served results referenced — the bytes an agent
    /// would have read by opening them directly. Accrues only on paths that compute a baseline
    /// (the MCP adapter); serde-default so pre-existing `stats.json` files load as 0.
    #[serde(default)]
    pub total_raw_bytes_baseline: u64,
    /// Baseline of file-backed results since `counting_since` — the ratio numerator. Kept apart
    /// from `total_raw_bytes_baseline` (legacy running total) so both ratio sides share one window.
    #[serde(default)]
    pub file_backed_raw_baseline: u64,
    /// Results that referenced source files (baseline > 0). The compression ratio is computed
    /// over these only. Pre-upgrade history mixed both populations and cannot be split, so these
    /// counters (and the path-less ones) start at `counting_since`.
    #[serde(default)]
    pub file_backed_calls: u64,
    #[serde(default)]
    pub file_backed_bytes_served: u64,
    /// Results with no file baseline (memory_*, project_status, ...).
    #[serde(default)]
    pub pathless_calls: u64,
    #[serde(default)]
    pub pathless_bytes_served: u64,
    /// Results served by callers that don't measure a baseline (the CLI): they may reference
    /// files, so they are neither file-backed nor path-less.
    #[serde(default)]
    pub unmeasured_calls: u64,
    #[serde(default)]
    pub unmeasured_bytes_served: u64,
    /// When the split counters started accruing (first call after upgrade).
    #[serde(default)]
    pub counting_since: Option<String>,
    pub per_command: HashMap<String, CommandStats>,
    pub first_used: Option<String>,
    pub last_used: Option<String>,
}

impl UsageStats {
    pub fn load(kungfu_dir: &Path) -> Self {
        let path = kungfu_dir.join("stats.json");
        if !path.exists() {
            return Self::default();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, kungfu_dir: &Path) -> std::io::Result<()> {
        let path = kungfu_dir.join("stats.json");
        let tmp = kungfu_dir.join(format!("stats.json.{}.tmp", std::process::id()));
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    }

    /// `raw_baseline`: `Some(n)` when the caller measured the referenced files (0 = path-less
    /// result), `None` when it did not measure at all.
    pub fn record(&mut self, command: &str, bytes_served: u64, raw_baseline: Option<u64>) {
        self.total_calls += 1;
        self.total_bytes_served += bytes_served;
        let cmd = self.per_command.entry(command.to_string()).or_default();
        cmd.calls += 1;
        match raw_baseline {
            None => {
                self.unmeasured_calls += 1;
                self.unmeasured_bytes_served += bytes_served;
                cmd.unmeasured_bytes_served += bytes_served;
            }
            Some(raw) => {
                self.total_raw_bytes_baseline += raw;
                cmd.bytes_served += bytes_served;
                cmd.raw_bytes_baseline += raw;
                if raw > 0 {
                    self.file_backed_calls += 1;
                    self.file_backed_bytes_served += bytes_served;
                    self.file_backed_raw_baseline += raw;
                } else {
                    self.pathless_calls += 1;
                    self.pathless_bytes_served += bytes_served;
                }
            }
        }

        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        if self.counting_since.is_none() {
            self.counting_since = Some(now.clone());
        }
        if self.first_used.is_none() {
            self.first_used = Some(now.clone());
        }
        self.last_used = Some(now);
    }

    /// Tokens saved over the same window as the ratio; 0 when the ratio is `None`.
    pub fn file_backed_tokens_saved(&self) -> u64 {
        if self.file_backed_ratio().is_none() {
            return 0;
        }
        self.file_backed_raw_baseline
            .saturating_sub(self.file_backed_bytes_served)
            / 4
    }

    /// File-backed ratio since `counting_since`; `None` when there is no file-backed data.
    pub fn file_backed_ratio(&self) -> Option<f64> {
        compression_ratio(self.file_backed_raw_baseline, self.file_backed_bytes_served)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_accumulates_served_and_baseline() {
        let mut s = UsageStats::default();
        s.record("ask_context", 100, Some(900));
        s.record("find_symbol", 50, Some(300));
        assert_eq!(s.total_calls, 2);
        assert_eq!(s.total_bytes_served, 150);
        assert_eq!(s.total_raw_bytes_baseline, 1200);
        assert_eq!(s.file_backed_raw_baseline, 1200);
    }

    #[test]
    fn baseline_defaults_to_zero_for_legacy_stats() {
        let legacy = r#"{"total_calls":3,"total_bytes_served":10,"per_command":{},"first_used":null,"last_used":null}"#;
        let s: UsageStats = serde_json::from_str(legacy).unwrap();
        assert_eq!(s.total_calls, 3);
        assert_eq!(s.total_raw_bytes_baseline, 0);
    }

    #[test]
    fn legacy_integer_per_command_loads() {
        let legacy = r#"{"total_calls":5,"total_bytes_served":10,"total_raw_bytes_baseline":7,"per_command":{"ask_context":4,"memory_search":1},"first_used":"a","last_used":"b"}"#;
        let s: UsageStats = serde_json::from_str(legacy).unwrap();
        assert_eq!(s.per_command["ask_context"].calls, 4);
        assert_eq!(s.per_command["ask_context"].bytes_served, 0);
        assert_eq!(s.file_backed_calls, 0);
        assert!(s.counting_since.is_none());
    }

    #[test]
    fn new_format_round_trips() {
        let mut s = UsageStats::default();
        s.record("ask_context", 100, Some(900));
        s.record("memory_list", 40, Some(0));
        let back: UsageStats = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.per_command["ask_context"].raw_bytes_baseline, 900);
        assert_eq!(back.pathless_bytes_served, 40);
        assert_eq!(back.file_backed_bytes_served, 100);
        assert_eq!(back.counting_since, s.counting_since);
    }

    #[test]
    fn pathless_call_does_not_change_ratio() {
        let mut s = UsageStats::default();
        s.record("ask_context", 100, Some(900));
        let before = s.file_backed_ratio();
        s.record("memory_list", 5000, Some(0));
        assert_eq!(s.file_backed_ratio(), before);
        assert_eq!(before, Some(9.0));
        assert_eq!(s.total_bytes_served, 5100);
    }

    #[test]
    fn legacy_baseline_does_not_inflate_lifetime_ratio() {
        let legacy = r#"{"total_calls":100,"total_bytes_served":1000,"total_raw_bytes_baseline":4736569,"per_command":{},"first_used":null,"last_used":null}"#;
        let mut s: UsageStats = serde_json::from_str(legacy).unwrap();
        assert_eq!(s.file_backed_ratio(), None);
        s.record("ask_context", 2000, Some(20000));
        assert_eq!(s.file_backed_ratio(), Some(10.0));
        assert_eq!(s.file_backed_tokens_saved(), 4500);
        assert_eq!(s.total_raw_bytes_baseline, 4756569);
    }

    #[test]
    fn unmeasured_calls_are_separate_and_do_not_dilute_commands() {
        let mut s = UsageStats::default();
        s.record("ask_context", 100, Some(900));
        s.record("ask_context", 7000, None);
        assert_eq!(s.unmeasured_calls, 1);
        assert_eq!(s.unmeasured_bytes_served, 7000);
        assert_eq!(s.pathless_calls, 0);
        assert_eq!(s.file_backed_ratio(), Some(9.0));
        let c = &s.per_command["ask_context"];
        assert_eq!(
            (c.calls, c.bytes_served, c.raw_bytes_baseline),
            (2, 100, 900)
        );
        assert_eq!(c.unmeasured_bytes_served, 7000);
    }

    #[test]
    fn zero_file_backed_ratio_is_none() {
        let mut s = UsageStats::default();
        s.record("memory_list", 50, Some(0));
        assert_eq!(s.file_backed_ratio(), None);
        assert_eq!(compression_ratio(0, 0), None);
    }

    #[test]
    fn save_then_load_roundtrip_leaves_no_tmp() {
        let dir = std::env::temp_dir().join(format!("kf-stats-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = UsageStats::default();
        s.record("find_symbol", 10, Some(50));
        s.save(&dir).unwrap();
        assert!(!dir
            .join(format!("stats.json.{}.tmp", std::process::id()))
            .exists());
        assert_eq!(UsageStats::load(&dir).total_calls, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
