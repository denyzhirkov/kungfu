#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KungfuConfig {
    #[serde(default = "default_project_name")]
    pub project_name: String,

    #[serde(default)]
    pub index_hidden: bool,

    #[serde(default)]
    pub follow_symlinks: bool,

    #[serde(default)]
    pub watch: bool,

    #[serde(default)]
    pub ignore: IgnoreConfig,

    #[serde(default)]
    pub languages: LanguagesConfig,

    #[serde(default)]
    pub search: SearchConfig,

    #[serde(default)]
    pub index: IndexConfig,

    #[serde(default)]
    pub git: GitConfig,
}

fn default_project_name() -> String {
    "unnamed".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IgnoreConfig {
    #[serde(default = "default_ignore_paths")]
    pub paths: Vec<String>,
}

fn default_ignore_paths() -> Vec<String> {
    vec![
        "node_modules".into(),
        "dist".into(),
        "build".into(),
        ".git".into(),
        "target".into(),
        "build".into(),
        ".kungfu".into(),
        "__pycache__".into(),
        ".venv".into(),
        "vendor".into(),
    ]
}

impl Default for IgnoreConfig {
    fn default() -> Self {
        Self {
            paths: default_ignore_paths(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguagesConfig {
    #[serde(default = "default_languages")]
    pub enabled: Vec<String>,
}

fn default_languages() -> Vec<String> {
    vec![
        "typescript".into(),
        "javascript".into(),
        "rust".into(),
        "go".into(),
        "python".into(),
        "java".into(),
        "csharp".into(),
        "kotlin".into(),
        "c".into(),
        "cpp".into(),
        "json".into(),
        "markdown".into(),
        "yaml".into(),
        "toml".into(),
    ]
}

impl Default for LanguagesConfig {
    fn default() -> Self {
        Self {
            enabled: default_languages(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    #[serde(default = "default_budget")]
    pub default_budget: String,

    #[serde(default = "default_top_k")]
    pub default_top_k: usize,

    #[serde(default = "default_max_lines")]
    pub max_lines: usize,
}

fn default_budget() -> String {
    "small".into()
}
fn default_top_k() -> usize {
    5
}
fn default_max_lines() -> usize {
    40
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            default_budget: default_budget(),
            default_top_k: default_top_k(),
            max_lines: default_max_lines(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexConfig {
    #[serde(default = "default_store_backend")]
    pub store_backend: String,

    #[serde(default = "default_true")]
    pub incremental: bool,

    /// Files larger than this are recorded by name only — their content is not
    /// read or parsed. Guards against a single huge/generated file blowing up
    /// memory during (re)indexing.
    #[serde(default = "default_max_file_bytes")]
    pub max_file_bytes: u64,
}

fn default_store_backend() -> String {
    "json".into()
}
fn default_true() -> bool {
    true
}
fn default_max_file_bytes() -> u64 {
    2 * 1024 * 1024
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            store_backend: default_store_backend(),
            incremental: true,
            max_file_bytes: default_max_file_bytes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Default for KungfuConfig {
    fn default() -> Self {
        Self {
            project_name: default_project_name(),
            index_hidden: false,
            follow_symlinks: false,
            watch: false,
            ignore: IgnoreConfig::default(),
            languages: LanguagesConfig::default(),
            search: SearchConfig::default(),
            index: IndexConfig::default(),
            git: GitConfig::default(),
        }
    }
}

impl KungfuConfig {
    pub fn with_project_name(mut self, name: &str) -> Self {
        self.project_name = name.to_string();
        self
    }

    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {}", path.display()))?;
        let config: KungfuConfig =
            toml::from_str(&content).with_context(|| "failed to parse config")?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self).context("failed to serialize config")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)
            .with_context(|| format!("failed to write config: {}", path.display()))?;
        Ok(())
    }

    pub fn global_config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("kungfu").join("config.toml"))
    }

    /// Load merged config: global defaults <- project config <- overrides
    pub fn load_merged(project_config_path: Option<&Path>) -> Result<Self> {
        let mut config = KungfuConfig::default();

        // Load global config if exists
        if let Some(global_path) = Self::global_config_path() {
            if global_path.exists() {
                let global = Self::load(&global_path)?;
                config = global;
            }
        }

        // Override with project config if exists
        if let Some(proj_path) = project_config_path {
            if proj_path.exists() {
                config = Self::load(proj_path)?;
            }
        }

        Ok(config)
    }
}
