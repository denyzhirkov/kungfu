use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageStats {
    pub total_calls: u64,
    pub total_bytes_served: u64,
    /// Cumulative on-disk size of the source files served results referenced — the bytes an agent
    /// would have read by opening them directly. Accrues only on paths that compute a baseline
    /// (the MCP adapter); serde-default so pre-existing `stats.json` files load as 0.
    #[serde(default)]
    pub total_raw_bytes_baseline: u64,
    pub per_command: HashMap<String, u64>,
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
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&path, json)
    }

    pub fn record(&mut self, command: &str, bytes_served: u64, raw_baseline: u64) {
        self.total_calls += 1;
        self.total_bytes_served += bytes_served;
        self.total_raw_bytes_baseline += raw_baseline;
        *self.per_command.entry(command.to_string()).or_default() += 1;

        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        if self.first_used.is_none() {
            self.first_used = Some(now.clone());
        }
        self.last_used = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_accumulates_served_and_baseline() {
        let mut s = UsageStats::default();
        s.record("ask_context", 100, 900);
        s.record("find_symbol", 50, 300);
        assert_eq!(s.total_calls, 2);
        assert_eq!(s.total_bytes_served, 150);
        assert_eq!(s.total_raw_bytes_baseline, 1200);
    }

    #[test]
    fn baseline_defaults_to_zero_for_legacy_stats() {
        let legacy = r#"{"total_calls":3,"total_bytes_served":10,"per_command":{},"first_used":null,"last_used":null}"#;
        let s: UsageStats = serde_json::from_str(legacy).unwrap();
        assert_eq!(s.total_calls, 3);
        assert_eq!(s.total_raw_bytes_baseline, 0);
    }
}
