#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use anyhow::{bail, Context, Result};
use kungfu_config::KungfuConfig;
use kungfu_types::project::ProjectMeta;
use std::path::{Path, PathBuf};
use tracing::info;

pub const KUNGFU_DIR: &str = ".kungfu";
pub const CONFIG_FILE: &str = "config.toml";
pub const PROJECT_FILE: &str = "project.json";
pub const KUNGFU_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct Project {
    pub root: PathBuf,
    pub kungfu_dir: PathBuf,
    pub config: KungfuConfig,
    pub meta: ProjectMeta,
}

impl Project {
    pub fn open(start_dir: &Path) -> Result<Self> {
        let root = find_project_root(start_dir)?;
        let kungfu_dir = root.join(KUNGFU_DIR);

        if !kungfu_dir.exists() {
            bail!("not a kungfu project (no .kungfu directory found). Run 'kungfu init' first.");
        }

        let config_path = kungfu_dir.join(CONFIG_FILE);
        let config = KungfuConfig::load_merged(Some(&config_path))?;

        let meta_path = kungfu_dir.join(PROJECT_FILE);
        let meta_content =
            std::fs::read_to_string(&meta_path).with_context(|| "failed to read project.json")?;
        let meta: ProjectMeta =
            serde_json::from_str(&meta_content).with_context(|| "failed to parse project.json")?;

        Ok(Self {
            root,
            kungfu_dir,
            config,
            meta,
        })
    }

    pub fn index_dir(&self) -> PathBuf {
        self.kungfu_dir.join("index")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.kungfu_dir.join("cache")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.kungfu_dir.join("logs")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.kungfu_dir.join("state")
    }
}

pub fn init_project(root: &Path) -> Result<Project> {
    let kungfu_dir = root.join(KUNGFU_DIR);

    if kungfu_dir.exists() {
        bail!("kungfu is already initialized in {}", root.display());
    }

    // Create directory structure
    std::fs::create_dir_all(kungfu_dir.join("index"))?;
    std::fs::create_dir_all(kungfu_dir.join("cache").join("summaries"))?;
    std::fs::create_dir_all(kungfu_dir.join("cache").join("queries"))?;
    std::fs::create_dir_all(kungfu_dir.join("logs"))?;
    std::fs::create_dir_all(kungfu_dir.join("state"))?;

    // Detect project name from directory
    let project_name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unnamed")
        .to_string();

    // Create config
    let config = KungfuConfig::default().with_project_name(&project_name);
    let config_path = kungfu_dir.join(CONFIG_FILE);
    config.save(&config_path)?;

    // Create project metadata
    let meta = ProjectMeta {
        name: project_name,
        root: root.to_string_lossy().to_string(),
        created_at: chrono::Utc::now(),
        kungfu_version: KUNGFU_VERSION.to_string(),
    };
    let meta_json = serde_json::to_string_pretty(&meta)?;
    std::fs::write(kungfu_dir.join(PROJECT_FILE), meta_json)?;

    info!("initialized kungfu project at {}", root.display());

    Ok(Project {
        root: root.to_path_buf(),
        kungfu_dir,
        config,
        meta,
    })
}

/// Walk up from start_dir looking for project root indicators
pub fn find_project_root(start_dir: &Path) -> Result<PathBuf> {
    let mut current = start_dir.to_path_buf();

    loop {
        // Check for .kungfu directory first
        if current.join(KUNGFU_DIR).exists() {
            return Ok(current);
        }

        // Check for common project root indicators
        let root_markers = [
            ".git",
            "Cargo.toml",
            "package.json",
            "go.mod",
            "pyproject.toml",
            "setup.py",
            "Makefile",
        ];

        for marker in &root_markers {
            if current.join(marker).exists() {
                return Ok(current);
            }
        }

        if !current.pop() {
            break;
        }
    }

    // Fallback to start_dir
    Ok(start_dir.to_path_buf())
}
