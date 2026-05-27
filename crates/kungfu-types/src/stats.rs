use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageStats {
    pub total_calls: u64,
    pub total_bytes_served: u64,
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

    pub fn record(&mut self, command: &str, bytes_served: u64) {
        self.total_calls += 1;
        self.total_bytes_served += bytes_served;
        *self.per_command.entry(command.to_string()).or_default() += 1;

        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        if self.first_used.is_none() {
            self.first_used = Some(now.clone());
        }
        self.last_used = Some(now);
    }
}
